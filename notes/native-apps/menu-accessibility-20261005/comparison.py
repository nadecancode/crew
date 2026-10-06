"""Unstyled GTK controls on the same private bus as Crew; no Crew code."""
import os
import gi

if not os.environ.get("AT_SPI_BUS_ADDRESS", "").startswith("unix:abstract=crew-designer-menu-a11y-"):
    raise SystemExit("Private diagnostic accessibility bus required")
gi.require_version("Gtk", "4.0")
from gi.repository import Gio, GLib, Gtk


def activated(action, parameter):
    print("activated", action.get_name(), parameter, flush=True)


def activate(app):
    window = Gtk.ApplicationWindow(application=app, title="GTK menu comparison")
    window.set_default_size(460, 320)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=16)
    for edge in ["top", "bottom", "start", "end"]:
        getattr(box, "set_margin_" + edge)(24)
    window.set_child(box)
    plain = Gtk.Button(label="Plain control label")
    plain.connect("clicked", lambda _: print("plain activated", flush=True))
    box.append(plain)
    box.append(Gtk.CheckButton(label="Direct checkbox label"))
    action = Gio.SimpleAction.new("plain", None)
    action.connect("activate", activated)
    app.add_action(action)
    checked = Gio.SimpleAction.new_stateful("checked", None, GLib.Variant("b", True))
    checked.connect("activate", lambda a, _: a.set_state(GLib.Variant("b", not a.get_state().get_boolean())))
    app.add_action(checked)
    choice = Gio.SimpleAction.new_stateful("choice", GLib.VariantType.new("s"), GLib.Variant("s", "first"))
    choice.connect("activate", lambda a, p: a.set_state(p))
    app.add_action(choice)
    model = Gio.Menu()
    model.append("Plain menu label", "app.plain")
    model.append("Checked menu label", "app.checked")
    submenu = Gio.Menu()
    submenu.append("First choice label", "app.choice('first')")
    submenu.append("Second choice label", "app.choice('second')")
    model.append_submenu("Submenu label", submenu)
    menu = Gtk.MenuButton(label="Model menu", menu_model=model)
    box.append(menu)
    window.present()


app = Gtk.Application(application_id="dev.crew.MenuAccessibilityComparison")
app.connect("activate", activate)
raise SystemExit(app.run([]))
