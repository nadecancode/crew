"""Black-box acceptance driver for the Linux Crew app.

Runs inside acceptance/session.sh: the app is the real GTK binary, the daemon
is a private scripted team, input is X11 keyboard events and observation uses
the private AT-SPI bus, screenshots and `crew ... --json` queries.
"""
import json
import os
import re
import subprocess
import sys
import time
import traceback

import gi

gi.require_version("Atspi", "2.0")
import pyatspi  # noqa: E402

OUT = os.environ["NATIVE_ACCEPTANCE_OUT"]
APP = os.environ["NATIVE_ACCEPTANCE_APP"]
CREW = os.environ["NATIVE_ACCEPTANCE_CREW"]
WS = os.path.join(OUT, "work", "ws")
CAPTURES = os.path.join(OUT, "captures")
LOGS = os.path.join(OUT, "logs")

if not os.environ.get("AT_SPI_BUS_ADDRESS", "").startswith("unix:abstract=crew-qa-acceptance-"):
    raise SystemExit("private acceptance accessibility bus required")

STATES = {
    name: getattr(pyatspi, "STATE_" + name.upper())
    for name in [
        "showing", "visible", "focused", "focusable", "selected", "selectable", "enabled",
        "sensitive", "checked", "expanded", "editable", "active", "defunct", "modal",
    ]
}


def crew(*args, check=True):
    result = subprocess.run([CREW, *args, "-C", WS], capture_output=True, text=True, timeout=60)
    if check and result.returncode != 0:
        raise RuntimeError(f"crew {' '.join(args)} failed: {result.stderr.strip()}")
    return result


def crew_json(*args):
    return json.loads(crew(*args, "--json").stdout)


class App:
    def __init__(self):
        self.process = None
        self.window = None

    def start(self, label="app"):
        os.makedirs(os.path.join(OUT, "work", "app"), exist_ok=True)
        log = open(os.path.join(LOGS, f"{label}.log"), "ab")
        self.process = subprocess.Popen([APP], cwd=os.path.join(OUT, "work", "app"),
                                        stdout=log, stderr=subprocess.STDOUT)
        with open(os.path.join(OUT, "app.pid"), "w") as handle:
            handle.write(str(self.process.pid))
        self.window = self.wait_window()
        return self

    def alive(self):
        return self.process is not None and self.process.poll() is None

    def accessible(self, timeout=20):
        end = time.time() + timeout
        while time.time() < end:
            if not self.alive():
                raise RuntimeError("application exited")
            desktop = pyatspi.Registry.getDesktop(0)
            for child in desktop:
                try:
                    if child is not None and child.get_process_id() == self.process.pid:
                        return child
                except Exception:
                    continue
            time.sleep(0.2)
        raise RuntimeError("application never appeared on the accessibility bus")

    def wait_window(self, timeout=20):
        end = time.time() + timeout
        while time.time() < end:
            found = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid",
                                    str(self.process.pid)], capture_output=True, text=True)
            ids = found.stdout.split()
            if ids:
                return ids[-1]
            if not self.alive():
                raise RuntimeError("application exited before mapping a window")
            time.sleep(0.2)
        raise RuntimeError("no mapped window")

    def target_window(self):
        """The app window that should receive keyboard input: an open dialog
        (which is modal) before the main window."""
        try:
            dialogs = find_all(self.accessible(timeout=2), role="dialog")
        except Exception:
            dialogs = []
        for dialog in reversed(dialogs):
            found = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(self.process.pid),
                                    "--name", "^" + re.escape(dialog.name) + "$"],
                                   capture_output=True, text=True, timeout=10)
            if found.stdout.split():
                return found.stdout.split()[-1]
        return self.window

    def activate(self):
        """Give X keyboard focus to the window that should receive input."""
        target = self.target_window()
        active = subprocess.run(["xdotool", "getactivewindow"], capture_output=True, text=True, timeout=10)
        if active.returncode == 0 and active.stdout.strip() == str(int(target)):
            return
        try:
            subprocess.run(["xdotool", "windowactivate", "--sync", target], capture_output=True, timeout=5)
        except subprocess.TimeoutExpired:
            pass
        time.sleep(0.3)

    def key(self, *combos, delay=0.25):
        self.activate()
        for combo in combos:
            subprocess.run(["xdotool", "key", "--clearmodifiers", combo], check=True, timeout=10)
            time.sleep(delay)

    def type(self, text, delay=0.3):
        self.activate()
        subprocess.run(["xdotool", "type", "--delay", "20", text], check=True, timeout=30)
        time.sleep(delay)

    def resize(self, width, height):
        """Resize to logical pixels; device pixels follow the display scale."""
        scale = int(os.environ.get("NATIVE_ACCEPTANCE_SCALE", "1"))
        subprocess.run(["xdotool", "windowsize", "--sync", self.window, str(width * scale), str(height * scale)],
                       capture_output=True, timeout=10)
        time.sleep(0.6)

    def capture(self, name):
        path = os.path.join(CAPTURES, f"{name}.png")
        subprocess.run(["magick", "import", "-window", "root", path], check=True, timeout=30)
        return path

    def stop(self):
        if self.alive():
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()


def describe(node):
    try:
        state = node.getState()
        return {
            "name": node.name,
            "role": node.getRoleName(),
            "description": node.description,
            "states": [name for name, value in STATES.items() if state.contains(value)],
        }
    except Exception as error:  # a node can disappear mid-walk
        return {"error": str(error)}


def walk(node, depth=0, limit=40):
    if node is None or depth > limit:
        return
    yield node, depth
    try:
        children = list(node)
    except Exception:
        return
    for child in children:
        yield from walk(child, depth + 1, limit)


def dump(root, name):
    rows = []
    for node, depth in walk(root):
        row = describe(node)
        row["depth"] = depth
        try:
            text = node.queryText()
            row["text"] = text.getText(0, min(text.characterCount, 400))
        except Exception:
            pass
        rows.append(row)
    with open(os.path.join(OUT, f"{name}.json"), "w") as handle:
        json.dump(rows, handle, indent=1)
    return rows


def text_of(node):
    try:
        text = node.queryText()
        return text.getText(0, text.characterCount)
    except Exception:
        return node.name or ""


def find_all(root, name=None, role=None, contains=None, showing=True):
    found = []
    for node, _ in walk(root):
        try:
            if role and node.getRoleName() != role:
                continue
            label = node.name or ""
            if name is not None and label != name:
                continue
            if contains is not None and contains not in label and contains not in text_of(node):
                continue
            if showing and not node.getState().contains(pyatspi.STATE_SHOWING):
                continue
            found.append(node)
        except Exception:
            continue
    return found


def find(root, name=None, role=None, contains=None, timeout=10, showing=True):
    end = time.time() + timeout
    while True:
        nodes = find_all(root, name, role, contains, showing)
        if nodes:
            return nodes[0]
        if time.time() > end:
            raise LookupError(f"no {role or 'node'} named {name or contains!r}")
        time.sleep(0.2)


def act(node, action=None):
    """Invoke a node's native accessible action (default: its first)."""
    actions = node.queryAction()
    index = 0
    if action is not None:
        names = [actions.getName(i) for i in range(actions.nActions)]
        index = names.index(action)
    actions.doAction(index)
    time.sleep(0.4)


def toplevel(node):
    while node is not None and node.getRoleName() not in ("frame", "dialog", "window"):
        node = node.parent
    return node


def click(app, node):
    """Pointer click at the centre of a node, in its own window's coordinates
    (GTK 4 implements no AT-SPI grabFocus)."""
    top = toplevel(node)
    found = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(app.process.pid),
                            "--name", "^" + re.escape(top.name) + "$"],
                           capture_output=True, text=True, timeout=10).stdout.split()
    window = found[-1] if found else app.window
    box = node.queryComponent().getExtents(pyatspi.WINDOW_COORDS)
    subprocess.run(["xdotool", "mousemove", "--window", window, str(box.x + box.width // 2),
                    str(box.y + box.height // 2), "click", "1"], check=True, timeout=10)
    time.sleep(0.4)


def focus_by_tab(app, name, role="button", limit=80, trace=None):
    """Move keyboard focus with Tab until the named control holds it."""
    for _ in range(limit):
        node = focused(app.accessible())
        if trace is not None:
            trace.append(None if node is None else (node.getRoleName(), node.name))
        if node is not None and (node.name or "") == name and node.getRoleName() == role:
            return node
        app.key("Tab", delay=0.1)
    return None


def press(app, node, key="space"):
    """Activate a control the way a keyboard user does: Tab to it, then a key.
    Returns "keyboard", or "pointer" when Tab never reached it."""
    name, role = node.name, node.getRoleName()
    if focus_by_tab(app, name, role) is None:
        click(app, node)
        return "pointer"
    app.key(key)
    return "keyboard"


def select_tab(app, node):
    """Select a page tab: through its tab list's selection where available,
    otherwise by clicking it (GTK tabs expose no action)."""
    try:
        if node.parent.querySelection().selectChild(node.getIndexInParent()):
            time.sleep(0.6)
            return
    except Exception:
        pass
    click(app, node)
    time.sleep(0.4)


class Attached(App):
    """The app of an already running session (probe mode)."""

    def __init__(self):
        super().__init__()
        pid = int(open(os.path.join(OUT, "app.pid")).read())

        class Process:
            def __init__(self, pid):
                self.pid = pid

            def poll(self):
                return None if os.path.exists(f"/proc/{pid}") else 0

        self.process = Process(pid)
        self.window = self.wait_window()


def explore(seconds):
    app = App().start()
    time.sleep(3)
    app.capture("explore-start")
    dump(app.accessible(), "explore-tree")
    end = time.time() + seconds
    stop = os.path.join(OUT, "stop")
    while time.time() < end and not os.path.exists(stop) and app.alive():
        time.sleep(1)
    app.stop()


def probe(command, args):
    app = Attached()
    root = app.accessible()
    if command == "tree":
        rows = dump(root, args[0] if args else "probe-tree")
        for row in rows:
            if "showing" not in row.get("states", []):
                continue
            line = "  " * row["depth"] + f"{row.get('role')} | {row.get('name')} | {','.join(row.get('states', []))}"
            if row.get("text") and row.get("text") != row.get("name"):
                line += " | T:" + row["text"][:100].replace("\n", "\\n")
            print(line)
    elif command == "act":
        node = find(root, name=args[0], role=args[1] if len(args) > 1 else None)
        act(node, args[2] if len(args) > 2 else None)
    elif command == "actions":
        node = find(root, name=args[0], role=args[1] if len(args) > 1 else None)
        actions = node.queryAction()
        print([actions.getName(i) for i in range(actions.nActions)])
    elif command == "key":
        app.key(*args)
    elif command == "type":
        app.type(args[0])
    elif command == "tabto":
        node = focus_by_tab(app, args[0], args[1] if len(args) > 1 else "button", limit=40)
        print(None if node is None else (node.getRoleName(), node.name))
    elif command == "focus":
        node = focused(root)
        print(None if node is None else (node.getRoleName(), node.name, toplevel(node).name))
    elif command == "capture":
        print(app.capture(args[0]))
    elif command == "resize":
        app.resize(int(args[0]), int(args[1]))
    else:
        raise SystemExit(f"unknown probe command {command}")


def focused(root):
    for node, _ in walk(root):
        try:
            if node.getState().contains(pyatspi.STATE_FOCUSED):
                return node
        except Exception:
            continue
    return None


def wait_for(predicate, timeout=15, interval=0.25):
    end = time.time() + timeout
    while True:
        try:
            value = predicate()
            if value:
                return value
        except Exception:
            pass
        if time.time() > end:
            return None
        time.sleep(interval)


def visible_text(root):
    """All showing names and text, for content assertions."""
    parts = []
    for node, _ in walk(root):
        try:
            if not node.getState().contains(pyatspi.STATE_SHOWING):
                continue
            parts.append(node.name or "")
            parts.append(text_of(node))
        except Exception:
            continue
    return "\n".join(part for part in parts if part)


class Recorder:
    """Runs cases, keeping each check, capture and failure as evidence."""

    def __init__(self, suite):
        self.suite = suite
        self.results = []
        self.checks = []
        self.captures = []
        self.case = None
        self.selected = [c for c in os.environ.get("NATIVE_ACCEPTANCE_CASES", "").split(",") if c]
        self.keep_dialogs = set()

    def check(self, ok, what, observed=None):
        self.checks.append({"ok": bool(ok), "check": what, "observed": observed})
        return bool(ok)

    def capture(self, app, label):
        path = app.capture(f"{self.case}-{label}")
        self.captures.append(os.path.relpath(path, OUT))
        return path

    def run(self, cid, title, rows, function, app):
        if self.selected and cid not in self.selected:
            return
        self.case, self.checks, self.captures = cid, [], []
        started = time.time()
        if app.alive():
            self.reset(app)
        status, error = "pass", None
        try:
            function(self, app)
        except Exception as failure:  # the case stops; later cases still run
            status, error = "error", f"{type(failure).__name__}: {failure}"
            with open(os.path.join(LOGS, f"{cid}-traceback.txt"), "w") as handle:
                handle.write(traceback.format_exc())
        if status == "pass" and any(not check["ok"] for check in self.checks):
            status = "fail"
        if status != "pass" and app.alive():
            try:
                self.capture(app, "final")
                dump(app.accessible(), f"trees/{cid}-final")
            except Exception:
                pass
        result = {
            "id": cid, "title": title, "rows": rows, "status": status, "error": error,
            "seconds": round(time.time() - started, 1), "checks": self.checks,
            "captures": self.captures,
        }
        self.results.append(result)
        print(f"{cid} {status}: {title}" + (f" ({error})" if error else ""), flush=True)
        for check in self.checks:
            if not check["ok"]:
                print(f"  FAIL {check['check']}: {check['observed']!r}"[:400], flush=True)

    def reset(self, app):
        """Dismiss dialogs a previous case left open, so one failure does not
        cascade. Escape is the dialogs' own Cancel. A suite can keep a
        multi-step dialog (gateway setup) open between its cases."""
        for _ in range(3):
            try:
                dialogs = [d for d in find_all(app.accessible(), role="dialog")
                           if d.name not in self.keep_dialogs]
            except Exception:
                return
            if not dialogs:
                return
            app.key("Escape")
            time.sleep(0.4)

    def write(self):
        with open(os.path.join(OUT, "results.json"), "w") as handle:
            json.dump({"suite": self.suite, "results": self.results}, handle, indent=1)
        lines = [f"# Linux acceptance: {self.suite} suite", "",
                 "| Case | Rows | Result | Checks | Notes |", "| --- | --- | --- | --- | --- |"]
        for result in self.results:
            failed = [c["check"] for c in result["checks"] if not c["ok"]]
            passed = sum(1 for c in result["checks"] if c["ok"])
            note = result["error"] or "; ".join(failed)
            lines.append(f"| {result['id']} {result['title']} | {result['rows']} | {result['status']} | "
                         f"{passed}/{len(result['checks'])} | {note.replace('|', '/')} |")
        with open(os.path.join(OUT, "results.md"), "w") as handle:
            handle.write("\n".join(lines) + "\n")
        return all(result["status"] == "pass" for result in self.results)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "probe":
        probe(sys.argv[2], sys.argv[3:])
        return 0
    seconds = int(os.environ.get("NATIVE_ACCEPTANCE_EXPLORE", "0"))
    if seconds:
        explore(seconds)
        return 0
    os.makedirs(os.path.join(OUT, "trees"), exist_ok=True)
    suite = os.environ.get("NATIVE_ACCEPTANCE_SUITE", "local")
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    if suite == "local":
        import cases_local as cases
    else:
        import cases_gateway as cases
    recorder = Recorder(suite)
    try:
        cases.run(recorder)
    finally:
        passed = recorder.write()
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
