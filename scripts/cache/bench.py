#!/usr/bin/env python3
"""Run the fixed cache workload without using an existing Crew session."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone

from usage import RATES, collect, totals

HERE = Path(__file__).resolve().parent
EXPECTED = {"paid_invoices": 4, "total_cents": 10206,
            "by_currency": {"EUR": 2205, "USD": 8001}}
CSV = """id,currency,amount,status
inv-01,USD,12.50,paid
inv-02,EUR,20.00,paid
inv-03,USD,999.99,void
inv-04,USD,67.51,paid
inv-05,EUR,2.05,paid
inv-06,EUR,10.00,void
"""
SPEC = """# Invoice summary fixture

Read invoices.csv and count only rows with status `paid`. Every amount has two
decimal places: parse it as integer cents, with no floating-point rounding.
Group the cents by currency. The total_cents field is an arithmetic checksum
across those groups, not a currency conversion or a financial valuation.

summary.json must be an object with exactly three fields: paid_invoices (the
integer count), total_cents (integer checksum), and by_currency (currency to
integer cents). No other files or network calls are needed for the calculation.
The rows, this specification and the task descriptions are fixed across runs.
"""
FOLLOWUP = (
    'Read task_get("audit"), cherry-pick its commit if needed, and re-read '
    'summary.json and audit.md. Write followup.md containing exactly '
    '"Rechecked after idle: 4 paid invoices, 10206 cents checksum.\\n" '
    '(the final \\n means a newline). Commit and task_complete with the commit id. '
    'The assigned reviewer handles the final review. Do not create more work.'
)


def utc():
    return datetime.now(timezone.utc).isoformat()


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def isolated_environment(output, runtime):
    # Keep runtime authentication in the normal Crew launch path. No credential
    # file is read or copied by this harness. Crew creates private agent homes.
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("CREW_", "GIT_"))}
    for key in ("CODEX_SESSION_ID", "CODEX_THREAD_ID", "CLAUDECODE",
                "CLAUDE_CODE_ENTRYPOINT", "DBUS_SESSION_BUS_ADDRESS", "DISPLAY",
                "WAYLAND_DISPLAY", "XAUTHORITY", "SSH_AUTH_SOCK"):
        env.pop(key, None)
    env.update(XDG_RUNTIME_DIR=str(runtime),
               GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null",
               PYTHONDONTWRITEBYTECODE="1")
    return env


def invoke(argv, env, cwd, timeout=20):
    return subprocess.run([str(x) for x in argv], env=env, cwd=cwd, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=timeout, check=False)


def runtime_versions(env, cwd):
    versions = {}
    for program in ("claude", "codex"):
        result = invoke([program, "--version"], env, cwd)
        if result.returncode or not result.stdout.strip():
            raise RuntimeError(f"{program} did not report its CLI version; cannot compare runs")
        versions[program] = result.stdout.strip()
    return versions


def process_identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        if fields[0] == "Z":
            return None
        return int(fields[1]), int(fields[19])  # parent pid, start time
    except (OSError, ValueError, IndexError):
        return None


def remember_descendants(owned):
    """Track exact process identities; never select another team by its name."""
    processes = {}
    for entry in Path("/proc").iterdir():
        if entry.name.isdigit():
            identity = process_identity(int(entry.name))
            if identity:
                processes[int(entry.name)] = identity
    changed = True
    while changed:
        changed = False
        for pid, (parent, started) in processes.items():
            parent_identity = processes.get(parent)
            if (pid not in owned and parent in owned and parent_identity
                    and parent_identity[1] == owned[parent]):
                owned[pid] = started
                changed = True


def still_owned(owned):
    return [pid for pid, started in owned.items()
            if (current := process_identity(pid)) and current[1] == started]


def fixture(output, env, sandbox_mode="required", scenario="short"):
    workspace = output / "workspace"
    workspace.mkdir()
    (workspace / "SPEC.md").write_text(SPEC)
    (workspace / "invoices.csv").write_text(CSV)
    if scenario == "lifecycle":
        from lifecycle import seed
        seed(workspace)
    git_env = dict(env, GIT_AUTHOR_NAME="Cache benchmark",
                   GIT_AUTHOR_EMAIL="cache-bench@example.invalid",
                   GIT_COMMITTER_NAME="Cache benchmark",
                   GIT_COMMITTER_EMAIL="cache-bench@example.invalid",
                   GIT_AUTHOR_DATE="2026-10-01T00:00:00Z",
                   GIT_COMMITTER_DATE="2026-10-01T00:00:00Z")
    for args in (["init", "--initial-branch=main"],
                 ["config", "user.name", "Cache benchmark"],
                 ["config", "user.email", "cache-bench@example.invalid"],
                 ["add", "."],
                 ["commit", "-m", "Seed the fixed invoice workload"]):
        command = invoke(["git", *args], git_env, workspace)
        if command.returncode:
            raise RuntimeError(f"fixture git {args[0]} failed: {command.stderr.strip()}")
    template = (HERE / ("team-lifecycle.yaml" if scenario == "lifecycle" else "team.yaml")).read_text()
    (output / "team.yaml").write_text(template.replace(
        "@WORKSPACE@", json.dumps(str(workspace))).replace("@SANDBOX@", sandbox_mode))
    return workspace


def inspect_daemon(workspace, runtime):
    """Only the daemon born in this new, private workspace can be stopped."""
    path = workspace / ".crew" / "daemon.json"
    if not path.exists():
        return None
    info = json.loads(path.read_text())
    if Path(info["workspace"]).resolve() != workspace:
        raise RuntimeError("fixture daemon workspace mismatch")
    if not Path(info["socket"]).is_relative_to(runtime):
        raise RuntimeError("fixture daemon socket escaped the private runtime directory")
    identity = process_identity(info["pid"])
    if identity and info.get("pid_start") != identity[1]:
        raise RuntimeError("fixture daemon process identity mismatch")
    return info


def evidence(snapshot, workspace):
    tasks = {t["id"]: t["status"] for t in snapshot.get("tasks", [])}
    messages = snapshot.get("messages", [])
    # Bodies stay in the private run directory. The public report keeps counts.
    kinds = {}
    for message in messages:
        kind = message.get("type", message.get("kind", "unknown"))
        kinds[kind] = kinds.get(kind, 0) + 1
    outputs = {}
    for agent in snapshot.get("agents", []):
        workdir = Path(agent["workdir"])
        if not workdir.resolve().is_relative_to(workspace):
            raise RuntimeError("agent worktree escaped the fixture workspace")
        for name in ("summary.json", "audit.md", "followup.md"):
            path = workdir / name
            if path.is_file() and not path.is_symlink():
                key = f'{agent["id"]}/{name}'
                outputs[key] = {"sha256": digest(path)}
                if name == "summary.json":
                    try:
                        outputs[key]["valid"] = json.loads(path.read_text()) == EXPECTED
                    except ValueError:
                        outputs[key]["valid"] = False
                elif name == "audit.md":
                    outputs[key]["valid"] = "Summary verified against invoices.csv." in path.read_text()
                else:
                    outputs[key]["valid"] = path.read_text() == (
                        "Rechecked after idle: 4 paid invoices, 10206 cents checksum.\n")
    return {"tasks": tasks, "message_kinds": kinds, "outputs": outputs}


def validate_evidence(result):
    return (all(result["tasks"].get(t) == "done" for t in ("summarize", "audit", "followup"))
            and result["message_kinds"].get("question", 0) > 0
            and result["message_kinds"].get("answer", 0) > 0
            and result["message_kinds"].get("review_result", 0) > 0
            and all(result["outputs"].get(key, {}).get("valid") is True for key in (
                "claude-worker/summary.json", "codex-worker/audit.md", "claude-worker/followup.md")))


def run(args):
    scenario = getattr(args, "scenario", "short")
    if not sys.platform.startswith("linux"):
        raise ValueError("the process-identity watchdog currently requires Linux /proc")
    if not 0 < args.stop_usd < args.budget_usd <= 3:
        raise ValueError("require 0 < stop-usd < budget-usd <= 3")
    if (not 1 <= args.idle_seconds <= 3600 or not 30 <= args.timeout_seconds <= 7200
            or args.idle_seconds >= args.timeout_seconds):
        raise ValueError("require idle-seconds 1..3600, timeout-seconds 30..7200, idle < timeout")
    crew = args.crew.resolve(strict=True)
    actual_sha = digest(crew)
    if actual_sha != args.expected_sha256:
        raise ValueError("Crew binary hash differs from --expected-sha256")
    output = args.out.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError("--out must be a new directory; runs never reuse state")
    output.mkdir(parents=True, mode=0o700)
    output = output.resolve()
    for name in ("state", "cache", "config", "data", "logs"):
        (output / name).mkdir(mode=0o700)
    runtime = Path(tempfile.mkdtemp(prefix="cb.", dir="/tmp"))
    env = isolated_environment(output, runtime)
    if scenario == "lifecycle":
        # Request bounded thinking for the native fold/rewrite probe. Actual
        # thinking-block presence is recorded, never inferred from this setting.
        env["MAX_THINKING_TOKENS"] = "2048"
    operator_homes = None
    if args.sandbox == "off":
        home = Path(env["HOME"])
        operator_homes = {
            "claude": Path(env.get("CLAUDE_CONFIG_DIR") or home / ".claude"),
            "codex": Path(env.get("CODEX_HOME") or home / ".codex"),
        }
    owned = {}
    workspace = output / "workspace"
    snapshot = {}
    lifecycle = None
    known_sessions = {}
    started = time.monotonic()
    report = {"schema": 1, "label": args.label, "started_at": utc(),
              "source_revision": args.source_revision,
              "source_revision_provenance": "caller-supplied; binary hash independently verified",
              "crew_sha256": actual_sha, "scenario": scenario,
              "team_template_sha256": digest(HERE / ("team-lifecycle.yaml" if scenario == "lifecycle" else "team.yaml")),
              "runner_files_sha256": {name: digest(HERE / name) for name in ("bench", "bench.py", "usage.py")},
              "sandbox_mode": args.sandbox,
              "native_session_storage": "operator-cli-homes" if operator_homes else "private-member-homes",
              "budget_usd": args.budget_usd, "stop_usd": args.stop_usd,
              "cost_basis": "API-equivalent estimate; not a subscription invoice",
              "rates_checked": "2026-10-07", "rates": RATES,
              "runtime_directory": str(runtime), "status": "starting", "agents": [],
              "idle_gap": {"requested_seconds": args.idle_seconds, "observed_seconds": 0}}
    write_json(output / "manifest.json", report)
    try:
        workspace = fixture(output, env, args.sandbox, scenario)
        if scenario == "lifecycle":
            from lifecycle import Lifecycle
            def lifecycle_command(*parts):
                result = invoke([crew, *parts, "-C", workspace], env, output, 30)
                if result.returncode:
                    raise RuntimeError(f"lifecycle command failed: {' '.join(parts[:3])}: {result.stderr.strip()}")
                return result
            lifecycle = Lifecycle(workspace, lifecycle_command, getattr(args, "long_idle", False))
            report["runner_files_sha256"]["lifecycle.py"] = digest(HERE / "lifecycle.py")
            report["requested_thinking_tokens"] = 2048
            write_json(output / "manifest.json", report)
        if args.prepare_only:
            report["status"] = "prepared"
            return 0
        programs = ["claude", "codex"] + (["bwrap", "socat"] if args.sandbox == "required" else [])
        for program in programs:
            if not shutil.which(program, path=env["PATH"]):
                raise RuntimeError(f"missing prerequisite: {program}")
        report["versions"] = runtime_versions(env, output)
        command = invoke([crew, "run", output / "team.yaml", "--no-attach"], env, output, 45)
        (output / "logs" / "launch.txt").write_text(command.stdout + command.stderr)
        if command.returncode:
            raise RuntimeError(f"crew run failed ({command.returncode}); see private logs/launch.txt")
        idle_since = None
        final_idle = None
        followup_sent = False
        last_usage_change = time.monotonic()
        previous_usage = None
        while time.monotonic() - started < args.timeout_seconds:
            daemon = inspect_daemon(workspace, runtime)
            if daemon and (identity := process_identity(daemon["pid"])):
                owned[daemon["pid"]] = identity[1]
                remember_descendants(owned)
            response = invoke([crew, "status", "--json", "-C", workspace], env, output)
            if response.returncode:
                raise RuntimeError("fixture status unavailable; refusing an unmetered run")
            snapshot = json.loads(response.stdout)
            write_json(output / "snapshot.json", snapshot)
            agents = snapshot.get("agents", [])
            if any(a["status"] in ("failed", "stopped", "blocked") and not (
                    lifecycle and lifecycle.transient_stop(a)) for a in agents):
                raise RuntimeError("an agent failed, stopped or blocked; see private snapshot.json")
            model_agents = [a for a in agents if a["runtime"] != "shell"]
            if {a["id"] for a in model_agents} != {"orchestrator", "claude-worker", "codex-worker"}:
                raise RuntimeError("unexpected model roster; refusing an unpriced team")
            for agent in model_agents:
                if agent.get("session_id"):
                    known_sessions[(agent["id"], agent["session_id"])] = dict(agent)
            observed = list(known_sessions.values())
            observed += [a for a in model_agents if not a.get("session_id")]
            report["agents"] = collect(workspace, observed, operator_homes)
            report.update(totals(report["agents"]))
            upper = report["observed_cost_usd_upper"]
            current_usage = tuple(sum(a.get(key, 0) for a in report["agents"])
                                  for key in ("request_count", "input_tokens", "output_tokens"))
            if current_usage != previous_usage:
                previous_usage, last_usage_change = current_usage, time.monotonic()
            if upper >= args.stop_usd:
                report["status"] = "budget-stop"
                break
            if lifecycle:
                idle_members = all(a["status"] in ("idle", "done", "waiting") for a in agents)
                if lifecycle.usage_stalled(last_usage_change, idle_members):
                    raise RuntimeError("no new native usage for 90 seconds; refusing an unmetered run")
                complete = lifecycle.tick(snapshot, report["agents"], time.monotonic() - last_usage_change)
                report["lifecycle"] = lifecycle.report()
                write_json(output / "progress.json", report)
                if complete:
                    report["status"] = "complete"
                    break
                time.sleep(1)
                continue
            tasks = {t["id"]: t["status"] for t in snapshot.get("tasks", [])}
            quiet = len(agents) == 3 and all(a["status"] in ("idle", "done", "waiting") for a in agents)
            initial_done = all(tasks.get(t) == "done" for t in ("summarize", "audit"))
            planned_idle = initial_done and not followup_sent and quiet
            if not planned_idle and time.monotonic() - last_usage_change > 90:
                raise RuntimeError("no new native usage for 90 seconds; refusing an unmetered run")
            if initial_done and not followup_sent:
                if quiet:
                    if idle_since is None or last_usage_change > idle_since:
                        idle_since = time.monotonic()
                        report["idle_gap"]["started_at"] = utc()
                    # Native usage must also remain unchanged throughout the gap.
                    idle_since = max(idle_since, last_usage_change)
                    gap = time.monotonic() - idle_since
                    if gap >= args.idle_seconds:
                        report["idle_gap"].update(observed_seconds=gap, ended_at=utc())
                        command = invoke([crew, "task", "add", "-C", workspace,
                                          "--id", "followup", "--title", "Recheck after idle",
                                          "--agent", "claude-worker", "--reviewer", "codex-worker",
                                          "--description", FOLLOWUP], env, output)
                        if command.returncode:
                            raise RuntimeError("could not dispatch the followup task")
                        followup_sent = True
                        last_usage_change = time.monotonic()
                else:
                    idle_since = None
            if followup_sent and tasks.get("followup") == "done" and quiet:
                final_idle = final_idle or time.monotonic()
                if time.monotonic() - max(final_idle, last_usage_change) >= 3:
                    complete_usage = len(report["agents"]) == 3 and all(
                        a["coverage"] == "observed" for a in report["agents"])
                    # Bus and file evidence are checked after the last model
                    # writes and after the full bus has been collected.
                    report["status"] = "complete" if complete_usage else "incomplete-evidence"
                    break
            else:
                final_idle = None
            write_json(output / "progress.json", report)
            time.sleep(1)
        else:
            report["status"] = "timeout"
    except (Exception, KeyboardInterrupt) as error:
        report["status"] = "error"
        report["error"] = str(error) or "interrupted"
    finally:
        # Shutdown is scoped by both the fixture directory and its daemon file.
        cleanup = {"stop_returncode": None, "remaining_processes": []}
        try:
            daemon = inspect_daemon(workspace, runtime)
            if daemon:
                if identity := process_identity(daemon["pid"]):
                    owned[daemon["pid"]] = identity[1]
                remember_descendants(owned)
                try:
                    messages = invoke([crew, "messages", "--json", "--lines", "10000",
                                       "-C", workspace], env, output)
                    if messages.returncode == 0 and snapshot:
                        snapshot["messages"] = json.loads(messages.stdout)
                        write_json(output / "snapshot.json", snapshot)
                    events = invoke([crew, "logs", "--json", "--lines", "10000",
                                     "-C", workspace], env, output)
                    (output / "events.jsonl").write_text(events.stdout)
                except Exception as error:
                    report["bus_collection_error"] = str(error)
                stopped = invoke([crew, "stop", "-C", workspace], env, output, 30)
                (output / "logs" / "stop.txt").write_text(stopped.stdout + stopped.stderr)
                cleanup["stop_returncode"] = stopped.returncode
        except Exception as error:
            cleanup["error"] = str(error)
        # A timed-out RPC or partial evidence must not skip owned-process cleanup.
        for sig in (signal.SIGTERM, signal.SIGKILL):
            if not still_owned(owned):
                break
            try:
                for pid in still_owned(owned):
                    try:
                        os.kill(pid, sig)
                    except ProcessLookupError:
                        pass
                time.sleep(0.5)
            except Exception as error:
                cleanup["error"] = str(error)
        cleanup["remaining_processes"] = still_owned(owned)
        report["cleanup"] = cleanup
        report["finished_at"] = utc()
        report["duration_seconds"] = time.monotonic() - started
        if snapshot:
            try:
                report["agents"] = collect(workspace, list(known_sessions.values()) or [
                    a for a in snapshot.get("agents", []) if a["runtime"] != "shell"], operator_homes)
                report.update(totals(report["agents"]))
                report["evidence"] = evidence(snapshot, workspace)
                if lifecycle:
                    report["lifecycle"] = lifecycle.report()
                valid = lifecycle.report()["complete"] if lifecycle else validate_evidence(report["evidence"])
                if report["status"] == "complete" and (not valid or report["usage_coverage"] != "observed"):
                    report["status"] = "incomplete-evidence"
            except Exception as error:
                report["collection_error"] = str(error)
                report["status"] = "incomplete-evidence"
        if report.get("observed_cost_usd_upper", 0) > args.budget_usd:
            report["status"] = "budget-exceeded"
        collector = HERE / "baseline.mjs"
        report["baseline_collector"] = {"status": "unavailable", "path": "scripts/cache/baseline.mjs"}
        session_files = [a for a in report["agents"] if a.get("source_path")]
        if (not args.prepare_only and collector.is_file() and session_files
                and (workspace / ".crew/crew.db").is_file()):
            try:
                argv = ["node", collector, "--workspace", workspace, "--output", output / "baseline.json"]
                # Explicit files suppress the collector's broader home discovery.
                for agent in session_files:
                    argv.extend(["--session-file", f'{agent["runtime"]}={agent["source_path"]}'])
                collected = invoke(argv, env, output, 60)
                (output / "logs/collector.txt").write_text(collected.stdout + collected.stderr)
                report["baseline_collector"] = {
                    "status": "collected" if collected.returncode == 0 else "failed",
                    "returncode": collected.returncode, "path": "baseline.json",
                    "collector_sha256": digest(collector), "explicit_session_files": len(session_files)}
                if collected.returncode and report["status"] == "complete":
                    report["status"] = "incomplete-evidence"
            except Exception as error:
                report["baseline_collector"] = {"status": "failed", "error": str(error)}
                if report["status"] == "complete":
                    report["status"] = "incomplete-evidence"
        if cleanup.get("error") or cleanup["remaining_processes"]:
            report["status"] = "cleanup-failed"
        else:
            shutil.rmtree(runtime)
        write_json(output / "report.json", report)
        print(json.dumps({"status": report["status"], "report": str(output / "report.json"),
                          "cost_usd_upper": report.get("cost_usd_upper")}))
    return 0 if report["status"] == "complete" else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--crew", type=Path, required=True)
    parser.add_argument("--expected-sha256", required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--label", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--budget-usd", type=float, default=3.0)
    parser.add_argument("--stop-usd", type=float, default=2.0)
    parser.add_argument("--idle-seconds", type=float, default=20)
    parser.add_argument("--timeout-seconds", type=float, default=600)
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--scenario", choices=("short", "lifecycle"), default="short")
    parser.add_argument("--long-idle", action="store_true",
                        help="lifecycle: add a 3665-second quiet gap and native >3600-second checks")
    parser.add_argument("--sandbox", choices=("required", "off"), default="required",
                        help="off uses fresh sessions with the operator's normal CLI logins")
    args = parser.parse_args()
    os.umask(0o077)
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        return run(args)
    except Exception as error:
        print(f"cache bench: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
