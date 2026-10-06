//! GTK presentation/controller. Shared Rust owns data, action eligibility,
//! confirmations, resources, retry and transport; GTK owns local UI state.
use crate::{
    availability::{Controls, Input as ControlInput, Intent},
    dialogs::{self, Form, RecipientPicker},
    navigation::Destination,
    service::{ClientService, Request, Response, ServiceHandle},
    shell::Shell,
    terminal::TerminalView,
    views::{
        self,
        agent::{AgentCallbacks, AgentInspector},
        common,
        events::EventsView,
        evidence::EvidenceView,
        messages::MessagesView,
        tasks::TasksView,
        team::TeamView,
        usage::UsageView,
    },
};
use crew_client::{api::*, policy};
use crew_client_transport::local::LocalWorkspace;
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::{Rc, Weak},
};

#[derive(Clone, PartialEq)]
enum QueryTarget {
    Main(Destination),
    Overview,
    Questions,
    Detail(EntityKey),
    Evidence(ResourceKey),
    Models(EntityKey),
    Availability {
        control: u64,
        intent: Box<ActionIntent>,
    },
    RestoreReceipt {
        form: u64,
        intent: Box<ActionIntent>,
    },
    /// Agents for the To selector of the compose form with this token.
    Recipients(u64),
}
#[derive(Clone)]
struct PendingAction {
    intent: ActionIntent,
    form: Option<u64>,
    terminal: Option<TerminalView>,
    canceled: bool,
    /// The control that had focus when the action was invoked: a
    /// confirmation returns focus there once it closes.
    opener: glib::WeakRef<gtk::Widget>,
}
struct OpenForm {
    form: Form,
    scope: ClientScope,
    target: String,
    submitted: Option<ActionIntent>,
    recipients: Option<RecipientPicker>,
}
/// Unsent text belongs to its endpoint, workspace and purpose, never to one
/// connection attempt: an ordinary reconnect keeps it, while a submission
/// still plans with the current verified scope.
#[derive(Clone, PartialEq, Eq)]
struct DraftKey {
    endpoint_id: String,
    workspace_id: String,
    purpose: String,
}
impl DraftKey {
    fn new(scope: &ClientScope, purpose: &str) -> Self {
        Self {
            endpoint_id: scope.endpoint_id.clone(),
            workspace_id: scope.workspace_id.clone(),
            purpose: purpose.into(),
        }
    }
}
/// A deduplication ID names one exact (recipient, body) pair. An unchanged
/// retry keeps its ID; an edited body or recipient takes a fresh one on
/// explicit submission, because the message bus returns the original message
/// as a duplicate without delivering the edit.
struct MessageIdentity {
    bound: Option<(String, String, String)>,
    fresh: String,
}
impl MessageIdentity {
    fn new(bound: Option<(String, String, String)>) -> Self {
        Self {
            bound,
            fresh: glib::uuid_string_random().to_string(),
        }
    }
    fn peek(&self, to: &str, body: &str) -> String {
        match &self.bound {
            Some((t, b, id)) if t == to && b == body => id.clone(),
            _ => self.fresh.clone(),
        }
    }
    fn submit(&mut self, to: &str, body: &str) -> String {
        let id = self.peek(to, body);
        if id == self.fresh {
            self.bound = Some((to.into(), body.into(), id.clone()));
            self.fresh = glib::uuid_string_random().to_string();
        }
        id
    }
}
struct Confirmation {
    id: u64,
    scope: ClientScope,
    dialog: gtk::Dialog,
}

pub struct Controller {
    pub shell: Shell,
    service: RefCell<Option<Rc<dyn ClientService>>>,
    weak: Weak<Self>,
    scope: RefCell<Option<ClientScope>>,
    instance: Cell<u64>,
    dropped_terminal_chunks: Cell<u64>,
    connection: RefCell<Option<ConnectionContext>>,
    listing: RefCell<Option<crew_protocol::rpc::Workspaces>>,
    profiles: RefCell<Vec<LocalWorkspace>>,
    profile_error: RefCell<Option<ClientReason>>,
    live: Cell<bool>,
    selected: RefCell<[Option<EntityKey>; 5]>,
    agent: RefCell<Option<(EntityKey, AgentInspector)>>,
    task_inspector: RefCell<Option<(EntityKey, views::task_detail::TaskInspector)>>,
    evidence: RefCell<Vec<(Destination, ResourceKey, EvidenceView)>>,
    team: TeamView,
    tasks: TasksView,
    messages: MessagesView,
    events: EventsView,
    usage: UsageView,
    status: gtk::Label,
    result: gtk::Label,
    overview: gtk::Label,
    questions_button: gtk::Button,
    workspace_label: gtk::Label,
    next_token: Cell<u64>,
    queries: RefCell<BTreeMap<u64, QueryTarget>>,
    plans: RefCell<BTreeMap<u64, PendingAction>>,
    operations: RefCell<BTreeMap<u64, PendingAction>>,
    early_outcomes: RefCell<Vec<OperationNotice>>,
    attachments: RefCell<Vec<(ClientScope, AttachmentId, TerminalView)>>,
    confirmation: RefCell<Option<Confirmation>>,
    focus_return: RefCell<Option<(glib::WeakRef<gtk::Widget>, glib::SignalHandlerId)>>,
    forms: RefCell<BTreeMap<u64, OpenForm>>,
    drafts: RefCell<Vec<(DraftKey, String)>>,
    questions: RefCell<Vec<crew_protocol::Message>>,
    message_filter: RefCell<MessageFilter>,
    event_filter: RefCell<EventFilter>,
    message_page: RefCell<Option<MessagePage>>,
    event_page: RefCell<Option<EventPage>>,
    message_bound: Cell<u32>,
    event_bound: Cell<u32>,
    message_before: RefCell<Option<HistoryCursor>>,
    event_before: RefCell<Option<HistoryCursor>>,
    model_loading: RefCell<Option<EntityKey>>,
    auxiliary: RefCell<Vec<gtk::Dialog>>,
    pages: RefCell<Vec<(Destination, gtk::Box)>>,
    controls: Controls,
}

impl Controller {
    pub fn start(shell: Shell) -> Rc<Self> {
        let controller = Self::construct(shell);
        let weak = Rc::downgrade(&controller);
        let service = ServiceHandle::start(move |response| {
            if let Some(controller) = weak.upgrade() {
                controller.respond(response);
            }
        });
        *controller.service.borrow_mut() = Some(Rc::new(service));
        controller
    }
    /// Deterministic construction uses the production request/result interface.
    pub fn with_service(shell: Shell, service: Rc<dyn ClientService>) -> Rc<Self> {
        let controller = Self::construct(shell);
        *controller.service.borrow_mut() = Some(service);
        controller
    }
    fn construct(shell: Shell) -> Rc<Self> {
        let controller = Rc::new_cyclic(|weak: &Weak<Self>| {
            let select = Rc::new({
                let weak = weak.clone();
                move |key| {
                    if let Some(c) = weak.upgrade() {
                        c.select(key);
                    }
                }
            });
            let team = TeamView::new(
                select.clone(),
                Rc::new({
                    let weak = weak.clone();
                    move |scope, first, second| {
                        if let Some(c) = weak.upgrade() {
                            if c.scope.borrow().as_ref() == Some(&scope) {
                                c.messages.set_pair(&first, &second);
                                c.shell.navigate(Destination::Messages);
                            }
                        }
                    }
                }),
            );
            let tasks = TasksView::new(select.clone());
            let messages = MessagesView::new(
                select.clone(),
                Rc::new({
                    let weak = weak.clone();
                    move |filter| {
                        if let Some(c) = weak.upgrade() {
                            *c.message_filter.borrow_mut() = filter;
                            c.message_before.borrow_mut().take();
                            c.refresh(Destination::Messages);
                        }
                    }
                }),
                Rc::new({
                    let weak = weak.clone();
                    move || {
                        if let Some(c) = weak.upgrade() {
                            c.older(true);
                        }
                    }
                }),
            );
            let events = EventsView::new(
                select,
                Rc::new({
                    let weak = weak.clone();
                    move |filter| {
                        if let Some(c) = weak.upgrade() {
                            *c.event_filter.borrow_mut() = filter;
                            c.event_before.borrow_mut().take();
                            c.refresh(Destination::Events);
                        }
                    }
                }),
                Rc::new({
                    let weak = weak.clone();
                    move || {
                        if let Some(c) = weak.upgrade() {
                            c.older(false);
                        }
                    }
                }),
            );
            let status = common::text("Not connected");
            let result = common::text("");
            status.set_max_width_chars(22);
            result.set_max_width_chars(22);
            let overview = common::text("");
            let questions_button = gtk::Button::with_label("Questions (0)");
            let workspace_label = gtk::Label::builder()
                .label("Choose workspace")
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(24)
                .build();
            Self {
                shell: shell.clone(),
                service: RefCell::new(None),
                weak: weak.clone(),
                scope: RefCell::new(None),
                instance: Cell::new(0),
                dropped_terminal_chunks: Cell::new(0),
                connection: RefCell::new(None),
                listing: RefCell::new(None),
                profiles: RefCell::new(Vec::new()),
                profile_error: RefCell::new(None),
                live: Cell::new(false),
                selected: RefCell::new(std::array::from_fn(|_| None)),
                agent: RefCell::new(None),
                task_inspector: RefCell::new(None),
                evidence: RefCell::new(Vec::new()),
                team,
                tasks,
                messages,
                events,
                usage: UsageView::new(),
                status,
                result,
                overview,
                questions_button,
                workspace_label,
                next_token: Cell::new(1),
                queries: RefCell::new(BTreeMap::new()),
                plans: RefCell::new(BTreeMap::new()),
                operations: RefCell::new(BTreeMap::new()),
                early_outcomes: RefCell::new(Vec::new()),
                attachments: RefCell::new(Vec::new()),
                confirmation: RefCell::new(None),
                focus_return: RefCell::new(None),
                forms: RefCell::new(BTreeMap::new()),
                drafts: RefCell::new(Vec::new()),
                questions: RefCell::new(Vec::new()),
                message_filter: RefCell::new(MessageFilter {
                    endpoints: MessageEndpoints::All,
                    kind: None,
                    text: String::new(),
                }),
                event_filter: RefCell::new(EventFilter {
                    agent_id: None,
                    level: None,
                    text: String::new(),
                    vcs_only: false,
                }),
                message_page: RefCell::new(None),
                event_page: RefCell::new(None),
                message_bound: Cell::new(300),
                event_bound: Cell::new(1000),
                message_before: RefCell::new(None),
                event_before: RefCell::new(None),
                model_loading: RefCell::new(None),
                auxiliary: RefCell::new(Vec::new()),
                pages: RefCell::new(Vec::new()),
                controls: Controls::default(),
            }
        });
        let status_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        status_box.append(&controller.status);
        status_box.append(&controller.result);
        controller.shell.set_status_widget(&status_box);
        let connections = gtk::Button::builder()
            .child(&controller.workspace_label)
            .build();
        connections.update_property(&[gtk::accessible::Property::Label(
            "Choose workspace or view connection details",
        )]);
        connections.connect_clicked({
            let weak = controller.weak.clone();
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.connections();
                }
            }
        });
        controller.shell.set_workspace_selector(&connections);
        // Empty disconnected pages offer the same Connections entry point.
        let choose = gtk::gio::SimpleAction::new("connections", None);
        choose.connect_activate({
            let weak = controller.weak.clone();
            move |_, _| {
                if let Some(c) = weak.upgrade() {
                    c.connections();
                }
            }
        });
        controller.shell.window().add_action(&choose);
        for (destination, widget) in [
            (
                Destination::Team,
                controller.team.root.clone().upcast::<gtk::Widget>(),
            ),
            (Destination::Tasks, controller.tasks.root.clone().upcast()),
            (
                Destination::Messages,
                controller.messages.root.clone().upcast(),
            ),
            (Destination::Events, controller.events.root.clone().upcast()),
            (Destination::Usage, controller.usage.root.clone().upcast()),
        ] {
            let page = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let controls = gtk::FlowBox::new();
            controls.set_selection_mode(gtk::SelectionMode::None);
            controls.set_min_children_per_line(1);
            controls.set_max_children_per_line(4);
            let refresh = gtk::Button::with_label("Refresh");
            refresh.connect_clicked({
                let weak = controller.weak.clone();
                move |_| {
                    if let Some(c) = weak.upgrade() {
                        c.refresh(destination);
                    }
                }
            });
            views::add_control(&controls, &refresh);
            if destination == Destination::Messages || destination == Destination::Team {
                let compose = gtk::Button::with_label("New message");
                compose.connect_clicked({
                    let weak = controller.weak.clone();
                    move |_| {
                        if let Some(c) = weak.upgrade() {
                            c.compose("*".into());
                        }
                    }
                });
                views::add_control(&controls, &compose);
                if destination == Destination::Messages {
                    let receipts = gtk::Button::with_label("Message receipts");
                    receipts.connect_clicked({
                        let weak = controller.weak.clone();
                        move |_| {
                            if let Some(c) = weak.upgrade() {
                                c.receipts();
                            }
                        }
                    });
                    views::add_control(&controls, &receipts);
                }
            }
            if destination == Destination::Team {
                views::add_control(&controls, &controller.questions_button);
                page.append(&controller.overview);
                let stop = gtk::Button::with_label("Stop team");
                stop.connect_clicked({
                    let weak = controller.weak.clone();
                    move |_| {
                        if let Some(c) = weak.upgrade() {
                            if let Some(scope) = c.scope.borrow().clone() {
                                c.plan(ActionIntent::StopTeam { scope }, None, None);
                            }
                        }
                    }
                });
                let reason = common::text("");
                reason.set_max_width_chars(26);
                let control = controller.token();
                controller.bind_control(
                    control,
                    &stop,
                    &reason,
                    Rc::new({
                        let weak = controller.weak.clone();
                        move || {
                            weak.upgrade()
                                .and_then(|c| c.scope.borrow().clone())
                                .map_or(
                                    ControlInput::Invalid("Choose a workspace.".into()),
                                    |scope| {
                                        ControlInput::Action(Box::new(ActionIntent::StopTeam {
                                            scope,
                                        }))
                                    },
                                )
                        }
                    }),
                    Rc::new(Cell::new(false)),
                );
                let group = gtk::Box::new(gtk::Orientation::Vertical, 4);
                group.append(&stop);
                group.append(&reason);
                views::add_control(&controls, &group);
            }
            if destination == Destination::Usage {
                for (name, restart) in [
                    ("Continue after limit", false),
                    ("Restart after limit", true),
                ] {
                    let button = gtk::Button::with_label(name);
                    button.connect_clicked({
                        let weak = controller.weak.clone();
                        move |_| {
                            if let Some(c) = weak.upgrade() {
                                if let Some(scope) = c.scope.borrow().clone() {
                                    c.plan(
                                        ActionIntent::ResumeLimited { scope, restart },
                                        None,
                                        None,
                                    );
                                }
                            }
                        }
                    });
                    let reason = common::text("");
                    reason.set_max_width_chars(26);
                    let control = controller.token();
                    controller.bind_control(
                        control,
                        &button,
                        &reason,
                        Rc::new({
                            let weak = controller.weak.clone();
                            move || {
                                weak.upgrade()
                                    .and_then(|c| c.scope.borrow().clone())
                                    .map_or(
                                        ControlInput::Invalid("Choose a workspace.".into()),
                                        |scope| {
                                            ControlInput::Action(Box::new(
                                                ActionIntent::ResumeLimited { scope, restart },
                                            ))
                                        },
                                    )
                            }
                        }),
                        Rc::new(Cell::new(false)),
                    );
                    let group = gtk::Box::new(gtk::Orientation::Vertical, 4);
                    group.append(&button);
                    group.append(&reason);
                    views::add_control(&controls, &group);
                }
            }
            page.append(&controls);
            page.append(&widget);
            controller.shell.set_main(destination, &page);
            controller.pages.borrow_mut().push((destination, page));
        }
        controller.questions_button.connect_clicked({
            let weak = controller.weak.clone();
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.question(0);
                }
            }
        });
        controller.shell.connect_destination_changed({
            let weak = controller.weak.clone();
            move |destination| {
                if let Some(c) = weak.upgrade() {
                    c.refresh(destination);
                }
            }
        });
        controller.shell.window().connect_close_request({
            let weak = controller.weak.clone();
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.invalidate(true);
                    c.send(Request::Close);
                    c.service.borrow_mut().take();
                }
                glib::Propagation::Proceed
            }
        });
        controller
    }
    fn token(&self) -> u64 {
        let token = self.next_token.get();
        self.next_token
            .set(token.checked_add(1).expect("native UI token range"));
        token
    }
    fn send(&self, request: Request) -> bool {
        let result = self.service.borrow().as_ref().map_or_else(
            || {
                Err(ClientReason {
                    code: "closed".into(),
                    message: "The client service is closed.".into(),
                })
            },
            |service| service.submit(request),
        );
        if let Err(reason) = result {
            self.result.set_label(&reason.message);
            false
        } else {
            true
        }
    }
    fn query(&self, query: ViewQuery, target: QueryTarget) {
        let token = self.token();
        if self.send(Request::Query {
            token,
            expected_scope: self.scope.borrow().clone(),
            query,
        }) {
            self.queries
                .borrow_mut()
                .retain(|_, older| match (&*older, &target) {
                    (
                        QueryTarget::Availability {
                            control: previous, ..
                        },
                        QueryTarget::Availability {
                            control: current, ..
                        },
                    ) => previous != current,
                    _ => older != &target,
                });
            self.queries.borrow_mut().insert(token, target);
        }
    }
    pub fn refresh(&self, destination: Destination) {
        let query = match destination {
            Destination::Team => ViewQuery::Team,
            Destination::Tasks => ViewQuery::Tasks,
            Destination::Usage => ViewQuery::Usage,
            Destination::Messages => ViewQuery::Messages {
                before: self.message_before.borrow().clone(),
                limit: policy::NATIVE_LIMITS.page_size,
                filter: self.message_filter.borrow().clone(),
            },
            Destination::Events => ViewQuery::Events {
                before: self.event_before.borrow().clone(),
                limit: policy::NATIVE_LIMITS.page_size,
                filter: self.event_filter.borrow().clone(),
            },
        };
        self.query(query, QueryTarget::Main(destination));
    }
    pub fn respond(&self, response: Response) {
        match response {
            Response::Session(instance) => {
                if instance != self.instance.replace(instance) {
                    self.invalidate(true);
                    self.scope.borrow_mut().take();
                    self.workspace_label.set_label("Choose workspace");
                    self.workspace_label.set_tooltip_text(None);
                    self.connection.borrow_mut().take();
                    self.listing.borrow_mut().take();
                }
            }
            Response::Profiles(result) => match result {
                Ok(profiles) => {
                    *self.profiles.borrow_mut() = profiles;
                    self.profile_error.borrow_mut().take();
                }
                Err(reason) => {
                    self.result.set_label(&reason.message);
                    *self.profile_error.borrow_mut() = Some(reason);
                }
            },
            Response::Context {
                instance,
                connection,
                workspaces,
            } => {
                if instance != self.instance.get() {
                    return;
                }
                if connection
                    .as_ref()
                    .is_some_and(|context| self.scope.borrow().as_ref() != Some(&context.scope))
                {
                    return;
                }
                *self.connection.borrow_mut() = connection;
                *self.listing.borrow_mut() = workspaces;
            }
            Response::Error(error) => self.result.set_label(&error.to_string()),
            Response::Notice { instance, notice } => {
                if instance == self.instance.get() {
                    self.notice(notice);
                }
            }
            Response::View {
                instance,
                token,
                expected_scope,
                result,
            } => {
                if instance != self.instance.get() {
                    return;
                }
                let Some(target) = self.queries.borrow_mut().remove(&token) else {
                    return;
                };
                if expected_scope != *self.scope.borrow() {
                    return;
                }
                match result {
                    Ok(data) if data.scope == *self.scope.borrow() => self.present(target, data),
                    Ok(_) => (),
                    Err(error) => {
                        if let QueryTarget::Availability { control, intent } = target {
                            self.controls.apply(control, &intent, Err(error));
                        } else {
                            self.result.set_label(&error.to_string());
                        }
                    }
                }
            }
            Response::Action {
                instance,
                token,
                scope,
                result,
            } => {
                if instance == self.instance.get() {
                    self.planned(token, scope, result);
                }
            }
            Response::Terminal { instance, chunk } => {
                if instance == self.instance.get() {
                    self.terminal_chunk(chunk);
                }
            }
        }
    }
    fn notice(&self, notice: ClientNotice) {
        let revoked = notice.workspace == WorkspaceStatus::AccessRevoked
            || notice.workspace == WorkspaceStatus::Removed
            || notice.endpoint == EndpointStatus::SignedOut;
        if notice.scope != *self.scope.borrow() || revoked {
            self.invalidate(revoked);
            let visible_scope = if revoked { None } else { notice.scope.clone() };
            *self.scope.borrow_mut() = visible_scope.clone();
            self.workspace_label.set_label(
                visible_scope
                    .as_ref()
                    .map_or("Choose workspace", |scope| scope.workspace_id.as_str()),
            );
            self.workspace_label
                .update_property(&[gtk::accessible::Property::Label(
                    visible_scope
                        .as_ref()
                        .map_or("No workspace selected", |scope| scope.workspace_id.as_str()),
                )]);
            self.message_before.borrow_mut().take();
            self.event_before.borrow_mut().take();
            self.query(ViewQuery::Overview, QueryTarget::Overview);
            self.query(ViewQuery::Questions, QueryTarget::Questions);
            self.send(Request::Context);
            if notice.scope.is_some() && !revoked {
                for (destination, page) in self.pages.borrow().iter() {
                    self.shell.set_main(*destination, page);
                }
                self.refresh(self.shell.destination());
            }
        }
        self.live.set(
            notice.endpoint == EndpointStatus::Authenticated
                && notice.workspace == WorkspaceStatus::Live
                && notice.scope.is_some(),
        );
        if !self.live.get() {
            self.cancel_confirmation();
        }
        let status = crate::lifecycle::status(
            notice.endpoint,
            notice.workspace,
            notice.reason.as_ref().map(|reason| reason.message.as_str()),
        );
        self.status.set_label(&status);
        if let Some(selector) = self.workspace_label.parent() {
            selector.set_tooltip_text(Some(&status));
        }
        if let Some((_, agent)) = self.agent.borrow().as_ref() {
            agent.set_live(self.live.get());
        }
        for outcome in notice.outcomes {
            self.outcome(outcome);
        }
        let destination = self.shell.destination();
        let changes = &notice.changes;
        let changed = changes.reset
            || match destination {
                Destination::Team => {
                    !changes.agents.is_empty() || !changes.tasks.is_empty() || changes.availability
                }
                Destination::Tasks => !changes.tasks.is_empty(),
                Destination::Messages => changes.messages,
                Destination::Events => changes.events,
                Destination::Usage => changes.usage || !changes.agents.is_empty(),
            };
        if changed {
            self.refresh(destination);
        }
        if changes.session || changes.questions || changes.reset {
            self.query(ViewQuery::Overview, QueryTarget::Overview);
        }
        if changes.questions || changes.reset {
            self.query(ViewQuery::Questions, QueryTarget::Questions);
        }
        if !changes.agents.is_empty()
            || !changes.tasks.is_empty()
            || changes.pending
            || changes.availability
            || changes.messages
        {
            let selected = self.selected.borrow().clone();
            for key in selected.into_iter().flatten() {
                self.detail_query(key);
            }
        }
        for (_, key, _) in self.evidence.borrow().iter() {
            if changes.pending {
                self.query(
                    ViewQuery::Evidence { key: key.clone() },
                    QueryTarget::Evidence(key.clone()),
                );
            }
        }
        if changes.pending {
            if let Some(key) = self.model_loading.borrow().clone() {
                self.query(
                    ViewQuery::Evidence {
                        key: ResourceKey {
                            entity: EntityKey {
                                scope: key.scope.clone(),
                                entity: Entity::Workspace,
                            },
                            kind: ResourceKind::Models,
                            options: ReadOptions::Default,
                        },
                    },
                    QueryTarget::Models(key),
                );
            }
        }
        if changes.reset
            || changes.availability
            || changes.pending
            || changes.questions
            || changes.session
            || !changes.agents.is_empty()
        {
            self.refresh_controls();
        }
    }
    fn present(&self, target: QueryTarget, data: ViewData) {
        match (target, data.payload) {
            (
                QueryTarget::Availability { control, intent },
                ViewPayload::Availability(availability),
            ) => self.controls.apply(control, &intent, Ok(availability)),
            (
                QueryTarget::RestoreReceipt { form, intent },
                ViewPayload::Availability(availability),
            ) => {
                if !availability.available {
                    self.result.set_label(
                        &availability
                            .reason
                            .map_or("Restoration unavailable.".into(), |reason| reason.message),
                    );
                    return;
                }
                let current = self
                    .forms
                    .borrow()
                    .get(&form)
                    .map(|open| (open.form.clone(), open.submitted.clone()));
                if let Some((
                    old,
                    Some(ActionIntent::SendMessage {
                        scope: original,
                        to,
                        message_id,
                        body: submitted,
                    }),
                )) = current
                {
                    if old.pending.get()
                        || self.scope.borrow().as_ref()
                            != Some(crew_client::actions::action_scope(&intent))
                    {
                        return;
                    }
                    if let ActionIntent::SendMessage { scope, body, .. } = *intent {
                        if scope.endpoint_id != original.endpoint_id
                            || scope.workspace_id != original.workspace_id
                            || body != common::buffer_text(&old.buffer)
                        {
                            return;
                        }
                        // The original stays a read-only receipt with its own
                        // outcome; the restored editor is a new submission.
                        old.dialog.set_visible(false);
                        self.compose_message(
                            to.clone(),
                            Some(body),
                            Some((to, submitted, message_id)),
                        );
                    }
                }
            }
            (QueryTarget::Main(Destination::Team), ViewPayload::Team(team)) => {
                self.team.update(data.scope.as_ref(), &team)
            }
            (QueryTarget::Recipients(form), ViewPayload::Team(team)) => {
                let picker = self
                    .forms
                    .borrow()
                    .get(&form)
                    .and_then(|open| open.recipients.clone());
                if let Some(picker) = picker {
                    let agents: Vec<String> =
                        team.agents.iter().map(|m| m.agent.id.clone()).collect();
                    picker.set_agents(&agents, &picker.recipient());
                }
            }
            (QueryTarget::Main(Destination::Tasks), ViewPayload::Tasks(tasks)) => {
                self.tasks.update(data.scope.as_ref(), &tasks)
            }
            (QueryTarget::Main(Destination::Messages), ViewPayload::Messages(page)) => {
                self.messages
                    .update(data.scope.as_ref(), &page, self.live.get());
                *self.message_page.borrow_mut() = Some(*page);
            }
            (QueryTarget::Main(Destination::Events), ViewPayload::Events(page)) => {
                self.events
                    .update(data.scope.as_ref(), &page, self.live.get());
                *self.event_page.borrow_mut() = Some(*page);
            }
            (QueryTarget::Main(Destination::Usage), ViewPayload::Usage(usage)) => {
                self.usage.update(&usage)
            }
            (QueryTarget::Overview, ViewPayload::Overview(overview)) => {
                let session = overview.session.as_ref();
                self.overview.set_label(&format!(
                    "{}\n{}\n{} agents · {} working · {} of {} tasks done · {} questions{}",
                    session.map_or("No workspace loaded", |s| s.workspace.as_str()),
                    session.map_or("", |s| s.objective.as_str()),
                    overview.agent_count,
                    overview.working_count,
                    overview.done_count,
                    overview.task_count,
                    overview.question_count,
                    overview
                        .warnings
                        .iter()
                        .map(|w| format!("\n{}", w.text))
                        .collect::<String>()
                ));
            }
            (QueryTarget::Questions, ViewPayload::Questions(questions)) => {
                self.questions_button
                    .set_label(&format!("Questions ({})", questions.len()));
                for form in self.forms.borrow().values() {
                    if let Some(id) = form.target.strip_prefix("answer:") {
                        if !questions.iter().any(|q| q.id == id) {
                            form.form.failed(
                                "This question was answered elsewhere or is no longer open.",
                            );
                            form.form.submit.set_sensitive(false);
                        }
                    }
                }
                *self.questions.borrow_mut() = questions;
            }
            (QueryTarget::Detail(key), ViewPayload::AgentDetail(detail))
                if self.selected.borrow()[Destination::Team.index()].as_ref() == Some(&key) =>
            {
                if let Some((_, inspector)) =
                    self.agent.borrow().as_ref().filter(|(k, _)| k == &key)
                {
                    inspector.update(&detail, self.live.get());
                }
            }
            (QueryTarget::Detail(key), ViewPayload::TaskDetail(detail))
                if self.selected.borrow()[Destination::Tasks.index()].as_ref() == Some(&key) =>
            {
                if let Some((_, inspector)) = self
                    .task_inspector
                    .borrow()
                    .as_ref()
                    .filter(|(k, _)| k == &key)
                {
                    inspector.update(&detail);
                }
            }
            (QueryTarget::Evidence(key), ViewPayload::Evidence(state)) => {
                self.resource(key, *state)
            }
            (QueryTarget::Models(key), ViewPayload::Evidence(state)) => {
                if self.model_loading.borrow().as_ref() == Some(&key) {
                    if let Some(ResourceValue::Models(models)) = state.value {
                        self.model_loading.borrow_mut().take();
                        self.models(key, models);
                    } else if let Some(error) = state.error {
                        self.model_loading.borrow_mut().take();
                        self.result.set_label(&error.message);
                    }
                }
            }
            _ => (),
        }
    }
    pub fn select(&self, key: EntityKey) {
        if self.scope.borrow().as_ref() != Some(&key.scope) {
            return;
        }
        let destination = entity_destination(&key.entity);
        if self.selected.borrow()[destination.index()].as_ref() != Some(&key) {
            match destination {
                Destination::Team => {
                    self.retire_attachments();
                    self.agent.borrow_mut().take();
                }
                Destination::Tasks => {
                    self.task_inspector.borrow_mut().take();
                }
                _ => self
                    .evidence
                    .borrow_mut()
                    .retain(|(d, _, _)| *d != destination),
            }
        }
        self.selected.borrow_mut()[destination.index()] = Some(key.clone());
        self.cancel_confirmation();
        match &key.entity {
            Entity::Agent(id) => {
                self.shell.navigate(Destination::Team);
                if self.agent.borrow().is_none() {
                    let action = Rc::new({
                        let weak = self.weak.clone();
                        move |intent| {
                            if let Some(c) = weak.upgrade() {
                                c.plan(intent, None, None);
                            }
                        }
                    });
                    let inspector = AgentInspector::new(
                        key.clone(),
                        AgentCallbacks {
                            action,
                            select: Rc::new({
                                let weak = self.weak.clone();
                                move |key| {
                                    if let Some(c) = weak.upgrade() {
                                        c.select(key);
                                    }
                                }
                            }),
                            compose: Rc::new({
                                let weak = self.weak.clone();
                                move |id| {
                                    if let Some(c) = weak.upgrade() {
                                        c.compose(id);
                                    }
                                }
                            }),
                            model: Rc::new({
                                let weak = self.weak.clone();
                                move |key| {
                                    if let Some(c) = weak.upgrade() {
                                        c.model_read(key);
                                    }
                                }
                            }),
                            read: Rc::new({
                                let weak = self.weak.clone();
                                move |key| {
                                    if let Some(c) = weak.upgrade() {
                                        c.read(key);
                                    }
                                }
                            }),
                            attach: Rc::new({
                                let weak = self.weak.clone();
                                move |key| {
                                    if let Some(c) = weak.upgrade() {
                                        c.attach(key);
                                    }
                                }
                            }),
                            prompt: Rc::new({
                                let weak = self.weak.clone();
                                move |key| {
                                    if let Some(c) = weak.upgrade() {
                                        c.prompt(key);
                                    }
                                }
                            }),
                        },
                    );
                    self.shell.set_detail(
                        Destination::Team,
                        &format!("Agent {id}"),
                        &inspector.root,
                    );
                    *self.agent.borrow_mut() = Some((key.clone(), inspector));
                }
            }
            Entity::Task(id) => {
                self.shell.navigate(Destination::Tasks);
                if self.task_inspector.borrow().is_none() {
                    let inspector = views::task_detail::TaskInspector::new(
                        key.clone(),
                        Rc::new({
                            let weak = self.weak.clone();
                            move |key| {
                                if let Some(c) = weak.upgrade() {
                                    c.read(key);
                                }
                            }
                        }),
                    );
                    self.shell.set_detail(
                        Destination::Tasks,
                        &format!("Task {id}"),
                        &inspector.root,
                    );
                    *self.task_inspector.borrow_mut() = Some((key.clone(), inspector));
                }
            }
            Entity::Message(_) => {
                self.shell.navigate(Destination::Messages);
                self.open_evidence(ResourceKey {
                    entity: key.clone(),
                    kind: ResourceKind::Message,
                    options: ReadOptions::Default,
                });
            }
            Entity::Event(_) => {
                self.shell.navigate(Destination::Events);
                self.open_evidence(ResourceKey {
                    entity: key.clone(),
                    kind: ResourceKind::Event,
                    options: ReadOptions::Default,
                });
            }
            Entity::Workspace => (),
        }
        self.detail_query(key);
        self.shell.set_detail_visible(true);
    }
    fn detail_query(&self, key: EntityKey) {
        if Some(&key.scope) != self.scope.borrow().as_ref() {
            return;
        }
        match &key.entity {
            Entity::Agent(id) => self.query(
                ViewQuery::AgentDetail { id: id.clone() },
                QueryTarget::Detail(key),
            ),
            Entity::Task(id) => self.query(
                ViewQuery::TaskDetail { id: id.clone() },
                QueryTarget::Detail(key),
            ),
            Entity::Message(_) | Entity::Event(_) => {
                if let Some((_, resource, _)) = self
                    .evidence
                    .borrow()
                    .iter()
                    .find(|(_, resource, _)| resource.entity == key)
                {
                    self.query(
                        ViewQuery::Evidence {
                            key: resource.clone(),
                        },
                        QueryTarget::Evidence(resource.clone()),
                    );
                }
            }
            Entity::Workspace => (),
        }
    }
    fn open_evidence(&self, key: ResourceKey) {
        let view = EvidenceView::new({
            let weak = self.weak.clone();
            let key = key.clone();
            move || {
                if let Some(c) = weak.upgrade() {
                    c.read(key.clone());
                }
            }
        });
        self.shell
            .set_detail(self.shell.destination(), "Evidence", &view.root);
        let destination = entity_destination(&key.entity.entity);
        self.evidence
            .borrow_mut()
            .retain(|(d, _, _)| *d != destination);
        self.evidence
            .borrow_mut()
            .push((destination, key.clone(), view));
        self.read(key);
    }
    fn resource(&self, key: ResourceKey, state: ResourceState) {
        if Some(&key.entity.scope) != self.scope.borrow().as_ref() {
            return;
        }
        if let Some((_, current, view)) = self.evidence.borrow().iter().find(|(_, k, _)| k == &key)
        {
            let _ = current;
            view.update(&state);
            if let Some(ResourceValue::Message(Some(message))) = state.value.as_ref() {
                let mut keys = Vec::new();
                if let Some(parent) = &message.reply_to {
                    keys.push((
                        "Open parent message".to_owned(),
                        Entity::Message(parent.clone()),
                    ));
                }
                for (label, id) in [
                    ("Open sender", &message.from),
                    ("Open recipient", &message.to),
                ] {
                    if id != crew_protocol::OPERATOR_ID && id != crew_protocol::BROADCAST {
                        keys.push((format!("{label} {id}"), Entity::Agent(id.clone())));
                    }
                }
                if let Some(page) = self.message_page.borrow().as_ref() {
                    for reply in &page.items {
                        if reply.reply_to.as_ref() == Some(&message.id) {
                            keys.push((
                                format!("Open loaded reply {}", reply.id),
                                Entity::Message(reply.id.clone()),
                            ));
                        }
                    }
                }
                let signature = format!("{}:{keys:?}", message.id);
                let links = keys
                    .into_iter()
                    .map(|(label, entity)| {
                        let key = EntityKey {
                            scope: key.entity.scope.clone(),
                            entity,
                        };
                        let weak = self.weak.clone();
                        (
                            label,
                            Rc::new(move || {
                                if let Some(c) = weak.upgrade() {
                                    c.select(key.clone());
                                }
                            }) as Rc<dyn Fn()>,
                        )
                    })
                    .collect();
                view.set_links(signature, links);
            }
        }
        if let Some((_, agent)) = self.agent.borrow().as_ref() {
            agent.resource(&state);
        }
        if let Some((entity, inspector)) = self.task_inspector.borrow().as_ref() {
            if entity == &state.key.entity && state.key.kind == ResourceKind::TaskCard {
                inspector.resource(&state);
            }
        }
        if matches!(key.kind, ResourceKind::History)
            && matches!(state.status, ResourceStatus::Ready | ResourceStatus::Empty)
        {
            self.refresh(Destination::Messages);
        }
        if matches!(key.kind, ResourceKind::Events)
            && matches!(state.status, ResourceStatus::Ready | ResourceStatus::Empty)
        {
            self.refresh(Destination::Events);
        }
    }
    fn read(&self, key: ResourceKey) {
        if Some(&key.entity.scope) != self.scope.borrow().as_ref() {
            return;
        }
        self.plan(
            ActionIntent::Read {
                scope: key.entity.scope.clone(),
                resource: ResourceRequest { key: key.clone() },
            },
            None,
            None,
        );
        self.query(
            ViewQuery::Evidence { key: key.clone() },
            QueryTarget::Evidence(key),
        );
    }
    fn older(&self, messages: bool) {
        let Some(scope) = self.scope.borrow().clone() else {
            return;
        };
        let (before, local, remote) = if messages {
            self.message_page
                .borrow()
                .as_ref()
                .map(|p| (p.before.clone(), p.has_older_local, p.may_have_older_remote))
        } else {
            self.event_page
                .borrow()
                .as_ref()
                .map(|p| (p.before.clone(), p.has_older_local, p.may_have_older_remote))
        }
        .unwrap_or((None, false, true));
        if local {
            if messages {
                *self.message_before.borrow_mut() = before;
                self.refresh(Destination::Messages);
            } else {
                *self.event_before.borrow_mut() = before;
                self.refresh(Destination::Events);
            }
        } else if remote {
            let bound = if messages {
                &self.message_bound
            } else {
                &self.event_bound
            };
            let cap = if messages {
                policy::NATIVE_LIMITS.messages
            } else {
                policy::NATIVE_LIMITS.events
            };
            bound.set(bound.get().saturating_mul(2).min(cap));
            self.result.set_label("Loading the bounded host history. Older remote history may remain beyond this client's retention.");
            self.read(ResourceKey {
                entity: EntityKey {
                    scope,
                    entity: Entity::Workspace,
                },
                kind: if messages {
                    ResourceKind::History
                } else {
                    ResourceKind::Events
                },
                options: ReadOptions::Bounded { limit: bound.get() },
            });
        }
    }

    fn plan(&self, intent: ActionIntent, form: Option<u64>, terminal: Option<TerminalView>) {
        let scope = crew_client::actions::action_scope(&intent);
        if self.scope.borrow().as_ref() != Some(scope) {
            self.result
                .set_label("This action belongs to a workspace that is no longer selected.");
            return;
        }
        // Captured now: by the time a confirmation arrives Core may already
        // report the action pending and the opener may have lost focus.
        let opener = glib::WeakRef::new();
        opener.set(gtk::prelude::GtkWindowExt::focus(self.shell.window()).as_ref());
        let token = self.token();
        if self.send(Request::Plan {
            token,
            intent: intent.clone(),
        }) {
            if let Some(id) = form {
                let ui = self.forms.borrow().get(&id).map(|f| f.form.clone());
                if let Some(ui) = ui {
                    ui.pending();
                    if matches!(intent, ActionIntent::SendMessage { .. }) {
                        ui.retain_receipt(
                            "Submitted · awaiting the daemon's authoritative result.",
                        );
                    }
                }
                if let Some(open) = self.forms.borrow_mut().get_mut(&id) {
                    open.submitted = Some(intent.clone());
                }
            }
            self.plans.borrow_mut().insert(
                token,
                PendingAction {
                    intent,
                    form,
                    terminal,
                    canceled: false,
                    opener,
                },
            );
        }
    }
    fn bind_control(
        &self,
        id: u64,
        button: &gtk::Button,
        reason: &gtk::Label,
        intent: Intent,
        pending: Rc<Cell<bool>>,
    ) {
        self.controls.bind(id, button, reason, intent, pending);
        self.refresh_control(id);
    }
    fn bind_form_control(&self, id: u64, form: &Form, intent: Intent) {
        self.bind_control(id, &form.submit, &form.error, intent, form.pending.clone());
        form.buffer.connect_changed({
            let weak = self.weak.clone();
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.refresh_control(id);
                }
            }
        });
    }
    fn refresh_control(&self, id: u64) {
        match self.controls.intent(id) {
            Some(ControlInput::Action(intent)) => {
                if self.scope.borrow().as_ref() == Some(crew_client::actions::action_scope(&intent))
                {
                    self.query(
                        ViewQuery::Availability {
                            intent: intent.clone(),
                        },
                        QueryTarget::Availability {
                            control: id,
                            intent,
                        },
                    );
                    return;
                }
            }
            Some(ControlInput::Local) => {
                self.controls.local(id);
                return;
            }
            Some(ControlInput::Invalid(reason)) => {
                self.controls.unavailable(id, &reason);
                return;
            }
            None => return,
        }
        self.controls
            .unavailable(id, "Choose a verified workspace to submit this action.");
    }
    fn refresh_controls(&self) {
        for id in self.controls.ids() {
            self.refresh_control(id);
        }
    }
    fn planned(&self, token: u64, scope: ClientScope, result: Result<ActionPlan, ClientError>) {
        let Some(action) = self.plans.borrow_mut().remove(&token) else {
            return;
        };
        if self.scope.borrow().as_ref() != Some(&scope)
            && !matches!(&action.intent, ActionIntent::SendMessage { scope: captured, .. } if captured == &scope)
        {
            return;
        }
        match result {
            Ok(ActionPlan::Dispatch { operation_id }) => {
                let identified_send = matches!(action.intent, ActionIntent::SendMessage { .. });
                // The global status is for mutations. A read shows its progress
                // in its own tab; terminal writes would flood it.
                let quiet = matches!(
                    action.intent,
                    ActionIntent::Read { .. }
                        | ActionIntent::AttachInput { .. }
                        | ActionIntent::AttachResize { .. }
                );
                if !quiet {
                    self.result
                        .set_label("Action pending · awaiting the daemon's result");
                }
                self.operations.borrow_mut().insert(operation_id, action);
                let early = {
                    let mut outcomes = self.early_outcomes.borrow_mut();
                    outcomes
                        .iter()
                        .position(|o| {
                            o.operation_id == operation_id
                                && if identified_send {
                                    o.scope.endpoint_id == scope.endpoint_id
                                        && o.scope.workspace_id == scope.workspace_id
                                } else {
                                    o.scope == scope
                                }
                        })
                        .map(|i| outcomes.remove(i))
                };
                if let Some(outcome) = early {
                    self.outcome(outcome);
                }
            }
            Ok(ActionPlan::Confirm {
                confirmation_id,
                kind,
                scope,
                target_ids,
            }) => {
                self.cancel_confirmation();
                let dialog = dialogs::confirmation(
                    self.shell.window(),
                    kind,
                    &target_ids,
                    Rc::new({
                        let weak = self.weak.clone();
                        let scope = scope.clone();
                        move || {
                            if let Some(c) = weak.upgrade() {
                                let valid =
                                    c.confirmation.borrow().as_ref().is_some_and(|v| {
                                        v.id == confirmation_id && v.scope == scope
                                    }) && c.scope.borrow().as_ref() == Some(&scope)
                                        && c.live.get();
                                c.confirmation.borrow_mut().take();
                                if valid {
                                    c.send(Request::Confirm(confirmation_id));
                                } else {
                                    c.send(Request::Cancel(confirmation_id));
                                    c.result.set_label("Confirmation is no longer current.");
                                }
                            }
                        }
                    }),
                    Rc::new({
                        let weak = self.weak.clone();
                        move || {
                            if let Some(c) = weak.upgrade() {
                                c.confirmation.borrow_mut().take();
                                c.send(Request::Cancel(confirmation_id));
                            }
                        }
                    }),
                    Rc::new({
                        let weak = self.weak.clone();
                        let opener = action.opener.clone();
                        move || {
                            if let Some(c) = weak.upgrade() {
                                c.return_focus(&opener);
                            }
                        }
                    }),
                );
                *self.confirmation.borrow_mut() = Some(Confirmation {
                    id: confirmation_id,
                    scope,
                    dialog,
                });
            }
            Ok(ActionPlan::Unavailable { reason }) => self.action_failed(&action, &reason.message),
            Err(error) => self.action_failed(&action, &error.to_string()),
        }
    }
    /// After a confirmation closes, focus goes back to the control that
    /// opened it. Core keeps that control unavailable until it has settled
    /// the held confirmation, so focus waits on the nearest available
    /// control and moves back when the opener is enabled again, unless the
    /// user has moved it meanwhile. An opener that is gone leaves focus on
    /// the page's first control.
    fn return_focus(&self, opener: &glib::WeakRef<gtk::Widget>) {
        self.clear_focus_return();
        let window = self.shell.window();
        let Some(opener) = opener.upgrade().filter(|o| o.is_mapped()) else {
            self.shell.focus_page();
            return;
        };
        if opener.is_sensitive() && opener.grab_focus() {
            return;
        }
        gtk::prelude::GtkWindowExt::set_focus(window, None::<&gtk::Widget>);
        if !focus_near(&opener) {
            self.shell.focus_page();
        }
        let placed = glib::WeakRef::new();
        placed.set(gtk::prelude::GtkWindowExt::focus(window).as_ref());
        let window = window.downgrade();
        let weak = self.weak.clone();
        let handler = opener.connect_sensitive_notify(move |opener| {
            if !opener.is_sensitive() || !opener.is_mapped() {
                return;
            }
            let untouched = window.upgrade().is_some_and(|window| {
                gtk::prelude::GtkWindowExt::focus(&window)
                    .is_none_or(|focus| placed.upgrade().is_some_and(|placed| placed == focus))
            });
            if untouched {
                opener.grab_focus();
            }
            if let Some(c) = weak.upgrade() {
                c.clear_focus_return();
            }
        });
        *self.focus_return.borrow_mut() = Some((opener.downgrade(), handler));
    }
    fn clear_focus_return(&self) {
        let pending = self.focus_return.borrow_mut().take();
        if let Some((opener, handler)) = pending {
            if let Some(opener) = opener.upgrade() {
                opener.disconnect(handler);
            }
        }
    }
    fn action_failed(&self, action: &PendingAction, message: &str) {
        // A failed read is shown by its evidence view, not the global status.
        if !matches!(action.intent, ActionIntent::Read { .. }) {
            self.result.set_label(message);
        }
        if let Some(id) = action.form {
            let form = self.forms.borrow().get(&id).map(|f| f.form.clone());
            if let Some(form) = form {
                form.failed(message);
                form.receipt.set(false);
                self.refresh_control(id);
            }
        }
        if let Some(terminal) = &action.terminal {
            terminal.end(&format!("Attach failed: {message} · read-only"));
        }
    }
    fn outcome(&self, outcome: OperationNotice) {
        let known = self.operations.borrow().get(&outcome.operation_id).cloned();
        let receipt_match = known.as_ref().is_some_and(|action| matches!(&action.intent, ActionIntent::SendMessage { scope, .. } if scope.endpoint_id == outcome.scope.endpoint_id && scope.workspace_id == outcome.scope.workspace_id));
        if known
            .as_ref()
            .is_some_and(|action| matches!(action.intent, ActionIntent::SendMessage { .. }))
            && !receipt_match
        {
            return;
        }
        let late_send = self.plans.borrow().values().any(|action| matches!(&action.intent, ActionIntent::SendMessage { scope, .. } if scope.endpoint_id == outcome.scope.endpoint_id && scope.workspace_id == outcome.scope.workspace_id));
        if self.scope.borrow().as_ref() != Some(&outcome.scope) && !receipt_match && !late_send {
            return;
        }
        let action = self.operations.borrow_mut().remove(&outcome.operation_id);
        let Some(action) = action else {
            // Confirm dispatch IDs are intentionally opaque at NativeSession's
            // host boundary. Surface every scoped failure without attributing
            // an unrelated read acknowledgment to the confirmed mutation.
            if !matches!(
                outcome.kind,
                OperationOutcomeKind::Success
                    | OperationOutcomeKind::SentUnacknowledged
                    | OperationOutcomeKind::ClosedLocally
            ) {
                self.result.set_label(&outcome.reason.as_ref().map_or_else(
                    || outcome_wording(outcome.kind).into(),
                    |reason| reason.message.clone(),
                ));
            }
            if self.scope.borrow().as_ref() == Some(&outcome.scope)
                && outcome.kind == OperationOutcomeKind::Success
            {
                if let Some(id) = outcome
                    .attachment_id
                    .filter(|id| *id == outcome.operation_id)
                {
                    self.identified_renderer(&outcome.scope, id);
                }
            }
            let mut early = self.early_outcomes.borrow_mut();
            if early.len() == policy::NATIVE_LIMITS.pending_operations as usize {
                early.remove(0);
            }
            early.push(outcome);
            return;
        };
        if outcome.kind == OperationOutcomeKind::Success {
            if let Some(terminal) = action.terminal {
                if let Some(id) = outcome
                    .attachment_id
                    .filter(|id| *id == outcome.operation_id)
                {
                    if action.canceled {
                        self.plan(
                            ActionIntent::Detach {
                                scope: outcome.scope.clone(),
                                attachment_id: id,
                            },
                            None,
                            None,
                        );
                        terminal.end("Selection changed · detached · read-only");
                    } else {
                        let renderer = self.identified_renderer(&outcome.scope, id);
                        terminal.adopt(renderer);
                    }
                } else {
                    terminal.end("Attachment identity was not supplied · read-only");
                }
            }
            if let Some(id) = action.form {
                let exact = self
                    .forms
                    .borrow()
                    .get(&id)
                    .is_some_and(|form| match &action.intent {
                        ActionIntent::SendMessage { body, .. }
                        | ActionIntent::Answer { body, .. } => {
                            common::buffer_text(&form.form.buffer) == *body
                        }
                        _ => true,
                    });
                if exact {
                    let form = self.forms.borrow_mut().remove(&id);
                    if let Some(form) = form {
                        self.controls.remove(id);
                        if let ActionIntent::SendMessage { body, .. } = &action.intent {
                            let key = DraftKey::new(&form.scope, &form.target);
                            self.drafts
                                .borrow_mut()
                                .retain(|(k, text)| k != &key || text != body);
                        } else {
                            self.save_draft(&form.scope, &form.target, "");
                        }
                        form.form.buffer.set_text("");
                        form.form
                            .error
                            .set_label("Submission acknowledged by the daemon.");
                        form.form.close();
                    }
                } else if let Some(form) = self.forms.borrow().get(&id) {
                    form.form.pending.set(false);
                    form.form.retain_receipt("Original submission acknowledged; later edits retained. Restore explicitly before submitting another message.");
                }
            }
            match action.intent {
                ActionIntent::Read { resource, .. } => {
                    self.query(
                        ViewQuery::Evidence {
                            key: resource.key.clone(),
                        },
                        QueryTarget::Evidence(resource.key),
                    );
                }
                ActionIntent::AttachInput { .. } | ActionIntent::AttachResize { .. } => (),
                ActionIntent::Agent {
                    action: AgentAction::Compact,
                    ..
                } => self
                    .result
                    .set_label("Compaction queued for the turn boundary."),
                _ => self.result.set_label("Action acknowledged by the daemon."),
            }
        } else if matches!(
            outcome.kind,
            OperationOutcomeKind::SentUnacknowledged | OperationOutcomeKind::ClosedLocally
        ) {
            // Fire-and-forget terminal writes are not a successful mutation of
            // domain state, and must not flood the status region per keystroke.
        } else {
            let message = outcome
                .reason
                .map_or_else(|| outcome_wording(outcome.kind).into(), |r| r.message);
            if matches!(action.intent, ActionIntent::SendMessage { .. }) {
                let explanation = if outcome.kind == OperationOutcomeKind::Unknown {
                    format!(
                        "Outcome unknown: {message}. Check message history before trying again."
                    )
                } else {
                    format!("{}: {message}", outcome_wording(outcome.kind))
                };
                if let Some(id) = action.form {
                    self.controls.remove(id);
                    if let Some(form) = self.forms.borrow().get(&id) {
                        form.form.pending.set(false);
                        form.form.retain_receipt(&explanation);
                    }
                }
                self.result.set_label(&explanation);
            } else {
                self.action_failed(&action, &message);
            }
            if outcome.kind == OperationOutcomeKind::Unknown
                && !matches!(action.intent, ActionIntent::Read { .. })
            {
                self.result.set_label(&format!(
                    "Outcome unknown: {message}. Check the current state before trying again."
                ));
            }
        }
    }
    fn terminal_chunk(&self, chunk: TerminalChunk) {
        if self.scope.borrow().as_ref() != Some(&chunk.scope) {
            return;
        }
        let view = self
            .attachments
            .borrow()
            .iter()
            .find(|(scope, id, _)| scope == &chunk.scope && *id == chunk.attachment_id)
            .map(|(_, _, view)| view.clone());
        if let Some(view) = view {
            view.accept(&chunk);
        } else {
            self.dropped_terminal_chunks
                .set(self.dropped_terminal_chunks.get().saturating_add(1));
        }
        // Unknown/pre-attach bytes are discarded. Only an identified success
        // can create a renderer; the chunk cannot select a target or identity.
    }
    fn identified_renderer(&self, scope: &ClientScope, id: AttachmentId) -> TerminalView {
        if let Some(view) = self
            .attachments
            .borrow()
            .iter()
            .find(|(s, i, _)| s == scope && *i == id)
            .map(|(_, _, v)| v.clone())
        {
            return view;
        }
        let renderer = TerminalView::new(Rc::new({
            let weak = self.weak.clone();
            move |intent| {
                if let Some(c) = weak.upgrade() {
                    c.plan(intent, None, None);
                }
            }
        }));
        renderer.bind(scope.clone(), id);
        self.attachments.borrow_mut().retain(|(_, old_id, view)| {
            view.is_active()
                || self
                    .early_outcomes
                    .borrow()
                    .iter()
                    .any(|o| o.attachment_id == Some(*old_id))
        });
        self.attachments
            .borrow_mut()
            .push((scope.clone(), id, renderer.clone()));
        renderer
    }
    fn attach(&self, key: EntityKey) {
        let Entity::Agent(agent) = &key.entity else {
            return;
        };
        let terminal = self
            .agent
            .borrow()
            .as_ref()
            .filter(|(k, _)| k == &key)
            .map(|(_, a)| a.terminal.clone());
        let Some(terminal) = terminal else {
            return;
        };
        if terminal.is_active()
            || self
                .plans
                .borrow()
                .values()
                .chain(self.operations.borrow().values())
                .any(|a| a.terminal.is_some() && !a.canceled)
        {
            self.result.set_label(
                "An attachment is already active or pending. Detach it before attaching again.",
            );
            return;
        }
        self.plan(
            ActionIntent::Attach {
                scope: key.scope,
                agent: agent.clone(),
                rows: 24,
                cols: 80,
            },
            None,
            Some(terminal),
        );
    }
    fn retire_attachments(&self) {
        for action in self
            .plans
            .borrow_mut()
            .values_mut()
            .chain(self.operations.borrow_mut().values_mut())
        {
            if let Some(terminal) = &action.terminal {
                action.canceled = true;
                terminal.end("Selection changed · read-only");
            }
        }
        let attachments = self.attachments.borrow_mut().drain(..).collect::<Vec<_>>();
        for (scope, attachment_id, view) in attachments {
            view.end("Selection changed · read-only");
            self.plan(
                ActionIntent::Detach {
                    scope,
                    attachment_id,
                },
                None,
                None,
            );
        }
    }
    fn cancel_confirmation(&self) {
        let confirmation = self.confirmation.borrow_mut().take();
        if let Some(confirmation) = confirmation {
            confirmation.dialog.response(gtk::ResponseType::Cancel);
        }
    }
    fn invalidate(&self, purge: bool) {
        self.live.set(false);
        self.cancel_confirmation();
        self.retire_attachments();
        *self.selected.borrow_mut() = std::array::from_fn(|_| None);
        self.agent.borrow_mut().take();
        self.task_inspector.borrow_mut().take();
        self.evidence.borrow_mut().clear();
        self.queries.borrow_mut().clear();
        self.model_loading.borrow_mut().take();
        self.plans.borrow_mut().retain(|_, action| {
            !purge && matches!(action.intent, ActionIntent::SendMessage { .. })
        });
        self.operations.borrow_mut().retain(|_, action| {
            !purge && matches!(action.intent, ActionIntent::SendMessage { .. })
        });
        self.early_outcomes.borrow_mut().retain(|outcome| !purge && self.plans.borrow().values().any(|action| matches!(&action.intent, ActionIntent::SendMessage { scope, .. } if scope.endpoint_id == outcome.scope.endpoint_id && scope.workspace_id == outcome.scope.workspace_id)));
        let forms = self.forms.borrow_mut().split_off(&0);
        for (id, form) in forms {
            self.controls.remove(id);
            if !purge && form.form.receipt.get() {
                if form.form.pending.get() {
                    form.form.retain_receipt("Original submitted message is still pending after reconnect. The client will not submit it again.");
                }
                self.forms.borrow_mut().insert(id, form);
                continue;
            }
            if !purge {
                self.save_draft(
                    &form.scope,
                    &form.target,
                    &common::buffer_text(&form.form.buffer),
                );
            }
            if purge {
                form.form.purge();
            } else {
                form.form.close();
            }
        }
        if purge {
            self.connection.borrow_mut().take();
            self.listing.borrow_mut().take();
            self.drafts.borrow_mut().clear();
            self.questions.borrow_mut().clear();
            self.questions_button.set_label("Questions (0)");
        }
        for dialog in self.auxiliary.borrow_mut().drain(..) {
            dialog.close();
        }
        for destination in Destination::ALL {
            self.shell
                .set_detail(destination, "Details", &views::no_selection());
        }
        self.overview.set_label(if purge {
            "Access unavailable"
        } else {
            "No workspace loaded"
        });
        self.questions.borrow_mut().clear();
        self.questions_button.set_label("Questions (0)");
        self.team.update(
            None,
            &TeamData {
                agents: Vec::new(),
                relations: Vec::new(),
            },
        );
        self.tasks.update(
            None,
            &TasksData {
                tasks: Vec::new(),
                layers: Vec::new(),
            },
        );
        self.messages.update(
            None,
            &MessagePage {
                items: Vec::new(),
                before: None,
                has_older_local: false,
                may_have_older_remote: true,
            },
            false,
        );
        self.events.update(
            None,
            &EventPage {
                items: Vec::new(),
                before: None,
                has_older_local: false,
                may_have_older_remote: true,
            },
            false,
        );
        self.usage.update(&UsagePresentation {
            total: None,
            reporters: 0,
            eligible: 0,
            overflowed: false,
            agents: Vec::new(),
            groups: Vec::new(),
        });
        self.message_page.borrow_mut().take();
        self.event_page.borrow_mut().take();
        for destination in Destination::ALL {
            self.shell
                .set_main(destination, &views::disconnected(destination));
        }
    }
    fn save_draft(&self, scope: &ClientScope, target: &str, text: &str) {
        let key = DraftKey::new(scope, target);
        let mut drafts = self.drafts.borrow_mut();
        drafts.retain(|(k, _)| k != &key);
        if !text.is_empty() {
            drafts.push((key, text.into()));
        }
    }
    fn draft(&self, scope: &ClientScope, target: &str) -> String {
        let key = DraftKey::new(scope, target);
        self.drafts
            .borrow()
            .iter()
            .find(|(k, _)| k == &key)
            .map_or(String::new(), |(_, text)| text.clone())
    }
    pub fn compose(&self, recipient: String) {
        self.compose_message(recipient, None, None);
    }
    /// `origin` is a restored (recipient, body, message ID); that ID stays
    /// bound to exactly that pair.
    fn compose_message(
        &self,
        recipient: String,
        initial: Option<String>,
        origin: Option<(String, String, String)>,
    ) {
        let Some(scope) = self.scope.borrow().clone() else {
            self.result
                .set_label("Choose a workspace before composing a message.");
            return;
        };
        let token = self.token();
        let target = format!("message:{recipient}");
        let identity = Rc::new(RefCell::new(MessageIdentity::new(origin)));
        let picker = RecipientPicker::new(&recipient);
        let form = Form::new(self.shell.window(), "New message", &format!("Workspace {} · Everyone sends to the whole team. Return inserts a newline; Send submits explicitly.", scope.workspace_id), &initial.unwrap_or_else(|| self.draft(&scope, &target)), "Send",
            Rc::new({ let weak = self.weak.clone(); let scope = scope.clone(); let picker = picker.clone(); let identity = identity.clone(); move |body| if let Some(c) = weak.upgrade() {
                let recipient = picker.recipient();
                if !c.command(&body, &recipient, &scope) { let message_id = identity.borrow_mut().submit(&recipient, &body); c.plan(ActionIntent::SendMessage { scope: scope.clone(), to: recipient, body, message_id }, Some(token), None); }
                else { let form = c.forms.borrow().get(&token).map(|f| f.form.clone()); if let Some(form) = form { form.failed(c.result.label().as_str()); } }
            }}), Rc::new({ let weak = self.weak.clone(); let scope = scope.clone(); let picker = picker.clone(); move |text| if let Some(c) = weak.upgrade() { c.save_draft(&scope, &format!("message:{}", picker.recipient()), &text); let receipt = c.forms.borrow().get(&token).is_some_and(|open| open.form.receipt.get()); if !receipt { c.forms.borrow_mut().remove(&token); c.controls.remove(token); } } }));
        form.dialog.content_area().prepend(&picker.root);
        form.add_editor(&picker.dropdown);
        form.name_editor("Message");
        self.bind_form_control(
            token,
            &form,
            Rc::new({
                let weak = self.weak.clone();
                let scope = scope.clone();
                let buffer = form.buffer.clone();
                let picker = picker.clone();
                move || {
                    let Some(c) = weak.upgrade() else {
                        return ControlInput::Invalid("Editor closed.".into());
                    };
                    let body = common::buffer_text(&buffer);
                    let recipient = picker.recipient();
                    let message_id = identity.borrow().peek(&recipient, &body);
                    c.composer_input(&scope, &body, &recipient, &message_id)
                }
            }),
        );
        picker.dropdown.connect_selected_notify({
            let weak = self.weak.clone();
            move |_| {
                if let Some(c) = weak.upgrade() {
                    c.refresh_control(token);
                }
            }
        });
        self.forms.borrow_mut().insert(
            token,
            OpenForm {
                form,
                scope,
                target,
                submitted: None,
                recipients: Some(picker),
            },
        );
        // The team's agents fill the To selector once the projection arrives.
        self.query(ViewQuery::Team, QueryTarget::Recipients(token));
    }
    fn prompt(&self, key: EntityKey) {
        let Entity::Agent(id) = key.entity else {
            return;
        };
        let token = self.token();
        let target = format!("prompt:{id}");
        let form = Form::new(self.shell.window(), "Send prompt", &format!("Prompt {id} in workspace {}. This is host terminal input, separate from a team message.", key.scope.workspace_id), &self.draft(&key.scope, &target), "Submit prompt",
            Rc::new({ let weak = self.weak.clone(); let scope = key.scope.clone(); let id = id.clone(); move |text| if let Some(c) = weak.upgrade() { c.plan(ActionIntent::AgentInput { scope: scope.clone(), id: id.clone(), text: Some(text), submit: true }, Some(token), None); } }),
            Rc::new({ let weak = self.weak.clone(); let scope = key.scope.clone(); let target = target.clone(); move |text| if let Some(c) = weak.upgrade() { c.save_draft(&scope, &target, &text); let receipt = c.forms.borrow().get(&token).is_some_and(|open| open.form.receipt.get()); if !receipt { c.forms.borrow_mut().remove(&token); c.controls.remove(token); } } }));
        form.name_editor("Prompt");
        self.bind_form_control(
            token,
            &form,
            Rc::new({
                let scope = key.scope.clone();
                let buffer = form.buffer.clone();
                move || {
                    ControlInput::Action(Box::new(ActionIntent::AgentInput {
                        scope: scope.clone(),
                        id: id.clone(),
                        text: Some(common::buffer_text(&buffer)),
                        submit: true,
                    }))
                }
            }),
        );
        self.forms.borrow_mut().insert(
            token,
            OpenForm {
                form,
                scope: key.scope,
                target,
                submitted: None,
                recipients: None,
            },
        );
    }
    fn question(&self, index: usize) {
        let Some(scope) = self.scope.borrow().clone() else {
            return;
        };
        let Some(question) = self.questions.borrow().get(index).cloned() else {
            self.result.set_label("No unanswered question is loaded.");
            return;
        };
        let token = self.token();
        let target = format!("answer:{}", question.id);
        let form = Form::new(
            self.shell.window(),
            "Answer question",
            &format!(
                "{} asks in workspace {}:\n\n{}",
                question.asker(),
                scope.workspace_id,
                question.body
            ),
            &self.draft(&scope, &target),
            "Answer",
            Rc::new({
                let weak = self.weak.clone();
                let scope = scope.clone();
                let id = question.id.clone();
                move |body| {
                    if let Some(c) = weak.upgrade() {
                        c.plan(
                            ActionIntent::Answer {
                                scope: scope.clone(),
                                question_id: id.clone(),
                                body,
                            },
                            Some(token),
                            None,
                        );
                    }
                }
            }),
            Rc::new({
                let weak = self.weak.clone();
                let scope = scope.clone();
                let target = target.clone();
                move |text| {
                    if let Some(c) = weak.upgrade() {
                        c.save_draft(&scope, &target, &text);
                        c.forms.borrow_mut().remove(&token);
                        c.controls.remove(token);
                    }
                }
            }),
        );
        form.name_editor("Answer");
        self.bind_form_control(
            token,
            &form,
            Rc::new({
                let scope = scope.clone();
                let id = question.id.clone();
                let buffer = form.buffer.clone();
                move || {
                    ControlInput::Action(Box::new(ActionIntent::Answer {
                        scope: scope.clone(),
                        question_id: id.clone(),
                        body: common::buffer_text(&buffer),
                    }))
                }
            }),
        );
        if let Some(button) = form
            .dialog
            .widget_for_response(gtk::ResponseType::Cancel)
            .and_downcast::<gtk::Button>()
        {
            button.set_label("Later");
        }
        let choices = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for option in question.options() {
            let button = gtk::Button::with_label(&option);
            button.connect_clicked({
                let buffer = form.buffer.clone();
                move |_| buffer.set_text(&option)
            });
            choices.append(&button);
        }
        if self.questions.borrow().len() > 1 {
            let previous = gtk::Button::with_label("Previous question");
            let next = gtk::Button::with_label("Next question");
            for (button, step) in [(&previous, -1isize), (&next, 1)] {
                button.connect_clicked({
                    let weak = self.weak.clone();
                    let dialog = form.dialog.downgrade();
                    move |_| {
                        if let Some(c) = weak.upgrade() {
                            let len = c.questions.borrow().len();
                            if len == 0 {
                                return;
                            }
                            if let Some(dialog) = dialog.upgrade() {
                                dialog.response(gtk::ResponseType::Cancel);
                            }
                            c.question((index as isize + step).rem_euclid(len as isize) as usize);
                        }
                    }
                });
                choices.append(button);
            }
        }
        form.dialog.content_area().prepend(&choices);
        self.forms.borrow_mut().insert(
            token,
            OpenForm {
                form,
                scope,
                target,
                submitted: None,
                recipients: None,
            },
        );
    }
    fn receipts(&self) {
        let dialog = gtk::Dialog::builder()
            .transient_for(self.shell.window())
            .modal(true)
            .title("Message receipts")
            .default_width(600)
            .default_height(420)
            .build();
        let body = common::column();
        body.append(&common::text("These are this client's submitted messages. A pending receipt stays read-only; closing it does not cancel the host operation."));
        for (&id, open) in self
            .forms
            .borrow()
            .iter()
            .filter(|(_, open)| open.form.receipt.get())
        {
            let Some(ActionIntent::SendMessage {
                scope,
                to,
                message_id,
                body: submitted,
            }) = &open.submitted
            else {
                continue;
            };
            body.append(&common::heading(&format!(
                "{} · to {to}",
                scope.workspace_id
            )));
            body.append(&common::selectable(&format!(
                "Message {message_id}\n\n{submitted}"
            )));
            let status = common::text("");
            open.form
                .error
                .bind_property("label", &status, "label")
                .sync_create()
                .build();
            body.append(&status);
            let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let show = gtk::Button::with_label("Show read-only receipt");
            show.connect_clicked({
                let weak = self.weak.clone();
                move |_| {
                    if let Some(c) = weak.upgrade() {
                        let receipt = c.forms.borrow().get(&id).map(|open| open.form.clone());
                        if let Some(receipt) = receipt {
                            receipt.dialog.present();
                        }
                    }
                }
            });
            controls.append(&show);
            let copy = gtk::Button::with_label("Copy submitted text");
            copy.connect_clicked({
                let submitted = submitted.clone();
                move |button| button.clipboard().set_text(&submitted)
            });
            controls.append(&copy);
            let restore = gtk::Button::with_label("Restore for explicit editing");
            restore.set_sensitive(!open.form.pending.get());
            restore.connect_clicked({
                let weak = self.weak.clone();
                move |_| {
                    if let Some(c) = weak.upgrade() {
                        c.restore_receipt(id);
                    }
                }
            });
            controls.append(&restore);
            body.append(&controls);
        }
        if !self
            .forms
            .borrow()
            .values()
            .any(|open| open.form.receipt.get())
        {
            body.append(&common::text(
                "No submitted receipts retained by this client.",
            ));
        }
        dialog.content_area().append(&common::scroll(&body));
        dialog.add_button("Close", gtk::ResponseType::Cancel);
        dialog.connect_response(|dialog, _| dialog.close());
        dialogs::escape(&dialog);
        dialog.present();
        self.auxiliary.borrow_mut().push(dialog);
    }
    fn restore_receipt(&self, id: u64) {
        let captured = self
            .forms
            .borrow()
            .get(&id)
            .map(|open| (open.form.clone(), open.submitted.clone()));
        let Some((
            form,
            Some(ActionIntent::SendMessage {
                scope: original,
                to,
                message_id,
                ..
            }),
        )) = captured
        else {
            return;
        };
        if form.pending.get() {
            self.result.set_label("The original message is still pending. No second submission is allowed from this receipt.");
            return;
        }
        let Some(scope) = self.scope.borrow().clone() else {
            self.result.set_label(
                "Select and verify the original workspace before restoring this message.",
            );
            return;
        };
        if scope.endpoint_id != original.endpoint_id || scope.workspace_id != original.workspace_id
        {
            self.result.set_label(
                "Select the original endpoint and workspace before restoring this message.",
            );
            return;
        }
        let intent = ActionIntent::SendMessage {
            scope,
            to,
            message_id,
            body: common::buffer_text(&form.buffer),
        };
        self.query(
            ViewQuery::Availability {
                intent: Box::new(intent.clone()),
            },
            QueryTarget::RestoreReceipt {
                form: id,
                intent: Box::new(intent),
            },
        );
    }
    fn command_agent(&self, rest: &str, recipient: &str) -> Option<String> {
        if !rest.is_empty() {
            Some(rest.to_owned())
        } else if recipient != "*" && !recipient.is_empty() {
            Some(recipient.to_owned())
        } else {
            self.selected.borrow()[self.shell.destination().index()]
                .as_ref()
                .and_then(|key| match &key.entity {
                    Entity::Agent(id) => Some(id.clone()),
                    _ => None,
                })
        }
    }
    fn composer_input(
        &self,
        scope: &ClientScope,
        text: &str,
        recipient: &str,
        message_id: &str,
    ) -> ControlInput {
        let Some((name, rest)) = policy::slash_command(text) else {
            return ControlInput::Action(Box::new(ActionIntent::SendMessage {
                scope: scope.clone(),
                to: recipient.into(),
                body: text.into(),
                message_id: message_id.into(),
            }));
        };
        let Some(command) = policy::command_policy(name) else {
            return ControlInput::Invalid(format!(
                "No command /{name}. Use /help, or start with a space to send this as text."
            ));
        };
        if command.args.is_empty() && !rest.is_empty() {
            return ControlInput::Invalid(format!("/{} takes no argument.", command.name));
        }
        let action = match command.name {
            "stop" => ActionIntent::StopTeam {
                scope: scope.clone(),
            },
            "resume" => ActionIntent::ResumeLimited {
                scope: scope.clone(),
                restart: false,
            },
            "kill" | "restart" | "restart-fresh" | "pause" | "unpause" | "compact" => {
                let Some(id) = self.command_agent(rest, recipient) else {
                    return ControlInput::Invalid(format!("Name an agent for /{}.", command.name));
                };
                let action = match command.name {
                    "kill" => AgentAction::Kill,
                    "restart" => AgentAction::Restart,
                    "restart-fresh" => AgentAction::RestartFresh,
                    "pause" => AgentAction::Pause,
                    "unpause" => AgentAction::Resume,
                    _ => AgentAction::Compact,
                };
                ActionIntent::Agent {
                    scope: scope.clone(),
                    id,
                    action,
                }
            }
            "model" => {
                if self.command_agent(rest, recipient).is_none() {
                    return ControlInput::Invalid("Name an agent for /model.".into());
                }
                ActionIntent::Read {
                    scope: scope.clone(),
                    resource: ResourceRequest {
                        key: ResourceKey {
                            entity: EntityKey {
                                scope: scope.clone(),
                                entity: Entity::Workspace,
                            },
                            kind: ResourceKind::Models,
                            options: ReadOptions::Default,
                        },
                    },
                }
            }
            _ => return ControlInput::Local,
        };
        ControlInput::Action(Box::new(action))
    }
    fn command(&self, text: &str, recipient: &str, captured_scope: &ClientScope) -> bool {
        let Some((name, rest)) = policy::slash_command(text) else {
            return false;
        };
        if self.scope.borrow().as_ref() != Some(captured_scope) {
            self.result
                .set_label("This command belongs to a workspace that is no longer selected.");
            return true;
        }
        match self.composer_input(captured_scope, text, recipient, "") {
            ControlInput::Invalid(reason) => self.result.set_label(&reason),
            ControlInput::Action(intent) => {
                if policy::command_policy(name).is_some_and(|command| command.name == "model") {
                    if let Some(id) = self.command_agent(rest, recipient) {
                        self.model_read(EntityKey {
                            scope: captured_scope.clone(),
                            entity: Entity::Agent(id),
                        });
                    }
                } else {
                    self.plan(*intent, None, None);
                }
            }
            ControlInput::Local => match policy::command_policy(name).map(|command| command.name) {
                Some("answer") => self.question(0),
                Some("tasks") => self.shell.navigate(Destination::Tasks),
                Some("logs") => self.shell.navigate(Destination::Events),
                Some("usage") => self.shell.navigate(Destination::Usage),
                Some("graph") => self.shell.navigate(Destination::Team),
                Some("help") => {
                    let _ = gtk::prelude::WidgetExt::activate_action(
                        self.shell.window(),
                        "win.help",
                        None,
                    );
                }
                Some("exit") => self.shell.window().close(),
                _ => self.result.set_label(
                    "Use native keyboard navigation and text selection for this local command.",
                ),
            },
        }
        true
    }
    fn model_read(&self, key: EntityKey) {
        if Some(&key.scope) != self.scope.borrow().as_ref() {
            return;
        }
        *self.model_loading.borrow_mut() = Some(key.clone());
        self.result.set_label("Loading model and CLI choices…");
        let resource = ResourceKey {
            entity: EntityKey {
                scope: key.scope.clone(),
                entity: Entity::Workspace,
            },
            kind: ResourceKind::Models,
            options: ReadOptions::Default,
        };
        self.plan(
            ActionIntent::Read {
                scope: key.scope.clone(),
                resource: ResourceRequest {
                    key: resource.clone(),
                },
            },
            None,
            None,
        );
        self.query(
            ViewQuery::Evidence { key: resource },
            QueryTarget::Models(key),
        );
    }
    fn models(&self, key: EntityKey, models: crew_protocol::rpc::ModelsResult) {
        let Entity::Agent(id) = &key.entity else {
            return;
        };
        let dialog = gtk::Dialog::builder()
            .transient_for(self.shell.window())
            .modal(true)
            .title("Model and CLI")
            .default_width(540)
            .build();
        let body = common::column();
        body.append(&common::text(&format!(
            "Agent {id} · workspace {}",
            key.scope.workspace_id
        )));
        let choice = gtk::ComboBoxText::new();
        choice.update_property(&[gtk::accessible::Property::Label("Model and runtime CLI")]);
        for (i, model) in models.models.iter().enumerate() {
            choice.append(
                Some(&i.to_string()),
                &format!("{} · {} · {}", model.name, model.runtime, model.price),
            );
        }
        let consequence =
            common::text("Same CLI resumes its session; another CLI starts a fresh session.");
        body.append(&choice);
        body.append(&consequence);
        dialog.content_area().append(&body);
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        let apply = dialog.add_button("Apply model", gtk::ResponseType::Accept);
        apply.set_sensitive(false);
        dialog.set_default_response(gtk::ResponseType::Cancel);
        choice.connect_changed({ let models = models.clone(); let consequence = consequence.clone(); let apply = apply.clone(); move |choice| {
            let selected = choice.active().and_then(|i| models.models.get(i as usize)); apply.set_sensitive(selected.is_some_and(|m| m.unavailable.is_none()));
            consequence.set_label(&selected.map_or("Select a model.".into(), |m| format!("{}\n{}\nSame CLI resumes its session; another CLI starts a fresh session.{}", m.summary, m.tier, m.unavailable.as_ref().map_or(String::new(), |r| format!("\nUnavailable: {r}")))));
        }});
        dialog.connect_response({
            let weak = self.weak.clone();
            let key = key.clone();
            move |dialog, response| {
                if response == gtk::ResponseType::Accept {
                    if let Some(c) = weak.upgrade() {
                        if let Some(model) = choice
                            .active()
                            .and_then(|i| models.models.get(i as usize))
                            .filter(|m| m.unavailable.is_none())
                        {
                            if let Entity::Agent(id) = &key.entity {
                                c.plan(
                                    ActionIntent::SetModel {
                                        scope: key.scope.clone(),
                                        id: id.clone(),
                                        runtime: Some(model.runtime),
                                        model: model.model.clone(),
                                        note: None,
                                    },
                                    None,
                                    None,
                                );
                            }
                        }
                    }
                }
                dialog.close();
            }
        });
        dialogs::escape(&dialog);
        dialog.present();
        self.auxiliary.borrow_mut().push(dialog);
    }
    pub fn connections(&self) {
        let dialog = gtk::Dialog::builder()
            .transient_for(self.shell.window())
            .modal(true)
            .title("Connect to Crew")
            .default_width(620)
            .default_height(480)
            .build();
        let body = common::column();
        body.append(&common::heading("Connections"));
        body.append(&common::text("Choose a discovered same-host workspace explicitly. Selecting a profile does not join a workspace, start a service or grant access."));
        if let Some(connection) = self.connection.borrow().as_ref() {
            body.append(&common::text(match connection.mode {
                ConnectionMode::LocalOperator => "Local operator · full Crew access",
                ConnectionMode::ScopedGateway => "This device's access · scoped gateway",
            }));
            for tier in &connection.grant.tiers {
                body.append(&common::text(&format!("{tier:?}")));
            }
        }
        if let Some(reason) = self.profile_error.borrow().as_ref() {
            body.append(&common::text(&reason.message));
        }
        for profile in self.profiles.borrow().iter() {
            body.append(&common::text(&format!(
                "{}\n{}\nLocal workspace · daemon {}",
                profile.project.as_deref().unwrap_or("Crew"),
                profile.workspace,
                profile.daemon_pid
            )));
            let connect = gtk::Button::with_label("Connect to local workspace");
            connect.connect_clicked({
                let weak = self.weak.clone();
                let profile = profile.profile.clone();
                let dialog = dialog.downgrade();
                move |_| {
                    if let Some(c) = weak.upgrade() {
                        c.send(Request::Connect {
                            profile_id: profile.id.clone(),
                            endpoint_id: profile.endpoint_id.clone(),
                            workspace_id: Some(profile.workspace_key.clone()),
                        });
                    }
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                }
            });
            body.append(&connect);
        }
        if self.profiles.borrow().is_empty() {
            body.append(&common::text("No local profiles discovered. Start Crew at the host console, then refresh. Protected discovery never scans arbitrary socket paths."));
        }
        if let Some(listing) = self.listing.borrow().as_ref() {
            body.append(&common::heading("Joined workspaces"));
            for entry in &listing.joined {
                let button = gtk::Button::with_label(&format!(
                    "{} · {}\n{}{}",
                    entry.project.as_deref().unwrap_or("Crew"),
                    entry.state,
                    entry.workspace,
                    if entry.detail.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", entry.detail)
                    }
                ));
                button.connect_clicked({
                    let weak = self.weak.clone();
                    let id = entry.id.clone();
                    let dialog = dialog.downgrade();
                    move |_| {
                        if let Some(c) = weak.upgrade() {
                            c.send(Request::SelectWorkspace(id.clone()));
                        }
                        if let Some(dialog) = dialog.upgrade() {
                            dialog.close();
                        }
                    }
                });
                body.append(&button);
            }
        }
        for (name, request) in [
            ("Refresh local profiles", 0),
            ("Reconnect", 1),
            ("Disconnect", 2),
        ] {
            let button = gtk::Button::with_label(name);
            button.connect_clicked({
                let weak = self.weak.clone();
                let dialog = dialog.downgrade();
                move |_| {
                    if let Some(c) = weak.upgrade() {
                        c.send(match request {
                            0 => Request::RefreshProfiles,
                            1 => Request::Reconnect,
                            _ => Request::Disconnect,
                        });
                    }
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                }
            });
            body.append(&button);
        }
        body.append(&common::text("Disconnect or closing Crew leaves the team running. Stop team is a separate confirmed action."));
        body.append(&common::text("Remote gateway setup is delivered by the separately reviewed connector integration. Browser pairing codes and crew web output are never accepted here."));
        dialog.content_area().append(&common::scroll(&body));
        dialog.add_button("Close", gtk::ResponseType::Cancel);
        dialog.connect_response(|d, _| d.close());
        dialogs::escape(&dialog);
        dialog.present();
        self.auxiliary.borrow_mut().push(dialog);
    }
}

/// Focus the first available control in the nearest container of `widget`
/// that has one: the same row of actions, then the panel or page.
fn focus_near(widget: &gtk::Widget) -> bool {
    let mut node = widget.parent();
    while let Some(ancestor) = node {
        if ancestor.is::<gtk::Window>() {
            return false;
        }
        if ancestor.child_focus(gtk::DirectionType::TabForward) {
            return true;
        }
        node = ancestor.parent();
    }
    false
}

fn outcome_wording(kind: OperationOutcomeKind) -> &'static str {
    match kind {
        OperationOutcomeKind::Success => "Acknowledged by the daemon",
        OperationOutcomeKind::Rejected => "Rejected by the daemon",
        OperationOutcomeKind::NotSent => "Not sent",
        OperationOutcomeKind::Canceled => "Canceled",
        OperationOutcomeKind::Unknown => "Outcome unknown",
        OperationOutcomeKind::ClosedLocally => "Closed locally",
        OperationOutcomeKind::SentUnacknowledged => "Sent without an acknowledgment",
    }
}
fn entity_destination(entity: &Entity) -> Destination {
    match entity {
        Entity::Agent(_) | Entity::Workspace => Destination::Team,
        Entity::Task(_) => Destination::Tasks,
        Entity::Message(_) => Destination::Messages,
        Entity::Event(_) => Destination::Events,
    }
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
