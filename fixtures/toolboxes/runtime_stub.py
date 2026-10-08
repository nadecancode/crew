#!/usr/bin/env python3
"""Capture a launch without running a model. This does not emulate native discovery."""

import argparse
import json
import os
from pathlib import Path
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture-runtime", required=True, choices=["claude", "codex", "pi", "hook"])
    parser.add_argument("--fixture-log", required=True)
    parser.add_argument("--fixture-mcp-catalog")
    parser.add_argument("--fixture-app-server", action="store_true")
    parser.add_argument("--fixture-reject-roots", action="store_true")
    args, native_args = parser.parse_known_args()
    record = {"runtime": args.fixture_runtime, "argv": native_args,
              "cwd": os.getcwd(), "environment_names": sorted(os.environ)}
    with Path(args.fixture_log).open("a", encoding="utf-8") as out:
        out.write(json.dumps(record) + "\n")
    if native_args == ["--version"]:
        print(args.fixture_runtime + " toolbox fixture (not a real runtime)")
    elif args.fixture_mcp_catalog and native_args[:3] == ["mcp", "list", "--json"]:
        # A caller-supplied discovery response for testing the closing step's
        # generated overrides. This does not interpret or enforce those overrides.
        print(Path(args.fixture_mcp_catalog).read_text())
    elif args.fixture_app_server:
        for line in sys.stdin:
            request = json.loads(line)
            with Path(args.fixture_log).open("a", encoding="utf-8") as out:
                out.write(json.dumps({"runtime": args.fixture_runtime, "request": request}) + "\n")
            if "id" not in request:
                continue
            reply = {"jsonrpc": "2.0", "id": request["id"]}
            if request.get("method") == "initialize":
                reply["result"] = {"userAgent": "codex-toolbox-fixture"}
            elif request.get("method") == "skills/extraRoots/set":
                roots = request.get("params", {}).get("extraRoots")
                if args.fixture_reject_roots or not isinstance(roots, list) or not all(
                        isinstance(root, str) and Path(root).is_absolute() for root in roots):
                    reply["error"] = {"code": -32602, "message": "fixture rejected extraRoots"}
                else:
                    reply["result"] = {}
            else:
                reply["error"] = {"code": -32601, "message": "fixture does not emulate this method"}
            print(json.dumps(reply), flush=True)
    # No simulated skill lists, hook lists or agent tools: those need native probes.
    return 0


if __name__ == "__main__":
    sys.exit(main())
