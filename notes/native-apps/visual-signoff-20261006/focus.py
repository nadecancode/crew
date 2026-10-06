"""Design-precheck AT-SPI reader for the owned private session only.

Usage: focus.py [tree WINDOW_SUBSTRING]
Without arguments, prints every focused/selected object of the Crew app.
"""
import json
import os
import sys

import pyatspi

if not os.environ.get("AT_SPI_BUS_ADDRESS", "").startswith("unix:path=/tmp/cvs"):
    raise SystemExit("Private precheck accessibility bus required")

KEYS = ["showing", "visible", "focused", "focusable", "selected", "enabled", "sensitive", "checked", "editable"]
STATES = {key: getattr(pyatspi, "STATE_" + key.upper()) for key in KEYS}


def describe(node):
    state = node.getState()
    description = ""
    try:
        description = node.description
    except Exception:
        pass
    return {"name": node.name, "role": node.getRoleName(), "description": description,
            "states": [k for k, v in STATES.items() if state.contains(v)]}


def walk(node, depth=0):
    if node is None or depth > 40:
        return
    yield node, depth
    try:
        children = list(node)
    except Exception:
        children = []
    for child in children:
        yield from walk(child, depth + 1)


desktop = pyatspi.Registry.getDesktop(0)
apps = [app for app in desktop if app is not None]
mode = sys.argv[1] if len(sys.argv) > 1 else "focus"
out = []
for app in apps:
    for node, depth in walk(app):
        info = describe(node)
        if mode == "focus":
            if "focused" in info["states"] or (info["role"] in ("frame", "dialog", "window") and depth <= 1):
                info["depth"] = depth
                out.append(info)
        elif mode == "tree":
            needle = sys.argv[2] if len(sys.argv) > 2 else ""
            if depth <= 1 and info["role"] in ("frame", "dialog", "window") and needle not in info["name"]:
                continue
            if "showing" in info["states"] or depth <= 1:
                info["depth"] = depth
                out.append(info)
print(json.dumps(out, indent=1))
