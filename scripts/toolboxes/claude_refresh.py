#!/usr/bin/env python3
"""Native Claude refresh, crash recovery and unchanged-history regression.

Extends QA's localhost T14/T18 fixture. Requires an explicit Crew binary/hash;
uses only disposable homes, dummy credentials and synthetic provider replies.
"""

import argparse
from contextlib import redirect_stdout
import hashlib
import io
import json
import os
from pathlib import Path
import signal

import native


def snapshot_disabled(step):
    argv = step["native_argv"]
    return any(pair == ["--system-prompt-snapshot", "off"]
               for pair in (argv[i:i + 2] for i in range(len(argv) - 1)))


class RecoverySession(native.Session):
    def __init__(self, args):
        super().__init__(args)
        self.team["defaults"]["max_restarts"] = 1
        native.write_json(self.root / "team.yaml", self.team)
        self.initial = None
        self.unchanged = None
        self.history_prefix = None

    def body(self, step):
        sequence = step["requests"][-1]
        return json.loads((self.root / f"request-{sequence:03}.json").read_text())

    def prompt(self, phase, call=False):
        step = super().prompt(phase, call)
        if phase == "initial-call":
            self.initial = step
            self.history_prefix = Path(step["native_receipt"]["source_path"]).read_bytes()
            self.command("resume", "--restart", "x")
            self.ready(step["pid"])
            self.unchanged = super().prompt("unchanged-explicit-restart")
        if phase != "refreshed-after-restart":
            return step

        # Kill only this fixture's exact native process incarnation. The real
        # daemon takes the automatic Start::Resume path, not an operator restart.
        pid = step["pid"]
        identity = native.process_identity(pid)
        if identity is None or self.owned.get(pid) != identity[1]:
            raise RuntimeError("native process identity changed before crash control")
        os.kill(pid, signal.SIGKILL)
        self.ready(pid)
        recovered = super().prompt("automatic-recovery-after-refresh")
        steady = super().prompt("steady-after-recovery")
        bodies = [self.body(item) for item in (step, recovered, steady)]
        catalogs = [[tool for tool in body.get("tools", [])
                     if tool.get("name", "").startswith("mcp__probe__")]
                    for body in bodies]
        transcript = Path(steady["native_receipt"]["source_path"]).read_bytes()
        checks = {
            "initial_snapshot_policy_unchanged": not snapshot_disabled(self.initial),
            "unchanged_restart_keeps_snapshot": not snapshot_disabled(self.unchanged),
            "unchanged_restart_keeps_digest": self.initial["toolbox"]["definition_digest"]
                == self.unchanged["toolbox"]["definition_digest"],
            "refresh_disables_old_snapshot": snapshot_disabled(step),
            "automatic_recovery_keeps_snapshot_disabled": snapshot_disabled(recovered),
            "same_session": step["session_id"] == recovered["session_id"] == steady["session_id"],
            "same_fingerprint": step["toolbox"]["fingerprint"] == recovered["toolbox"]["fingerprint"],
            "same_digest": step["toolbox"]["definition_digest"] == recovered["toolbox"]["definition_digest"],
            "refreshed_catalog_survives_recovery": bool(catalogs[0]) and catalogs[0] == catalogs[1] == catalogs[2]
                and all("changed-definition" in tool.get("description", "") for tool in catalogs[2]),
            "system_bytes_stable": len({native.canonical(body.get("system")) for body in bodies}) == 1,
            "all_tool_bytes_stable": len({native.canonical(body.get("tools")) for body in bodies}) == 1,
            "original_transcript_prefix_unchanged": transcript.startswith(self.history_prefix),
            "one_changed_event": sum(event["event_type"] == "toolbox.changed" for event in self.events()) == 1,
        }
        native.write_json(self.root / "claude-recovery.json", {
            "passed": all(checks.values()), "checks": checks,
            "steps": [self.unchanged, step, recovered, steady],
            "original_transcript_prefix_sha256": hashlib.sha256(self.history_prefix).hexdigest(),
            "system_sha256": [hashlib.sha256(native.canonical(body.get("system"))).hexdigest() for body in bodies],
            "tools_sha256": [hashlib.sha256(native.canonical(body.get("tools"))).hexdigest() for body in bodies],
        })
        return step


def run(args):
    native.Session = RecoverySession
    # Keep one final stdout summary with the combined verdict. Native run owns
    # process cleanup and still retains its complete T14/T18 report on failure.
    with redirect_stdout(io.StringIO()):
        result = native.run(args)
    report_path = args.out.resolve() / "report.json"
    report = json.loads(report_path.read_text())
    recovery_path = args.out.resolve() / "claude-recovery.json"
    recovery = json.loads(recovery_path.read_text()) if recovery_path.exists() else None
    report["claude_recovery"] = recovery
    if not recovery or not recovery["passed"]:
        report["status"] = "failed" if report["status"] != "error" else "error"
        result = 1
    native.write_json(report_path, report)
    print(json.dumps({"status": report["status"], "report": str(report_path),
                      "error": report.get("error"), "cleanup": report["cleanup"]}))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--crew", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.runtime = "claude"
    args.pi = None
    os.umask(0o077)

    def interrupted(*_args):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    raise SystemExit(run(args))
