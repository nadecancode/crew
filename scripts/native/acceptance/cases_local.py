"""Local-operator acceptance cases (notes/native-apps.md N01-N18, C01 local part).

Each case drives the real app on the private display and checks its effect in
the accessibility tree and in the private daemon (`crew ... --json`). Cases run
in order and share one daemon; L16 stops the team last.
"""
import os
import re
import signal
import subprocess
import time

import pyatspi

from driver import (
    OUT, WS, App, act, click, crew, crew_json, dump, find, find_all, focus_by_tab, focused, press,
    select_tab, toplevel,
    text_of, visible_text, wait_for,
)

WIDTH, HEIGHT = 1280, 760


def status():
    return crew_json("status")


def agent(snapshot, agent_id):
    return next(a for a in snapshot["agents"] if a["id"] == agent_id)


def task(snapshot, task_id):
    return next(t for t in snapshot["tasks"] if t["id"] == task_id)


def messages(count=200):
    return crew_json("messages", "-n", str(count))


def daemon_alive():
    return crew("status", "--json", check=False).returncode == 0


def root(app):
    return app.accessible()


def frame(app):
    return find(root(app), role="frame")


def dialog(app, title=None, timeout=10):
    return find(root(app), name=title, role="dialog", timeout=timeout)


def no_dialog(app, title):
    return wait_for(lambda: not find_all(root(app), name=title, role="dialog"), timeout=8)


def button(container, name, timeout=10):
    return find(container, name=name, role="button", timeout=timeout)


def local_connect(connections):
    """The same-host Connect control: `Connect to local workspace`, or the
    first `Connect` (under This computer) where remote profiles are listed."""
    named = find_all(connections, name="Connect to local workspace", role="button")
    return named[0] if named else find(connections, name="Connect", role="button")


def live_labels(app):
    """Connection status labels that claim an authenticated live connection.
    A label such as `Disconnected · Live` contradicts itself and is not live."""
    return [n for n in find_all(root(app), contains="Live", role="label")
            if (n.name or "").startswith("Authenticated") and "Live" in (n.name or "")]


def type_into(app, node, text):
    """Replace a field's text by keyboard. In a dialog, Tab reaches the field
    (GTK scrolls it into view); in the main window, where Tab would cross the
    sidebar, a pointer click focuses it."""
    name, role = node.name, node.getRoleName()
    how, trace = "tab", []
    if toplevel(node).getRoleName() != "dialog" or focus_by_tab(app, name, role, limit=40, trace=trace) is None:
        click(app, node)
        how = "click"
    now = focused(app.accessible())
    with open(os.path.join(OUT, "logs", "typing.log"), "a") as log:
        log.write(f"{name!r} via {how}; focused {safe(lambda: (now.getRoleName(), now.name), None)!r}; tab trace {trace!r}\n")
    app.key("ctrl+a")
    app.type(text)


def agent_node(app, agent_id, timeout=10):
    """The selectable agent entry in the Team destination (graph or list)."""
    prefixes = (f"{agent_id} · ", f"{agent_id}, ")
    def search():
        for node in find_all(root(app)):
            try:
                name = node.name or ""
                if name.startswith(prefixes) and node.getRoleName() in ("button", "list item"):
                    return node
            except Exception:
                continue
        return None
    node = wait_for(search, timeout=timeout)
    if node is None:
        raise LookupError(f"no Team entry for {agent_id}")
    return node


def select_agent(app, agent_id):
    app.key("ctrl+1")
    act(agent_node(app, agent_id))
    find(root(app), name=f"Agent {agent_id}", role="label", timeout=10)


def connect(rec, app, label):
    act(find(root(app), name="Choose workspace or view connection details", role="button"))
    connections = dialog(app, "Connect to Crew")
    rec.capture(app, f"{label}-connections")
    act(local_connect(connections))
    live = wait_for(lambda: live_labels(app), timeout=20)
    rec.check(live, f"{label}: connection reaches Live", [n.name for n in live or []])
    no_dialog(app, "Connect to Crew")
    return live


# L01 ---------------------------------------------------------------------
def l01_connect(rec, app):
    before = find(root(app), contains="Disconnected", role="label", timeout=15)
    rec.check(before is not None, "app starts without joining a workspace", before.name)
    rec.check(not re.search(r"[a-z][A-Z]", before.name),
              "disconnected status uses readable copy, not an internal state name", before.name)
    rec.capture(app, "disconnected")
    daemon = open(os.path.join(OUT, "daemon.txt")).read().split()[0].split("=")[1]
    act(find(root(app), name="Choose workspace or view connection details", role="button"))
    connections = dialog(app, "Connect to Crew")
    listing = visible_text(connections)
    rec.check(WS in listing, "Connections lists the private same-host workspace", listing[:400])
    editable = [n for n, _ in walk_nodes(connections)
                if safe(lambda: n.getState().contains(pyatspi.STATE_EDITABLE), False)]
    rec.check(not editable, "no code, token or key field in local connection setup",
              [n.name for n in editable])
    rec.check(daemon_alive() and status()["session"]["daemon_pid"] == int(daemon),
              "opening Connections does not start, join or replace a daemon")
    rec.capture(app, "connections")
    act(local_connect(connections))
    live = wait_for(lambda: live_labels(app), timeout=20)
    rec.check(live, "connection reaches Live", [n.name for n in live or []])
    text = visible_text(root(app))
    rec.check("Native Linux acceptance fixture" in text, "objective shown after connection")
    rec.check("5 agents" in text, "agent count shown after connection")
    rec.check("Local operator · full Crew access" in text,
              "local authority labelled 'Local operator · full Crew access' (S-section copy)",
              [n.name for n in live or []])
    rec.capture(app, "live")


def walk_nodes(node):
    from driver import walk
    return walk(node)


def safe(function, default):
    try:
        return function()
    except Exception:
        return default


# L02 ---------------------------------------------------------------------
def l02_destinations(rec, app):
    for index, name in enumerate(["Team", "Tasks", "Messages", "Events", "Usage"], start=1):
        app.key(f"ctrl+{index}")
        item = wait_for(lambda: next((n for n in find_all(root(app), name=name, role="list item")
                                      if n.getState().contains(pyatspi.STATE_SELECTED)), None), timeout=5)
        title = safe(lambda: frame(app).name, "")
        rec.check(item is not None, f"Ctrl+{index} selects {name}", title)
        rec.check(title.startswith(name), f"window title names {name}", title)
        rec.capture(app, name.lower())
    app.key("ctrl+1")


# L03 ---------------------------------------------------------------------
def l03_team(rec, app):
    snapshot = status()
    app.key("ctrl+1")
    dev = agent_node(app, "dev")
    reviewer = agent_node(app, "reviewer")
    rec.check(re.search(r"current(?: task)?:? schema", dev.name, re.I), "dev's executing task is current", dev.name)
    rec.check(agent(snapshot, "reviewer")["status"] in reviewer.name
              and re.search(r"current(?: task)?:? release", reviewer.name, re.I),
              "started Blocked work stays current with its status", reviewer.name)
    rec.capture(app, "graph")
    select_tab(app, find(root(app), name="List", role="page tab"))
    time.sleep(0.5)
    text = visible_text(root(app))
    rec.check("dev · dev" in text and "Current task schema" in text, "list agrees with graph for dev",
              text[:400])
    rows = [n for n in find_all(root(app), role="list item") if "dev · dev" in visible_text(n)]
    rec.check(any("dev" in (n.name or "") for n in rows),
              "list row exposes the agent as its accessible name", [n.name for n in rows])
    rec.capture(app, "list")
    select_tab(app, find(root(app), name="Graph", role="page tab"))
    select_agent(app, "dev")
    detail = visible_text(find(root(app), name="Agent dev", role="label").parent.parent)
    rec.check("Current work: schema" in detail, "inspector puts current work first", detail[:300])
    rec.capture(app, "inspector")


# L04 ---------------------------------------------------------------------
def l04_tasks(rec, app):
    before = {t["id"]: t["status"] for t in status()["tasks"]}
    app.key("ctrl+2")
    time.sleep(1)
    text = visible_text(root(app))
    for task_id, task_status in before.items():
        rec.check(task_id in text, f"task {task_id} listed", task_status)
    rec.capture(app, "tasks")
    docs = wait_for(lambda: next((n for n in find_all(root(app), contains="docs")
                                  if n.getRoleName() in ("list item", "button", "table cell")), None))
    if rec.check(docs is not None, "queued docs card selectable"):
        click(app, docs)
        detail = wait_for(lambda: "Write the operator notes" in visible_text(root(app)) and visible_text(root(app)))
        rec.check(detail and "A second card queued" in detail, "task detail shows description")
        rec.capture(app, "docs-detail")
    api = visible_text(root(app))
    after = {t["id"]: t["status"] for t in status()["tasks"]}
    rec.check(after == before, "inspecting tasks changes no task or execution state", after)
    rec.check("Build the API" in api or "api" in api, "dependent api card visible")
    app.key("ctrl+1")
    dev = agent_node(app, "dev")
    rec.check(safe(lambda: dev.getState().contains(pyatspi.STATE_SELECTED) or
                   bool(find_all(root(app), name="Agent dev", role="label")), False),
              "returning to Team keeps the selected agent", dev.name)


# L05 ---------------------------------------------------------------------
def l05_question(rec, app):
    pending = crew_json("questions")
    rec.check(len(pending) == 1 and pending[0]["from"] == "asker", "fixture question pending", pending)
    app.key("ctrl+1")
    act(button(root(app), "Questions (1)"))
    question = wait_for(lambda: next((n for n in find_all(root(app), role="dialog")
                                      if "Which deployment target" in visible_text(n)), None))
    if not rec.check(question is not None, "question dialog shows the full body"):
        return
    text = visible_text(question)
    rec.check("asker" in text, "question names its asker", text[:300])
    rec.capture(app, "dialog")
    editor = find(question, name="Message or answer text", role="text")
    type_into(app, editor, "Use the staging target")
    act(button(question, "Answer"))
    answered = wait_for(lambda: not crew_json("questions"), timeout=20)
    rec.check(answered, "answer clears the daemon question")
    sent = [m for m in messages() if m["from"] == "operator" and m["to"] == "asker"]
    rec.check(any("Use the staging target" in m["body"] for m in sent), "answer delivered to the asker",
              [m["body"] for m in sent])
    rec.check(wait_for(lambda: find_all(root(app), name="Questions (0)", role="button")),
              "question count returns to zero")
    rec.capture(app, "answered")


# L06 ---------------------------------------------------------------------
def l06_messages(rec, app):
    app.key("ctrl+3")
    time.sleep(1)
    text = visible_text(root(app))
    rec.check("Which deployment target" in text, "question appears in Messages")
    rec.check("Use the staging target" in text, "operator answer appears in Messages")
    rec.capture(app, "messages")
    row = wait_for(lambda: next((n for n in find_all(root(app), contains="Use the staging target")
                                 if n.getRoleName() in ("list item", "button", "table cell")), None))
    if rec.check(row is not None, "answer row selectable"):
        click(app, row)
        time.sleep(0.8)
        rec.capture(app, "detail")


# L07 ---------------------------------------------------------------------
def l07_send(rec, app):
    body = "acceptance ping from the Linux app"
    app.key("ctrl+1")
    act(button(root(app), "New message"))
    composer = dialog(app, "New message")
    type_into(app, find(composer, name="Recipient; * broadcasts to the team", role="text"), "echo")
    type_into(app, find(composer, name="Message or answer text", role="text"), body)
    rec.capture(app, "composer")
    act(button(composer, "Send"))
    delivered = wait_for(lambda: [m for m in messages() if m["from"] == "operator" and m["to"] == "echo"
                                  and m["body"] == body], timeout=20)
    rec.check(delivered and len(delivered) == 1, "exactly one operator message reaches echo", delivered)
    reply = wait_for(lambda: [m for m in messages() if m["from"] == "echo" and "ack from echo" in m["body"]],
                     timeout=20)
    rec.check(reply, "echo's reply arrives at the daemon")
    no_dialog(app, "New message")
    app.key("ctrl+3")
    shown = wait_for(lambda: "ack from echo" in visible_text(root(app)), timeout=15)
    rec.check(shown, "reply appears in the app without reload")
    rec.capture(app, "reply")


# L08 ---------------------------------------------------------------------
def l08_controls(rec, app):
    select_agent(app, "idle")
    act(button(root(app), "Pause"))
    paused = wait_for(lambda: agent(status(), "idle").get("paused") is True, timeout=15)
    rec.check(paused, "Pause reaches the daemon", agent(status(), "idle").get("paused"))
    rec.check(wait_for(lambda: "paused" in visible_text(root(app)), timeout=10),
              "inspector shows the paused state from the daemon")
    rec.capture(app, "paused")
    act(button(root(app), "Unpause"))
    resumed = wait_for(lambda: agent(status(), "idle").get("paused") is False, timeout=15)
    rec.check(resumed, "Unpause reaches the daemon", agent(status(), "idle").get("paused"))
    compact = button(root(app), "Compact")
    rec.check(not compact.getState().contains(pyatspi.STATE_SENSITIVE), "unsupported Compact is unavailable")
    rec.check("does not support context compaction" in visible_text(root(app)),
              "unavailable Compact shows its reason")


# L09 ---------------------------------------------------------------------
def l09_confirmations(rec, app):
    select_agent(app, "idle")
    before = agent(status(), "idle")
    # Keyboard activation, so the opener really holds focus before the dialog.
    press(app, button(root(app), "Kill"))
    kill = dialog(app, "Kill agent")
    text = visible_text(kill)
    rec.check("idle" in text and "task stays open" in text, "Kill names its target and consequence", text[:200])
    default = focused(kill)
    rec.check(default is not None and default.name == "Cancel", "Cancel is the initial focus in Kill",
              safe(lambda: (default.getRoleName(), default.name), None))
    rec.capture(app, "kill")
    app.key("Escape")
    rec.check(no_dialog(app, "Kill agent"), "Escape dismisses Kill")
    time.sleep(1)
    after = agent(status(), "idle")
    rec.check(after["status"] == before["status"] and after.get("pid") == before.get("pid"),
              "cancelled Kill sends nothing", (before["status"], after["status"]))
    now = focused(root(app))
    rec.check(now is not None and (now.name or "") == "Kill", "focus returns to Kill",
              safe(lambda: (now.getRoleName(), now.name), None))
    act(button(root(app), "Restart fresh"))
    fresh = dialog(app, "Restart fresh")
    rec.check("without resuming" in visible_text(fresh), "Restart fresh explains the consequence")
    rec.capture(app, "restart-fresh")
    press(app, button(fresh, "Cancel"))
    rec.check(no_dialog(app, "Restart fresh"), "Cancel dismisses Restart fresh")
    app.key("ctrl+1")
    act(button(root(app), "Stop team"))
    stop = dialog(app, "Stop team")
    rec.check("Committed work remains" in visible_text(stop), "Stop team keeps committed-work wording")
    rec.capture(app, "stop-team")
    app.key("Escape")
    rec.check(no_dialog(app, "Stop team"), "Escape dismisses Stop team")
    rec.check(daemon_alive(), "cancelled Stop team leaves the daemon running")


# L10 ---------------------------------------------------------------------
def l10_events(rec, app):
    app.key("ctrl+4")
    time.sleep(1)
    text = visible_text(root(app))
    rec.check("task" in text.lower() or "agent" in text.lower(), "Events lists daemon events", text[:300])
    rec.capture(app, "events")
    select_agent(app, "dev")
    tabs = [n.name for n in find_all(root(app), role="page tab", showing=False)]
    for name in ["Overview", "Transcript", "Diff", "Inbox", "Terminal", "Host screen"]:
        rec.check(name in tabs, f"agent tab '{name}' exposed to assistive technology", tabs)
    select_tab(app, find(root(app), name="Transcript", role="page tab"))
    time.sleep(1.5)
    rec.capture(app, "transcript")


# L11 ---------------------------------------------------------------------
def terminal_tab(app):
    tab = find_all(root(app), name="Terminal", role="page tab", showing=False)
    if tab:
        select_tab(app, tab[0])
        return "accessible tab"
    # The tab is not exposed: reach it as a keyboard user would.
    overview = find(root(app), name="Overview", role="page tab")
    select_tab(app, overview)
    click(app, overview)
    app.key("ctrl+Page_Down", "ctrl+Page_Down", "ctrl+Page_Down", "ctrl+Page_Down")
    return "keyboard"


def l11_terminal(rec, app):
    select_agent(app, "dev")
    how = terminal_tab(app)
    attach = wait_for(lambda: find(root(app), name="Attach PTY terminal", role="button", timeout=1), timeout=5)
    if not rec.check(attach is not None, f"Terminal page reachable ({how})"):
        return
    act(attach)
    term = wait_for(lambda: find(root(app), role="terminal", timeout=1), timeout=10)
    if not rec.check(term is not None, "attach opens a native terminal"):
        return
    shown = wait_for(lambda: "[dev]" in text_of(term), timeout=10)
    rec.check(shown, "terminal exposes the attached PTY screen to assistive technology",
              text_of(term)[:200])
    rec.capture(app, "attached")
    click(app, term)
    app.type("acceptance-input")
    app.key("Return")
    echoed = wait_for(lambda: "acceptance-input" in crew("agent", "output", "dev").stdout, timeout=10)
    rec.check(echoed, "terminal input reaches the agent's PTY")
    rec.check(wait_for(lambda: "acceptance-input" in text_of(term), timeout=10),
              "terminal renders the echoed input", text_of(term)[-300:])
    rec.check("[dev]" in text_of(term), "terminal accessible text includes the earlier PTY output",
              text_of(term)[:300])
    app.resize(980, 700)
    rec.check(app.alive(), "terminal survives a window resize")
    rec.capture(app, "resized")
    app.resize(WIDTH, HEIGHT)


# L12 ---------------------------------------------------------------------
def l12_usage(rec, app):
    app.key("ctrl+5")
    time.sleep(1)
    text = visible_text(root(app))
    rec.check("not reported" in text.lower(), "unreported usage says Not reported", text[:300])
    rec.check(not re.search(r"\b0 tokens\b", text), "unreported usage is not shown as zero")
    rec.capture(app, "usage")


# L13 ---------------------------------------------------------------------
def l13_lifecycle(rec, app):
    pid = status()["session"]["daemon_pid"]
    app.key("ctrl+w")
    exited = wait_for(lambda: not app.alive(), timeout=10)
    rec.check(exited, "Ctrl+W closes the client")
    rec.check(daemon_alive() and status()["session"]["daemon_pid"] == pid, "closing the app leaves the team running")
    app.start("app-reopened")
    app.resize(WIDTH, HEIGHT)
    rec.check(connect(rec, app, "reopen"), "reopened app reconnects")
    os.kill(pid, signal.SIGKILL)
    lost = wait_for(lambda: not live_labels(app), timeout=20)
    text = visible_text(root(app))
    rec.check(lost, "lost daemon is no longer shown Live", text[:300])
    rec.check(any(word in text for word in ["Reconnecting", "not running", "cannot be reached",
                                             "Unreachable", "Disconnected"]),
              "lost daemon shows an honest connection state", text[:300])
    rec.check("Last known" in text, "retained content is labelled Last known while disconnected", text[:300])
    rec.capture(app, "daemon-lost")
    def relaunch():
        return subprocess.run([os.environ["NATIVE_ACCEPTANCE_CREW"], "run", "team.yaml", "--no-attach"],
                              cwd=os.path.join(OUT, "work"), capture_output=True, text=True, timeout=120)
    run = relaunch()
    if run.returncode != 0:
        # Fixture recovery, not an app check: a killed daemon can leave team
        # state that `crew stop` clears. Record it, then restart.
        with open(os.path.join(OUT, "logs", "L13-relaunch.log"), "w") as handle:
            handle.write(run.stdout + run.stderr)
        crew("stop", check=False)
        run = relaunch()
    rec.check(run.returncode == 0, "team restarts", run.stderr[-300:])
    wait_for(lambda: all(a["status"] != "spawning" for a in status()["agents"]), timeout=40)
    new_pid = status()["session"]["daemon_pid"]
    with open(os.path.join(OUT, "daemon.txt"), "a") as handle:
        handle.write(f"daemon_pid={new_pid}\n")
    automatic = wait_for(lambda: live_labels(app), timeout=30)
    if not automatic:
        act(find(root(app), name="Choose workspace or view connection details", role="button"))
        act(button(dialog(app, "Connect to Crew"), "Refresh local profiles"))
        act(local_connect(dialog(app, "Connect to Crew")))
    rec.check(automatic or wait_for(lambda: live_labels(app), timeout=20),
              "app returns to Live on the restarted daemon", "automatic" if automatic else "explicit")
    snapshot = status()
    rec.check(wait_for(lambda: f"{len(snapshot['agents'])} agents" in visible_text(root(app)), timeout=10),
              "header reflects the new daemon's snapshot")
    rec.capture(app, "reconnected")


# L14 ---------------------------------------------------------------------
def l14_layout(rec, app):
    for width, height, label in [(1024, 600, "1024x600"), (560, 700, "narrow")]:
        app.resize(width, height)
        app.key("ctrl+3")
        title = safe(lambda: frame(app).name, "")
        rec.check(title.startswith("Messages"), f"{label}: destinations reachable by keyboard", title)
        rec.capture(app, label)
    sidebar = lambda: safe(lambda: find(root(app), name="Destinations", role="list", timeout=1)
                           .getState().contains(pyatspi.STATE_SHOWING), False)
    shown = sidebar()
    app.key("ctrl+b")
    rec.check(sidebar() != shown, "Ctrl+B toggles the sidebar", (shown, sidebar()))
    rec.capture(app, "narrow-sidebar-toggled")
    app.key("ctrl+b")
    app.resize(WIDTH, HEIGHT)
    rec.capture(app, "restored")


# L15 ---------------------------------------------------------------------
def l15_keyboard(rec, app):
    app.key("F1")
    help_dialog = wait_for(lambda: next((n for n in find_all(root(app))
                                         if n.getRoleName() in ("dialog", "frame", "window")
                                         and "Keyboard shortcuts" in visible_text(n)
                                         and not n.name.endswith("Crew")), None), timeout=5)
    rec.check(help_dialog is not None, "F1 opens shortcut help")
    rec.capture(app, "help")
    app.key("Escape")
    app.key("ctrl+1")
    sequence, unnamed = [], []
    for _ in range(45):
        app.key("Tab", delay=0.15)
        node = focused(root(app))
        if node is None:
            sequence.append(None)
            continue
        entry = (node.getRoleName(), (node.name or text_of(node) or "").split("\n")[0][:60])
        sequence.append(entry)
        if not entry[1] and entry[0] not in ("terminal",):
            unnamed.append(entry)
    with open(os.path.join(OUT, "logs", "tab-sequence.txt"), "w") as handle:
        handle.write("\n".join(repr(entry) for entry in sequence) + "\n")
    rec.check(None not in sequence, "Tab never drops keyboard focus", sequence.count(None))
    rec.check(not unnamed, "every Tab stop has an accessible name", unnamed[:10])
    doubled = [b for a, b in zip(sequence, sequence[1:]) if a and b and a[1] == b[1] and a[0] != b[0]]
    rec.check(not doubled, "each control is one Tab stop (no wrapper plus control)", doubled[:10])
    title = safe(lambda: frame(app).name, "")
    rec.check(title.startswith("Team"), "Tab traversal does not change the destination", title)
    rec.check(not any(entry and "busy" in entry[1] for entry in sequence),
              "keyboard traversal never reports the client as busy",
              [entry for entry in sequence if entry and "busy" in entry[1]])
    plain = [entry for entry in sequence if entry and entry[0] == "label"]
    rec.check(len(plain) <= 2, "static text is not a Tab stop", plain[:10])
    reached = {name for _, name in filter(None, sequence)}
    for name in ["New message", "Stop team"]:
        rec.check(name in reached, f"Tab reaches {name}", sorted(reached)[:30])


# L16 ---------------------------------------------------------------------
def l16_stop(rec, app):
    app.key("ctrl+1")
    act(button(root(app), "Stop team"))
    stop = dialog(app, "Stop team")
    destructive = [n for n in find_all(stop, name="Stop team", role="button")]
    act(destructive[-1])
    stopped = wait_for(lambda: not daemon_alive(), timeout=30)
    rec.check(stopped, "confirmed Stop team stops the daemon")
    text = wait_for(lambda: (lambda t: t if "Live" not in t else None)(visible_text(root(app))), timeout=20)
    rec.check(text, "app leaves Live after the team stops", (text or "")[:300])
    rec.check(app.alive(), "app keeps running after Stop team")
    rec.capture(app, "stopped")


CASES = [
    ("L01", "Local connection", "N01 N02 C01", l01_connect),
    ("L02", "Five destinations by keyboard", "N15 N17", l02_destinations),
    ("L03", "Team current work, graph and list", "N03", l03_team),
    ("L04", "Task inspection without side effects", "N04", l04_tasks),
    ("L05", "Answer a question", "N05", l05_question),
    ("L06", "Messages and replies", "N06", l06_messages),
    ("L07", "Send a message", "N07", l07_send),
    ("L08", "Agent controls and unavailable reasons", "N08", l08_controls),
    ("L09", "Confirmations: Cancel sends nothing", "N09", l09_confirmations),
    ("L10", "Events and evidence tabs", "N10 N17", l10_events),
    ("L11", "Terminal attach, input and resize", "N11", l11_terminal),
    ("L12", "Usage reporting", "N12", l12_usage),
    ("L13", "Close, reopen, daemon loss and reconnect", "N13", l13_lifecycle),
    ("L14", "Desktop layout and pane toggles", "N15", l14_layout),
    ("L15", "Keyboard traversal and help", "N17", l15_keyboard),
    ("L16", "Stop team", "N09 N13", l16_stop),
]


def run(rec):
    app = App().start()
    app.resize(WIDTH, HEIGHT)
    time.sleep(2)
    dump(app.accessible(), "trees/start")
    try:
        for cid, title, rows, function in CASES:
            if not app.alive():
                app.start(f"app-{cid}")
                app.resize(WIDTH, HEIGHT)
                if cid != "L01":
                    connect(rec, app, cid)
            rec.run(cid, title, rows, function, app)
    finally:
        app.stop()
