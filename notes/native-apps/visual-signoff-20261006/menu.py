"""Read the open menu/popover subtrees of the Crew app on the owned bus.

For each menu item: role, raw Name, and the text of its LABELLED_BY target
(the classification in notes/native-apps/menu-accessibility-20261005.md).
Usage: menu.py [press NAME]  (press first invokes a named control's action)
"""
import json
import os
import sys

import pyatspi

if not os.environ.get("AT_SPI_BUS_ADDRESS", "").startswith("unix:path=/tmp/cvs"):
    raise SystemExit("Private signoff accessibility bus required")


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


def text_of(node):
    try:
        t = node.queryText()
        return t.getText(0, -1)
    except Exception:
        return None


desktop = pyatspi.Registry.getDesktop(0)
if len(sys.argv) > 2 and sys.argv[1] == "press":
    for app in desktop:
        for node, _ in walk(app):
            if node.name == sys.argv[2] and node.getState().contains(pyatspi.STATE_SHOWING):
                action = node.queryAction()
                action.doAction(0)
                print(json.dumps({"pressed": node.name, "role": node.getRoleName()}))
                raise SystemExit(0)
    raise SystemExit("not found: " + sys.argv[2])

out = []
for app in desktop:
    for node, depth in walk(app):
        role = node.getRoleName()
        if role in ("menu item", "check menu item", "radio menu item", "menu"):
            rel_text = []
            for rel in node.getRelationSet():
                if rel.getRelationType() == pyatspi.RELATION_LABELLED_BY:
                    for i in range(rel.getNTargets()):
                        target = rel.getTarget(i)
                        rel_text.append({"role": target.getRoleName(), "name": target.name, "text": text_of(target)})
            state = node.getState()
            out.append({"depth": depth, "role": role, "name": node.name,
                        "showing": state.contains(pyatspi.STATE_SHOWING),
                        "checked": state.contains(pyatspi.STATE_CHECKED),
                        "labelled_by": rel_text})
print(json.dumps(out, indent=1))
