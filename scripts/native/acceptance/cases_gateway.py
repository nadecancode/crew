"""Scoped gateway acceptance cases (notes/native-apps.md C01-C20, S01-S08).

The host console is the real `crew native` CLI against an isolated loopback
gateway with the synthetic TLS fixture; the device is the real GTK app with
its Secret Service signer on the session's private keyring. Fingerprints,
labels and codes are fixture values, never operator credentials.
"""
import json
import os
import socket
import subprocess
import time

import pyatspi

from cases_local import (
    agent, agent_action, button, dialog, live_labels, messages, open_menu, root, status, type_into,
    WIDTH, HEIGHT,
)
from driver import (
    OUT, WS, App, act, crew, find, find_all, visible_text, wait_for,
)

REPO = os.environ["NATIVE_ACCEPTANCE_REPO"]
CREW = os.environ["NATIVE_ACCEPTANCE_CREW"]
FIXTURES = os.path.join(REPO, "crates", "crew-client-transport", "tests", "fixtures", "gateway")
DEVICE = "Acceptance laptop"
CONNECTIONS = "Connect to Crew"


def native(*args, check=True):
    result = subprocess.run([CREW, "native", *args], capture_output=True, text=True, timeout=60)
    with open(os.path.join(OUT, "logs", "host-console.log"), "a") as log:
        log.write(f"$ crew native {' '.join(args)}\n{result.stdout}{result.stderr}rc={result.returncode}\n")
    if check and result.returncode != 0:
        raise RuntimeError(f"crew native {' '.join(args)}: {result.stderr.strip()}")
    return json.loads(result.stdout) if result.returncode == 0 and result.stdout.strip() else None


class Gateway:
    """The isolated loopback gateway: fixture TLS identity in the private runtime."""

    def __init__(self):
        paths = native("paths")
        self.native_dir = paths["native_dir"]
        os.makedirs(self.native_dir, mode=0o700, exist_ok=True)
        self.cert = os.path.join(self.native_dir, "cert.pem")
        self.key = os.path.join(self.native_dir, "key.pem")
        with open(os.path.join(FIXTURES, "cert.pem"), "rb") as source, open(self.cert, "wb") as target:
            target.write(source.read())
        descriptor = os.open(self.key, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with open(os.path.join(FIXTURES, "key.pem"), "rb") as source, os.fdopen(descriptor, "wb") as target:
            target.write(source.read())
        probe = socket.socket()
        probe.bind(("127.0.0.1", 0))
        self.port = probe.getsockname()[1]
        probe.close()
        self.status = None

    def start(self):
        self.status = native("start", "--listen", f"127.0.0.1:{self.port}",
                             "--endpoint", f"wss://127.0.0.1:{self.port}/native/ws",
                             "--tls-cert", self.cert, "--tls-key", self.key, "--name", "Acceptance host")
        with open(os.path.join(OUT, "logs", "gateway-status.json"), "w") as handle:
            json.dump(self.status, handle, indent=1)
        return self.status

    def stop(self):
        native("stop", check=False)

    def fingerprint(self):
        return self.status["profile"]["tls_leaf_sha256"]


def connections(app):
    return dialog(app, CONNECTIONS)


def open_connections(app):
    if not find_all(root(app), name=CONNECTIONS, role="dialog"):
        act(find(root(app), name="Choose workspace or view connection details", role="button"))
    return connections(app)


def page_text(app):
    return visible_text(connections(app))


def shows(app, text, timeout=20):
    return wait_for(lambda: text in page_text(app), timeout=timeout)


def click_button(app, name, timeout=10):
    act(button(connections(app), name, timeout=timeout))


def field(app, name):
    """The editable field itself, not its caption label of the same name."""
    return find(connections(app), name=name, role="text")


def editable_fields(container):
    from driver import walk
    found = []
    for node, _ in walk(container):
        try:
            state = node.getState()
            if state.contains(pyatspi.STATE_EDITABLE) and state.contains(pyatspi.STATE_SHOWING):
                found.append(node.name)
        except Exception:
            continue
    return found


def grouped(hex_text):
    return " ".join(hex_text[i:i + 4] for i in range(0, len(hex_text), 4))


# C01 ---------------------------------------------------------------------
def c01_connections(rec, app, gateway):
    before = native("status", check=False)
    page = visible_text(open_connections(app))
    rec.check("This computer" in page and WS in page, "local same-host entry listed", page[:300])
    rec.check("Remote hosts" in page, "remote hosts section present")
    rec.check(find_all(connections(app), name="Add remote connection", role="button"),
              "Add remote connection is labelled")
    rec.check(before is None and native("status", check=False) is None,
              "opening Connections starts no gateway")
    rec.capture(app, "s01")


# C02 ---------------------------------------------------------------------
def c02_profile(rec, app, gateway):
    open_connections(app)
    click_button(app, "Add remote connection")
    fields = editable_fields(connections(app))
    rec.check(set(fields) == {"Connection name", "Public host profile", "Host fingerprint"},
              "profile form has only the public-profile fields", fields)
    rec.check("crew web" in page_text(app), "browser output is explicitly excluded")
    rec.check("crew native status at the trusted host console" in page_text(app),
              "fingerprint is read from the trusted host console, not the profile")
    rec.capture(app, "s02-empty")
    click_button(app, "Verify this host")
    error = shows(app, "Public host profile", timeout=5) and page_text(app)
    rec.check(error and ("required" in error.lower() or "paste" in error.lower() or "invalid" in error.lower()),
              "empty submit names the missing public field", (error or "")[:300])
    invalid = [n.name for n in find_all(connections(app)) if n.getState().contains(pyatspi.STATE_INVALID_ENTRY)]
    rec.check(invalid, "invalid field is marked invalid for assistive technology", invalid)
    rec.capture(app, "s02-empty-error")
    type_into(app, field(app, "Connection name"), "Acceptance host")
    type_into(app, field(app, "Public host profile"), "{not a profile")
    type_into(app, field(app, "Host fingerprint"), grouped(gateway.fingerprint()))
    click_button(app, "Verify this host")
    time.sleep(1)
    rec.check(field(app, "Connection name").queryText().getText(0, -1) == "Acceptance host",
              "other input kept after a malformed profile")
    rec.check(native("pending") == [], "no enrollment from a malformed profile")
    rec.capture(app, "s02-malformed")


# C03 ---------------------------------------------------------------------
def c03_trust(rec, app, gateway):
    profile = json.dumps(gateway.status, indent=1)
    wrong = gateway.fingerprint()[:-4] + ("0000" if not gateway.fingerprint().endswith("0000") else "1111")
    type_into(app, field(app, "Public host profile"), profile)
    type_into(app, field(app, "Host fingerprint"), grouped(wrong))
    click_button(app, "Verify this host")
    text = shows(app, "differs", timeout=10) and page_text(app)
    rec.check(text, "the differing fingerprint group is named", page_text(app)[:400])
    text = text or ""
    rec.check(not find_all(connections(app), name="Trust this host", role="button"),
              "a mismatched fingerprint offers no way to trust the host")
    rec.check("anyway" not in text.lower(), "no Connect anyway override")
    rec.check(native("pending") == [], "no device proof or request after a mismatch")
    rec.capture(app, "s02-mismatch")
    for name in ["Review host profile", "Back"]:
        if find_all(connections(app), name=name, role="button"):
            click_button(app, name)
            break
    type_into(app, field(app, "Host fingerprint"), grouped(gateway.fingerprint()))
    click_button(app, "Verify this host")
    trust = wait_for(lambda: find_all(connections(app), name="Trust this host", role="button"), timeout=10)
    rec.check(trust, "matching fingerprint reaches Trust this host")
    text = page_text(app)
    fingerprint = gateway.fingerprint()
    groups = [fingerprint[i:i + 4] for i in range(0, 64, 4)]
    rec.check(all(g in text for g in groups) and text.count("matches") >= 16 and "differs" not in text,
              "all 16 fingerprint groups shown and matching", text[:400])
    rec.capture(app, "s02-trust")
    click_button(app, "Trust this host")


# C12 ---------------------------------------------------------------------
def c12_device(rec, app, gateway):
    ready = shows(app, "Secure storage ready", timeout=15)
    rec.check(ready, "private Secret Service store is ready", page_text(app)[:300])
    text = page_text(app)
    rec.check("Use until this app closes" not in text,
              "memory-only storage is not offered while secure storage is ready")
    rec.check("does not verify" in text or "recognize" in text, "device label is not presented as verification")
    rec.capture(app, "s03-device")
    type_into(app, field(app, "Device name"), DEVICE)
    click_button(app, "Continue")
    rec.check(wait_for(lambda: find_all(connections(app), name="Request access", role="button"), timeout=20),
              "device step reaches Request access")
    rec.check("crew native enroll" in page_text(app), "request page names the host-console command")
    rec.capture(app, "s04-request")


# C04 ---------------------------------------------------------------------
def c04_no_window(rec, app, gateway):
    click_button(app, "Request access")
    refused = shows(app, "Access request was not accepted", timeout=20)
    rec.check(refused, "no host window gives one bounded refusal", page_text(app)[:300])
    time.sleep(3)
    rec.check(native("pending") == [], "no pending intent and no automatic retry")
    rec.capture(app, "s04-refused")


# C05 ---------------------------------------------------------------------
def c05_compare(rec, app, gateway):
    native("enroll")
    if find_all(connections(app), name="Try again", role="button"):
        click_button(app, "Try again")
    click_button(app, "Request access", timeout=15)
    code_seen = shows(app, "Comparison code", timeout=20)
    rec.check(code_seen, "accepted intent shows the comparison code", page_text(app)[:300])
    pending = native("pending")
    rec.check(len(pending) == 1, "exactly one attempt reached the host", pending)
    if not pending:
        return
    code = pending[0]["comparison_code"]
    groups = [code[i:i + 4] for i in range(0, 16, 4)]
    text = page_text(app)
    rec.check(all(g.upper() in text.upper() for g in groups), "all four groups match the host console",
              (groups, text[:400]))
    labels = [n.name for n in find_all(connections(app)) if (n.name or "").startswith("Group ")]
    rec.check(len(labels) == 4, "each group has its own accessible label", labels)
    rec.check(pending[0]["label"] == DEVICE, "host sees the typed device label", pending[0]["label"])
    rec.check(not find_all(connections(app), name="Approve", role="button"), "no device-side Approve button")
    rec.capture(app, "s05-compare")
    return pending[0]


# C10 ---------------------------------------------------------------------
def c10_mismatch(rec, app, gateway, intent):
    """A host-console approval with the wrong comparison code approves nothing."""
    code = intent["comparison_code"]
    wrong = code[:-1] + ("0" if code[-1] != "0" else "1")
    refused = native("approve", intent["enrollment_id"], "--code", wrong,
                     "--key-hash", intent["public_key_hash"], "--workspace", os.path.realpath(WS),
                     "--lifetime-secs", "3600", check=False)
    rec.check(refused is None, "the host console refuses a mismatched comparison code", refused)
    time.sleep(3)
    rec.check("Access granted" not in page_text(app), "the app shows no grant after a mismatch")
    rec.check(native("devices") in ([], None), "no device was registered", native("devices", check=False))
    pending = native("pending")
    rec.check(len(pending) == 1 and pending[0]["enrollment_id"] == intent["enrollment_id"],
              "one failed comparison leaves the same request pending", pending)
    rec.capture(app, "after-mismatch")


# C06/C08 -------------------------------------------------------------------
def c06_approve(rec, app, gateway, intent):
    approved = native("approve", intent["enrollment_id"], "--code", intent["comparison_code"],
                      "--key-hash", intent["public_key_hash"], "--workspace", os.path.realpath(WS),
                      "--lifetime-secs", "3600")
    rec.check(approved and approved.get("device_id"), "host approval names the device", approved)
    granted = shows(app, "Access granted", timeout=20)
    rec.check(granted, "app shows Access granted after host approval", page_text(app)[:300])
    listed = wait_for(lambda: os.path.realpath(WS) in page_text(app) or WS in page_text(app), timeout=20)
    rec.check(listed, "only the granted workspace is listed", page_text(app)[:400])
    rec.capture(app, "s07-granted")
    return approved["device_id"] if approved else None


def c08_disclosure(rec, app, gateway):
    click_button(app, "Connection details")
    text = wait_for(lambda: "What each access allows" in page_text(app) and page_text(app), timeout=10) or ""
    for phrase in ["may include project secrets and host paths",
                   "existing host permissions, which may include files outside the selected workspace",
                   "Send commands and keystrokes to an agent's terminal",
                   "do not create an OS or filesystem sandbox"]:
        rec.check(phrase.lower() in text.lower(), f"disclosure: {phrase}")
    rec.check("Disconnect" in text and "How to revoke access" in text and "Remove from this app" in text,
              "details offer Disconnect, revoke instructions and local removal")
    rec.capture(app, "s08-details")


# C07 ---------------------------------------------------------------------
def open_workspace(rec, app):
    workspace = wait_for(lambda: next((n for n in find_all(connections(app), role="button")
                                       if WS in (n.name or "") or os.path.basename(WS) in (n.name or "")), None),
                         timeout=10)
    if workspace is None:
        if find_all(connections(app), name="Back", role="button"):
            click_button(app, "Back")
        workspace = wait_for(lambda: next((n for n in find_all(connections(app), role="button")
                                           if WS in (n.name or "")), None), timeout=10)
    if not rec.check(workspace is not None, "granted workspace can be opened"):
        return False
    act(workspace)
    return wait_for(lambda: live_labels(app), timeout=30)


def c07_read_only(rec, app, gateway):
    live = open_workspace(rec, app)
    rec.check(live, "gateway workspace reaches Live")
    before = len(messages())
    app.key("ctrl+1")
    from cases_local import select_agent
    select_agent(app, "idle")
    text = visible_text(root(app))
    open_menu(app)
    pause = button(root(app), "Pause")
    rec.check(not pause.getState().contains(pyatspi.STATE_SENSITIVE), "Read-only device cannot Pause")
    text = visible_text(root(app))
    rec.check("Control" in text and "required" in text, "missing Control is named", text[-400:])
    app.key("Escape")
    from cases_local import terminal_tab
    terminal_tab(app)
    attach = wait_for(lambda: find(root(app), name="Attach terminal", role="button", timeout=1), timeout=5)
    rec.check(attach is not None and not attach.getState().contains(pyatspi.STATE_SENSITIVE),
              "Read-only device cannot attach a terminal")
    rec.check("Terminal" in visible_text(root(app)) and "required" in visible_text(root(app)),
              "missing Terminal is named")
    rec.check(len(messages()) == before, "no message was dispatched from unavailable actions")
    rec.capture(app, "read-only")


# C15 ---------------------------------------------------------------------
def c15_grant_change(rec, app, gateway, device):
    native("grant", device, "--workspace", os.path.realpath(WS), "--control")
    open_connections(app)
    changed = shows(app, "Open it again", timeout=20)
    rec.check(changed, "host grant change retires the old authority", page_text(app)[:300])
    rec.capture(app, "grant-changed")
    if find_all(connections(app), name="Open workspace again", role="button"):
        click_button(app, "Open workspace again")
    live = wait_for(lambda: live_labels(app), timeout=30)
    rec.check(live, "reopened workspace is Live with the new grant")
    from cases_local import select_agent
    select_agent(app, "idle")
    def enabled_pause():
        open_menu(app)
        node = button(root(app), "Pause")
        if node.getState().contains(pyatspi.STATE_SENSITIVE):
            return node
        app.key("Escape")
        return None
    pause = wait_for(enabled_pause, timeout=15, interval=1)
    rec.check(pause, "Control grant enables Pause")
    if pause:
        act(pause)
        rec.check(wait_for(lambda: agent(status(), "idle").get("paused") is True, timeout=15),
                  "Pause through the gateway reaches the daemon")
        agent_action(app, "Unpause")
        wait_for(lambda: agent(status(), "idle").get("paused") is False, timeout=15)
    rec.capture(app, "control")


# C14 ---------------------------------------------------------------------
def status_labels(app):
    return [n.name for n in find_all(root(app), contains=" · ", role="label")
            if any(word in (n.name or "") for word in ("Live", "Disconnected", "Reconnecting", "Authenticated"))]


def remote_connect(app):
    """Connect the saved remote profile from Connections (the last Connect,
    below Remote hosts), then open the granted workspace if it is offered."""
    open_connections(app)
    for name in ["Reconnect now", "Reconnect", "Try again"]:
        if find_all(connections(app), name=name, role="button"):
            click_button(app, name)
            return "reconnect"
    buttons = find_all(connections(app), name="Connect", role="button")
    if buttons:
        act(buttons[-1])
        return "connect"
    return None


def c14_restart(rec, app, gateway):
    gateway.stop()
    lost = wait_for(lambda: not live_labels(app), timeout=30)
    labels = status_labels(app)
    rec.check(lost, "stopped gateway is no longer shown authenticated and Live", labels)
    rec.check(not any("Disconnected" in label and "Live" in label for label in labels),
              "connection status never reads Disconnected and Live together", labels)
    rec.check(crew("status", "--json", check=False).returncode == 0, "stopping the gateway leaves the team running")
    rec.capture(app, "gateway-stopped")
    gateway.start()
    automatic = wait_for(lambda: live_labels(app), timeout=20)
    how = "automatic"
    if not automatic:
        how = remote_connect(app)
        if not wait_for(lambda: live_labels(app), timeout=20) and find_all(connections(app), role="button"):
            open_workspace(rec, app)
    back = wait_for(lambda: live_labels(app), timeout=20)
    rec.check(back, "retained grants reconnect after a normal gateway restart", (how, status_labels(app)))
    rec.capture(app, "gateway-restarted")


# C16/C18 -------------------------------------------------------------------
def c16_revoke(rec, app, gateway, device):
    if not live_labels(app):
        remote_connect(app)
        if not wait_for(lambda: live_labels(app), timeout=20):
            open_workspace(rec, app)
    rec.check(wait_for(lambda: live_labels(app), timeout=20), "device is Live before revocation", status_labels(app))
    revoked = native("revoke", device)
    rec.check(revoked is not None, "host revocation completes", revoked)
    ended = wait_for(lambda: not live_labels(app), timeout=30)
    rec.check(ended, "revoked device leaves Live")
    open_connections(app)
    text = wait_for(lambda: (lambda t: t if ("no longer available" in t or "unavailable" in t
                                             or "access ended" in t or "revoked" in t.lower())
                             else None)(page_text(app)), timeout=20)
    rec.check(text, "app explains the access ended", (text or page_text(app))[:300])
    devices = native("devices")
    rec.check(devices is not None, "host lists devices after revocation", devices)
    rec.check(crew("status", "--json", check=False).returncode == 0, "revocation leaves the team running")
    rec.capture(app, "revoked")


def c18_remove(rec, app, gateway):
    if find_all(connections(app), name="Connection details", role="button"):
        click_button(app, "Connection details")
    if find_all(connections(app), name="Remove from this app", role="button"):
        click_button(app, "Remove from this app")
        text = page_text(app)
        rec.check("device key" in text.lower() and ("revoked" in text.lower() or "expire" in text.lower()),
                  "removal explains local key retirement and remaining host access", text[:300])
        rec.capture(app, "remove")
        click_button(app, "Cancel")
    else:
        rec.check(False, "Remove from this app reachable from details", page_text(app)[:300])


def run(rec):
    rec.keep_dialogs.add(CONNECTIONS)
    gateway = Gateway()
    app = App().start()
    app.resize(WIDTH, HEIGHT)
    time.sleep(2)
    state = {"intent": None, "device": None}
    try:
        rec.run("C01", "Connections without a running gateway", "N01 C01", lambda r, a: c01_connections(r, a, gateway), app)
        gateway.start()
        rec.run("C02", "Public profile form and errors", "N01 C02", lambda r, a: c02_profile(r, a, gateway), app)
        rec.run("C03", "Fingerprint mismatch and trust", "N01 C03", lambda r, a: c03_trust(r, a, gateway), app)
        rec.run("C12", "Device name and key storage", "N01 C12", lambda r, a: c12_device(r, a, gateway), app)
        rec.run("C04", "No host enrollment window", "N01 C04", lambda r, a: c04_no_window(r, a, gateway), app)

        def compare(r, a):
            state["intent"] = c05_compare(r, a, gateway)
        rec.run("C05", "Comparison code matches the host console", "N01 C05", compare, app)

        def mismatch(r, a):
            if r.check(state["intent"] is not None, "an accepted intent exists"):
                c10_mismatch(r, a, gateway, state["intent"])
        rec.run("C10", "Mismatched code at the host approves nothing", "N01 C10", mismatch, app)

        def approve(r, a):
            if not r.check(state["intent"] is not None, "an accepted intent exists"):
                return
            state["device"] = c06_approve(r, a, gateway, state["intent"])
        rec.run("C06", "Host-selected Read grant", "N01 N02 C06 C09", approve, app)
        rec.run("C08", "Access disclosure and details", "N01 C08 C18", lambda r, a: c08_disclosure(r, a, gateway), app)
        rec.run("C07", "Read-only device: unavailable actions", "N08 N11 C07", lambda r, a: c07_read_only(r, a, gateway), app)
        rec.run("C15", "Host adds Control; old authority retires", "N09 C15", lambda r, a: c15_grant_change(r, a, gateway, state["device"]), app)
        rec.run("C14", "Gateway restart with retained grants", "N13 C14", lambda r, a: c14_restart(r, a, gateway), app)
        rec.run("C16", "Host revocation", "N13 C16", lambda r, a: c16_revoke(r, a, gateway, state["device"]), app)
        rec.run("C18", "Local removal copy", "N13 C18", lambda r, a: c18_remove(r, a, gateway), app)
    finally:
        app.stop()
        gateway.stop()
