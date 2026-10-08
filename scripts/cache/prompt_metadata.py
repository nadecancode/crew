#!/usr/bin/env python3
"""Print body-free fingerprints of native surfaces from a completed bench run.

These are persisted Codex metadata/messages, not a captured provider request.
No discovery, authentication, native-file writes, or model calls occur here.
"""

import hashlib
import json
from pathlib import Path
import re
import sys


def fingerprint(value):
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    return {"canonical_json_bytes": len(encoded), "sha256": hashlib.sha256(encoded).hexdigest()}


def surfaces(values):
    result = []
    for line, value in enumerate(values, 1):
        payload = value.get("payload") or {}
        row = {"line": line, "timestamp": value.get("timestamp")}
        if value.get("type") == "session_meta":
            instructions = payload.get("base_instructions")
            if isinstance(instructions, dict) and isinstance(instructions.get("text"), str):
                raw = instructions["text"].encode()
                result.append(dict(row, surface="session_meta.base_instructions.text",
                                   utf8_bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest()))
            tools = payload.get("dynamic_tools")
            if isinstance(tools, list):
                result.append(dict(row, surface="session_meta.dynamic_tools",
                                   tool_count=len(tools), **fingerprint(tools)))
        if (value.get("type") == "response_item" and payload.get("type") == "message"
                and payload.get("role") in ("system", "developer")):
            content = payload.get("content")
            if isinstance(content, list):
                result.append(dict(row, surface=f'response_item.{payload["role"]}.content',
                                   **fingerprint(content)))
    return result


def read_completed_report(path):
    raw_report = path.read_bytes()
    report = json.loads(raw_report)
    if report.get("status") != "complete":
        raise ValueError("native prompt metadata requires a completed benchmark report")
    rows = []
    for agent in report["agents"]:
        session = agent.get("session_id")
        if not isinstance(session, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", session):
            raise ValueError("invalid session id")
        source = Path(agent["source_path"])
        matches = (source.name == session + ".jsonl" if agent["runtime"] == "claude"
                   else source.name.startswith("rollout-") and source.name.endswith("-" + session + ".jsonl"))
        if not matches or source.is_symlink() or not source.is_file():
            raise ValueError("native path does not name the selected session")
        row = {"agent": agent["agent"], "runtime": agent["runtime"],
               "session_id": session, "source_path": str(source)}
        if agent["runtime"] != "codex":
            row.update(coverage="unknown", surfaces=[],
                       reason="No full effective Claude prompt or tool catalog in the selected native format")
        else:
            # The completed run is stopped, but bind this pass to the initial
            # byte extent anyway. A trailing partial record is not a surface.
            with source.open("rb") as stream:
                data = stream.read(source.stat().st_size)
            values = []
            for line in data.splitlines():
                try:
                    value = json.loads(line)
                except ValueError:
                    value = {}
                values.append(value if isinstance(value, dict) else {})
            row.update(coverage="persisted-native-surfaces", surfaces=surfaces(values),
                       native_prefix_bytes=len(data), native_prefix_sha256=hashlib.sha256(data).hexdigest())
        rows.append(row)
    return {"schema": 1, "input_report_sha256": hashlib.sha256(raw_report).hexdigest(),
            "tool_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "scope": "Persisted native surfaces only; not provider wire-prefix equality or the full MCP/skills catalog",
            "generated_layer": "Crew briefing hashes and byte deltas remain in the input report's lifecycle steps",
            "synthetic_rendering_layer": "Separate fixture-only capability probes; not captured by paid benchmark runs",
            "sessions": rows}


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: python3 -B scripts/cache/prompt_metadata.py RUN/report.json")
    print(json.dumps(read_completed_report(Path(sys.argv[1])), indent=2, sort_keys=True))
