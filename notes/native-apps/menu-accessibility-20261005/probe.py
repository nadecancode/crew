"""Compare pyatspi/libatspi results with raw AT-SPI D-Bus replies."""
import json
import os
import sys

import gi
import pyatspi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, Gio, GLib

address = os.environ.get("AT_SPI_BUS_ADDRESS", "")
if not address.startswith("unix:abstract=crew-designer-menu-a11y-"):
    raise SystemExit("Private diagnostic accessibility bus required")
connection = Gio.DBusConnection.new_for_address_sync(
    address,
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None,
    None,
)


def raw(bus, path, interface, method, args=None):
    try:
        return connection.call_sync(bus, path, interface, method, args, None,
                                    Gio.DBusCallFlags.NONE, 3000, None).unpack()
    except GLib.Error as error:
        return {"error": str(error)}


def dbus_accessible(bus, path):
    interface = "org.a11y.atspi.Accessible"
    return {
        "properties": raw(bus, path, "org.freedesktop.DBus.Properties", "GetAll", GLib.Variant("(s)", (interface,))),
        "role": raw(bus, path, interface, "GetRoleName"),
        "states": raw(bus, path, interface, "GetState"),
        "interfaces": raw(bus, path, interface, "GetInterfaces"),
    }


def walk(node, depth=0):
    if depth > 24:
        return
    yield node, depth
    for child in node:
        if child is not None:
            yield from walk(child, depth + 1)


states = {key: getattr(pyatspi, "STATE_" + key.upper()) for key in [
    "showing", "visible", "focused", "focusable", "selected", "enabled", "checked", "expanded", "defunct"
]}


def describe(node):
    state = node.getState()
    return {"name": node.name, "role": node.getRoleName(),
            "states": [name for name, value in states.items() if state.contains(value)]}


apps = list(pyatspi.Registry.getDesktop(0))
if len(sys.argv) == 3 and sys.argv[1] == "activate":
    candidates = []
    for app in apps:
        for node, _ in walk(app):
            if node.name != sys.argv[2] or node.getRoleName() != "toggle button" or not node.getState().contains(pyatspi.STATE_SHOWING):
                continue
            try:
                if node.queryAction().nActions:
                    candidates.append(node)
            except NotImplementedError:
                pass
    if len(candidates) != 1:
        raise SystemExit(f"Expected one showing {sys.argv[2]!r}; got {len(candidates)}")
    success = candidates[0].queryAction().doAction(0)
    print(json.dumps({"node": describe(candidates[0]), "operation": sys.argv[1], "success": success}))
    raise SystemExit(0)

result = []
for app in apps:
    for node, depth in walk(app):
        item = {"depth": depth, "bus": node.app.bus_name, "path": node.path,
                "cached": describe(node)}
        if depth < 2 or "menu" in item["cached"]["role"] or item["cached"]["name"] in ["Plain control label", "Direct checkbox label"]:
            node.clear_cache()
            item["after_clear"] = describe(node)
            item["dbus"] = dbus_accessible(node.app.bus_name, node.path)
            item["relations"] = raw(node.app.bus_name, node.path, "org.a11y.atspi.Accessible", "GetRelationSet")
            related = []
            for relation in node.getRelationSet():
                for i in range(relation.getNTargets()):
                    target = relation.getTarget(i)
                    related.append({"type": int(relation.getRelationType()), "bus": target.app.bus_name,
                                    "path": target.path, "libatspi": describe(target),
                                    "dbus": dbus_accessible(target.app.bus_name, target.path),
                                    "text": raw(target.app.bus_name, target.path, "org.a11y.atspi.Text", "GetText", GLib.Variant("(ii)", (0, -1)))})
            item["relation_targets"] = related
        result.append(item)
print(json.dumps(result, indent=2))
