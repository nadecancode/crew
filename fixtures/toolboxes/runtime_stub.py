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
    args, native_args = parser.parse_known_args()
    record = {"runtime": args.fixture_runtime, "argv": native_args,
              "cwd": os.getcwd(), "environment_names": sorted(os.environ)}
    with Path(args.fixture_log).open("a", encoding="utf-8") as out:
        out.write(json.dumps(record) + "\n")
    if native_args == ["--version"]:
        print(args.fixture_runtime + " toolbox fixture (not a real runtime)")
    # No simulated skill lists, hook lists or agent tools: those need native probes.
    return 0


if __name__ == "__main__":
    sys.exit(main())
