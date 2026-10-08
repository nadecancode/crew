#!/usr/bin/env python3
"""Publish only hashes, cache boundaries and exact native text byte positions."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
from standing_probe import positions, text_blocks, lcp
from provider import canonical, tool_definitions

CASES = ["claude-worktree-01", "codex-worktree-01", "claude-shared-01", "codex-shared-01",
         "claude-shared-section-02", "claude-firstparty-observe-02", "claude-firstparty-shared-02"]


def sha(data):
    return hashlib.sha256(data).hexdigest()


def summarize(root):
    result = {"schema": 1, "source_scope": "fresh synthetic native sessions; no provider cache measurement",
              "provider_calls": 0, "network": "isolated network namespace; loopback server/proxy only",
              "claude_version": "2.1.291", "codex_version": "0.159.0", "cases": [],
              "rejected": [{"case": "claude-shared-section-01", "reason": "duplicate prompt.compose hook rejected; common policy absent"},
                           {"case": "claude-firstparty-observe-01", "reason": "port 80 denied before native launch"}]}
    for name in CASES:
        directory = root / name
        report = json.loads((directory / "standing.json").read_text())
        assert report["status"] == "captured" and report["cleanup"]["remaining_processes"] == []
        members, bodies = [], []
        for member in report["members"]:
            raw = (directory / f"request-{member['sequence']:03}.json").read_bytes()
            body = json.loads(raw)
            bodies.append(body)
            agent = {"id": member["id"], "workdir": member["workdir"]}
            members.append({"id": member["id"], "session_id_sha256": sha(member["session_id"].encode()),
                "workdir": str(Path(member["workdir"]).relative_to(directory)),
                "request": f"{name}/request-{member['sequence']:03}.json", "request_sha256": sha(raw),
                "catalog_sha256": sha(canonical(tool_definitions(body))),
                "toolbox_fingerprint": (member.get("toolbox") or {}).get("fingerprint"),
                "blocks": positions(body, report["runtime"], agent),
                "system_cache_boundaries": [{"pointer": f"/system/{i}", "cache_control": block.get("cache_control"),
                    "bytes": len(block.get("text", "").encode()), "sha256": sha(block.get("text", "").encode())}
                    for i, block in enumerate(body.get("system", []))],
                "prompt_cache_key_sha256": sha(body["prompt_cache_key"].encode()) if body.get("prompt_cache_key") else None})
        assert len({m["session_id_sha256"] for m in members}) == 2
        assert members[0]["catalog_sha256"] == members[1]["catalog_sha256"]
        aa, bb = (list(text_blocks(body, report["runtime"])) for body in bodies)
        components = [{"pointer": a[0], "kind": a[1], "left_bytes": len(a[2].encode()),
                       "right_bytes": len(b[2].encode()), "lcp_utf8_bytes": lcp(a[2].encode(), b[2].encode()),
                       "equal": a[2] == b[2]} for a, b in zip(aa, bb)]
        case = {"name": name, "runtime": report["runtime"], "isolation": report["isolation"],
                "endpoint_shape": report.get("endpoint_shape", "loopback"), "compose_probe": report.get("compose_probe"),
                "native_binary": report["native_binary"], "crew_sha256": report["crew_sha256"],
                "members": members, "components": components, "remaining_processes": 0}
        sdk = directory / "work/.crew/agents/x/mod/crew-bridge/.claude-plugin/types/claude-code/index.d.ts"
        if sdk.exists():
            case["sdk_sha256"] = sha(sdk.read_bytes())
        composition = directory / "composition-x.json"
        if composition.exists():
            case["composition"] = json.loads(composition.read_text())
            trace = directory / "compose-x.jsonl"
            assert trace.exists(), "native hook evidence required"
            case["compose_trace_sha256"] = sha(trace.read_bytes())
        result["cases"].append(case)
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    value = summarize(args.root.resolve())
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"cases": len(value["cases"]), "fresh_sessions": 2 * len(value["cases"]), "provider_calls": 0}))
