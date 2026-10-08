"""Fixed lifecycle probes, driven by native usage and explicit Crew actions.

Step windows attribute observations in time, not provider-internal causality.
Only numeric evidence and synthetic marker metadata leave the native reader.
"""

from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import time

from usage import records

WORKERS = ("claude-worker", "codex-worker")
FOLD_MARKER = "cache-fold-01"
FOLD_MESSAGE = "cache-fold-injected-01"
CONTROL_MARKER = "cache-control-01"


def iso():
    return datetime.now(timezone.utc).isoformat()


def seconds(stamp):
    return datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()


def seed(workspace):
    context = workspace / "context"
    context.mkdir()
    for part in range(6):
        lines = []
        for row in range(400):
            key = f"cache-context-{part:02d}-{row:04d}"
            token = hashlib.sha256(key.encode()).hexdigest()[:24]
            lines.append(f"{key} alpha bravo charlie delta {token} fixed synthetic data")
        (context / f"part-{part:02d}.txt").write_text("\n".join(lines) + "\n")


def native_markers(path, marker, message_marker=None):
    message_marker = message_marker or marker
    pending = {}
    thinking_seen = set()
    result = {"folded_rows": 0, "idle_rows": 0, "post_fold_thinking_blocks": 0,
              "thinking_drops": [], "pending_tool_ids": [], "turn_ends": 0}
    for value in records(path):
        kind = value.get("type")
        message = value.get("message") or {}
        content = message.get("content", [])
        if kind == "assistant" and isinstance(content, list):
            for block_index, block in enumerate(content):
                if block.get("type") == "tool_use" and marker in str((block.get("input") or {}).get("command", "")):
                    pending[block["id"]] = value.get("timestamp")
                if block.get("type") == "thinking":
                    key = (message.get("id", value.get("uuid")), block_index)
                    if key not in thinking_seen and result["folded_rows"]:
                        result["post_fold_thinking_blocks"] += 1
                    thinking_seen.add(key)
        if kind == "user":
            if isinstance(content, list):
                for block in content:
                    if block.get("type") == "tool_result":
                        pending.pop(block.get("tool_use_id"), None)
                    if block.get("type") == "text" and message_marker in block.get("text", ""):
                        result["folded_rows" if value.get("isMeta") else "idle_rows"] += 1
            elif isinstance(content, str) and message_marker in content:
                result["idle_rows"] += 1
        if kind == "system" and value.get("subtype") == "turn_duration":
            result["turn_ends"] += 1
        attachment = value.get("attachment") or {}
        if kind == "attachment" and attachment.get("type") == "thinking_drop":
            result["thinking_drops"].append({
                "timestamp": value.get("timestamp"),
                "request_id": attachment.get("requestId", value.get("requestId")),
                "client_change_kinds": (attachment.get("clientChange") or {}).get("kinds"),
                "newly_dropped_reason": (attachment.get("newlyDropped") or {}).get("reason"),
                "newly_dropped_blocks": (attachment.get("newlyDropped") or {}).get("blockCount"),
                "first_changed_message_index": (attachment.get("clientChange") or {}).get("firstChangedMessageIndex"),
            })
    result["pending_tool_ids"] = list(pending)
    return result


def prefix_record(raw):
    return {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}


def prefix_delta(before, after):
    common = next((i for i, (a, b) in enumerate(zip(before, after)) if a != b), min(len(before), len(after)))
    return {"before": prefix_record(before), "after": prefix_record(after),
            "common_prefix_bytes": common, "changed": before != after,
            "scope": "Crew briefing file only; not the provider's full effective prompt"}


def aligned_usage(before, after):
    """Before indices are fixed at the action boundary; keep the first request."""
    out = {}
    for agent in after:
        prior = before.get(agent["agent"], 0)
        selected = agent.get("requests", [])[prior:]
        total = sum(r["input_tokens"] for r in selected)
        read = sum(r["cached_tokens"] for r in selected)
        out[agent["agent"]] = {
            "requests": len(selected), "input_tokens": total, "cache_read_tokens": read,
            "uncached_input_tokens": total - read, "hit_rate": read / total if total else None,
            "cost_usd_lower": sum(r["cost_usd_lower"] for r in selected),
            "cost_usd_upper": sum(r["cost_usd_upper"] for r in selected),
            "first_request": selected[0] if selected else None,
        }
    return out


class Lifecycle:
    def __init__(self, workspace, command, long_idle=False):
        self.workspace, self.command = workspace, command
        self.plan = [("warm", "warm", None, None)]
        for label, kind in (("same", None), ("added", "add"), ("removed", "remove")):
            if kind:
                self.plan.append((kind, f"roster-{label}", None, None))
            for agent in WORKERS:
                self.plan.append(("restart", f"{label}-briefing-{agent}", agent, label != "same"))
        self.plan += [
            ("task", "control-window", WORKERS[0], CONTROL_MARKER),
            ("message", "idle-delivery-control", WORKERS[0], CONTROL_MARKER),
            ("fold", "folded-delivery", WORKERS[0], FOLD_MARKER),
            ("message", "after-fold-probe", WORKERS[0], "cache-fold-next-turn"),
            ("load", "large-context", WORKERS[0], None),
            ("compact", "forced-compaction", WORKERS[0], None),
            ("message", "post-compaction", WORKERS[0], "cache-compact-probe"),
            ("idle", "over-five-minutes", None, 310),
            ("probes", "after-five-minutes", None, 300),
        ]
        if long_idle:
            self.plan += [("idle", "over-one-hour", None, 3665),
                          ("probes", "after-one-hour", None, 3600)]
        self.index = 0
        self.current = None
        self.history = []
        self.waiting_since = None
        self.last_idle = None
        self.session_ids = {}

    def is_idle(self):
        return self.current is not None and self.plan[self.index][0] == "idle"

    def transient_stop(self, agent):
        if self.index >= len(self.plan):
            return False
        kind, _, target, _ = self.plan[self.index]
        return kind == "restart" and target == agent["id"] and agent["status"] == "stopped"

    def briefing(self, agent):
        return (self.workspace / ".crew/agents" / agent / "briefing.md").read_bytes()

    def task(self, identity, agent, description):
        self.command("task", "add", "--id", identity, "--title", identity,
                     "--agent", agent, "--description", description)

    def marker_message(self, agent, marker):
        self.command("send", agent,
                     f"[{marker}] Read SPEC.md, independently recompute the paid-invoice checksum, "
                     "and run one local calculation tool to check it. If a task is in progress, "
                     "continue it through its required file, git_commit and task_complete before "
                     "ending your turn. Otherwise reply with one short line. "
                     "Do not create a task or ask a question.", "--type", "message")

    def start(self, snapshot, usage):
        kind, label, agent, arg = self.plan[self.index]
        agents = {a["id"]: a for a in snapshot["agents"]}
        self.current = {"step": label, "kind": kind, "started_at": iso(),
                        "request_offsets": {a["agent"]: a.get("request_count", 0) for a in usage}}
        self.started = time.monotonic()
        if agent:
            self.current["agent"] = agent
            self.current["session_id_before"] = agents[agent].get("session_id")
        if kind == "restart":
            self.prefix_before = self.briefing(agent)
            self.current["pid_before"] = agents[agent].get("pid")
            self.command("agent", "restart", agent)
        elif kind == "add":
            self.command("agent", "add", "--name", "cache-observer", "--runtime", "shell",
                         "--role", "Temporary scripted roster observer; never calls a model.")
        elif kind == "remove":
            self.command("agent", "remove", "cache-observer")
        elif kind in ("task", "fold"):
            call = json.dumps({"command": f"sleep 8 # {arg}", "run_in_background": False, "timeout": 15000})
            self.task(label, agent, f"Call Bash exactly once with these input fields: {call}. "
                      "The foreground wait is the measurement; do not set run_in_background=true "
                      "or append &, because that invalidates this benchmark. "
                      "After it returns, reason through the paid invoice checksum from SPEC.md and "
                      "invoices.csv and verify with another local tool call. Handle any incoming "
                      "benchmark marker messages while preserving this card. Write " + label + ".txt containing exactly "
                      "'4 10206' and newline, commit, then task_complete. Do not read context/ yet.")
        elif kind == "message":
            self.marker_message(agent, arg)
        elif kind == "load":
            self.task(label, agent, "Read all six context/part-*.txt files as full text with the Read "
                      "tool or a separate cat command per file. Hashes, tails and partial reads do "
                      "not satisfy this probe. The content is synthetic and need not be interpreted. "
                      "Write context-loaded.txt listing the six basenames, commit and task_complete.")
        elif kind == "compact":
            row = next(a for a in usage if a["agent"] == agent)
            size = row["requests"][-1]["input_tokens"]
            if size < 65_000:
                raise RuntimeError(f"compaction needs observed input >=65000 tokens; got {size}")
            self.current["input_tokens_before"] = size
            self.current["compactions_before"] = agents[agent].get("compactions", 0)
            self.command("agent", "compact", agent, "--keep",
                         "Keep benchmark scope, fixed roster, SPEC.md's 4 paid invoices and 10206 checksum. "
                         "Drop the synthetic context file contents. Wait for the harness's next probe.")
        elif kind == "idle":
            self.current["requested_seconds"] = arg
            self.current["last_request_before"] = {
                a["agent"]: a["requests"][-1]["timestamp"] for a in usage if a.get("requests")}
        elif kind == "probes":
            self.current["minimum_gap_seconds"] = arg
            for worker in WORKERS:
                self.task(f"{label}-{worker}", worker,
                          f"Read SPEC.md and invoices.csv. Recompute the paid count and checksum, "
                          f"write {label}.txt containing exactly '4 10206' plus newline, "
                          "commit and task_complete. Do not read context/. Then wait quietly.")

    def tick(self, snapshot, usage, stable_for):
        agents = {a["id"]: a for a in snapshot["agents"]}
        tasks = {t["id"]: t["status"] for t in snapshot.get("tasks", [])}
        quiet = len([a for a in snapshot["agents"] if a["runtime"] != "shell"]) == 3 and all(
            a["status"] in ("idle", "done", "waiting") for a in snapshot["agents"])
        rows = {a["agent"]: a for a in usage}
        if self.index == len(self.plan):
            return True
        kind, label, agent, arg = self.plan[self.index]
        if self.current is None:
            if kind != "warm" and (not quiet or stable_for < 3):
                return False
            self.start(snapshot, usage)
            return False
        if kind != "idle" and time.monotonic() - self.started > 180:
            raise RuntimeError(f"lifecycle step timed out: {label}")
        done = False
        if kind == "warm":
            done = all(tasks.get(t) == "done" for t in ("warm-claude", "warm-codex"))
        elif kind == "restart":
            changed_pid = agents[agent].get("pid") != self.current["pid_before"]
            done = changed_pid and rows[agent].get("request_count", 0) > self.current["request_offsets"][agent]
            if done and agents[agent].get("session_id") != self.current["session_id_before"]:
                raise RuntimeError("restart did not preserve the session id")
        elif kind == "add":
            done = "cache-observer" in agents
        elif kind == "remove":
            done = "cache-observer" not in agents
        elif kind in ("task", "fold", "load"):
            if kind == "fold" and "sent_at" not in self.current:
                markers = native_markers(Path(rows[agent]["source_path"]), arg, FOLD_MESSAGE)
                if markers["pending_tool_ids"]:
                    self.current["pending_tool_ids_at_send"] = markers["pending_tool_ids"]
                    self.current["sent_at"] = iso()
                    self.marker_message(agent, FOLD_MESSAGE)
            done = tasks.get(label) == "done"
        elif kind == "message":
            done = rows[agent].get("request_count", 0) > self.current["request_offsets"][agent]
        elif kind == "compact":
            done = agents[agent].get("compactions", 0) > self.current["compactions_before"]
        elif kind == "idle":
            if not quiet or stable_for < 3:
                self.started = time.monotonic()
                self.current["started_at"] = iso()
                self.current["last_request_before"] = {
                    a["agent"]: a["requests"][-1]["timestamp"] for a in usage if a.get("requests")}
            elapsed = time.monotonic() - self.started
            done = quiet and elapsed > arg
            if done:
                self.current["observed_quiet_seconds"] = elapsed
                self.last_idle = self.current
        elif kind == "probes":
            done = all(tasks.get(f"{label}-{worker}") == "done" for worker in WORKERS)
        if not done or not quiet or stable_for < 3:
            return False
        expected_files = []
        if kind == "warm":
            expected_files = [(worker, f"warm-{worker.split('-')[0]}.txt") for worker in WORKERS]
        elif kind in ("task", "fold"):
            expected_files = [(agent, label + ".txt")]
        elif kind == "probes":
            expected_files = [(worker, label + ".txt") for worker in WORKERS]
        for worker, name in expected_files:
            path = Path(agents[worker]["workdir"]) / name
            if path.is_symlink() or path.read_text() != "4 10206\n":
                raise RuntimeError(f"lifecycle output oracle failed: {worker}/{name}")
        self.current["ended_at"] = iso()
        self.current["usage"] = aligned_usage(self.current["request_offsets"], usage)
        if kind == "restart":
            self.current["briefing"] = prefix_delta(self.prefix_before, self.briefing(agent))
            self.current["session_id_after"] = agents[agent].get("session_id")
            if self.current["briefing"]["changed"] != arg:
                raise RuntimeError(f"unexpected briefing change for {label}")
        if kind == "fold":
            markers = native_markers(Path(rows[agent]["source_path"]), arg, FOLD_MESSAGE)
            self.current["native_evidence"] = markers
            if "sent_at" not in self.current or not markers["folded_rows"]:
                raise RuntimeError("mid-turn delivery was not observed as a native folded row")
        if label == "after-fold-probe":
            self.current["native_evidence"] = native_markers(Path(rows[agent]["source_path"]), FOLD_MARKER, FOLD_MESSAGE)
            self.current["thinking_observed_after_fold"] = self.current["native_evidence"]["post_fold_thinking_blocks"] > 0
            request_ids = {r.get("request_id") for r in rows[agent]["requests"][
                self.current["request_offsets"][agent]:] if r.get("request_id")}
            self.current["matched_thinking_drops"] = [drop for drop in self.current["native_evidence"]["thinking_drops"]
                                                       if drop["request_id"] in request_ids]
        if kind == "probes":
            gaps = {}
            for worker in WORKERS:
                first = self.current["usage"][worker]["first_request"]
                if not first:
                    raise RuntimeError("idle probe has no native request")
                gaps[worker] = seconds(first["timestamp"]) - seconds(self.last_idle["last_request_before"][worker])
                if gaps[worker] <= arg:
                    raise RuntimeError("native last-request gap did not exceed the requested threshold")
            self.current["native_request_gap_seconds"] = gaps
        self.current["passed"] = True
        self.history.append(self.current)
        self.current = None
        self.index += 1
        return self.index == len(self.plan)

    def report(self):
        return {"schema": 1, "attribution": "Temporal step windows; not proof of provider-internal cause",
                "steps": self.history, "active_step": self.current,
                "planned_steps": len(self.plan), "completed_steps": len(self.history),
                "complete": len(self.history) == len(self.plan)}
