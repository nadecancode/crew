"""AT-SPI reader for the owned private interaction session only.

Usage:
  atspi.py focus          one line per focused object of the Crew app
  atspi.py tree TITLE     showing controls and labels of the window TITLE
  atspi.py press NAME     invoke the action of the showing button NAME
  atspi.py has NAME       exit 0 when a showing object is named NAME
  atspi.py prefix TEXT    exit 0 when a showing object's name starts with TEXT
  atspi.py focused        the name of the focused object, if any
  atspi.py pressprefix T  invoke the first showing button whose name starts with T
  atspi.py text NAME      the accessible text of the showing object NAME
"""
import os
import sys

import pyatspi

if not os.environ.get("AT_SPI_BUS_ADDRESS", "").startswith("unix:path=/tmp/claude-1000/cdp"):
    raise SystemExit("Private interaction accessibility bus required")

KEYS = ["focused", "focusable", "selected", "selectable", "sensitive", "editable"]
STATES = {key: getattr(pyatspi, "STATE_" + key.upper()) for key in KEYS}


def states(node):
    state = node.getState()
    return [k for k, v in STATES.items() if state.contains(v)]


def line(node, depth):
    return f"{depth} {node.getRoleName()} {node.name!r}  {','.join(states(node))}"


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


def showing(node):
    return node.getState().contains(pyatspi.STATE_SHOWING)


def crew():
    desktop = pyatspi.Registry.getDesktop(0)
    return [app for app in desktop if app is not None]


mode = sys.argv[1] if len(sys.argv) > 1 else "focus"
if mode == "focus":
    found = False
    for app in crew():
        for node, depth in walk(app):
            if depth == 1 and showing(node):
                print(f"  window {node.name!r}")
            if node.getState().contains(pyatspi.STATE_FOCUSED):
                print("  " + line(node, depth))
                found = True
    if not found:
        print("  (no focused descendant)")
elif mode == "tree":
    title = sys.argv[2]
    for app in crew():
        for window in app:
            if window is None or window.name != title:
                continue
            for node, depth in walk(window):
                if showing(node) and node.getRoleName() in (
                    "push button", "button", "toggle button", "combo box", "label", "text",
                    "list item", "page tab", "terminal", "check box",
                ) and node.name:
                    print("  " + line(node, depth))
elif mode == "focused":
    for app in crew():
        for node, depth in walk(app):
            if node.getState().contains(pyatspi.STATE_FOCUSED):
                print(node.name)
elif mode == "prefix":
    for app in crew():
        for node, depth in walk(app):
            if node.name and node.name.startswith(sys.argv[2]) and showing(node):
                raise SystemExit(0)
    raise SystemExit(1)
elif mode == "text":
    for app in crew():
        for node, depth in walk(app):
            if node.name == sys.argv[2] and showing(node):
                text = node.queryText()
                content = text.getText(0, -1)
                lines = [line.rstrip() for line in content.splitlines() if line.strip()]
                print(f"  {len(content)} characters, {len(lines)} non-empty lines")
                for line in lines[:3]:
                    print(f"  | {line}")
                raise SystemExit(0)
    raise SystemExit(1)
elif mode == "pressprefix":
    for app in crew():
        for node, depth in walk(app):
            if node.name and node.name.startswith(sys.argv[2]) and showing(node) and node.getRoleName() in ("push button", "button"):
                node.queryAction().doAction(0)
                raise SystemExit(0)
    raise SystemExit(1)
elif mode in ("press", "has"):
    name = sys.argv[2]
    for app in crew():
        for node, depth in walk(app):
            if node.name == name and showing(node):
                if mode == "has":
                    raise SystemExit(0)
                if node.getRoleName() not in ("push button", "button"):
                    continue
                action = node.queryAction()
                action.doAction(0)
                raise SystemExit(0)
    raise SystemExit(1)
