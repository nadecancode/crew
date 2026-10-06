//! Remote connections on Linux: the real GTK Connections window with a typed
//! recording host, the Secret Service device signer against a private
//! keyring, then the production controller and service against an isolated
//! loopback gateway and daemons. Run only through
//! `tests/run-window-test.sh --gateway`, which owns the display, session bus,
//! keyring and Crew runtime. Fingerprints, labels and codes are synthetic.
#![cfg(all(feature = "gateway-tests", target_os = "linux"))]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use crew_client::api::*;
use crew_client_transport::gateway::{
    EnrollmentOutcome, EnrollmentProgress, EnrollmentResult, EnrollmentUpdate, GatewayDeviceAccess,
    SetupContext,
};
use crew_desktop::remote::{
    profile::ProfileStore, secret, Host, LocalEntry, Remote, RemoteRequest, RemoteResponse,
};
use crew_desktop::{application, controller::Controller, service::Request, shell::Shell};
use crew_protocol::native::PublicProfile;
use gtk::{gio, glib, prelude::*};
use serde_json::{json, Value};

const CERT: &[u8] = include_bytes!("../../crew-client-transport/tests/fixtures/gateway/cert.pem");
const KEY: &[u8] = include_bytes!("../../crew-client-transport/tests/fixtures/gateway/key.pem");

static CRITICALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

// GTK initialization and every assertion stay on this single test thread.
#[test]
fn remote_connections_secret_store_and_isolated_gateway() {
    assert!(
        std::env::var_os("CREW_NATIVE_UI_TEST_PRIVATE").is_some(),
        "use tests/run-window-test.sh --gateway; never use the operator's display, keyring or registry"
    );
    gtk::init().unwrap();
    // GTK criticals fail the run, with the Rust frame that caused them.
    let _criticals = glib::log_set_handler(
        Some("Gtk"),
        glib::LogLevels::LEVEL_CRITICAL,
        false,
        false,
        |_, _, message| {
            CRITICALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "GTK critical: {message}\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        },
    );
    application::register_resources();
    let app = application::build();
    app.register(None::<&gio::Cancellable>).unwrap();
    presentation(&app);
    eprintln!("Deterministic Connections presentation passed");
    secret_store();
    eprintln!("Secret Service signer passed");
    live(&app);
    eprintln!("Live isolated gateway journey passed");
    assert_eq!(
        CRITICALS.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "no GTK criticals"
    );
}

// Widget helpers ------------------------------------------------------------

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut child = widget.first_child();
    while let Some(w) = child {
        out.extend(descendants(&w));
        child = w.next_sibling();
        out.push(w);
    }
    out
}
fn text(widget: &impl IsA<gtk::Widget>) -> String {
    descendants(widget.as_ref())
        .into_iter()
        .filter(|w| w.is_visible())
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .map(|l| l.label().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
/// The window status reads the lifecycle phrase for a live workspace, from the
/// same table the app renders, so a copy change cannot drift from this check.
fn status_live(window: &gtk::ApplicationWindow) -> bool {
    status_reads(
        window,
        crew_desktop::lifecycle::phrase(EndpointStatus::Authenticated, WorkspaceStatus::Live),
    )
}
/// Some visible label's first line is this lifecycle phrase.
fn status_reads(window: &gtk::ApplicationWindow, phrase: &str) -> bool {
    descendants(window.upcast_ref())
        .into_iter()
        .filter(|w| w.is_visible())
        .filter_map(|w| w.downcast::<gtk::Label>().ok())
        .any(|l| l.label().lines().next() == Some(phrase))
}
fn find(widget: &impl IsA<gtk::Widget>, name: &str) -> Option<gtk::Widget> {
    descendants(widget.as_ref())
        .into_iter()
        .find(|w| w.widget_name() == name && w.is_visible())
}
fn named(widget: &impl IsA<gtk::Widget>, name: &str) -> gtk::Widget {
    find(widget, name).unwrap_or_else(|| {
        capture("missing-widget");
        panic!("native widget {name}; showing:\n{}", text(widget))
    })
}
fn click(widget: &impl IsA<gtk::Widget>, name: &str) {
    let button = named(widget, name).downcast::<gtk::Button>().unwrap();
    assert!(button.is_sensitive(), "{name} is enabled");
    button.emit_clicked();
    settle();
}
/// A widget inside an inspector's section pages (`views::common::Tabs`).
fn in_inspector_tabs(widget: &impl IsA<gtk::Widget>) -> bool {
    let mut node = widget.as_ref().parent();
    while let Some(parent) = node {
        if parent.has_css_class("crew-tab-pages") {
            return true;
        }
        node = parent.parent();
    }
    false
}
fn button_labelled(widget: &impl IsA<gtk::Widget>, label: &str) -> gtk::Button {
    descendants(widget.as_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Button>().ok())
        .find(|b| b.is_visible() && b.label().as_deref() == Some(label))
        .unwrap_or_else(|| panic!("native button {label}"))
}
fn entry(widget: &impl IsA<gtk::Widget>, name: &str) -> gtk::Entry {
    named(widget, name).downcast().unwrap()
}
fn common_text(buffer: &gtk::TextBuffer) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}
fn buffer(widget: &impl IsA<gtk::Widget>, name: &str) -> gtk::TextBuffer {
    named(widget, name)
        .downcast::<gtk::TextView>()
        .unwrap()
        .buffer()
}
fn focused(dialog: &gtk::Dialog) -> gtk::Widget {
    gtk::prelude::GtkWindowExt::focus(dialog).expect("a focused widget")
}
fn focus_is(dialog: &gtk::Dialog, name: &str) -> bool {
    let focus = focused(dialog);
    focus.widget_name() == name
        || focus.ancestor(gtk::Widget::static_type()).is_some_and(|_| {
            let mut parent = focus.parent();
            while let Some(widget) = parent {
                if widget.widget_name() == name {
                    return true;
                }
                parent = widget.parent();
            }
            false
        })
}
fn settle() {
    let main = glib::MainContext::default();
    while main.pending() {
        main.iteration(false);
    }
}
fn until_for(limit: Duration, what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !condition() {
        if Instant::now() >= deadline {
            capture("timeout");
            let shown = gtk::Window::list_toplevels()
                .into_iter()
                .filter(|w| w.is_visible())
                .map(|w| text(&w))
                .collect::<Vec<_>>()
                .join("\n----\n");
            panic!("timed out waiting for {what}; showing:\n{shown}");
        }
        settle();
        std::thread::sleep(Duration::from_millis(5));
    }
    settle();
}
fn until(what: &str, condition: impl FnMut() -> bool) {
    until_for(Duration::from_secs(15), what, condition)
}
/// Wait for visible text; on timeout the failure names what was shown.
fn until_shows(widget: &impl IsA<gtk::Widget>, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !text(widget).contains(needle) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; showing:\n{}",
            text(widget)
        );
        settle();
        std::thread::sleep(Duration::from_millis(5));
    }
    settle();
}
fn capture(name: &str) {
    // Several frames let a replaced page repaint fully before the X11 grab.
    for _ in 0..8 {
        settle();
        std::thread::sleep(Duration::from_millis(30));
    }
    settle();
    if let Some(dir) = std::env::var_os("CREW_NATIVE_UI_EVIDENCE_DIR") {
        let file = PathBuf::from(dir).join(format!("{name}.png"));
        assert!(std::process::Command::new("magick")
            .args(["import", "-window", "root"])
            .arg(file)
            .status()
            .unwrap()
            .success());
    }
}

fn xdotool() -> String {
    std::env::var("CREW_TEST_XDOTOOL").expect("runner supplies xdotool")
}
/// Make a window the X input focus. There is no window manager on the
/// private display, so the X server is asked directly if GTK cannot.
fn activate(window: &impl IsA<gtk::Window>) {
    window.present();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !window.is_active() && Instant::now() < deadline {
        settle();
        std::thread::sleep(Duration::from_millis(10));
    }
    if !window.is_active() {
        let title = window.title().unwrap_or_default().to_string();
        let _ = std::process::Command::new(xdotool())
            .args([
                "search",
                "--onlyvisible",
                "--name",
                &title,
                "windowfocus",
                "--sync",
            ])
            .status();
    }
    until("window active for keyboard input", || window.is_active());
}
/// Real X11 key events on the private display (xdotool from the runner).
fn keys(window: &impl IsA<gtk::Window>, sequence: &[&str]) {
    activate(window);
    let tool = xdotool();
    for key in sequence {
        assert!(std::process::Command::new(&tool)
            .args(["key", "--clearmodifiers", key])
            .status()
            .unwrap()
            .success());
        for _ in 0..4 {
            settle();
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}
fn type_text(window: &impl IsA<gtk::Window>, text: &str) {
    activate(window);
    let tool = xdotool();
    assert!(std::process::Command::new(&tool)
        .args(["type", "--delay", "5", text])
        .status()
        .unwrap()
        .success());
    for _ in 0..6 {
        settle();
        std::thread::sleep(Duration::from_millis(15));
    }
}

// Deterministic presentation --------------------------------------------------

struct Recorder {
    requests: RefCell<Vec<Request>>,
    parent: gtk::ApplicationWindow,
    connection: RefCell<Option<ConnectionContext>>,
    listing: RefCell<Option<crew_protocol::rpc::Workspaces>>,
}
impl Host for Recorder {
    fn send(&self, request: Request) -> bool {
        self.requests.borrow_mut().push(request);
        true
    }
    fn parent(&self) -> gtk::ApplicationWindow {
        self.parent.clone()
    }
    fn local_entries(&self) -> (Vec<LocalEntry>, Option<String>) {
        (
            vec![LocalEntry {
                title: "fixture-project".into(),
                detail: "/work/local\nLocal workspace · daemon 4000".into(),
                profile_id: "local-profile".into(),
                endpoint_id: "local-endpoint".into(),
                workspace_key: "0123456789abcdef".into(),
            }],
            None,
        )
    }
    fn connection(&self) -> Option<ConnectionContext> {
        self.connection.borrow().clone()
    }
    fn listing(&self) -> Option<crew_protocol::rpc::Workspaces> {
        self.listing.borrow().clone()
    }
}
impl Recorder {
    /// Explicit requests; routine context refreshes after notices are skipped.
    fn take(&self) -> Vec<Request> {
        self.requests
            .borrow_mut()
            .drain(..)
            .filter(|r| !matches!(r, Request::Context))
            .collect()
    }
}

const PIN: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9";

fn fixture_profile() -> PublicProfile {
    PublicProfile {
        endpoint_id: crew_protocol::native::HexBytes([0x5a; 16]),
        name: "Fixture host".into(),
        url: "wss://127.0.0.1:18443/native/ws".into(),
        tls_leaf_sha256: crew_protocol::native::HexBytes::from_hex(PIN).unwrap(),
        trust_pem: String::from_utf8(CERT.to_vec()).unwrap(),
    }
}

fn grouped(hex: &str, separator: &str) -> String {
    hex.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap().to_uppercase())
        .collect::<Vec<_>>()
        .join(separator)
}

fn listing() -> crew_protocol::rpc::Workspaces {
    serde_json::from_value(json!({"rev": 3, "joined": [
        {"id": "a1a1a1a1a1a1a1a1", "generation": 1, "workspace": "/work/alpha", "state": "running", "joined_at": 1},
        {"id": "b2b2b2b2b2b2b2b2", "generation": 1, "workspace": "/work/beta", "state": "stopped", "joined_at": 2}
    ]}))
    .unwrap()
}

fn notice(
    endpoint: EndpointStatus,
    workspace: WorkspaceStatus,
    reason: Option<&str>,
) -> ClientNotice {
    ClientNotice {
        scope: None,
        revision: 1,
        changes: ChangeSet::default(),
        endpoint,
        workspace,
        reason: reason.map(|code| ClientReason {
            code: code.into(),
            message: "connector detail".into(),
        }),
        outcomes: Vec::new(),
    }
}

fn presentation(app: &gtk::Application) {
    let config = tempfile::tempdir().unwrap();
    let store_path = config.path().join("crew-desktop").join("connections.json");
    let shell = Shell::new(app);
    shell.window().present();
    let recorder = Rc::new(Recorder {
        requests: RefCell::new(Vec::new()),
        parent: shell.window().clone(),
        connection: RefCell::new(None),
        listing: RefCell::new(None),
    });
    let host: Weak<dyn Host> = Rc::downgrade(&recorder) as Weak<dyn Host>;
    let remote = Remote::new(host, ProfileStore::new(store_path.clone()));
    remote.open();
    let dialog = remote.dialog().unwrap();
    until("Connections dialog", || dialog.is_mapped());
    assert_eq!(dialog.title().as_deref(), Some("Connect to Crew"));
    let page = text(&dialog);
    for expected in [
        "Connections",
        "This computer",
        "fixture-project",
        "Remote hosts",
        "No remote hosts",
        "Never paste or scan crew web output here",
    ] {
        assert!(page.contains(expected), "S01 shows {expected}");
    }
    capture("fixture-gateway-s01-connections");

    // Keyboard only: Return on Add remote connection, type, Tab onward.
    dialog.present();
    named(&dialog, "add").grab_focus();
    keys(&dialog, &["Return"]);
    assert!(
        focus_is(&dialog, "field:Label"),
        "S02 focuses its first field"
    );
    type_text(&dialog, "Keyboard host");
    assert_eq!(entry(&dialog, "field:Label").text(), "Keyboard host");
    keys(&dialog, &["Tab"]);
    assert!(
        focus_is(&dialog, "field:Profile"),
        "Tab leaves the name field"
    );
    keys(&dialog, &["Tab"]);
    assert!(
        !focus_is(&dialog, "field:Profile"),
        "Tab is not trapped in the profile paste area"
    );
    entry(&dialog, "field:Label").set_text("");

    // S02: field errors in reading order, focused after submit.
    // An empty name is allowed (it defaults to the host name); the empty
    // profile is the first error.
    click(&dialog, "verify");
    assert!(focus_is(&dialog, "field:Profile"));
    assert!(text(&dialog).contains("Public host profile: Check the host profile."));
    buffer(&dialog, "field:Profile").set_text("crew web http://127.0.0.1:7000/#code");
    assert!(
        !text(&dialog).contains("Public host profile: Check"),
        "editing a field clears its error"
    );
    click(&dialog, "verify");
    assert!(focus_is(&dialog, "field:Profile"));
    assert!(text(&dialog).contains("Paste the output of crew native status from the host."));
    let status = json!({"profile": fixture_profile(), "endpoint_epoch": "00"}).to_string();
    buffer(&dialog, "field:Profile").set_text(&status);
    entry(&dialog, "field:Fingerprint").set_text(&grouped(&PIN[..60], " "));
    settle();
    assert!(text(&dialog).contains("60 of 64"));
    click(&dialog, "verify");
    assert!(focus_is(&dialog, "field:Fingerprint"));
    assert!(text(&dialog).contains("This one has 60."));
    assert_eq!(
        common_text(&buffer(&dialog, "field:Profile")),
        status,
        "safe values survive a failed submit"
    );
    capture("fixture-gateway-s02-errors");

    // A differing group offers no trust action.
    let wrong = format!("{}{}", &PIN[..16], "ffff").to_owned() + &PIN[20..];
    entry(&dialog, "field:Fingerprint").set_text(&grouped(&wrong, ":"));
    click(&dialog, "verify");
    let trust_page = text(&dialog);
    assert!(trust_page.contains("Verify this host"));
    assert!(trust_page.contains("differs"));
    assert!(trust_page.contains("1 of 16 groups differ"));
    assert!(
        find(&dialog, "trust").is_none(),
        "no Trust this host on mismatch"
    );
    assert!(trust_page.contains("If the host loses its private runtime state"));
    capture("fixture-gateway-s02-mismatch");
    click(&dialog, "review");
    assert!(focus_is(&dialog, "field:Fingerprint"));
    entry(&dialog, "field:Fingerprint").set_text(&grouped(PIN, " "));
    click(&dialog, "verify");
    assert!(!text(&dialog).contains("differs"));
    capture("fixture-gateway-s02-verify");
    click(&dialog, "trust");
    assert!(matches!(
        recorder.take().as_slice(),
        [Request::Remote(RemoteRequest::ProbeStore)]
    ));

    // S03: unavailable secure storage offers only an explicit memory choice.
    assert!(text(&dialog).contains("Checking secure storage"));
    remote.respond(RemoteResponse::Store(secret::StoreState::Unavailable));
    settle();
    assert!(text(&dialog).contains("Use until this app closes"));
    assert!(text(&dialog).contains("never writes device keys to ordinary files"));
    entry(&dialog, "field:DeviceName").set_text("Fixture laptop");
    click(&dialog, "continue");
    assert!(text(&dialog).contains("Device secure storage is unavailable"));
    assert!(
        recorder.take().is_empty(),
        "no key without an explicit choice"
    );
    capture("fixture-gateway-s03-unavailable");
    named(&dialog, "memory")
        .downcast::<gtk::CheckButton>()
        .unwrap()
        .set_active(true);
    click(&dialog, "continue");
    let requests = recorder.take();
    let Some(Request::Remote(RemoteRequest::Prepare { profile })) = requests.first() else {
        panic!("one Prepare request");
    };
    assert_eq!(
        profile.storage,
        crew_desktop::remote::profile::KeyStorage::Memory
    );
    assert_eq!(profile.device_label.as_deref(), Some("Fixture laptop"));
    let profile_id = profile.id.clone();
    remote.respond(RemoteResponse::Prepared {
        profile_id: profile_id.clone(),
        result: Ok(secret::KeyOrigin::Created),
    });
    settle();
    let saved = std::fs::read_to_string(&store_path).unwrap();
    assert!(saved.contains("Fixture laptop") && saved.contains("\"memory\""));
    assert!(!saved.contains("PRIVATE KEY"));

    // S04/S05: one explicit attempt; the code appears only when accepted.
    assert!(text(&dialog).contains("crew native enroll"));
    click(&dialog, "request");
    assert!(matches!(
        recorder.take().as_slice(),
        [Request::Remote(RemoteRequest::Enroll { .. })]
    ));
    assert!(
        find(&dialog, "back").is_none(),
        "Back cannot abandon a request"
    );
    let context = SetupContext {
        profile_id: profile_id.clone(),
        endpoint_id: "5a".repeat(16),
        attempt: 7,
    };
    remote.respond(RemoteResponse::Setup(EnrollmentUpdate {
        context: context.clone(),
        progress: EnrollmentProgress::Connecting,
    }));
    settle();
    assert!(text(&dialog).contains("verifying its identity"));
    assert!(find(&dialog, "code:0").is_none());
    remote.respond(RemoteResponse::Setup(EnrollmentUpdate {
        context: context.clone(),
        progress: EnrollmentProgress::AwaitingApproval {
            enrollment_id: "77".repeat(16),
            comparison_code: "0a1b2c3d4e5f6071".into(),
            expires_at_ms: 1_800_000_000_000,
        },
    }));
    settle();
    let codes = (0..4)
        .map(|i| {
            named(&dialog, &format!("code:{i}"))
                .downcast::<gtk::Label>()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        codes
            .iter()
            .map(|l| l.label().to_string())
            .collect::<Vec<_>>(),
        ["0a1b", "2c3d", "4e5f", "6071"],
        "leading zeroes and every group stay exact"
    );
    for label in &codes {
        assert!(label.is_selectable());
        assert!(gtk::test_accessible_has_property(
            label,
            gtk::AccessibleProperty::Label
        ));
    }
    let compare = text(&dialog);
    assert!(compare.contains("Approve access there only if they match."));
    assert!(compare.contains("There is no approve button on this device"));
    assert!(find(&dialog, "approve").is_none());
    capture("fixture-gateway-s05-compare");
    dialog.present();
    keys(&dialog, &["Escape"]);
    assert!(dialog.is_visible(), "Escape cancels the request first");
    let canceled = recorder.take();
    assert!(matches!(
        canceled.as_slice(),
        [Request::Remote(RemoteRequest::CancelEnrollment {
            attempt: 7,
            ..
        })]
    ));
    assert!(text(&dialog).contains("Closing the request"));
    let result = |outcome| RemoteResponse::Enrolled {
        profile_id: profile_id.clone(),
        result: Ok(EnrollmentResult {
            context: context.clone(),
            outcome,
        }),
    };
    remote.respond(result(EnrollmentOutcome::Canceled));
    settle();
    assert!(text(&dialog).contains("Request closed."));

    // Lost approval reply: unknown, recovered only by explicit same-key check.
    click(&dialog, &format!("details:{profile_id}"));
    click(&dialog, "connect");
    assert!(matches!(
        recorder.take().as_slice(),
        [Request::Remote(RemoteRequest::Connect { .. })]
    ));
    remote.respond(RemoteResponse::ConnectFailed {
        profile_id: profile_id.clone(),
        reason: ClientReason {
            code: "authentication".into(),
            message: "connector detail".into(),
        },
    });
    settle();
    assert!(text(&dialog).contains("Device access is unavailable. Check with the host operator."));
    assert!(!text(&dialog).contains("connector detail"));
    remote.respond(result(EnrollmentOutcome::OutcomeUnknown));
    settle();
    let unknown = text(&dialog);
    assert!(unknown.contains("The host may have approved this device."));
    capture("fixture-gateway-outcome-unknown");
    click(&dialog, "recovery:ReconnectToCheck");
    assert!(matches!(
        recorder.take().as_slice(),
        [Request::Remote(RemoteRequest::Connect { .. })]
    ));

    // S07: granted is distinct from live; only returned workspaces appear.
    remote.respond(result(EnrollmentOutcome::Approved {
        access: GatewayDeviceAccess {
            endpoint_id: "5a".repeat(16),
            device_id: "c3".repeat(16),
            credential_expires_at_ms: 1_800_000_000_000,
        },
    }));
    settle();
    assert!(text(&dialog).contains("Access granted"));
    assert!(text(&dialog).contains("Loading workspaces"));
    assert!(matches!(
        recorder.take().as_slice(),
        [Request::Remote(RemoteRequest::Connect { .. })]
    ));
    remote.respond(RemoteResponse::Active(Some(profile_id.clone())));
    *recorder.listing.borrow_mut() = Some(listing());
    remote.notice(
        &notice(
            EndpointStatus::Authenticated,
            WorkspaceStatus::NoneSelected,
            None,
        ),
        None,
    );
    settle();
    let granted = text(&dialog);
    assert!(granted.contains("/work/alpha") && granted.contains("/work/beta"));
    capture("fixture-gateway-s07-granted");
    *recorder.listing.borrow_mut() =
        Some(serde_json::from_value(json!({"rev": 4, "joined": []})).unwrap());
    remote.changed();
    settle();
    assert!(text(&dialog).contains("No workspace access."));
    *recorder.listing.borrow_mut() = Some(listing());
    remote.changed();
    settle();
    named(&dialog, "workspace:a1a1a1a1a1a1a1a1")
        .downcast::<gtk::Button>()
        .unwrap()
        .emit_clicked();
    settle();
    assert!(matches!(
        recorder.take().as_slice(),
        [Request::SelectWorkspace(id)] if id == "a1a1a1a1a1a1a1a1"
    ));
    assert!(
        remote.dialog().is_none(),
        "opening a workspace closes setup"
    );

    // S08 details: read-only access with each power's disclosure.
    let scope = ClientScope {
        endpoint_id: "5a".repeat(16),
        workspace_id: "a1a1a1a1a1a1a1a1".into(),
        generation: Some(1),
        daemon_pid: Some(4100),
        daemon_started_at_ms: Some(1),
        attempt: 2,
    };
    *recorder.connection.borrow_mut() = Some(ConnectionContext {
        mode: ConnectionMode::ScopedGateway,
        grant: Grant {
            grant_id: "gateway:fixture".into(),
            tiers: vec![GrantTier::Read, GrantTier::Terminal],
            workspace_ids: vec![scope.workspace_id.clone()],
            expires_at_ms: Some(1_800_000_000_000),
        },
        snapshot_mode: SnapshotMode::OrderedConnection,
        scope,
    });
    remote.notice(
        &notice(EndpointStatus::Authenticated, WorkspaceStatus::Live, None),
        recorder.connection.borrow().as_ref(),
    );
    remote.open();
    let dialog = remote.dialog().unwrap();
    until("reopened Connections", || dialog.is_mapped());
    assert!(text(&dialog).contains("This device's access · Fixture host"));
    assert!(find(&dialog, "close").is_some(), "one exit on the list");
    click(&dialog, &format!("details:{profile_id}"));
    let details = text(&dialog);
    for expected in [
        "Access in /work/alpha",
        "Read: Granted · Control: Not granted · Terminal: Granted",
        "Other authorized workspaces",
        "/work/beta · stopped",
        "Memory only, until Crew closes",
        "c3c3 c3c3",
        "Workspace grants limit Crew access. They do not create an OS or filesystem sandbox.",
        "This content may include project secrets and host paths.",
        "Agents act with their existing host permissions",
        "Commands run with that agent's existing host permissions",
        "cannot grant, renew or revoke access",
    ] {
        assert!(details.contains(expected), "S08 shows {expected}");
    }
    assert!(find(&dialog, "reconnect").is_some() && find(&dialog, "disconnect").is_some());
    capture("fixture-gateway-s08-details");
    click(&dialog, "revoke");
    assert!(text(&dialog).contains(&format!("crew native revoke {}", "c3".repeat(16))));
    click(&dialog, "back");

    // Access loss focuses a safe explanation without the ungranted name.
    *recorder.connection.borrow_mut() = None;
    remote.notice(
        &notice(
            EndpointStatus::Authenticated,
            WorkspaceStatus::AccessRevoked,
            Some("grant_changed"),
        ),
        None,
    );
    settle();
    let ended = text(&dialog);
    assert!(ended.contains("This workspace is no longer available to this device."));
    assert!(!ended.contains("/work/alpha"));
    assert!(gtk::test_accessible_has_role(
        &focused(&dialog),
        gtk::AccessibleRole::Status
    ));
    capture("fixture-gateway-access-ended");

    // Remove: local key retirement only, with host revocation instructions.
    click(&dialog, "connections");
    click(&dialog, &format!("details:{profile_id}"));
    click(&dialog, "remove");
    let remove = text(&dialog);
    assert!(remove.contains("Host access remains until revoked or expired."));
    assert!(remove.contains("crew native revoke"));
    click(&dialog, "confirm-remove");
    assert!(matches!(
        recorder.take().as_slice(),
        [Request::Remote(RemoteRequest::Remove { .. })]
    ));
    remote.respond(RemoteResponse::Removed {
        profile_id: profile_id.clone(),
        result: Ok(()),
    });
    settle();
    assert!(text(&dialog).contains("No remote hosts"));
    assert!(!std::fs::read_to_string(&store_path)
        .unwrap()
        .contains(&profile_id));
    dialog.close();
    settle();
    shell.window().close();
    settle();
}

// Secret Service --------------------------------------------------------------

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn secret_store() {
    use crew_client_transport::gateway::{DeviceSigner, SignerError};
    assert_eq!(secret::probe(), secret::StoreState::Ready);
    let signer = secret::SecretServiceSigner::new("remote-probe", &"ab".repeat(16), "Probe");
    assert_eq!(
        block_on(signer.public_key()),
        Err(SignerError::Retired),
        "no implicit key"
    );
    assert_eq!(signer.prepare(), Ok(secret::KeyOrigin::Created));
    assert_eq!(signer.prepare(), Ok(secret::KeyOrigin::Existing));
    let first = block_on(signer.public_key()).unwrap();
    assert_eq!(
        block_on(signer.public_key()).unwrap(),
        first,
        "stable device key"
    );
    let other = secret::SecretServiceSigner::new("remote-other", &"ab".repeat(16), "Other");
    assert_eq!(other.prepare(), Ok(secret::KeyOrigin::Created));
    assert_ne!(
        block_on(other.public_key()).unwrap(),
        first,
        "one key per profile"
    );
    assert!(!format!("{signer:?}").contains(&first.to_hex()));
    block_on(signer.retire()).unwrap();
    assert_eq!(block_on(signer.public_key()), Err(SignerError::Retired));
    block_on(other.retire()).unwrap();
}

/// Test-only keyring control for the private GNOME Keyring in this session.
fn keyring(lock: bool) {
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
    let call = |path: &str, interface: &str, method: &str, args: glib::Variant, reply: &str| {
        bus.call_sync(
            Some("org.freedesktop.secrets"),
            path,
            interface,
            method,
            Some(&args),
            Some(glib::VariantTy::new(reply).unwrap()),
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .unwrap()
    };
    let login =
        glib::variant::ObjectPath::try_from("/org/freedesktop/secrets/collection/login").unwrap();
    if lock {
        call(
            "/org/freedesktop/secrets",
            "org.freedesktop.Secret.Service",
            "Lock",
            (vec![login],).to_variant(),
            "(aoo)",
        );
        return;
    }
    let session = call(
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service",
        "OpenSession",
        ("plain", "".to_variant()).to_variant(),
        "(vo)",
    )
    .child_value(1);
    let password = std::env::var("CREW_TEST_KEYRING_PASSWORD").unwrap();
    let master = glib::Variant::tuple_from_iter([
        session,
        glib::Variant::array_from_fixed_array::<u8>(&[]),
        glib::Variant::array_from_fixed_array(password.as_bytes()),
        "text/plain".to_variant(),
    ]);
    call(
        "/org/freedesktop/secrets",
        "org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface",
        "UnlockWithMasterPassword",
        glib::Variant::tuple_from_iter([login.to_variant(), master]),
        "()",
    );
}

// Live isolated gateway ---------------------------------------------------------

fn admin(method: &'static str, params: Value) -> Value {
    crew::native::admin_call(method, params.clone()).unwrap_or_else(|e| {
        let pending = crew::native::admin_call("native_pending", json!({}));
        let devices = crew::native::admin_call("native_devices", json!({}));
        panic!("{method}: {e:#}\nparams {params}\npending {pending:?}\ndevices {devices:?}")
    })
}

struct Daemon {
    workspace: tempfile::TempDir,
    path: PathBuf,
    socket: PathBuf,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    fn start(prefix: &str) -> Self {
        let workspace = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/tmp")
            .unwrap();
        let path = workspace.path().canonicalize().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let daemon_path = path.clone();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let running = crew::daemon::start(crew::daemon::DaemonOptions {
                    workspace: daemon_path,
                    crew_bin: std::env::current_exe().unwrap(),
                    pin: None,
                })
                .await
                .unwrap();
                send.send(running.socket().to_path_buf()).unwrap();
                running.serve().await.unwrap();
            });
        });
        let socket = receive.recv_timeout(Duration::from_secs(10)).unwrap();
        Self {
            workspace,
            path,
            socket,
            thread: Some(thread),
        }
    }
    fn caller(&self) -> crew_core::client::Client {
        crew_core::client::Client::operator(&self.socket, "gtk-gateway-test")
            .unwrap()
            .0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Ok((mut caller, _)) =
            crew_core::client::Client::operator(&self.socket, "gtk-gateway-cleanup")
        {
            let _ = caller.call::<Value>(crew_core::rpc::Call::Shutdown { stop_agents: true });
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = &self.workspace;
    }
}

struct Gateway {
    config: crew::native::Config,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Gateway {
    fn start() -> Self {
        let runtime = crew_core::paths::runtime_dir();
        let native = runtime.join("native");
        crew_core::paths::ensure_private_dir(&native).unwrap();
        crew_core::paths::write_private_file(&native.join("key.pem"), KEY).unwrap();
        std::fs::write(native.join("cert.pem"), CERT).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let config = crew::native::Config {
            listen: address,
            endpoint: format!("wss://127.0.0.1:{}/native/ws", address.port()),
            certificate: native.join("cert.pem"),
            private_key: native.join("key.pem"),
            name: "Isolated test host".into(),
            runtime,
        };
        let mut gateway = Self {
            config,
            thread: None,
        };
        gateway.run();
        gateway
    }
    fn run(&mut self) {
        let config = self.config.clone();
        self.thread = Some(std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(async move {
                    let gateway = crew::native::Gateway::open(config).unwrap();
                    gateway.run().await.unwrap();
                });
        }));
        let deadline = Instant::now() + Duration::from_secs(10);
        while crew::native::admin_call("native_status", json!({})).is_err() {
            assert!(Instant::now() < deadline, "isolated gateway startup");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    /// A normal service restart on the same endpoint with retained state.
    fn restart(&mut self) {
        admin("native_stop", json!({}));
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
        self.run();
    }
    fn stop(mut self) {
        admin("native_stop", json!({}));
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = crew::native::admin_call("native_stop", json!({}));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

fn connections_dialog() -> gtk::Dialog {
    let mut found = None;
    until("Connections dialog", || {
        found = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Dialog>().ok())
            .find(|d| d.is_visible() && d.title().as_deref() == Some("Connect to Crew"));
        found.is_some()
    });
    found.unwrap()
}

fn row(window: &gtk::ApplicationWindow, needle: &str) -> gtk::ListBoxRow {
    let mut found = None;
    until(needle, || {
        found = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::ListBoxRow>().ok())
            .find(|r| r.is_visible() && text(r).contains(needle));
        found.is_some()
    });
    found.unwrap()
}

fn load_team(daemon: &Daemon) {
    let config: crew_core::config::TeamConfig = serde_json::from_value(json!({
        "project": "native-gateway-validation", "workspace": {"path": daemon.path, "isolation":"shared"}, "backend":"pty", "orchestrator":{"agent":false},
        "agents":[{"id":"pty-fixture", "role":"native terminal", "runtime":"shell", "command":["/bin/sh","-c","printf 'native PTY ready\\n'; while IFS= read -r line; do printf 'native-input:%s\\n' \"$line\"; done"]}], "tasks":[]
    })).unwrap();
    let _: Value = daemon
        .caller()
        .call(crew_core::rpc::Call::LoadTeam {
            config: Box::new(config),
            herdr_socket: None,
        })
        .unwrap();
}

fn agent_output(daemon: &Daemon) -> String {
    let output: crew_core::rpc::OutputResult = daemon
        .caller()
        .call(crew_core::rpc::Call::AgentOutput {
            id: "pty-fixture".into(),
            lines: Some(80),
            screen: false,
            ansi: false,
        })
        .unwrap();
    output.text
}

fn workspace_id(daemon: &Daemon) -> String {
    crew_core::paths::session_id(&daemon.path.join(".crew"))
}

fn open_connections(controller: &Controller, ready: impl Fn(&str) -> bool) -> gtk::Dialog {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        controller.connections();
        let dialog = connections_dialog();
        if ready(&text(&dialog)) {
            return dialog;
        }
        assert!(Instant::now() < deadline, "Connections content");
        dialog.close();
        settle();
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn live(app: &gtk::Application) {
    // Crew state and sockets stay in this run's private runtime directory,
    // never the shared /tmp fallback.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap();
    assert!(
        crew_core::paths::runtime_dir().starts_with(&runtime),
        "private Crew runtime directory"
    );
    let alpha = Daemon::start("crew-gw-alpha");
    let beta = Daemon::start("crew-gw-beta");
    load_team(&alpha);
    let alpha_path = alpha.path.to_string_lossy().to_string();
    let beta_path = beta.path.to_string_lossy().to_string();
    let mut gateway = Gateway::start();
    let status = admin("native_status", json!({}));
    let profile: PublicProfile = serde_json::from_value(status["profile"].clone()).unwrap();
    // What the operator reads from crew native status at the host console.
    let console_fingerprint = profile.tls_leaf_sha256.to_hex();

    let shell = Shell::new(app);
    let controller = Controller::start(shell.clone());
    shell.window().present();
    until("main window", || shell.window().is_mapped());
    let window = shell.window().clone();
    let dialog = open_connections(&controller, |page| {
        page.contains(&alpha_path) && page.contains(&beta_path)
    });
    assert!(
        focused(&dialog).is::<gtk::Button>(),
        "Connections opens on its first control"
    );
    assert!(text(&dialog).contains("No remote hosts"));
    capture("live-gateway-s01-connections");

    // S02: paste the host's status output, type the console fingerprint.
    click(&dialog, "add");
    entry(&dialog, "field:Label").set_text("Isolated host");
    buffer(&dialog, "field:Profile").set_text(&serde_json::to_string_pretty(&status).unwrap());
    entry(&dialog, "field:Fingerprint").set_text(&grouped(&console_fingerprint, " "));
    click(&dialog, "verify");
    assert!(!text(&dialog).contains("differs"));
    capture("live-gateway-s02-verify");
    click(&dialog, "trust");
    until("secure storage readiness", || {
        text(&dialog).contains("Secure storage ready")
    });
    entry(&dialog, "field:DeviceName").set_text("Isolated laptop");
    capture("live-gateway-s03-device");
    click(&dialog, "continue");
    until("request page", || find(&dialog, "request").is_some());
    let saved_path = glib::user_config_dir()
        .join("crew-desktop")
        .join("connections.json");
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&saved_path).unwrap()).unwrap();
    let profile_id = saved["profiles"][0]["id"].as_str().unwrap().to_owned();
    let store = secret::SecretServiceSigner::new(
        &profile_id,
        &profile.endpoint_id.to_hex(),
        "Isolated host",
    );
    let device_key = block_on(crew_client_transport::gateway::DeviceSigner::public_key(
        &store,
    ))
    .unwrap();
    capture("live-gateway-s04-request");

    // No host window: one bounded, generic refusal; nothing pending, no retry.
    click(&dialog, "request");
    until("refusal", || {
        text(&dialog).contains("Access request was not accepted.")
    });
    assert!(admin("native_pending", json!({}))
        .as_array()
        .unwrap()
        .is_empty());
    capture("live-gateway-s04-refused");

    // The trusted host console opens a window; the app shows the verified code.
    admin("native_enroll", json!({}));
    click(&dialog, "recovery:TryAgain");
    click(&dialog, "request");
    until("comparison code", || find(&dialog, "code:3").is_some());
    let code = (0..4)
        .map(|i| {
            named(&dialog, &format!("code:{i}"))
                .downcast::<gtk::Label>()
                .unwrap()
                .label()
                .to_lowercase()
        })
        .collect::<String>();
    let pending = admin("native_pending", json!({}));
    let pending = pending.as_array().unwrap();
    assert_eq!(pending.len(), 1, "exactly one attempt");
    assert_eq!(
        pending[0]["comparison_code"],
        code.as_str(),
        "console and app codes match"
    );
    assert_eq!(pending[0]["label"], "Isolated laptop");
    let key_hash = pending[0]["public_key_hash"].as_str().unwrap();
    assert_eq!(
        key_hash,
        crew_protocol::native::HexBytes::<32>(
            <sha2::Sha256 as sha2::Digest>::digest(device_key.0).into()
        )
        .to_hex(),
        "the request carries the Secret Service device key"
    );
    capture("live-gateway-s05-compare");
    // Host-selected workspaces and powers: alpha Read+Terminal, beta Read.
    let approved = admin(
        "native_approve",
        json!({"enrollment_id": pending[0]["enrollment_id"], "comparison_code": code,
            "public_key_hash": key_hash,
            "grants": [{"workspace": alpha.path, "control": false, "terminal": true},
                       {"workspace": beta.path, "control": false, "terminal": false}],
            "lifetime_secs": 3600}),
    );
    let device_id = approved["device_id"].as_str().unwrap().to_owned();
    until_shows(&dialog, "Access granted");
    until("authorized listing", || {
        let page = text(&dialog);
        page.contains(&alpha_path) && page.contains(&beta_path)
    });
    capture("live-gateway-s07-granted");

    // Selected workspace: Read+Terminal without Control.
    click(&dialog, &format!("workspace:{}", workspace_id(&alpha)));
    until("live alpha", || status_live(&window));
    until("team listed", || text(&window).contains("pty-fixture"));
    row(&window, "pty-fixture").activate();
    // Agent controls are in the inspector's Actions menu, closed here: each
    // withheld item carries its reason (beside it in the menu, and as its
    // tooltip).
    let menu_item = |label: &str| {
        descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some(label))
    };
    until("agent controls withheld", || {
        menu_item("Pause").is_some_and(|pause| {
            !pause.is_sensitive()
                && pause
                    .tooltip_text()
                    .is_some_and(|reason| reason.contains("Control authority is required"))
        })
    });
    let attach = button_labelled(&window, "Attach terminal");
    until("terminal attach allowed", || attach.is_sensitive());
    assert!(!menu_item("Pause").unwrap().is_sensitive());
    capture("live-gateway-read-terminal");
    attach.emit_clicked();
    // The identified renderer exists only after the Attach notice; input is
    // enabled on that renderer alone.
    let mut found = None;
    until("identified renderer", || {
        found = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<vte4::Terminal>().ok())
            .find(|t| {
                use vte4::prelude::*;
                t.is_input_enabled() && in_inspector_tabs(t)
            });
        found.is_some()
    });
    until_shows(&window, "Attached · input goes to this agent's terminal");
    let terminal = found.unwrap();
    // Attach terminal opens the inspector's Terminal section.
    descendants(window.upcast_ref())
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::ToggleButton>().ok())
        .find(|tab| tab.label().as_deref() == Some("Terminal"))
        .expect("Terminal tab")
        .set_active(true);
    until("terminal shown", || terminal.is_realized());
    // Keyboard input reaches only the focused terminal of the active window.
    activate(&window);
    {
        use vte4::prelude::*;
        terminal.grab_focus();
        until("terminal focused", || terminal.has_focus());
        terminal.feed_child(b"gateway-input\n");
    }
    until("terminal input through the gateway", || {
        agent_output(&alpha).contains("native-input:gateway-input")
    });
    capture("live-gateway-terminal");

    // Host grant change adds Control; the old authority retires first.
    admin(
        "native_grant",
        json!({"device_id": device_id, "selection": {"workspace": alpha.path, "control": true, "terminal": true}}),
    );
    let dialog = connections_dialog();
    until_shows(
        &dialog,
        "changed at the host. Open it again to use its current access.",
    );
    capture("live-gateway-grant-changed");
    click(&dialog, "reopen");
    until("control granted live", || {
        let main = text(&window);
        status_live(&window) && !main.contains("Control authority is required")
    });
    row(&window, "pty-fixture").activate();
    until("control enabled", || {
        menu_item("Pause").is_some_and(|pause| pause.is_sensitive())
    });
    capture("live-gateway-control-granted");

    // Revoke alpha only: safe explanation; beta stays usable.
    admin(
        "native_revoke",
        json!({"device_id": device_id, "workspace": workspace_id(&alpha)}),
    );
    let dialog = connections_dialog();
    until("access ended", || {
        text(&dialog).contains("This workspace is no longer available to this device.")
    });
    assert!(
        !text(&window).contains(&alpha_path),
        "no retained revoked name"
    );
    capture("live-gateway-access-ended");
    click(&dialog, "back-to-workspaces");
    // The index reconnects on its own after a dropped channel; after a grant
    // revocation the operator may need Reconnect now (reported to transport).
    until_for(Duration::from_secs(10), "index settles", || {
        let page = text(&dialog);
        page.contains(&beta_path) || find(&dialog, "reconnect").is_some()
    });
    if let Some(reconnect) = find(&dialog, "reconnect") {
        capture("live-gateway-reconnecting");
        reconnect.downcast::<gtk::Button>().unwrap().emit_clicked();
    }
    until_for(Duration::from_secs(20), "beta remains", || {
        let page = text(&dialog);
        page.contains(&beta_path) && !page.contains(&alpha_path)
    });
    capture("live-gateway-remaining-workspace");
    click(&dialog, &format!("workspace:{}", workspace_id(&beta)));
    until("beta live", || status_live(&window));

    // Normal service restart with retained state: the same key reconnects
    // and refreshes authority and snapshot before acting again.
    gateway.restart();
    until("service stop observed", || !status_live(&window));
    let dialog = open_connections(&controller, |_| true);
    if find(&dialog, &format!("details:{profile_id}")).is_none() {
        click(&dialog, "connections");
    }
    click(&dialog, &format!("details:{profile_id}"));
    assert!(text(&dialog).contains("Native service is unavailable"));
    capture("live-gateway-service-stopped");
    click(&dialog, "reconnect");
    dialog.close();
    until("reconnected after service restart", || {
        let main = text(&window);
        status_live(&window) && main.contains(&beta_path)
    });
    capture("live-gateway-service-restart");

    // Locked keyring: signing refuses promptly without an unlock prompt.
    keyring(true);
    let dialog = open_connections(&controller, |_| true);
    if find(&dialog, &format!("details:{profile_id}")).is_none() {
        click(&dialog, "connections");
    }
    click(&dialog, &format!("details:{profile_id}"));
    click(&dialog, "reconnect");
    let dialog = connections_dialog();
    until_shows(
        &dialog,
        "Unlock this device's credential storage and try again.",
    );
    assert!(status_reads(
        &window,
        crew_desktop::lifecycle::phrase(EndpointStatus::SignedOut, WorkspaceStatus::NoneSelected),
    ));
    capture("live-gateway-keyring-locked");
    // Unlocking happens outside Crew (here the private keyring's own
    // interface); Try again authenticates the same key afresh.
    keyring(false);
    click(&dialog, "recovery:TryAgain");
    until("authorized listing after unlock", || {
        find(&dialog, &format!("workspace:{}", workspace_id(&beta))).is_some()
    });
    click(&dialog, &format!("workspace:{}", workspace_id(&beta)));
    until("reconnected after unlock", || status_live(&window));

    // Host revocation of the device ends access on every channel.
    admin("native_revoke", json!({"device_id": device_id}));
    until("device revoked", || {
        status_reads(
            &window,
            crew_desktop::lifecycle::phrase(
                EndpointStatus::SignedOut,
                WorkspaceStatus::NoneSelected,
            ),
        )
    });
    capture("live-gateway-device-revoked");
    let saved_text = std::fs::read_to_string(&saved_path).unwrap();
    for forbidden in ["PRIVATE KEY", "signature", &code] {
        assert!(!saved_text.contains(forbidden), "{forbidden} persisted");
    }

    // Host trust failure: the entered value matches a profile whose pin is
    // not the served certificate. TLS refuses before any device proof, so the
    // open enrollment window receives nothing.
    admin("native_enroll", json!({}));
    let mut forged = status.clone();
    forged["profile"]["tls_leaf_sha256"] = json!("ab".repeat(32));
    let dialog = open_connections(&controller, |_| true);
    if find(&dialog, "add").is_none() {
        click(&dialog, "connections");
    }
    click(&dialog, "add");
    entry(&dialog, "field:Label").set_text("Wrong pin host");
    buffer(&dialog, "field:Profile").set_text(&forged.to_string());
    entry(&dialog, "field:Fingerprint").set_text(&grouped(&"ab".repeat(32), " "));
    click(&dialog, "verify");
    click(&dialog, "trust");
    until_shows(&dialog, "Secure storage ready");
    entry(&dialog, "field:DeviceName").set_text("Isolated laptop");
    click(&dialog, "continue");
    until("request page", || find(&dialog, "request").is_some());
    click(&dialog, "request");
    until_shows(&dialog, "Host identity could not be verified.");
    assert!(find(&dialog, "recovery:ReviewProfile").is_some());
    assert!(find(&dialog, "trust").is_none(), "no connect-anyway action");
    assert!(
        admin("native_pending", json!({}))
            .as_array()
            .unwrap()
            .is_empty(),
        "no device proof reached the host"
    );
    capture("live-gateway-trust-failure");
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&saved_path).unwrap()).unwrap();
    let forged_id = saved["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["label"] == "Wrong pin host")
        .and_then(|p| p["id"].as_str())
        .unwrap()
        .to_owned();
    click(&dialog, "connections");
    click(&dialog, &format!("details:{forged_id}"));
    click(&dialog, "remove");
    click(&dialog, "confirm-remove");
    until_shows(&dialog, "Profile removed");
    dialog.close();
    settle();

    // Remove from this app retires the OS key; the host record stays revoked.
    let dialog = open_connections(&controller, |_| true);
    click(&dialog, &format!("details:{profile_id}"));
    click(&dialog, "remove");
    click(&dialog, "confirm-remove");
    until("removed", || text(&dialog).contains("Profile removed"));
    assert_eq!(
        block_on(crew_client_transport::gateway::DeviceSigner::public_key(
            &store
        )),
        Err(crew_client_transport::gateway::SignerError::Retired),
        "Remove from this app retired the OS key"
    );
    dialog.close();
    window.close();
    drop(controller);
    settle();
    gateway.stop();
    drop(beta);
    drop(alpha);
}
