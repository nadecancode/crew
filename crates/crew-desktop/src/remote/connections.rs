//! GTK Connections settings (S01–S08): local and remote profiles, host trust,
//! device naming, the reverse-approval request, access review and recovery.
//! Widgets hold only public profile fields, typed setup progress and safe
//! connector reasons. Grants, authority and eligibility come from the shared
//! connector and core; this module renders them and sends explicit requests.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};

use crew_client::api::{
    ClientNotice, ClientReason, ConnectionContext, ConnectionMode, EndpointStatus, GrantTier,
    WorkspaceStatus,
};
use crew_client_transport::gateway::{EnrollmentOutcome, EnrollmentProgress};
use crew_protocol::rpc::Workspaces;
use gtk::{glib, prelude::*};

use super::copy::{self, Phase, Problem, Recovery};
use super::profile::{
    self, CheckedInput, Field, FieldError, KeyStorage, ProfileInput, ProfileStore, RemoteProfile,
    SavedAccess,
};
use super::secret::StoreState;
use super::worker::{RemoteRequest, RemoteResponse};
use crate::service::Request;
use crate::views::common;

/// A discovered same-host workspace, described by the controller.
pub struct LocalEntry {
    pub title: String,
    pub detail: String,
    pub profile_id: String,
    pub endpoint_id: String,
    pub workspace_key: String,
}

/// What the Connections window needs from the application controller.
pub trait Host {
    fn send(&self, request: Request) -> bool;
    fn parent(&self) -> gtk::ApplicationWindow;
    fn local_entries(&self) -> (Vec<LocalEntry>, Option<String>);
    fn connection(&self) -> Option<ConnectionContext>;
    fn listing(&self) -> Option<Workspaces>;
}

#[derive(Clone, Debug, PartialEq)]
enum Page {
    List,
    /// S02 public profile form.
    Profile,
    /// S02 fingerprint check.
    Trust,
    /// S03 device name and secure storage.
    Device,
    /// S04 request access.
    Request(String),
    /// S05 comparison code.
    Compare(String),
    /// S07 after approval, or the authorized workspaces of a connection.
    Workspaces {
        profile: String,
        granted: bool,
    },
    /// S08 connection details and this device's access.
    Details(String),
    Revoke(String),
    Remove(String),
    Problem {
        profile: String,
        problem: Problem,
    },
    AccessEnded(String),
}

impl Page {
    /// Pages whose content follows background connection updates. Pages with
    /// form input never rebuild underneath the user.
    fn live(&self) -> bool {
        matches!(
            self,
            Self::List | Self::Workspaces { .. } | Self::Details(_) | Self::AccessEnded(_)
        )
    }
}

#[derive(Default)]
struct Draft {
    input: ProfileInput,
    error: Option<FieldError>,
    checked: Option<CheckedInput>,
    focus_fingerprint: bool,
    trusted: Option<RemoteProfile>,
    device_name: String,
    memory: bool,
    preparing: bool,
    store_error: Option<ClientReason>,
}

struct Ceremony {
    profile: String,
    attempt: Option<u64>,
    phase: Phase,
    code: Option<(String, i64)>,
    stage: &'static str,
    canceling: bool,
}

struct Window {
    dialog: gtk::Dialog,
    slot: gtk::Box,
    back: gtk::Button,
    history: RefCell<Vec<Page>>,
    page: RefCell<Page>,
    opener: glib::WeakRef<gtk::Widget>,
}

pub struct Remote {
    weak: Weak<Remote>,
    host: Weak<dyn Host>,
    store: ProfileStore,
    profiles: RefCell<Vec<RemoteProfile>>,
    store_message: RefCell<Option<&'static str>>,
    active: RefCell<Option<String>>,
    endpoint: Cell<Option<EndpointStatus>>,
    workspace: Cell<Option<WorkspaceStatus>>,
    reasons: RefCell<BTreeMap<String, ClientReason>>,
    store_state: Cell<Option<StoreState>>,
    draft: RefCell<Draft>,
    ceremony: RefCell<Option<Ceremony>>,
    window: RefCell<Option<Window>>,
    list_status: RefCell<Option<String>>,
    context_pending: Cell<bool>,
    context_again: Cell<bool>,
    /// The last bound workspace, for the access-change explanation.
    lost_workspace: RefCell<Option<String>>,
}

fn named(widget: &impl IsA<gtk::Widget>, name: &str) {
    widget.set_widget_name(name);
}

fn button(name: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    named(&button, name);
    button
}

fn section(body: &gtk::Box, title: &str) {
    let label = common::text(title);
    label.add_css_class("crew-title");
    label.set_margin_top(12);
    label.update_property(&[gtk::accessible::Property::Label(title)]);
    body.append(&label);
}

fn status_label(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .accessible_role(gtk::AccessibleRole::Status)
        .build();
    named(&label, "status");
    label
}

fn tier_text(tiers: &[GrantTier], tier: GrantTier) -> &'static str {
    if tiers.contains(&tier) {
        "Granted"
    } else {
        "Not granted"
    }
}

/// One connection entry with its state and the actions that fit it, in the
/// shape the local-only build uses too.
fn row(title: &str, detail: &str, state: &str, actions: &gtk::Box) -> gtk::Box {
    common::entry_row(title, detail, state, actions)
}

/// Wide aligned content scrolls sideways instead of truncating identity.
fn scroll_horizontal(widget: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .child(widget)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .build()
}

/// Swap the page content. Focus inside the old page is released first, so
/// GTK never tracks a removed widget as the focus origin.
fn replace(window: &Window, content: &gtk::Box) {
    if gtk::prelude::GtkWindowExt::focus(&window.dialog)
        .is_some_and(|focus| focus.is_ancestor(&window.slot))
    {
        gtk::prelude::GtkWindowExt::set_focus(&window.dialog, None::<&gtk::Widget>);
    }
    common::clear(&window.slot);
    let scroll = common::scroll(content);
    // Keep the focused field in view, e.g. after a failed submit.
    if let Some(viewport) = scroll.child().and_downcast::<gtk::Viewport>() {
        if viewport.find_property("scroll-to-focus").is_some() {
            viewport.set_property("scroll-to-focus", true);
        }
    }
    window.slot.append(&scroll);
}

/// The first enabled control in reading order.
fn first_control(root: &gtk::Widget) -> Option<gtk::Widget> {
    let mut child = root.first_child();
    while let Some(widget) = child {
        let control = widget.is::<gtk::Button>()
            || widget.is::<gtk::Entry>()
            || widget.is::<gtk::TextView>()
            || widget.is::<gtk::CheckButton>();
        if widget.is_visible() && widget.is_sensitive() {
            if control {
                return Some(widget);
            }
            if let Some(found) = first_control(&widget) {
                return Some(found);
            }
        }
        child = widget.next_sibling();
    }
    None
}

fn find_named(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    let mut child = root.first_child();
    while let Some(widget) = child {
        if widget.widget_name() == name {
            return Some(widget);
        }
        if let Some(found) = find_named(&widget, name) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

impl Remote {
    pub fn new(host: Weak<dyn Host>, store: ProfileStore) -> Rc<Self> {
        let (profiles, store_message) = match store.load() {
            Ok(profiles) => (profiles, None),
            Err(message) => (Vec::new(), Some(message)),
        };
        Rc::new_cyclic(|weak| Self {
            weak: weak.clone(),
            host,
            store,
            profiles: RefCell::new(profiles),
            store_message: RefCell::new(store_message),
            active: RefCell::new(None),
            endpoint: Cell::new(None),
            workspace: Cell::new(None),
            reasons: RefCell::new(BTreeMap::new()),
            store_state: Cell::new(None),
            draft: RefCell::new(Draft::default()),
            ceremony: RefCell::new(None),
            window: RefCell::new(None),
            list_status: RefCell::new(None),
            context_pending: Cell::new(false),
            context_again: Cell::new(false),
            lost_workspace: RefCell::new(None),
        })
    }

    fn host(&self) -> Option<Rc<dyn Host>> {
        self.host.upgrade()
    }

    fn send(&self, request: RemoteRequest) -> bool {
        self.host()
            .is_some_and(|host| host.send(Request::Remote(request)))
    }

    pub fn profiles(&self) -> Vec<RemoteProfile> {
        self.profiles.borrow().clone()
    }

    fn profile(&self, id: &str) -> Option<RemoteProfile> {
        self.profiles.borrow().iter().find(|p| p.id == id).cloned()
    }

    pub fn active_profile(&self) -> Option<RemoteProfile> {
        let active = self.active.borrow().clone()?;
        self.profile(&active)
    }

    fn save(&self) {
        let result = self.store.save(&self.profiles.borrow());
        *self.store_message.borrow_mut() = result.err();
    }

    fn upsert(&self, profile: RemoteProfile) {
        {
            let mut profiles = self.profiles.borrow_mut();
            match profiles.iter_mut().find(|p| p.id == profile.id) {
                Some(existing) => *existing = profile,
                None => profiles.push(profile),
            }
        }
        self.save();
    }

    /// The current Connections dialog, if open.
    pub fn dialog(&self) -> Option<gtk::Dialog> {
        self.window.borrow().as_ref().map(|w| w.dialog.clone())
    }

    /// Open Connections at its list (S01), or present the open dialog.
    pub fn open(&self) {
        if let Some(dialog) = self.dialog() {
            dialog.present();
            return;
        }
        let Some(host) = self.host() else { return };
        let parent = host.parent();
        let dialog = gtk::Dialog::builder()
            .transient_for(&parent)
            .modal(true)
            .destroy_with_parent(true)
            .title("Connect to Crew")
            .default_width(640)
            .default_height(560)
            .build();
        let opener = glib::WeakRef::new();
        opener.set(gtk::prelude::GtkWindowExt::focus(&parent).as_ref());
        let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.set_margin_top(8);
        bar.set_margin_start(8);
        bar.set_margin_end(8);
        let back = button("back", "Back");
        back.update_property(&[gtk::accessible::Property::Label("Back to previous page")]);
        back.set_visible(false);
        back.connect_clicked({
            let weak = self.weak.clone();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    remote.back();
                }
            }
        });
        bar.append(&back);
        outer.append(&bar);
        let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        slot.set_vexpand(true);
        outer.append(&slot);
        dialog.content_area().append(&outer);
        dialog.connect_response({
            let weak = self.weak.clone();
            move |dialog, response| {
                let Some(remote) = weak.upgrade() else {
                    dialog.close();
                    return;
                };
                // Escape cancels an in-flight request before closing anything.
                if response == gtk::ResponseType::Cancel && remote.cancel_request() {
                    return;
                }
                dialog.close();
            }
        });
        dialog.connect_close_request({
            let weak = self.weak.clone();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    remote.cancel_request();
                    if let Some(window) = remote.window.borrow_mut().take() {
                        if let Some(opener) = window.opener.upgrade() {
                            opener.grab_focus();
                        }
                    }
                }
                glib::Propagation::Proceed
            }
        });
        crate::dialogs::escape(&dialog);
        *self.window.borrow_mut() = Some(Window {
            dialog: dialog.clone(),
            slot,
            back,
            history: RefCell::new(Vec::new()),
            page: RefCell::new(Page::List),
            opener,
        });
        self.show(Page::List, false);
        dialog.present();
        // Focus requested before the dialog was mapped does not stick.
        if let Some(control) = self
            .window
            .borrow()
            .as_ref()
            .and_then(|w| first_control(w.slot.upcast_ref()))
        {
            control.grab_focus();
        }
    }

    fn current(&self) -> Option<Page> {
        self.window
            .borrow()
            .as_ref()
            .map(|w| w.page.borrow().clone())
    }

    fn back(&self) {
        let previous = self
            .window
            .borrow()
            .as_ref()
            .and_then(|w| w.history.borrow_mut().pop());
        if let Some(page) = previous {
            self.show(page, false);
        }
    }

    /// Replace the visible page and move focus to its first meaningful item.
    fn show(&self, page: Page, push: bool) {
        let (content, focus) = self.build(&page);
        let window = self.window.borrow();
        let Some(window) = window.as_ref() else {
            return;
        };
        if push {
            let current = window.page.borrow().clone();
            if current != page {
                window.history.borrow_mut().push(current);
            }
        }
        if matches!(page, Page::List) {
            window.history.borrow_mut().clear();
        }
        *window.page.borrow_mut() = page;
        replace(window, &content);
        let in_ceremony = self
            .ceremony
            .borrow()
            .as_ref()
            .is_some_and(|c| c.attempt.is_some() || c.stage == "Connecting to the host");
        window
            .back
            .set_visible(!window.history.borrow().is_empty() && !in_ceremony);
        // A page opens on its first control. Error summaries, invalid fields
        // and safe explanations keep focus where the builder put it.
        let focus = if focus.is::<gtk::Label>() && focus.widget_name() != "status" {
            first_control(content.upcast_ref()).unwrap_or(focus)
        } else {
            focus
        };
        focus.grab_focus();
        if let Some(label) = focus.downcast_ref::<gtk::Label>() {
            label.select_region(0, 0);
        }
    }

    /// Rebuild a live page in place. Focus stays on the same named control
    /// if it survives; background updates never move focus elsewhere.
    fn refresh(&self) {
        let Some(page) = self.current().filter(Page::live) else {
            return;
        };
        let (content, _) = self.build(&page);
        let window = self.window.borrow();
        let Some(window) = window.as_ref() else {
            return;
        };
        let focused = gtk::prelude::GtkWindowExt::focus(&window.dialog)
            .filter(|focus| focus.is_ancestor(&window.slot))
            .map(|focus| focus.widget_name().to_string());
        replace(window, &content);
        if let Some(name) = focused.filter(|name| !name.is_empty()) {
            if let Some(widget) = find_named(window.slot.upcast_ref(), &name) {
                widget.grab_focus();
            }
        }
    }

    fn build(&self, page: &Page) -> (gtk::Box, gtk::Widget) {
        match page {
            Page::List => self.list_page(),
            Page::Profile => self.profile_page(),
            Page::Trust => self.trust_page(),
            Page::Device => self.device_page(),
            Page::Request(id) => self.request_page(id),
            Page::Compare(id) => self.compare_page(id),
            Page::Workspaces { profile, granted } => self.workspaces_page(profile, *granted),
            Page::Details(id) => self.details_page(id),
            Page::Revoke(id) => self.revoke_page(id),
            Page::Remove(id) => self.remove_page(id),
            Page::Problem { profile, problem } => self.problem_page(profile, problem),
            Page::AccessEnded(id) => self.access_ended_page(id),
        }
    }

    fn go(&self, page: Page) -> impl Fn(&gtk::Button) + 'static {
        let weak = self.weak.clone();
        move |_| {
            if let Some(remote) = weak.upgrade() {
                remote.show(page.clone(), true);
            }
        }
    }

    // S01 -----------------------------------------------------------------

    fn list_page(&self) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("Connections");
        body.append(&heading);
        body.append(&common::text("Choose a connection explicitly. Selecting a profile does not join a workspace, start a service or grant access."));
        if let Some(status) = self.list_status.borrow().as_ref() {
            body.append(&status_label(status));
        }
        let host = self.host();
        let connection = host.as_ref().and_then(|h| h.connection());
        if let Some(connection) = connection.as_ref() {
            body.append(&common::text(&match connection.mode {
                ConnectionMode::LocalOperator => "Local operator · full Crew access".to_owned(),
                ConnectionMode::ScopedGateway => format!(
                    "This device's access · {}",
                    self.active_profile()
                        .map_or("remote host".into(), |p| p.label)
                ),
            }));
        }

        section(&body, "This computer");
        let (local, local_error) = host
            .as_ref()
            .map_or((Vec::new(), None), |host| host.local_entries());
        if let Some(error) = local_error {
            body.append(&common::text(&error));
        }
        if local.is_empty() {
            body.append(&common::text("No local workspaces found. Start Crew at the host console, then refresh. Protected discovery never scans arbitrary socket paths."));
        }
        for entry in local {
            let connected = connection.as_ref().is_some_and(|c| {
                c.mode == ConnectionMode::LocalOperator
                    && c.scope.workspace_id == entry.workspace_key
            });
            let state = if connected {
                "Connected"
            } else {
                "Not connected"
            };
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            if connected {
                let disconnect = button(
                    &format!("local-disconnect:{}", entry.workspace_key),
                    "Disconnect",
                );
                disconnect.connect_clicked({
                    let weak = self.weak.clone();
                    move |_| {
                        if let Some(host) = weak.upgrade().and_then(|r| r.host()) {
                            host.send(Request::Disconnect);
                        }
                    }
                });
                actions.append(&disconnect);
            } else {
                let connect = button(&format!("local:{}", entry.workspace_key), "Connect");
                connect.add_css_class("suggested-action");
                connect.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Connect to local workspace {}",
                    entry.title
                ))]);
                connect.connect_clicked({
                    let weak = self.weak.clone();
                    let entry = (
                        entry.profile_id.clone(),
                        entry.endpoint_id.clone(),
                        entry.workspace_key.clone(),
                    );
                    move |_| {
                        let Some(remote) = weak.upgrade() else { return };
                        if let Some(host) = remote.host() {
                            host.send(Request::Connect {
                                profile_id: entry.0.clone(),
                                endpoint_id: entry.1.clone(),
                                workspace_id: Some(entry.2.clone()),
                            });
                        }
                        if let Some(dialog) = remote.dialog() {
                            dialog.close();
                        }
                    }
                });
                actions.append(&connect);
            }
            body.append(&row(&entry.title, &entry.detail, state, &actions));
        }

        section(&body, "Remote hosts");
        if let Some(message) = *self.store_message.borrow() {
            body.append(&common::text(message));
        }
        let profiles = self.profiles();
        if profiles.is_empty() {
            body.append(&common::text("No remote hosts. Add a remote connection with the public profile from crew native status at the host."));
        }
        let active = self.active.borrow().clone();
        for profile in profiles {
            let is_active = active.as_deref() == Some(profile.id.as_str());
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            if is_active
                && self
                    .endpoint
                    .get()
                    .is_some_and(|e| e != EndpointStatus::Disconnected)
            {
                let disconnect = button(&format!("disconnect:{}", profile.id), "Disconnect");
                disconnect.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Disconnect from {}",
                    profile.label
                ))]);
                disconnect.connect_clicked({
                    let weak = self.weak.clone();
                    move |_| {
                        if let Some(host) = weak.upgrade().and_then(|r| r.host()) {
                            host.send(Request::Disconnect);
                        }
                    }
                });
                actions.append(&disconnect);
                if self.endpoint.get() == Some(EndpointStatus::Authenticated) {
                    let workspaces = button(&format!("workspaces:{}", profile.id), "Workspaces");
                    workspaces.connect_clicked(self.go(Page::Workspaces {
                        profile: profile.id.clone(),
                        granted: false,
                    }));
                    actions.append(&workspaces);
                }
            } else {
                let connect = button(&format!("connect:{}", profile.id), "Connect");
                connect.add_css_class("suggested-action");
                connect.update_property(&[gtk::accessible::Property::Label(&format!(
                    "Connect to {}",
                    profile.label
                ))]);
                let id = profile.id.clone();
                connect.connect_clicked({
                    let weak = self.weak.clone();
                    move |_| {
                        if let Some(remote) = weak.upgrade() {
                            remote.connect(&id);
                        }
                    }
                });
                actions.append(&connect);
            }
            let details = button(&format!("details:{}", profile.id), "Connection details");
            details.update_property(&[gtk::accessible::Property::Label(&format!(
                "Connection details for {}",
                profile.label
            ))]);
            details.connect_clicked(self.go(Page::Details(profile.id.clone())));
            actions.append(&details);
            body.append(&row(
                &profile.label,
                &format!("Remote · scoped device access\n{}", profile.public.url),
                &self.profile_state(&profile, is_active),
                &actions,
            ));
        }
        let add = button("add", "Add remote connection");
        add.set_halign(gtk::Align::Start);
        add.connect_clicked({
            let weak = self.weak.clone();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    *remote.draft.borrow_mut() = Draft::default();
                    remote.show(Page::Profile, true);
                }
            }
        });
        body.append(&add);

        if let (Some(listing), Some(connection)) =
            (host.as_ref().and_then(|h| h.listing()), connection.as_ref())
        {
            if connection.mode == ConnectionMode::LocalOperator {
                section(&body, "Joined workspaces");
                self.workspace_buttons(&body, &listing);
            }
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let refresh = button("session:0", "Refresh");
        refresh.connect_clicked({
            let weak = self.weak.clone();
            move |_| {
                let Some(remote) = weak.upgrade() else { return };
                if let Some(host) = remote.host() {
                    host.send(Request::RefreshProfiles);
                }
                if let Some(dialog) = remote.dialog() {
                    dialog.close();
                }
            }
        });
        actions.append(&refresh);
        // Reconnect is offered only while a connection is reconnecting or stale.
        let stale = connection.is_some()
            && matches!(
                self.endpoint.get(),
                Some(EndpointStatus::Reconnecting | EndpointStatus::SignedOut)
            );
        if stale {
            let reconnect = button("session:1", "Reconnect");
            reconnect.connect_clicked({
                let weak = self.weak.clone();
                move |_| {
                    if let Some(host) = weak.upgrade().and_then(|r| r.host()) {
                        host.send(Request::Reconnect);
                    }
                }
            });
            actions.append(&reconnect);
        }
        let close = button("close", "Close");
        close.connect_clicked({
            let weak = self.weak.clone();
            move |_| {
                if let Some(dialog) = weak.upgrade().and_then(|r| r.dialog()) {
                    dialog.close();
                }
            }
        });
        actions.append(&close);
        body.append(&actions);
        body.append(&common::text("Disconnect or closing Crew leaves the team running. Stop team is a separate confirmed action."));
        body.append(&common::text(copy::NO_BROWSER_CODES));
        (body, heading.upcast())
    }

    fn profile_state(&self, profile: &RemoteProfile, active: bool) -> String {
        if active {
            if let Some(endpoint) = self.endpoint.get() {
                let state = match endpoint {
                    EndpointStatus::Authenticated => match self.workspace.get() {
                        Some(WorkspaceStatus::Live) => "Connected · workspace live",
                        _ => "Connected · choose a workspace",
                    },
                    EndpointStatus::Connecting => "Connecting",
                    EndpointStatus::Reconnecting => "Reconnecting · last known content",
                    EndpointStatus::Disconnected => {
                        return self.reasons.borrow().get(&profile.id).map_or_else(
                            || "Not connected".to_owned(),
                            |reason| {
                                copy::problem(reason, Phase::Live, profile.storage)
                                    .title
                                    .to_owned()
                            },
                        );
                    }
                    EndpointStatus::SignedOut => "Device access unavailable",
                    EndpointStatus::Incompatible => "Incompatible versions",
                    EndpointStatus::Stopped => "Native service is unavailable",
                };
                return state.to_owned();
            }
        }
        if let Some(reason) = self.reasons.borrow().get(&profile.id) {
            return copy::problem(reason, Phase::Connect, profile.storage)
                .title
                .to_owned();
        }
        match (&profile.saved_access, profile.storage) {
            (None, _) => "Not connected · access not approved yet".into(),
            (Some(_), KeyStorage::Memory) => "Not connected · temporary device key".into(),
            (Some(_), KeyStorage::SecureStore) => "Not connected".into(),
        }
    }

    fn workspace_buttons(&self, body: &gtk::Box, listing: &Workspaces) {
        for entry in &listing.joined {
            let label = format!(
                "{} · {}\n{}{}",
                entry.project.as_deref().unwrap_or("Crew"),
                entry.state,
                entry.workspace,
                if entry.detail.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", entry.detail)
                }
            );
            let open = gtk::Button::with_label(&label);
            named(&open, &format!("workspace:{}", entry.id));
            open.update_property(&[gtk::accessible::Property::Label(&format!(
                "Open workspace {}, {}",
                entry.workspace, entry.state
            ))]);
            open.connect_clicked({
                let weak = self.weak.clone();
                let id = entry.id.clone();
                move |_| {
                    let Some(remote) = weak.upgrade() else { return };
                    if let Some(host) = remote.host() {
                        host.send(Request::SelectWorkspace(id.clone()));
                    }
                    if let Some(dialog) = remote.dialog() {
                        dialog.close();
                    }
                }
            });
            body.append(&open);
        }
    }

    fn connect(&self, id: &str) {
        let Some(profile) = self.profile(id) else {
            return;
        };
        self.reasons.borrow_mut().remove(id);
        self.list_status.borrow_mut().take();
        if self.send(RemoteRequest::Connect { profile }) {
            self.show(
                Page::Workspaces {
                    profile: id.to_owned(),
                    granted: false,
                },
                true,
            );
        }
    }

    // S02 -----------------------------------------------------------------

    fn profile_page(&self) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("Host profile");
        body.append(&heading);
        body.append(&common::text("Paste the public profile of the host and enter its fingerprint. The profile contains no secret and grants nothing; Crew trusts the host only when the fingerprint you enter matches."));
        let draft = self.draft.borrow();
        let summary = status_label(&draft.error.as_ref().map_or(String::new(), |e| {
            format!("{}: {}", e.field.title(), e.message)
        }));
        summary.add_css_class("error");
        summary.set_visible(draft.error.is_some());
        body.append(&summary);
        let error_for = |widget: &gtk::Widget, body: &gtk::Box, field: Field| {
            let error = draft.error.as_ref().filter(|e| e.field == field)?;
            let message = common::text(&error.message);
            message.add_css_class("error");
            body.append(&message);
            widget.update_relation(&[gtk::accessible::Relation::DescribedBy(&[
                message.upcast_ref()
            ])]);
            widget.update_state(&[gtk::accessible::State::Invalid(
                gtk::AccessibleInvalidState::True,
            )]);
            Some((widget.clone(), message))
        };
        // A field's error clears as soon as that field is edited.
        let clear_on_edit = |error: Option<(gtk::Widget, gtk::Label)>, field: Field| {
            let weak = self.weak.clone();
            let summary = summary.clone();
            move || {
                if let Some((widget, message)) = error.as_ref() {
                    message.set_visible(false);
                    summary.set_visible(false);
                    widget.update_state(&[gtk::accessible::State::Invalid(
                        gtk::AccessibleInvalidState::False,
                    )]);
                }
                if let Some(remote) = weak.upgrade() {
                    let mut draft = remote.draft.borrow_mut();
                    if draft.error.as_ref().is_some_and(|e| e.field == field) {
                        draft.error = None;
                    }
                }
            }
        };

        let label_caption = gtk::Label::builder()
            .label(Field::Label.title())
            .xalign(0.0)
            .build();
        let label = gtk::Entry::builder()
            .text(&draft.input.label)
            .placeholder_text("Defaults to the host name")
            .build();
        named(&label, "field:Label");
        label.update_property(&[gtk::accessible::Property::Label(Field::Label.title())]);
        label_caption.set_mnemonic_widget(Some(&label));
        body.append(&label_caption);
        body.append(&label);
        let label_error = error_for(label.upcast_ref(), &body, Field::Label);
        let mut first_invalid = label_error.as_ref().map(|(w, _)| w.clone());
        let clear = clear_on_edit(label_error, Field::Label);
        label.connect_changed(move |_| clear());

        let profile_caption = gtk::Label::builder()
            .label(Field::Profile.title())
            .xalign(0.0)
            .build();
        let profile = gtk::TextView::builder()
            .monospace(true)
            .wrap_mode(gtk::WrapMode::Char)
            .left_margin(8)
            .right_margin(8)
            .top_margin(8)
            .bottom_margin(8)
            // Tab moves to the next field; a pasted profile needs no tab characters.
            .accepts_tab(false)
            .input_hints(gtk::InputHints::NO_SPELLCHECK | gtk::InputHints::NO_EMOJI)
            .build();
        named(&profile, "field:Profile");
        profile.buffer().set_text(&draft.input.profile);
        profile.update_property(&[
            gtk::accessible::Property::Label(Field::Profile.title()),
            gtk::accessible::Property::Description(
                "Paste the output of crew native status from the host.",
            ),
        ]);
        profile_caption.set_mnemonic_widget(Some(&profile));
        body.append(&profile_caption);
        // About eight lines; a long profile scrolls inside its own frame so the
        // fingerprint and Verify this host stay in view.
        let profile_scroll = gtk::ScrolledWindow::builder()
            .child(&profile)
            .min_content_height(152)
            .max_content_height(152)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        body.append(&gtk::Frame::builder().child(&profile_scroll).build());
        body.append(&common::text(
            "Paste the output of crew native status from the host.",
        ));
        let import = button("import", "Import profile file…");
        import.set_halign(gtk::Align::Start);
        body.append(&import);
        let profile_error = error_for(profile.upcast_ref(), &body, Field::Profile);
        first_invalid = first_invalid.or(profile_error.as_ref().map(|(w, _)| w.clone()));
        let clear = clear_on_edit(profile_error, Field::Profile);
        profile.buffer().connect_changed(move |_| clear());

        let fingerprint_caption = gtk::Label::builder()
            .label(Field::Fingerprint.title())
            .xalign(0.0)
            .build();
        let fingerprint = gtk::Entry::builder()
            .text(&draft.input.fingerprint)
            .input_hints(gtk::InputHints::NO_SPELLCHECK | gtk::InputHints::NO_EMOJI)
            .build();
        fingerprint.add_css_class("monospace");
        named(&fingerprint, "field:Fingerprint");
        fingerprint.update_property(&[
            gtk::accessible::Property::Label(Field::Fingerprint.title()),
            gtk::accessible::Property::Description(copy::FINGERPRINT_HELP),
        ]);
        fingerprint_caption.set_mnemonic_widget(Some(&fingerprint));
        body.append(&fingerprint_caption);
        body.append(&fingerprint);
        body.append(&common::text(copy::FINGERPRINT_HELP));
        let echo = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::Word)
            .selectable(true)
            .build();
        echo.add_css_class("monospace");
        named(&echo, "fingerprint-echo");
        let count = common::text("");
        named(&count, "fingerprint-count");
        let update = {
            let echo = echo.clone();
            let count = count.clone();
            move |text: &str| {
                let kept = profile::fingerprint_characters(text);
                echo.set_label(&profile::group_hex(&kept, 4));
                echo.set_visible(!kept.is_empty());
                count.set_label(&format!("{} of 64", kept.chars().count()));
            }
        };
        update(&draft.input.fingerprint);
        body.append(&echo);
        body.append(&count);
        let fingerprint_error = error_for(fingerprint.upcast_ref(), &body, Field::Fingerprint);
        first_invalid = first_invalid.or(fingerprint_error.as_ref().map(|(w, _)| w.clone()));
        let clear = clear_on_edit(fingerprint_error, Field::Fingerprint);
        fingerprint.connect_changed(move |entry| {
            update(&entry.text());
            clear();
        });
        let focus_fingerprint = draft.focus_fingerprint;
        drop(draft);

        let collect = {
            let label = label.clone();
            let profile = profile.clone();
            let fingerprint = fingerprint.clone();
            move || ProfileInput {
                label: label.text().to_string(),
                profile: common::buffer_text(&profile.buffer()),
                fingerprint: fingerprint.text().to_string(),
            }
        };
        import.connect_clicked({
            let weak = self.weak.clone();
            let collect = collect.clone();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    remote.draft.borrow_mut().input = collect();
                    remote.import();
                }
            }
        });
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let verify = button("verify", "Verify this host");
        verify.add_css_class("suggested-action");
        verify.connect_clicked({
            let weak = self.weak.clone();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    remote.verify(collect());
                }
            }
        });
        actions.append(&verify);
        let cancel = button("cancel", "Cancel");
        cancel.connect_clicked(self.go(Page::List));
        actions.append(&cancel);
        body.append(&actions);
        body.append(&common::text(copy::NO_BROWSER_CODES));
        self.draft.borrow_mut().focus_fingerprint = false;
        let focus = if focus_fingerprint {
            fingerprint.upcast()
        } else {
            first_invalid.unwrap_or_else(|| label.upcast())
        };
        (body, focus)
    }

    fn import(&self) {
        let Some(window) = self.dialog() else { return };
        let chooser = gtk::FileChooserNative::new(
            Some("Import public host profile"),
            Some(&window),
            gtk::FileChooserAction::Open,
            Some("Import"),
            Some("Cancel"),
        );
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Public host profile (JSON)"));
        filter.add_pattern("*.json");
        chooser.add_filter(&filter);
        chooser.connect_response({
            let weak = self.weak.clone();
            move |chooser, response| {
                if response == gtk::ResponseType::Accept {
                    if let (Some(remote), Some(path)) =
                        (weak.upgrade(), chooser.file().and_then(|f| f.path()))
                    {
                        remote.import_path(&path);
                    }
                }
                chooser.destroy();
            }
        });
        chooser.show();
    }

    /// Fill the paste area from a public profile file. Only a parseable
    /// public profile is accepted; it is not trusted until verified, and the
    /// fingerprint field is never filled from it.
    pub fn import_path(&self, path: &std::path::Path) {
        let text = std::fs::metadata(path)
            .ok()
            .filter(|m| m.len() <= 128 * 1024)
            .and_then(|_| std::fs::read_to_string(path).ok())
            .filter(|text| profile::parse_public_profile(text.trim()).is_some());
        {
            let mut draft = self.draft.borrow_mut();
            match text {
                Some(text) => {
                    draft.input.profile = text.trim().to_owned();
                    draft.error = None;
                }
                None => {
                    draft.error = Some(FieldError {
                        field: Field::Profile,
                        message: "This file is not a public host profile. Use the output of crew native status.".into(),
                    });
                }
            }
        }
        self.show(Page::Profile, false);
    }

    pub fn verify(&self, input: ProfileInput) {
        let result = input.check();
        let mut draft = self.draft.borrow_mut();
        draft.input = input;
        match result {
            Ok(checked) => {
                draft.error = None;
                draft.checked = Some(checked);
                drop(draft);
                self.show(Page::Trust, true);
            }
            Err(error) => {
                draft.error = Some(error);
                drop(draft);
                self.show(Page::Profile, false);
            }
        }
    }

    fn trust_page(&self) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("Verify this host");
        body.append(&heading);
        let Some(checked) = self.draft.borrow().checked.clone() else {
            return self.list_page();
        };
        common::fact(&body, "Host name", &checked.public.name);
        common::fact(&body, "Host endpoint", &checked.public.url);
        common::fact(
            &body,
            "Endpoint ID",
            &profile::group_hex(&checked.public.endpoint_id.to_hex(), 4),
        );
        let profile_groups = profile::groups(&checked.public.tls_leaf_sha256.to_hex(), 4);
        let entered_groups = profile::groups(&checked.entered.to_hex(), 4);
        let grid = gtk::Grid::builder()
            .column_spacing(16)
            .row_spacing(4)
            .build();
        named(&grid, "fingerprint-comparison");
        let header = |text: &str, column| {
            let label = common::text(text);
            label.add_css_class("crew-muted");
            grid.attach(&label, column, 0, 1, 1);
        };
        header("Group", 0);
        header("Profile fingerprint", 1);
        header("You entered", 2);
        header("Result", 3);
        let mut differing = 0;
        for (index, (profile_group, entered_group)) in
            profile_groups.iter().zip(&entered_groups).enumerate()
        {
            let row = index as i32 + 1;
            let same = profile_group == entered_group;
            differing += usize::from(!same);
            let cell = |text: &str, column| {
                let label = gtk::Label::builder()
                    .label(text)
                    .xalign(0.0)
                    .selectable(true)
                    .build();
                label.add_css_class("monospace");
                grid.attach(&label, column, row, 1, 1);
                label
            };
            cell(&format!("{}", index + 1), 0);
            let spell = |group: &str| {
                group
                    .chars()
                    .map(String::from)
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            cell(profile_group, 1).update_property(&[gtk::accessible::Property::Label(&format!(
                "Group {}, profile: {}",
                index + 1,
                spell(profile_group)
            ))]);
            cell(entered_group, 2).update_property(&[gtk::accessible::Property::Label(&format!(
                "Group {}, entered: {}",
                index + 1,
                spell(entered_group)
            ))]);
            let result = cell(if same { "matches" } else { "differs" }, 3);
            result.update_property(&[gtk::accessible::Property::Label(&format!(
                "group {} {}",
                index + 1,
                if same { "matches" } else { "differs" }
            ))]);
            if !same {
                result.add_css_class("error");
            }
        }
        body.append(&scroll_horizontal(&grid));
        body.append(&common::text(copy::PERSISTENCE));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let focus: gtk::Widget = if differing == 0 {
            body.append(&common::text("The fingerprints match. Trusting checks the host's certificate and this pin again before any device proof."));
            let trust = button("trust", "Trust this host");
            trust.add_css_class("suggested-action");
            trust.connect_clicked({
                let weak = self.weak.clone();
                move |_| {
                    if let Some(remote) = weak.upgrade() {
                        remote.trust();
                    }
                }
            });
            actions.append(&trust);
            heading.upcast()
        } else {
            let message = status_label(&format!(
                "Host identity could not be verified. Compare the profile with crew native status at the trusted host console. {differing} of 16 groups differ."
            ));
            message.add_css_class("error");
            body.append(&message);
            let review = button("review", "Review host profile");
            review.connect_clicked({
                let weak = self.weak.clone();
                move |_| {
                    if let Some(remote) = weak.upgrade() {
                        remote.draft.borrow_mut().focus_fingerprint = true;
                        remote.back();
                    }
                }
            });
            actions.append(&review);
            message.upcast()
        };
        let cancel = button("cancel", "Cancel");
        cancel.connect_clicked(self.go(Page::List));
        actions.append(&cancel);
        body.append(&actions);
        (body, focus)
    }

    /// Trust this host: the connector checks the profile with the entered
    /// fingerprint. Its result is authoritative over the grouped comparison.
    fn trust(&self) {
        let mut draft = self.draft.borrow_mut();
        let Some(checked) = draft.checked.clone() else {
            return;
        };
        let id = match draft
            .trusted
            .as_ref()
            .map(|p| p.id.clone())
            .map_or_else(profile::new_profile_id, Ok)
        {
            Ok(id) => id,
            Err(message) => {
                draft.error = Some(FieldError {
                    field: Field::Label,
                    message: message.into(),
                });
                drop(draft);
                self.show(Page::Profile, false);
                return;
            }
        };
        let previous = draft.trusted.take();
        let candidate = RemoteProfile {
            id,
            label: checked.label,
            public: checked.public,
            entered_fingerprint: checked.entered,
            device_label: previous.as_ref().and_then(|p| p.device_label.clone()),
            storage: previous
                .as_ref()
                .map_or(KeyStorage::SecureStore, |p| p.storage),
            saved_access: None,
        };
        match candidate.trusted() {
            Ok(_) => {
                draft.trusted = Some(candidate);
                drop(draft);
                self.store_state.set(None);
                self.send(RemoteRequest::ProbeStore);
                self.show(Page::Device, true);
            }
            // An unusable profile returns to its field; a pin or certificate the
            // connector refuses gets the trust failure copy and no trust action.
            Err(crew_client_transport::gateway::GatewayError::InvalidProfile) => {
                draft.error = Some(FieldError {
                    field: Field::Profile,
                    message: "Check the host profile. Paste the output of crew native status from the host.".into(),
                });
                drop(draft);
                self.show(Page::Profile, true);
            }
            Err(failure) => {
                drop(draft);
                let reason = failure.reason();
                self.show(
                    Page::Problem {
                        profile: String::new(),
                        problem: copy::problem(&reason, Phase::Connect, KeyStorage::SecureStore),
                    },
                    true,
                );
            }
        }
    }

    // S03 -----------------------------------------------------------------

    fn device_page(&self) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("Name this device");
        body.append(&heading);
        let draft = self.draft.borrow();
        let label = gtk::Label::builder()
            .label(Field::DeviceName.title())
            .xalign(0.0)
            .build();
        let entry = gtk::Entry::builder()
            .text(&draft.device_name)
            .placeholder_text("Work laptop")
            .build();
        named(&entry, "field:DeviceName");
        entry.update_property(&[gtk::accessible::Property::Label(Field::DeviceName.title())]);
        label.set_mnemonic_widget(Some(&entry));
        body.append(&label);
        body.append(&entry);
        body.append(&common::text(copy::DEVICE_NAME_HELP));
        let mut focus: gtk::Widget = entry.clone().upcast();
        if let Some(error) = draft
            .error
            .as_ref()
            .filter(|e| e.field == Field::DeviceName)
        {
            let message = status_label(&error.message);
            message.add_css_class("error");
            body.append(&message);
            entry.update_state(&[gtk::accessible::State::Invalid(
                gtk::AccessibleInvalidState::True,
            )]);
            focus = message.upcast();
        }
        section(&body, "Device key storage");
        let state = self.store_state.get();
        body.append(&status_label(match state {
            None => "Checking secure storage…",
            Some(StoreState::Ready) => "Secure storage ready. The device key is kept by this desktop's keyring (Secret Service) and never shown or exported.",
            Some(StoreState::Locked) => "Secure storage is locked. Unlock this device's credential storage when your keyring asks, then continue.",
            Some(StoreState::Unavailable) => "Secure storage is unavailable on this desktop. Crew never writes device keys to ordinary files.",
        }));
        let memory = gtk::CheckButton::with_label(copy::MEMORY_ONLY);
        named(&memory, "memory");
        memory.set_active(draft.memory);
        let offer_memory = state == Some(StoreState::Unavailable)
            || draft
                .store_error
                .as_ref()
                .is_some_and(|r| r.code == "key_unavailable");
        if offer_memory {
            body.append(&memory);
            body.append(&common::text(copy::MEMORY_CONSEQUENCE));
        }
        if let Some(reason) = draft.store_error.as_ref() {
            let problem = copy::problem(reason, Phase::Connect, KeyStorage::SecureStore);
            let message = status_label(&problem.message);
            message.add_css_class("error");
            body.append(&message);
            focus = message.upcast();
        }
        let preparing = draft.preparing;
        drop(draft);
        let progress = status_label(if preparing {
            "Preparing the device key…"
        } else {
            ""
        });
        progress.set_visible(preparing);
        body.append(&progress);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let next = button("continue", "Continue");
        next.add_css_class("suggested-action");
        next.set_sensitive(!preparing && state.is_some());
        next.connect_clicked({
            let weak = self.weak.clone();
            let entry = entry.clone();
            let memory = memory.clone();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    remote.prepare(entry.text().to_string(), memory.is_active());
                }
            }
        });
        actions.append(&next);
        let cancel = button("cancel", "Cancel");
        cancel.connect_clicked(self.go(Page::List));
        actions.append(&cancel);
        body.append(&actions);
        (body, focus)
    }

    fn prepare(&self, name: String, memory: bool) {
        let mut draft = self.draft.borrow_mut();
        draft.device_name = name.clone();
        draft.memory = memory;
        draft.store_error = None;
        let device = match profile::valid_device_name(&name) {
            Ok(device) => device,
            Err(error) => {
                draft.error = Some(error);
                drop(draft);
                self.show(Page::Device, false);
                return;
            }
        };
        draft.error = None;
        let Some(profile) = draft.trusted.as_mut() else {
            drop(draft);
            self.show(Page::List, false);
            return;
        };
        // Without a usable store, only the explicit memory choice continues.
        if !memory && self.store_state.get() == Some(StoreState::Unavailable) {
            draft.store_error = Some(ClientReason {
                code: "key_unavailable".into(),
                message: String::new(),
            });
            drop(draft);
            self.show(Page::Device, false);
            return;
        }
        profile.device_label = Some(device);
        profile.storage = if memory {
            KeyStorage::Memory
        } else {
            KeyStorage::SecureStore
        };
        let request = RemoteRequest::Prepare {
            profile: profile.clone(),
        };
        draft.preparing = true;
        drop(draft);
        if !self.send(request) {
            self.draft.borrow_mut().preparing = false;
        }
        self.show(Page::Device, false);
    }

    // S04 / S05 -------------------------------------------------------------

    fn request_page(&self, id: &str) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("Request access");
        body.append(&heading);
        let Some(profile) = self.profile(id) else {
            return self.list_page();
        };
        common::fact(
            &body,
            "Host",
            &format!("{}\n{}", profile.public.name, profile.public.url),
        );
        common::fact(
            &body,
            "This device",
            profile.device_label.as_deref().unwrap_or("Unnamed device"),
        );
        body.append(&common::text(copy::REQUEST_ACCESS));
        body.append(&common::text("The host's enrollment window lasts at most 120 seconds. Each request is one attempt; nothing retries automatically."));
        let ceremony = self.ceremony.borrow();
        let running = ceremony.as_ref().is_some_and(|c| c.profile == id);
        let stage = ceremony
            .as_ref()
            .filter(|c| c.profile == id)
            .map_or("", |c| {
                if c.canceling {
                    "Closing the request…"
                } else {
                    c.stage
                }
            });
        drop(ceremony);
        let progress = status_label(stage);
        body.append(&progress);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let request = button("request", "Request access");
        request.add_css_class("suggested-action");
        request.set_sensitive(!running);
        request.connect_clicked({
            let weak = self.weak.clone();
            let id = id.to_owned();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    remote.request(&id);
                }
            }
        });
        actions.append(&request);
        let cancel = button("cancel", "Cancel");
        cancel.connect_clicked({
            let weak = self.weak.clone();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    if !remote.cancel_request() {
                        remote.show(Page::List, false);
                    }
                }
            }
        });
        actions.append(&cancel);
        body.append(&actions);
        let focus = if running {
            progress.upcast()
        } else {
            heading.upcast()
        };
        (body, focus)
    }

    fn request(&self, id: &str) {
        let Some(profile) = self.profile(id) else {
            return;
        };
        let Some(device_label) = profile.device_label.clone() else {
            return;
        };
        *self.ceremony.borrow_mut() = Some(Ceremony {
            profile: id.to_owned(),
            attempt: None,
            phase: Phase::Request,
            code: None,
            stage: "Connecting to the host",
            canceling: false,
        });
        if !self.send(RemoteRequest::Enroll {
            profile,
            device_label,
        }) {
            self.ceremony.borrow_mut().take();
        }
        self.show(Page::Request(id.to_owned()), false);
    }

    /// Cancel an in-flight request explicitly. Returns false if none runs.
    fn cancel_request(&self) -> bool {
        let mut ceremony = self.ceremony.borrow_mut();
        let Some(current) = ceremony.as_mut() else {
            return false;
        };
        current.canceling = true;
        let request = current
            .attempt
            .map(|attempt| RemoteRequest::CancelEnrollment {
                profile_id: current.profile.clone(),
                attempt,
            });
        let page = if current.code.is_some() {
            Page::Compare(current.profile.clone())
        } else {
            Page::Request(current.profile.clone())
        };
        drop(ceremony);
        if let Some(request) = request {
            self.send(request);
        }
        if self.dialog().is_some() && self.current().is_some_and(|p| p == page) {
            self.show(page, false);
        }
        true
    }

    fn compare_page(&self, id: &str) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("Compare at the host");
        body.append(&heading);
        let Some(profile) = self.profile(id) else {
            return self.list_page();
        };
        let ceremony = self.ceremony.borrow();
        let Some((code, expires)) = ceremony
            .as_ref()
            .filter(|c| c.profile == id)
            .and_then(|c| c.code.clone())
        else {
            drop(ceremony);
            return self.request_page(id);
        };
        let canceling = ceremony.as_ref().is_some_and(|c| c.canceling);
        drop(ceremony);
        common::fact(
            &body,
            "Host",
            &format!("{}\n{}", profile.public.name, profile.public.url),
        );
        common::fact(
            &body,
            "This device",
            profile.device_label.as_deref().unwrap_or("Unnamed device"),
        );
        let caption = common::text("Comparison code");
        caption.add_css_class("crew-title");
        body.append(&caption);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let groups = profile::groups(&code, 4);
        for (index, group) in groups.iter().enumerate() {
            let label = gtk::Label::builder().label(group).selectable(true).build();
            label.add_css_class("title-1");
            label.add_css_class("monospace");
            named(&label, &format!("code:{index}"));
            let spelled = group
                .chars()
                .map(String::from)
                .collect::<Vec<_>>()
                .join(" ");
            label.update_property(&[gtk::accessible::Property::Label(&format!(
                "Group {} of {}: {spelled}",
                index + 1,
                groups.len()
            ))]);
            row.append(&label);
        }
        body.append(&row);
        body.append(&common::text(copy::COMPARE_CODE));
        let waiting = status_label(&if canceling {
            "Closing the request…".to_owned()
        } else {
            format!(
                "Waiting for approval at the host console. This request expires at {}.",
                common::timestamp(expires)
            )
        });
        body.append(&waiting);
        body.append(&common::text("There is no approve button on this device: the host operator chooses each workspace and its powers at the trusted host console."));
        let cancel = button("cancel", "Cancel");
        cancel.connect_clicked({
            let weak = self.weak.clone();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    remote.cancel_request();
                }
            }
        });
        body.append(&cancel);
        (body, heading.upcast())
    }

    // S07 / S08 -------------------------------------------------------------

    fn workspaces_page(&self, id: &str, granted: bool) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let Some(profile) = self.profile(id) else {
            return self.list_page();
        };
        let heading = common::heading(if granted {
            "Access granted"
        } else {
            &profile.label
        });
        body.append(&heading);
        if granted {
            common::fact(
                &body,
                "This device",
                profile.device_label.as_deref().unwrap_or("Unnamed device"),
            );
            common::fact(
                &body,
                "Authenticated host",
                &format!("{}\n{}", profile.public.name, profile.public.url),
            );
            if let Some(access) = profile.saved_access.as_ref() {
                common::fact(
                    &body,
                    "Access expires",
                    &common::timestamp(access.expires_at_ms),
                );
            }
        }
        section(&body, "Workspaces");
        let active = self.active.borrow().as_deref() == Some(id);
        let host = self.host();
        let connection = host.as_ref().and_then(|h| h.connection());
        let listing = host
            .as_ref()
            .and_then(|h| h.listing())
            .filter(|_| active && self.endpoint.get() == Some(EndpointStatus::Authenticated));
        let status = match (&listing, self.endpoint.get()) {
            (Some(listing), _) if listing.joined.is_empty() => copy::NO_ACCESS.to_owned(),
            (Some(_), _) => {
                "Choose a workspace to open. Only workspaces this device may use are listed."
                    .to_owned()
            }
            (None, _) if !active => "Loading workspaces".to_owned(),
            (
                None,
                Some(
                    EndpointStatus::SignedOut
                    | EndpointStatus::Stopped
                    | EndpointStatus::Incompatible,
                ),
            ) => self.reasons.borrow().get(id).map_or_else(
                || "Device access is unavailable. Check with the host operator.".to_owned(),
                |reason| copy::problem(reason, Phase::Connect, profile.storage).message,
            ),
            (None, Some(EndpointStatus::Reconnecting)) => {
                "Reconnecting to the host. Workspaces appear when this device's access is confirmed again.".to_owned()
            }
            (None, _) => "Loading workspaces".to_owned(),
        };
        let status = status_label(&status);
        body.append(&status);
        if let Some(listing) = listing.as_ref() {
            self.workspace_buttons(&body, listing);
        }
        if let Some(connection) = connection
            .as_ref()
            .filter(|c| active && c.mode == ConnectionMode::ScopedGateway)
        {
            self.access_rows(&body, connection, listing.as_ref());
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        if active
            && matches!(
                self.endpoint.get(),
                Some(EndpointStatus::Reconnecting | EndpointStatus::Disconnected)
            )
        {
            let reconnect = button("reconnect", "Reconnect now");
            reconnect.add_css_class("suggested-action");
            reconnect.connect_clicked({
                let weak = self.weak.clone();
                move |_| {
                    if let Some(host) = weak.upgrade().and_then(|r| r.host()) {
                        host.send(Request::Reconnect);
                    }
                }
            });
            actions.append(&reconnect);
        }
        let details = button("details", "Connection details");
        details.connect_clicked(self.go(Page::Details(id.to_owned())));
        actions.append(&details);
        body.append(&actions);
        (body, heading.upcast())
    }

    fn access_rows(
        &self,
        body: &gtk::Box,
        connection: &ConnectionContext,
        listing: Option<&Workspaces>,
    ) {
        // Verified powers exist only for the open route; other authorized
        // workspaces are listed by name and availability.
        let name = listing
            .and_then(|l| {
                l.joined
                    .iter()
                    .find(|w| w.id == connection.scope.workspace_id)
            })
            .map_or(connection.scope.workspace_id.clone(), |w| {
                w.workspace.clone()
            });
        section(body, &format!("Access in {name}"));
        let tiers = &connection.grant.tiers;
        let row = common::text(&format!(
            "Read: {} · Control: {} · Terminal: {}",
            tier_text(tiers, GrantTier::Read),
            tier_text(tiers, GrantTier::Control),
            tier_text(tiers, GrantTier::Terminal),
        ));
        named(&row, "access:current");
        body.append(&row);
        if let Some(expires) = connection.grant.expires_at_ms {
            common::fact(body, "Access expires", &common::timestamp(expires));
        }
        let others = listing
            .map(|l| {
                l.joined
                    .iter()
                    .filter(|w| w.id != connection.scope.workspace_id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !others.is_empty() {
            section(body, "Other authorized workspaces");
            for other in others {
                body.append(&common::text(&format!(
                    "{} · {}",
                    other.workspace, other.state
                )));
            }
            body.append(&common::text(
                "Open a workspace to see the access this device has there.",
            ));
        }
    }

    fn details_page(&self, id: &str) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("Connection details");
        body.append(&heading);
        let Some(profile) = self.profile(id) else {
            return self.list_page();
        };
        let active = self.active.borrow().as_deref() == Some(id);
        common::fact(&body, "Connection name", &profile.label);
        common::fact(&body, "Host name", &profile.public.name);
        common::fact(&body, "Host endpoint", &profile.public.url);
        common::fact(
            &body,
            "Endpoint ID",
            &profile::group_hex(&profile.public.endpoint_id.to_hex(), 4),
        );
        common::fact(
            &body,
            "SHA-256 fingerprint",
            &profile::group_hex(&profile.public.tls_leaf_sha256.to_hex(), 4),
        );
        common::fact(
            &body,
            "Connection mode",
            &format!(
                "Scoped device access · {}",
                self.profile_state(&profile, active)
            ),
        );
        common::fact(
            &body,
            "This device",
            profile.device_label.as_deref().unwrap_or("Unnamed device"),
        );
        common::fact(
            &body,
            "Device key",
            match profile.storage {
                KeyStorage::SecureStore => "Desktop keyring (Secret Service)",
                KeyStorage::Memory => "Memory only, until Crew closes",
            },
        );
        if let Some(access) = profile.saved_access.as_ref() {
            common::fact(
                &body,
                "Device ID",
                &profile::group_hex(&access.device_id, 4),
            );
            common::fact(
                &body,
                "Saved access expiry",
                &common::timestamp(access.expires_at_ms),
            );
        }
        let host = self.host();
        if let Some(connection) = host
            .as_ref()
            .and_then(|h| h.connection())
            .filter(|c| active && c.mode == ConnectionMode::ScopedGateway)
        {
            self.access_rows(
                &body,
                &connection,
                host.as_ref().and_then(|h| h.listing()).as_ref(),
            );
        } else {
            section(&body, "This device's access");
            body.append(&common::text(
                "Connect to review the access the host currently grants this device.",
            ));
        }
        section(&body, "What each access allows");
        common::fact(&body, "Read", copy::READ);
        common::fact(&body, "Control", copy::CONTROL);
        common::fact(&body, "Terminal", copy::TERMINAL);
        body.append(&common::text(copy::NO_SANDBOX));
        body.append(&common::text("This device cannot grant, renew or revoke access, or list other devices. The host operator manages access at the trusted host console."));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        if active {
            // A bound profile reconnects with the same key and selection, for
            // example after the host restarts its native service.
            let reconnect = button("reconnect", "Reconnect");
            reconnect.add_css_class("suggested-action");
            reconnect.connect_clicked({
                let weak = self.weak.clone();
                move |_| {
                    if let Some(host) = weak.upgrade().and_then(|r| r.host()) {
                        host.send(Request::Reconnect);
                    }
                }
            });
            actions.append(&reconnect);
            if self.endpoint.get() != Some(EndpointStatus::Disconnected) {
                let disconnect = button("disconnect", "Disconnect");
                disconnect.connect_clicked({
                    let weak = self.weak.clone();
                    move |_| {
                        if let Some(host) = weak.upgrade().and_then(|r| r.host()) {
                            host.send(Request::Disconnect);
                        }
                    }
                });
                actions.append(&disconnect);
            }
        } else {
            let connect = button("connect", "Connect");
            connect.add_css_class("suggested-action");
            connect.connect_clicked({
                let weak = self.weak.clone();
                let id = id.to_owned();
                move |_| {
                    if let Some(remote) = weak.upgrade() {
                        remote.connect(&id);
                    }
                }
            });
            actions.append(&connect);
        }
        let revoke = button("revoke", "How to revoke access");
        revoke.connect_clicked(self.go(Page::Revoke(id.to_owned())));
        actions.append(&revoke);
        let remove = button("remove", "Remove from this app");
        remove.connect_clicked(self.go(Page::Remove(id.to_owned())));
        actions.append(&remove);
        let wrap = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(4)
            .build();
        while let Some(child) = actions.first_child() {
            actions.remove(&child);
            wrap.insert(&child, -1);
        }
        body.append(&wrap);
        body.append(&common::text(copy::DISCONNECT));
        (body, heading.upcast())
    }

    fn revoke_page(&self, id: &str) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("How to revoke access");
        body.append(&heading);
        let profile = self.profile(id);
        let device = profile
            .as_ref()
            .and_then(|p| p.saved_access.as_ref())
            .map(|a| a.device_id.clone());
        // The device ID is typed at the host console: keep it copyable.
        body.append(&common::selectable(&copy::revoke_instructions(
            device.as_deref(),
        )));
        body.append(&common::text("Revoking ends this device's channels. Commands the host already accepted may finish, and data this device already received is not erased."));
        (body, heading.upcast())
    }

    fn remove_page(&self, id: &str) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("Remove from this app");
        body.append(&heading);
        body.append(&common::text(copy::REMOVE));
        let device = self
            .profile(id)
            .and_then(|p| p.saved_access)
            .map(|a| a.device_id);
        // The device ID is typed at the host console: keep it copyable.
        body.append(&common::selectable(&copy::revoke_instructions(
            device.as_deref(),
        )));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let remove = button("confirm-remove", "Remove from this app");
        remove.connect_clicked({
            let weak = self.weak.clone();
            let id = id.to_owned();
            move |button| {
                let Some(remote) = weak.upgrade() else { return };
                if let Some(profile) = remote.profile(&id) {
                    button.set_sensitive(false);
                    remote.send(RemoteRequest::Remove { profile });
                }
            }
        });
        actions.append(&remove);
        let cancel = button("cancel", "Cancel");
        cancel.connect_clicked({
            let weak = self.weak.clone();
            move |_| {
                if let Some(remote) = weak.upgrade() {
                    remote.back();
                }
            }
        });
        actions.append(&cancel);
        body.append(&actions);
        (body, cancel.upcast())
    }

    fn problem_page(&self, id: &str, problem: &Problem) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading(problem.title);
        body.append(&heading);
        let message = status_label(&problem.message);
        body.append(&message);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        for recovery in &problem.actions {
            let control = button(&format!("recovery:{recovery:?}"), recovery.label());
            let page = match recovery {
                Recovery::TryAgain
                    if self.profile(id).is_some_and(|p| p.saved_access.is_none()) =>
                {
                    Some(Page::Request(id.to_owned()))
                }
                Recovery::TryAgain | Recovery::ReconnectToCheck => None,
                Recovery::EditProfile | Recovery::ReviewProfile => Some(Page::Profile),
                Recovery::ConnectionDetails => Some(Page::Details(id.to_owned())),
                Recovery::RemoveProfile => Some(Page::Remove(id.to_owned())),
            };
            let weak = self.weak.clone();
            let id = id.to_owned();
            let edit = matches!(recovery, Recovery::EditProfile | Recovery::ReviewProfile);
            control.connect_clicked(move |_| {
                let Some(remote) = weak.upgrade() else { return };
                if edit {
                    remote.edit(&id);
                }
                match page.clone() {
                    Some(page) => remote.show(page, true),
                    None => remote.connect(&id),
                }
            });
            actions.append(&control);
        }
        let back = button("connections", "Back to connections");
        back.connect_clicked(self.go(Page::List));
        actions.append(&back);
        body.append(&actions);
        (body, message.upcast())
    }

    /// Reopen a saved profile's public fields for review. A changed pin needs
    /// a new fingerprint check and never becomes "connect anyway".
    fn edit(&self, id: &str) {
        let Some(profile) = self.profile(id) else {
            self.draft.borrow_mut().focus_fingerprint = true;
            return;
        };
        let mut draft = self.draft.borrow_mut();
        *draft = Draft::default();
        // The fingerprint is entered again from the trusted host console.
        draft.input = ProfileInput {
            label: profile.label.clone(),
            profile: serde_json::to_string_pretty(&profile.public).unwrap_or_default(),
            fingerprint: String::new(),
        };
        draft.device_name = profile.device_label.clone().unwrap_or_default();
        draft.focus_fingerprint = true;
        draft.trusted = Some(profile);
    }

    fn access_ended_page(&self, id: &str) -> (gtk::Box, gtk::Widget) {
        let body = common::column();
        let heading = common::heading("Access changed");
        body.append(&heading);
        // A grant edit retires the old authority even though the workspace
        // stays listed; only a still-authorized workspace is named again.
        let lost = self.lost_workspace.borrow().clone();
        let host = self.host();
        let still_listed = lost.as_ref().and_then(|workspace| {
            host.as_ref()
                .and_then(|h| h.listing())
                .filter(|_| self.endpoint.get() == Some(EndpointStatus::Authenticated))
                .and_then(|listing| listing.joined.into_iter().find(|w| &w.id == workspace))
        });
        let message = status_label(&match still_listed.as_ref() {
            Some(entry) => format!(
                "Access to {} changed at the host. Open it again to use its current access.",
                entry.workspace
            ),
            None => copy::ACCESS_ENDED.to_owned(),
        });
        body.append(&message);
        if let Some(reason) = self.reasons.borrow().get(id) {
            if let Some(profile) = self.profile(id) {
                body.append(&common::text(
                    &copy::problem(reason, Phase::Live, profile.storage).message,
                ));
            }
        }
        if let Some(entry) = still_listed {
            let reopen = button("reopen", "Open workspace again");
            reopen.connect_clicked({
                let weak = self.weak.clone();
                move |_| {
                    let Some(remote) = weak.upgrade() else { return };
                    if let Some(host) = remote.host() {
                        host.send(Request::SelectWorkspace(entry.id.clone()));
                    }
                    if let Some(dialog) = remote.dialog() {
                        dialog.close();
                    }
                }
            });
            body.append(&reopen);
        }
        let back = button("back-to-workspaces", "Back to workspaces");
        back.connect_clicked(self.go(Page::Workspaces {
            profile: id.to_owned(),
            granted: false,
        }));
        body.append(&back);
        let connections = button("connections", "Connections");
        connections.connect_clicked(self.go(Page::List));
        body.append(&connections);
        (body, message.upcast())
    }

    // Service results -------------------------------------------------------

    pub fn respond(&self, response: RemoteResponse) {
        match response {
            RemoteResponse::Store(state) => {
                self.store_state.set(Some(state));
                if self.current() == Some(Page::Device) && !self.draft.borrow().preparing {
                    self.show(Page::Device, false);
                }
            }
            RemoteResponse::Prepared { profile_id, result } => {
                let mut draft = self.draft.borrow_mut();
                draft.preparing = false;
                let Some(profile) = draft.trusted.clone().filter(|p| p.id == profile_id) else {
                    return;
                };
                match result {
                    Ok(_) => {
                        drop(draft);
                        self.upsert(profile);
                        self.show(Page::Request(profile_id), true);
                    }
                    Err(reason) => {
                        draft.store_error = Some(reason);
                        drop(draft);
                        self.show(Page::Device, false);
                    }
                }
            }
            RemoteResponse::Setup(update) => {
                let mut ceremony = self.ceremony.borrow_mut();
                let Some(current) = ceremony
                    .as_mut()
                    .filter(|c| c.profile == update.context.profile_id)
                else {
                    return;
                };
                if current
                    .attempt
                    .is_some_and(|attempt| attempt != update.context.attempt)
                {
                    return;
                }
                current.attempt = Some(update.context.attempt);
                let profile = current.profile.clone();
                let page = match update.progress {
                    EnrollmentProgress::Connecting => {
                        current.stage = "Connecting to the host and verifying its identity";
                        Page::Request(profile)
                    }
                    EnrollmentProgress::AwaitingApproval {
                        comparison_code,
                        expires_at_ms,
                        ..
                    } => {
                        current.phase = Phase::Approval;
                        current.code = Some((comparison_code, expires_at_ms));
                        Page::Compare(profile)
                    }
                };
                drop(ceremony);
                if self.dialog().is_some() {
                    self.show(page, false);
                }
            }
            RemoteResponse::Enrolled { profile_id, result } => {
                let ceremony = self
                    .ceremony
                    .borrow_mut()
                    .take_if(|c| c.profile == profile_id);
                let phase = ceremony.as_ref().map_or(Phase::Request, |c| c.phase);
                let Some(mut profile) = self.profile(&profile_id) else {
                    return;
                };
                let page = match result.map(|r| r.outcome) {
                    Ok(EnrollmentOutcome::Approved { access }) => {
                        profile.saved_access = Some(SavedAccess {
                            device_id: access.device_id,
                            expires_at_ms: access.credential_expires_at_ms,
                        });
                        self.upsert(profile.clone());
                        self.send(RemoteRequest::Connect {
                            profile: profile.clone(),
                        });
                        Page::Workspaces {
                            profile: profile_id,
                            granted: true,
                        }
                    }
                    Ok(EnrollmentOutcome::Canceled) => {
                        *self.list_status.borrow_mut() = Some(copy::REQUEST_CLOSED.to_owned());
                        Page::List
                    }
                    Ok(EnrollmentOutcome::OutcomeUnknown) => Page::Problem {
                        profile: profile_id,
                        problem: Problem {
                            title: "Approval not confirmed",
                            message: copy::LOST_REPLY.to_owned(),
                            actions: vec![Recovery::ReconnectToCheck],
                        },
                    },
                    Ok(EnrollmentOutcome::Failed { reason }) | Err(reason) => Page::Problem {
                        problem: copy::problem(&reason, phase, profile.storage),
                        profile: profile_id,
                    },
                };
                if self.dialog().is_some() {
                    self.show(page, true);
                }
            }
            RemoteResponse::Active(profile) => {
                *self.active.borrow_mut() = profile;
                if self.active.borrow().is_none() {
                    self.endpoint.set(None);
                    self.workspace.set(None);
                }
                self.refresh();
            }
            RemoteResponse::ConnectFailed { profile_id, reason } => {
                let storage = self
                    .profile(&profile_id)
                    .map_or(KeyStorage::SecureStore, |p| p.storage);
                let problem = copy::problem(&reason, Phase::Connect, storage);
                self.reasons.borrow_mut().insert(profile_id.clone(), reason);
                if self.dialog().is_some() {
                    self.show(
                        Page::Problem {
                            profile: profile_id,
                            problem,
                        },
                        true,
                    );
                }
            }
            RemoteResponse::Removed { profile_id, result } => match result {
                Ok(()) => {
                    self.profiles.borrow_mut().retain(|p| p.id != profile_id);
                    self.reasons.borrow_mut().remove(&profile_id);
                    self.save();
                    *self.list_status.borrow_mut() = Some(
                        "Profile removed and this app's device key retired. Host access remains until revoked or expired."
                            .into(),
                    );
                    if self.dialog().is_some() {
                        self.show(Page::List, false);
                    }
                }
                Err(reason) => {
                    let storage = self
                        .profile(&profile_id)
                        .map_or(KeyStorage::SecureStore, |p| p.storage);
                    if self.dialog().is_some() {
                        self.show(
                            Page::Problem {
                                problem: copy::problem(&reason, Phase::Connect, storage),
                                profile: profile_id,
                            },
                            true,
                        );
                    }
                }
            },
        }
    }

    /// A context reply arrived, current or stale; another refresh may start.
    pub fn context_settled(&self) {
        self.context_pending.set(false);
        if self.context_again.replace(false) {
            self.refresh_context();
        }
    }

    /// Ask for the bound session's context and authorized listing. At most
    /// one request is outstanding; a later notice asks once more after it.
    fn refresh_context(&self) {
        if self.context_pending.get() {
            self.context_again.set(true);
        } else if let Some(host) = self.host() {
            self.context_pending.set(host.send(Request::Context));
        }
    }

    /// Connection context or listing changed; live pages follow it in place.
    pub fn changed(&self) {
        self.refresh();
    }

    /// A shared-core notice for the bound session. Remote access loss shows a
    /// safe explanation and focuses it; other workspaces stay reachable.
    pub fn notice(&self, notice: &ClientNotice, connection: Option<&ConnectionContext>) {
        let Some(active) = self.active.borrow().clone() else {
            return;
        };
        // A gateway index has no workspace scope, so its authorized listing
        // is refreshed explicitly. At most one refresh is outstanding.
        self.refresh_context();
        let previous = self.workspace.replace(Some(notice.workspace));
        if let Some(scope) = notice.scope.as_ref() {
            *self.lost_workspace.borrow_mut() = Some(scope.workspace_id.clone());
        }
        let previous_endpoint = self.endpoint.replace(Some(notice.endpoint));
        if let Some(reason) = notice.reason.as_ref() {
            self.reasons
                .borrow_mut()
                .insert(active.clone(), reason.clone());
        } else if notice.endpoint == EndpointStatus::Authenticated {
            self.reasons.borrow_mut().remove(&active);
        }
        let lost = matches!(
            notice.workspace,
            WorkspaceStatus::AccessRevoked | WorkspaceStatus::Removed
        ) || (notice.endpoint == EndpointStatus::SignedOut
            && previous.is_some_and(|w| w != WorkspaceStatus::NoneSelected));
        let remote = connection.is_none_or(|c| c.mode == ConnectionMode::ScopedGateway);
        // A locked or unavailable key store is a local condition, not lost
        // access: offer unlock and try again instead.
        let key = notice
            .reason
            .as_ref()
            .filter(|r| notice.endpoint == EndpointStatus::SignedOut && r.code.starts_with("key_"));
        if let (Some(reason), true) = (key, previous_endpoint != Some(EndpointStatus::SignedOut)) {
            let storage = self
                .profile(&active)
                .map_or(KeyStorage::SecureStore, |p| p.storage);
            if self.dialog().is_none() {
                self.open();
            }
            self.show(
                Page::Problem {
                    problem: copy::problem(reason, Phase::Connect, storage),
                    profile: active,
                },
                true,
            );
        } else if key.is_none() && lost && remote && previous != Some(notice.workspace) {
            if self.dialog().is_none() {
                self.open();
            }
            self.show(Page::AccessEnded(active), true);
        } else {
            self.refresh();
        }
    }
}
