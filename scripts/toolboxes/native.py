#!/usr/bin/env python3
"""Exercise native toolbox restart/removal against a loopback provider.

Creates only fresh synthetic homes. Never loads operator auth or calls a paid
provider. Raw native request bodies remain under the ignored output directory.
"""

import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time

from provider import Provider, canonical

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/cache"))
from bench import inspect_daemon, process_identity, remember_descendants, still_owned
sys.path.insert(0, str(ROOT / "fixtures/toolboxes"))
from harness import RpcProcess


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def hashes(directory):
    result = {}
    for name in ["mcp.json", "settings.json", "briefing.md", "crew-pi.ts", "codex-toolbox.json"]:
        p = directory / name
        if p.is_file():
            result[name] = hashlib.sha256(p.read_bytes()).hexdigest()
    plugin = directory / "crew-toolbox"
    if plugin.is_dir():
        for path in sorted(plugin.rglob("*")):
            if path.is_file() and not path.is_symlink():
                result[str(path.relative_to(directory))] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def native_receipt(root, runtime, session_id, phase):
    if not isinstance(session_id, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", session_id):
        return None
    homes = {"claude": root / "home/.claude/projects", "codex": root / "home/.codex/sessions",
             "pi": root / "work/.crew/agents/x/pi-sessions"}
    home = homes[runtime]
    patterns = {"claude": f"*/{session_id}.jsonl", "codex": f"**/rollout-*-{session_id}.jsonl", "pi": f"*_{session_id}.jsonl"}
    paths = [p.resolve() for p in home.glob(patterns[runtime]) if p.is_file() and p.resolve().is_relative_to(home.resolve())]
    if len(paths) != 1:
        return None
    marker = f"Toolbox acceptance {phase}."
    active = False
    for line_number, line in enumerate(paths[0].read_text().splitlines(), 1):
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if runtime == "codex":
            message = value.get("payload", {}) if value.get("type") == "response_item" else {}
        elif runtime == "pi":
            message = value.get("message", {}) if value.get("type") == "message" else {}
        else:
            message = value.get("message", {}) if value.get("type") in ("user", "assistant") else {}
        content = message.get("content")
        texts = [content] if isinstance(content, str) else [part.get("text", "") for part in content or [] if isinstance(part, dict) and part.get("type") in ("text", "input_text", "output_text")]
        if message.get("role") == "user" and texts:
            active = any(marker in text for text in texts)
        if active and message.get("role") == "assistant" and "Fixture response complete." in texts:
            return {"source_path": str(paths[0]), "phase": phase, "reply_line": line_number,
                    "reply_observed_after_phase_prompt": True}
    return None


class Session:
    def __init__(self, args):
        self.args = args
        self.root = args.out.resolve()
        if self.root.exists():
            raise ValueError("output directory must be new")
        self.root.mkdir(parents=True, mode=0o700)
        self.work = self.root / "work"
        self.work.mkdir()
        self.home = self.root / "home"
        self.home.mkdir()
        self.runtime = Path(tempfile.mkdtemp(prefix="tqa.", dir="/tmp"))
        self.crew = args.crew.resolve(strict=True)
        if hashlib.sha256(self.crew.read_bytes()).hexdigest() != args.sha256:
            raise ValueError("binary hash mismatch")
        self.owned = {}
        self.snapshot = None
        self.log_number = 0
        self.provider = Provider(self.root)
        self.env = {"PATH": os.environ["PATH"], "HOME": str(self.home), "LANG": "C.UTF-8",
                    "XDG_RUNTIME_DIR": str(self.runtime), "XDG_CONFIG_HOME": str(self.home / ".config"),
                    "XDG_CACHE_HOME": str(self.root / "cache"), "XDG_DATA_HOME": str(self.root / "data"),
                    "CLAUDE_CONFIG_DIR": str(self.home / ".claude"), "CODEX_HOME": str(self.home / ".codex"),
                    "PI_CODING_AGENT_DIR": str(self.home / ".pi/agent"),
                    "CREW_GLOBAL_CONFIG": str(self.root / "empty-config.toml"),
                    "TOOLBOX_STUB_KEY": "toolbox-native-fixture-not-a-credential",
                    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "DISABLE_AUTOUPDATER": "1",
                    "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                    "PYTHONDONTWRITEBYTECODE": "1"}
        if args.pi:
            self.env["CREW_PI"] = str(args.pi.resolve(strict=True))
        (self.root / "empty-config.toml").write_text("")
        write_json(self.home / ".claude.json", {"hasCompletedOnboarding": True, "lastOnboardingVersion": "2.1.291"})
        for directory in [self.home / ".claude", self.home / ".codex", self.home / ".pi/agent", self.work / ".crew"]:
            directory.mkdir(parents=True, exist_ok=True)
        write_json(self.home / ".claude/.claude.json", {
            "hasCompletedOnboarding": True, "lastOnboardingVersion": "2.1.291", "theme": "dark"})
        write_json(self.root / "owner.json", {"pid": os.getpid(), "pid_start": process_identity(os.getpid())[1],
                   "runtime_directory": str(self.runtime), "workspace": str(self.work)})
        (self.work / "README.md").write_text("# Isolated toolbox native acceptance\nNo files or network targets outside this fixture.\n")
        for parts in [("init", "--initial-branch=main"), ("config", "user.name", "Toolbox fixture"),
                      ("config", "user.email", "toolbox@example.invalid"), ("add", "README.md"),
                      ("commit", "-m", "Seed native toolbox fixture")]:
            subprocess.run(["git", *parts], env=self.env, cwd=self.work, check=True, capture_output=True)
        (self.work / ".crew/config.toml").write_text(
            f'[claude]\nbase_url={json.dumps(self.provider.url)}\napi_key_env="TOOLBOX_STUB_KEY"\n'
            f'[codex]\nbase_url={json.dumps(self.provider.url + "/v1")}\napi_key_env="TOOLBOX_STUB_KEY"\n')
        write_json(self.home / ".pi/agent/models.json", {"providers": {"fixture": {
            "baseUrl": self.provider.url, "api": "anthropic-messages", "apiKey": "fixture-not-a-credential",
            "models": [{"id": "claude-haiku-4-5", "contextWindow": 200000, "maxTokens": 4096}]}}})
        self.control = self.root / "upstream.json"
        write_json(self.control, {"description": "original-definition", "always_load": True})
        self.team = {"project": "toolbox-native-acceptance", "objective": "Run only the supplied fixture prompts.",
                     "instructions": "This is an isolated fixture. Never contact a real provider or another workspace.",
                     "backend": "pty", "workspace": {"path": str(self.work), "isolation": "shared", "vcs": "git", "sandbox": {"mode": "off"}},
                     "orchestrator": {"agent": False}, "defaults": {"runtime": args.runtime, "permissions": "guarded", "permission_mode": "acceptEdits", "allowed_tools": ["mcp__probe"], "auto_trust": True,
                         "max_restarts": 0, "long_context": False, "model_fallback": False},
                     "limits": {"max_subagents": 0, "max_nudges": 0, "idle_nudge_secs": 600, "context_warn_percent": 0, "auto_resume": False},
                     "agents": [{"id": "x", "role": "Native toolbox acceptance member", "runtime": args.runtime,
                         "model": {"claude": "claude-haiku-4-5", "codex": "gpt-6-luna", "pi": "fixture/claude-haiku-4-5"}[args.runtime],
                         "toolbox": {"mcp": {"probe": {"command": [sys.executable, str(ROOT / "fixtures/toolboxes/mcp_stub.py"),
                             "--control", str(self.control), "--log", str(self.root / "mcp.log")], "required": True}}}}]}
        write_json(self.root / "team.yaml", self.team)

    def command(self, *args, checked=True):
        result = subprocess.run([str(self.crew), *map(str, args), "-C", str(self.work)], env=self.env,
                                cwd=self.work, capture_output=True, text=True, timeout=40)
        self.log_number += 1
        (self.root / f"command-{self.log_number:03}.log").write_text(result.stdout + result.stderr)
        if checked and result.returncode:
            raise RuntimeError(f"crew {' '.join(map(str,args[:3]))}: exit {result.returncode}; command-{self.log_number:03}.log")
        return result

    def poll(self, allow_stopped=False):
        daemon = inspect_daemon(self.work, self.runtime)
        if daemon and (identity := process_identity(daemon["pid"])):
            self.owned[daemon["pid"]] = identity[1]
            remember_descendants(self.owned)
        result = self.command("status", "--json")
        self.snapshot = json.loads(result.stdout)
        write_json(self.root / "snapshot.json", self.snapshot)
        agent = next((a for a in self.snapshot["agents"] if a["id"] == "x"), None)
        if agent and agent["status"] in ("failed", "blocked", "stopped") and not (allow_stopped and agent["status"] == "stopped"):
            raise RuntimeError(f"member {agent['status']}: {agent.get('error') or agent.get('activity')}")
        return agent

    def ready(self, old_pid=None):
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            agent = self.poll(allow_stopped=old_pid is not None)
            if agent and agent.get("pid") != old_pid and agent["status"] in ("idle", "waiting", "done"):
                return agent
            time.sleep(.3)
        raise TimeoutError("native member did not become ready")

    def prompt(self, phase, call=False):
        offset = self.provider.set_phase(phase, "call" if call else "inspect")
        self.command("agent", "prompt", "x", f"Toolbox acceptance {phase}. Reply briefly after the requested fixture tool.")
        deadline = time.monotonic() + 90
        quiet_since = None
        previous = None
        while time.monotonic() < deadline:
            agent = self.poll()
            requests = self.provider.observed()
            signature = len(requests)
            if signature != previous:
                previous, quiet_since = signature, time.monotonic()
            if signature > offset and time.monotonic() - quiet_since >= 2 and agent["status"] in ("idle", "waiting", "done"):
                receipt = native_receipt(self.root, self.args.runtime, agent.get("session_id"), phase)
                if receipt is None:
                    time.sleep(.3)
                    continue
                if call and not any(item["phase"] == phase for item in self.provider.calls):
                    raise AssertionError("fixture tool was not offered/called by the native runtime")
                if call and not any(json.loads(line).get("method") == "tools/call" for line in (self.root / "mcp.log").read_text().splitlines()):
                    raise AssertionError("native runtime did not forward the requested MCP call")
                process = process_identity(agent["pid"])
                if process is None or self.owned.get(agent["pid"]) != process[1]:
                    raise RuntimeError("native argv process identity changed")
                argv = Path(f"/proc/{agent['pid']}/cmdline").read_bytes().decode().rstrip("\0").split("\0")
                return {"phase": phase, "session_id": agent.get("session_id"), "pid": agent.get("pid"),
                        "native_argv": argv,
                        "toolbox": agent.get("toolbox"), "artifacts": hashes(self.work / ".crew/agents/x"),
                        "native_receipt": receipt,
                        "requests": list(range(offset + 1, len(requests) + 1))}
            time.sleep(.3)
        raise TimeoutError("native prompt did not finish")

    def events(self):
        with closing(sqlite3.connect(f"file:{(self.work / '.crew/crew.db').resolve()}?mode=ro", uri=True)) as db:
            return [dict(zip(["seq", "timestamp", "event_type", "agent_id"], row)) for row in db.execute(
                "SELECT seq,timestamp,event_type,agent_id FROM events WHERE event_type LIKE 'toolbox.%' ORDER BY seq")]

    def direct_catalog(self):
        command = self.team["agents"][0]["toolbox"]["mcp"]["probe"]["command"]
        with RpcProcess(command, env={"PATH": os.defpath, "HOME": str(self.home)}, cwd=self.work) as server:
            server.request("initialize", {"protocolVersion": "2025-06-18", "clientInfo": {"name": "toolbox-negative-control", "version": "1"}, "capabilities": {}})
            server.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            return server.request("tools/list")["result"]["tools"]

    def close(self):
        cleanup = {"stop_returncode": None, "remaining_processes": []}
        try:
            daemon = inspect_daemon(self.work, self.runtime)
            if daemon:
                remember_descendants(self.owned)
                cleanup["stop_returncode"] = self.command("stop", checked=False).returncode
        finally:
            for signum in [signal.SIGTERM, signal.SIGKILL]:
                for pid in still_owned(self.owned):
                    try:
                        os.kill(pid, signum)
                    except ProcessLookupError:
                        pass
                time.sleep(.3)
            cleanup["remaining_processes"] = still_owned(self.owned)
            self.provider.close()
            if not cleanup["remaining_processes"]:
                shutil.rmtree(self.runtime)
        return cleanup


def run(args):
    session = Session(args)
    report = {"schema": 1, "runtime": args.runtime, "crew_sha256": args.sha256, "provider_calls": 0,
              "provider": "synthetic loopback only; usage is not a cache measurement", "status": "running", "steps": []}
    try:
        program = args.pi.resolve() if args.runtime == "pi" and args.pi else Path(shutil.which(args.runtime, path=session.env["PATH"])).resolve()
        version = subprocess.run([str(program), "--version"], env=session.env, cwd=session.work,
                                 check=True, capture_output=True, text=True, timeout=15)
        report["native_program"] = {"path": str(program), "sha256": hashlib.sha256(program.read_bytes()).hexdigest(),
                                    "version": version.stdout.strip()}
        session.command("run", session.root / "team.yaml", "--no-attach")
        session.ready()
        report["steps"].append(session.prompt("initial-call", call=True))
        write_json(session.control, {"description": "changed-definition", "always_load": True})
        report["direct_changed_catalog"] = session.direct_catalog()
        assert all("changed-definition" in t["description"] for t in report["direct_changed_catalog"])
        report["steps"].append(session.prompt("pinned-before-restart"))
        initial = report["steps"][0]
        session.command("resume", "--restart", "x")
        session.ready(initial["pid"])
        report["steps"].append(session.prompt("refreshed-after-restart"))
        refreshed = report["steps"][-1]
        def catalog(step):
            request = session.provider.observed()[step["requests"][-1] - 1]
            if request["native_probe_catalog"] is not None:
                return request["native_probe_catalog"]
            return [t for t in request["tools"] if "probe" in t.get("name", "")]
        before_restart = report["steps"][1]
        checks = {
            "same_session": refreshed["session_id"] == initial["session_id"],
            "same_fingerprint": refreshed["toolbox"]["fingerprint"] == initial["toolbox"]["fingerprint"],
            "running_digest_pinned": before_restart["toolbox"]["definition_digest"] == initial["toolbox"]["definition_digest"],
            "running_native_catalog_pinned": bool(catalog(initial)) and catalog(before_restart) == catalog(initial),
            "restart_digest_changed": refreshed["toolbox"]["definition_digest"] != initial["toolbox"]["definition_digest"],
            "restart_native_catalog_changed": bool(catalog(refreshed)) and all("changed-definition" in t.get("description", "") for t in catalog(refreshed)),
            "one_changed_event": sum(e["event_type"] == "toolbox.changed" for e in session.events()) == 1,
        }
        report["T14"] = {"passed": all(checks.values()), "checks": checks}
        session.team["agents"][0]["toolbox"]["mcp"] = {}
        write_json(session.root / "team.yaml", session.team)
        session.command("toolbox", "reload", session.root / "team.yaml")
        session.command("resume", "--restart", "x")
        session.ready(refreshed["pid"])
        report["steps"].append(session.prompt("removed-after-restart"))
        removed = report["steps"][-1]
        body = json.loads((session.root / f"request-{removed['requests'][-1]:03}.json").read_text())
        history = json.dumps(body.get("input", body.get("messages", [])))
        call = next(call for call in session.provider.calls if call["phase"] == "initial-call")
        identity = f"call_fixture_{call['request']}" if args.runtime == "codex" else f"toolu_fixture_{call['request']}"
        checks = {"same_session": removed["session_id"] == initial["session_id"],
                  "native_catalog_removed": catalog(removed) == [],
                  "historical_call_retained": identity in history,
                  "fingerprint_changed": removed["toolbox"]["fingerprint"] != refreshed["toolbox"]["fingerprint"],
                  "native_response_completed": True}
        report["T18"] = {"passed": all(checks.values()), "checks": checks, "historical_call_id": identity}
        report["events"] = session.events()
        report["status"] = "passed" if report["T14"]["passed"] and report["T18"]["passed"] else "failed"
    except (Exception, KeyboardInterrupt) as error:
        report.update(status="error", error=str(error))
    finally:
        report["cleanup"] = session.close()
        report["requests"] = [{key: value for key, value in item.items() if key != "tools"} | {
            "probe_tools": [tool for tool in item["tools"] if "probe" in tool.get("name", "")],
            "tool_count": len(item["tools"])} for item in session.provider.observed()]
        report["calls"] = session.provider.calls
        write_json(session.root / "report.json", report)
        print(json.dumps({"status": report["status"], "error": report.get("error"), "report": str(session.root / "report.json"), "cleanup": report["cleanup"]}))
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--crew", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--runtime", choices=["claude", "codex", "pi"], required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--pi", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    def interrupted(*_args):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    raise SystemExit(run(args))
