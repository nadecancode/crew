"""Drive the private precheck session's Crew window through AT-SPI.

Usage: drive.py COMMAND [ARGS]
  click NAME          activate the first showing object named NAME
  click-in WIN NAME   same, inside the window/dialog whose name contains WIN
  row TEXT            select and activate the first showing list item containing TEXT
  tab NAME            select the notebook/stack page tab named NAME
  names [FILTER]      print showing object names and roles
Only the owned private accessibility bus is accepted.
"""
import os
import sys
import time

import pyatspi

if not os.environ.get("AT_SPI_BUS_ADDRESS", "").startswith("unix:path=/tmp/cdp"):
    raise SystemExit("Private precheck accessibility bus required")


def walk(node, depth=0):
    if node is None or depth > 60:
        return
    yield node, depth
    try:
        children = list(node)
    except Exception:
        children = []
    for child in children:
        yield from walk(child, depth + 1)


def showing(node):
    try:
        return node.getState().contains(pyatspi.STATE_SHOWING)
    except Exception:
        return False


def roots(window=None):
    desktop = pyatspi.Registry.getDesktop(0)
    for app in desktop:
        if app is None:
            continue
        for top in app:
            if top is None:
                continue
            if window and window not in (top.name or ""):
                continue
            yield top


def find(predicate, window=None):
    for top in roots(window):
        for node, _ in walk(top):
            if showing(node) and predicate(node):
                return node
    return None


def actionable(node):
    try:
        return node.queryAction().nActions > 0
    except Exception:
        return False


def act(node):
    action = node.queryAction()
    names = [action.getName(i) for i in range(action.nActions)]
    for wanted in ("click", "activate", "press", "toggle", "select"):
        if wanted in names:
            action.doAction(names.index(wanted))
            return wanted
    if names:
        action.doAction(0)
        return names[0]
    raise SystemExit(f"no action on {node.name!r} ({node.getRoleName()})")


def main():
    command = sys.argv[1]
    if command in ("click", "click-in"):
        window = sys.argv[2] if command == "click-in" else None
        name = sys.argv[-1]
        node = None
        for _ in range(50):
            node = find(lambda n: n.name == name and actionable(n), window)
            if node:
                break
            time.sleep(0.1)
        if not node:
            raise SystemExit(f"not found: {name!r}")
        print(act(node), node.getRoleName(), name)
    elif command == "row":
        text = sys.argv[2]
        node = find(lambda n: n.getRoleName() in ("list item", "table row") and text in (n.name or ""))
        if not node:
            raise SystemExit(f"no row: {text!r}")
        # GTK list rows expose no AT-SPI action: focus, then activate with Return.
        node.queryComponent().grabFocus()
        time.sleep(0.2)
        import subprocess
        subprocess.run(["xdotool", "key", "Return"], check=True)
        print("return", node.name)
    elif command == "tab":
        name = sys.argv[2]
        node = find(lambda n: n.getRoleName() in ("page tab", "tab", "radio button", "toggle button") and n.name == name)
        if not node:
            raise SystemExit(f"no tab: {name!r}")
        print(act(node), name)
    elif command == "names":
        needle = sys.argv[2] if len(sys.argv) > 2 else ""
        for top in roots():
            for node, depth in walk(top):
                if showing(node) and needle in (node.name or ""):
                    print("  " * min(depth, 20) + f"{node.getRoleName()}: {node.name!r}")
    else:
        raise SystemExit(__doc__)


main()
