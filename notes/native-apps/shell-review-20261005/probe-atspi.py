import os

# Diagnostic for the explicitly owned empty-shell accessibility bus only.
if not os.environ.get("AT_SPI_BUS_ADDRESS", "").startswith("unix:abstract=crew-designer-shell-a11y-"):
    raise SystemExit("Use the owned private review accessibility bus; never the operator desktop")

import json
import sys
import time

import pyatspi


def nodes(root, depth=0):
    if depth > 24:
        return
    yield root, depth
    for child in root:
        if child is not None:
            yield from nodes(child, depth + 1)


desktop = pyatspi.Registry.getDesktop(0)
apps = list(desktop)
if not apps:
    raise SystemExit("No application on private AT-SPI desktop")

if len(sys.argv) > 2 and sys.argv[1] == "activate":
    matches = []
    for app in apps:
        for node, depth in nodes(app):
            if node.name == sys.argv[2] and node.getState().contains(pyatspi.STATE_SHOWING):
                try:
                    action = node.queryAction()
                    if action.nActions:
                        matches.append((node, action))
                except NotImplementedError:
                    pass
    if len(matches) != 1:
        raise SystemExit(f"Expected one showing action named {sys.argv[2]!r}; found {len(matches)}")
    node, action = matches[0]
    print(json.dumps({"name": node.name, "role": node.getRoleName(), "action": action.getName(0), "sent": action.doAction(0)}))
    time.sleep(0.15)
    raise SystemExit(0)

states = {
    "showing": pyatspi.STATE_SHOWING,
    "visible": pyatspi.STATE_VISIBLE,
    "focused": pyatspi.STATE_FOCUSED,
    "focusable": pyatspi.STATE_FOCUSABLE,
    "selected": pyatspi.STATE_SELECTED,
    "enabled": pyatspi.STATE_ENABLED,
    "checked": pyatspi.STATE_CHECKED,
    "expanded": pyatspi.STATE_EXPANDED,
}
result = []
for app in apps:
    for node, depth in nodes(app):
        state = node.getState()
        item = {"depth": depth, "role": node.getRoleName(), "name": node.name,
                "states": [name for name, value in states.items() if state.contains(value)]}
        try:
            rect = node.queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
            item["bounds"] = [rect.x, rect.y, rect.width, rect.height]
        except NotImplementedError:
            pass
        result.append(item)
print(json.dumps(result, indent=2))
