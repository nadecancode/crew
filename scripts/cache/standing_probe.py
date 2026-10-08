#!/usr/bin/env python3
"""Capture fresh native standing prompts with isolated homes and loopback replies.

This is a rendering probe, not a cache meter. Run inside an isolated network
namespace. It never loads operator credentials; fake usage is not reported as a
cache result. Raw request bodies remain in the ignored output directory.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import sys
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/toolboxes"))
import native
from provider import canonical, tool_definitions


PINNED = {
    "claude": Path("/home/nade/.local/share/claude/versions/2.1.291"),
    "codex": Path("/nix/store/pmh4yzyd0g8rz5l1jv2lnsb2gz33ljhi-codex-0.159.0/libexec/codex"),
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def text_leaves(value, pointer=""):
    if isinstance(value, str):
        yield pointer, value
    elif isinstance(value, list):
        for index, item in enumerate(value):
            yield from text_leaves(item, f"{pointer}/{index}")
    elif isinstance(value, dict):
        for key, item in value.items():
            yield from text_leaves(item, pointer + "/" + key.replace("~", "~0").replace("/", "~1"))


def text_blocks(body, runtime):
    """Keep native component/array order; never infer token order from JSON keys."""
    if runtime == "claude":
        for index, block in enumerate(body.get("system", [])):
            if isinstance(block, dict) and isinstance(block.get("text"), str):
                yield f"/system/{index}/text", "system", block["text"]
        for index, message in enumerate(body.get("messages", [])):
            for pointer, text in text_leaves(message.get("content"), f"/messages/{index}/content"):
                if pointer.endswith("/text") or isinstance(message.get("content"), str):
                    yield pointer, message.get("role", "unknown"), text
    else:
        if isinstance(body.get("instructions"), str):
            yield "/instructions", "instructions", body["instructions"]
        for index, item in enumerate(body.get("input", [])):
            if not isinstance(item, dict):
                continue
            for pointer, text in text_leaves(item.get("content"), f"/input/{index}/content"):
                if pointer.endswith("/text") or isinstance(item.get("content"), str):
                    yield pointer, item.get("role", item.get("type", "unknown")), text


def positions(body, runtime, agent):
    markers = {
        "workdir": agent["workdir"],
        "member_identity": f'You are "{agent["id"]}", one agent',
        "member_branch": "agents/" + agent["id"],
        "coordination": "## Coordination\nYou have a",
        "member_tone": "# Tone and style\nYou are a member of a team",
    }
    rows = []
    for pointer, kind, text in text_blocks(body, runtime):
        raw = text.encode()
        offsets = {name: raw.find(value.encode()) for name, value in markers.items()}
        rows.append({"pointer": pointer, "kind": kind, "bytes": len(raw), "sha256": digest(raw),
                     "offsets_utf8": {key: value for key, value in offsets.items() if value >= 0}})
    return rows


def lcp(left, right):
    return next((i for i, pair in enumerate(zip(left, right)) if pair[0] != pair[1]), min(len(left), len(right)))


class Pair(native.Session):
    def __init__(self, args):
        if (args.compose or args.endpoint_shape == "anthropic-host") and args.runtime != "claude":
            raise ValueError("composition/hostname controls require Claude")
        super().__init__(args)
        if args.endpoint_shape == "anthropic-host":
            if args.runtime != "claude":
                raise ValueError("first-party hostname control is Claude-only")
            old_url = self.provider.url
            self.provider.url = "http://api.anthropic.com"
            self.env.update(HTTP_PROXY=old_url, HTTPS_PROXY=old_url,
                            NO_PROXY="localhost,127.0.0.1")
            config = self.work / ".crew/config.toml"
            config.write_text(config.read_text().replace(old_url, self.provider.url))
        self.team["workspace"]["isolation"] = args.isolation
        self.team["project"] = "standing-prompt-native-probe"
        self.team["agents"] = [dict(self.team["agents"][0], id=ident,
            role="Identical native prompt probe role", toolbox={"mcp": {}}) for ident in ["x", "y"]]
        self.team["defaults"]["allowed_tools"] = []
        native.write_json(self.root / "team.yaml", self.team)
        bindir = self.root / "bin"
        bindir.mkdir()
        # Pin exact tested versions even if the operator's PATH later upgrades.
        for name, binary in PINNED.items():
            if not binary.is_file():
                raise ValueError(f"pinned native binary missing: {name}")
            (bindir / name).symlink_to(binary)
        self.env["PATH"] = str(bindir) + os.pathsep + self.env["PATH"]
        self.env["CLAUDE_CODE_NO_MODEL_FALLBACK"] = "1"
        if args.compose:
            if args.runtime != "claude":
                raise ValueError("compose probe is Claude-only")
            (bindir / "claude").unlink()
            (bindir / "claude").write_text(f"#!{sys.executable}\nimport runpy\nrunpy.run_path({str(ROOT / 'scripts/cache/standing_wrap.py')!r}, run_name='__main__')\n")
            (bindir / "claude").chmod(0o700)
            self.env.update(D3_FIXTURE_ROOT=str(self.root), D3_COMPOSE=args.compose,
                            D3_PINNED_CLAUDE=str(PINNED["claude"]))

    def capture(self):
        self.command("run", self.root / "team.yaml", "--no-attach")
        deadline = time.monotonic() + 120
        captured = {}
        while time.monotonic() < deadline:
            self.poll()
            agents = {a["id"]: a for a in self.snapshot["agents"]}
            for item in self.provider.observed():
                body = json.loads((self.root / f"request-{item['sequence']:03}.json").read_text())
                for ident in ["x", "y"]:
                    marker = f'You are "{ident}", one agent'
                    if ident not in captured and any(marker in text for _, _, text in text_blocks(body, self.args.runtime)):
                        captured[ident] = (item, body)
            if set(captured) == {"x", "y"} and all(a.get("session_id") and a["status"] in ("idle", "waiting", "done") for a in agents.values()):
                break
            if any(a["status"] in ("failed", "blocked", "stopped") for a in agents.values()):
                raise RuntimeError("a synthetic member failed before capture")
            time.sleep(.25)
        else:
            raise TimeoutError("two fresh native sessions did not finish")
        if self.args.compose and not all((self.root / f"compose-{ident}.jsonl").is_file() for ident in ["x", "y"]):
            raise AssertionError("compose hook did not produce native evidence")
        if agents["x"]["session_id"] == agents["y"]["session_id"]:
            raise AssertionError("fresh members reused one native session")
        records = []
        for ident in ["x", "y"]:
            item, body = captured[ident]
            briefing = Path(self.work / f".crew/agents/{ident}/briefing.md")
            if not briefing.exists():
                # Codex carries its composed instructions in the native config.
                briefing = None
            records.append({"id": ident, "session_id": agents[ident]["session_id"],
                "workdir": agents[ident]["workdir"], "sequence": item["sequence"],
                "request_sha256": digest(canonical(body)), "catalog_sha256": digest(canonical(tool_definitions(body))),
                "toolbox": agents[ident].get("toolbox"),
                "briefing_sha256": digest(briefing.read_bytes()) if briefing else None,
                "blocks": positions(body, self.args.runtime, agents[ident])})
        a, b = (captured[ident][1] for ident in ["x", "y"])
        components = []
        aa, bb = list(text_blocks(a, self.args.runtime)), list(text_blocks(b, self.args.runtime))
        for index, (left, right) in enumerate(zip(aa, bb)):
            components.append({"index": index, "left_pointer": left[0], "right_pointer": right[0],
                "kind": left[1], "same_kind": left[1] == right[1],
                "left_bytes": len(left[2].encode()), "right_bytes": len(right[2].encode()),
                "lcp_utf8_bytes": lcp(left[2].encode(), right[2].encode()),
                "equal": left[2] == right[2]})
        return {"members": records, "same_catalog": records[0]["catalog_sha256"] == records[1]["catalog_sha256"],
                "components": components, "block_counts": [len(aa), len(bb)],
                "native_binary": {"path": str(PINNED[self.args.runtime]),
                    "sha256": digest(PINNED[self.args.runtime].read_bytes())}}


def main(args):
    interfaces = [line.split(":", 1)[0].strip() for line in Path("/proc/net/dev").read_text().splitlines()[2:] if ":" in line]
    if interfaces != ["lo"]:
        raise RuntimeError("capture requires an isolated network namespace with loopback only")
    pair = Pair(args)
    report = {"schema": 1, "runtime": args.runtime, "isolation": args.isolation, "compose_probe": args.compose, "endpoint_shape": args.endpoint_shape,
              "crew_sha256": args.sha256, "provider_calls": 0,
              "usage": "synthetic; not a cache measurement", "status": "running"}
    try:
        report.update(pair.capture())
        report["status"] = "captured"
    except (Exception, KeyboardInterrupt) as error:
        report.update(status="error", error=str(error))
    finally:
        report["cleanup"] = pair.close()
        native.write_json(pair.root / "standing.json", report)
        print(json.dumps({"status": report["status"], "runtime": args.runtime,
                          "isolation": args.isolation, "error": report.get("error"),
                          "report": str(pair.root / "standing.json"), "cleanup": report["cleanup"]}))
    return 0 if report["status"] == "captured" and not report["cleanup"]["remaining_processes"] else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--crew", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--runtime", choices=list(PINNED), required=True)
    parser.add_argument("--isolation", choices=["shared", "worktree"], required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--compose", choices=["observe", "shared"])
    parser.add_argument("--endpoint-shape", choices=["loopback", "anthropic-host"], default="loopback")
    args = parser.parse_args()
    args.pi = None
    os.umask(0o077)
    def interrupted(*_args):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    raise SystemExit(main(args))
