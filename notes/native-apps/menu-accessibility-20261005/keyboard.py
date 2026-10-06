"""Real X keyboard menu navigation; reduced records retain raw name/state data."""
import json
import os
import pathlib
import subprocess
import sys
import time

if not os.environ.get("AT_SPI_BUS_ADDRESS", "").startswith("unix:abstract=crew-designer-menu-a11y-"):
    raise SystemExit("Private diagnostic accessibility bus required")
root = pathlib.Path(__file__).resolve().parent
out = pathlib.Path(sys.argv[1])
app_pid = sys.argv[2]
records = []


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def key(*keys):
    for name in keys:
        command("xdotool", "key", name)
        time.sleep(0.4)


def activate(label):
    command(sys.executable, str(root / "probe.py"), "activate", label)
    time.sleep(0.4)


def capture(stage):
    tree = json.loads(command(sys.executable, str(root / "probe.py")))
    crew = [x for x in tree if x["path"].startswith("/dev/crew/Crew/")]
    focus = [x for x in crew if "focused" in x["cached"]["states"]]
    menus = [x for x in crew if "menu" in x["cached"]["role"]]
    record = {"stage": stage, "focus": [], "menu_items": []}
    for field, nodes in [("focus", focus), ("menu_items", menus)]:
        for node in nodes:
            item = {"path": node["path"], **node["cached"]}
            if "dbus" in node:
                item["dbus_name"] = node["dbus"]["properties"][0]["Name"]
                item["dbus_role"] = node["dbus"]["role"]
                item["dbus_states"] = node["dbus"]["states"]
                item["labels"] = [{"path": r["path"], "text": r["text"], "relation_type": r["type"],
                                   "role": r["dbus"]["role"], "name": r["libatspi"]["name"]}
                                  for r in node.get("relation_targets", []) if r["type"] == 2]
            record[field].append(item)
    records.append(record)
    print(stage, [(x["name"], [r["text"] for r in x.get("labels", [])], x["states"]) for x in record["focus"]], flush=True)
    (out / "keyboard-sequence.json").write_text(json.dumps(records, indent=2) + "\n")


window = command("xdotool", "search", "--onlyvisible", "--all", "--pid", app_pid, "--name", "Crew")
if not window.isdigit():
    raise SystemExit(f"Expected one Crew window; got {window!r}")
command("xdotool", "windowactivate", "--sync", window)
key("Escape", "Escape")
activate("Application menu")
capture("01-application-first")
for i in range(5):
    key("Down")
    capture(f"02-application-down-{i + 1}")
key("Escape")
capture("03-application-dismissed")
activate("Application menu")
key("Down", "Down", "Right")
capture("04-appearance-open")
for i in range(3):
    key("Down")
    capture(f"05-appearance-down-{i + 1}")
key("Return")
capture("06-appearance-chosen")
activate("Application menu")
key("Down", "Down", "Right")
capture("07-appearance-reopened")
key("Escape", "Escape")
capture("08-appearance-dismissed")
