use super::{
    common::{self, EntityList, Facts, Select},
    evidence::EvidenceView,
};
use crate::terminal::TerminalView;
use crew_client::api::*;
use gtk::prelude::*;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

pub struct AgentCallbacks {
    pub action: Rc<dyn Fn(ActionIntent)>,
    pub select: Select,
    pub compose: Rc<dyn Fn(String)>,
    pub model: Rc<dyn Fn(EntityKey)>,
    pub read: Rc<dyn Fn(ResourceKey)>,
    pub attach: Rc<dyn Fn(EntityKey)>,
    pub prompt: Rc<dyn Fn(EntityKey)>,
}

/// One agent action control. Header controls share the header's reason
/// line; Actions menu items show their own reason beside them.
struct Control {
    kind: ActionKind,
    button: gtk::Button,
    reason: Option<gtk::Label>,
}

/// A titled overview section that hides itself while its list is empty.
#[derive(Clone)]
struct Section {
    root: gtk::Box,
    list: EntityList,
}

impl Section {
    fn new(title: &str, label: &str, select: Select) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.append(&common::heading(title));
        let list = EntityList::new(label, select);
        list.list().add_css_class("crew-section-list");
        list.root.set_vscrollbar_policy(gtk::PolicyType::Never);
        list.root.set_propagate_natural_height(true);
        root.append(&list.root);
        Self { root, list }
    }

    fn replace(&self, rows: Vec<(EntityKey, gtk::Widget, String)>) {
        self.root.set_visible(!rows.is_empty());
        self.list.replace(rows);
    }
}

#[derive(Clone)]
pub struct AgentInspector {
    pub root: gtk::Box,
    pub terminal: TerminalView,
    key: EntityKey,
    title: gtk::Label,
    chip: gtk::Label,
    subtitle: gtk::Label,
    warnings: gtk::Box,
    facts: Facts,
    process: Facts,
    work: Section,
    tasks: Section,
    messages: Section,
    reasons: gtk::Label,
    controls: Rc<Vec<Control>>,
    evidence: Vec<(ResourceKey, EvidenceView)>,
    latest: Rc<RefCell<Option<AgentPresentation>>>,
    live: Rc<Cell<bool>>,
}
impl AgentInspector {
    pub fn new(key: EntityKey, callbacks: AgentCallbacks) -> Self {
        let Entity::Agent(id) = &key.entity else {
            panic!("agent inspector key")
        };
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let tabs = gtk::Notebook::new();
        tabs.set_scrollable(true);
        tabs.set_hexpand(true);
        tabs.set_vexpand(true);
        let mut controls = Vec::new();

        // Header: who this is, its state, and the two everyday actions. The
        // rest are in the Actions menu, each with its reason beside it.
        let header = gtk::Box::new(gtk::Orientation::Vertical, 12);
        header.set_margin_top(12);
        header.set_margin_bottom(12);
        header.set_margin_start(24);
        header.set_margin_end(24);
        let identity = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let title = common::title(id);
        title.set_hexpand(true);
        identity.append(&title);
        let chip = common::status_chip("", Tone::Neutral);
        chip.set_visible(false);
        identity.append(&chip);
        header.append(&identity);
        let subtitle = common::muted("");
        subtitle.set_visible(false);
        header.append(&subtitle);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let compose = gtk::Button::with_label("Message");
        compose.connect_clicked({
            let callback = callbacks.compose;
            let id = id.clone();
            move |_| callback(id.clone())
        });
        bar.append(&compose);
        controls.push(Control {
            kind: ActionKind::SendMessage,
            button: compose,
            reason: None,
        });
        let attach = gtk::Button::with_label("Attach terminal");
        bar.append(&attach);
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 4);
        menu.set_margin_top(8);
        menu.set_margin_bottom(8);
        menu.set_margin_start(8);
        menu.set_margin_end(8);
        let popover = gtk::Popover::builder().child(&menu).build();
        let more = gtk::MenuButton::builder()
            .label("Actions")
            .popover(&popover)
            .build();
        more.update_property(&[gtk::accessible::Property::Label("Agent actions")]);
        bar.append(&more);
        header.append(&bar);
        let reasons = common::reason();
        header.append(&reasons);
        let warnings = gtk::Box::new(gtk::Orientation::Vertical, 4);
        warnings.set_visible(false);
        header.append(&warnings);
        root.append(&header);

        // Pause and Unpause share one place; the agent's state picks which.
        for (name, kind, action, destructive) in [
            ("Pause", ActionKind::Pause, Some(AgentAction::Pause), false),
            ("Unpause", ActionKind::Resume, Some(AgentAction::Resume), false),
            ("Compact", ActionKind::Compact, Some(AgentAction::Compact), false),
            ("Restart", ActionKind::Restart, Some(AgentAction::Restart), false),
            ("Model and CLI…", ActionKind::SetModel, None, false),
            ("Jump to host pane", ActionKind::Focus, Some(AgentAction::Focus), false),
            (
                "Restart fresh…",
                ActionKind::RestartFresh,
                Some(AgentAction::RestartFresh),
                true,
            ),
            ("Kill…", ActionKind::Kill, Some(AgentAction::Kill), true),
        ] {
            let item = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let button = gtk::Button::with_label(name);
            button.add_css_class("flat");
            if let Some(label) = button.child().and_downcast::<gtk::Label>() {
                label.set_xalign(0.0);
            }
            if destructive {
                button.add_css_class("destructive-action");
            }
            button.connect_clicked({
                let popover = popover.downgrade();
                let callback = callbacks.action.clone();
                let model = callbacks.model.clone();
                let key = key.clone();
                move |_| {
                    if let Some(popover) = popover.upgrade() {
                        popover.popdown();
                    }
                    match action {
                        Some(action) => {
                            let Entity::Agent(id) = &key.entity else {
                                return;
                            };
                            callback(ActionIntent::Agent {
                                scope: key.scope.clone(),
                                id: id.clone(),
                                action,
                            })
                        }
                        None => model(key.clone()),
                    }
                }
            });
            item.append(&button);
            let reason = common::reason();
            reason.set_max_width_chars(32);
            reason.set_valign(gtk::Align::Center);
            item.append(&reason);
            menu.append(&item);
            controls.push(Control {
                kind,
                button,
                reason: Some(reason),
            });
        }

        let overview = common::column();
        overview.set_spacing(24);
        let facts = Facts::new();
        overview.append(&facts.root);
        let work = Section::new(
            "Current and waiting work",
            "Current and waiting task links",
            callbacks.select.clone(),
        );
        overview.append(&work.root);
        let tasks = Section::new(
            "Assigned and review tasks",
            "Assigned and review tasks",
            callbacks.select.clone(),
        );
        overview.append(&tasks.root);
        let messages = Section::new("Related messages", "Agent messages", callbacks.select);
        overview.append(&messages.root);
        let process = Facts::new();
        let process_group = gtk::Expander::builder()
            .label("Process details")
            .child(&process.root)
            .build();
        overview.append(&process_group);
        tabs.append_page(
            &common::scroll(&overview),
            Some(&gtk::Label::new(Some("Overview"))),
        );
        let mut evidence = Vec::new();
        for (name, kind) in [
            ("Transcript", ResourceKind::Transcript),
            ("Diff", ResourceKind::Diff),
            ("Inbox", ResourceKind::Inbox),
        ] {
            let resource = ResourceKey {
                entity: key.clone(),
                kind,
                options: ReadOptions::Default,
            };
            let view = EvidenceView::new({
                let read = callbacks.read.clone();
                let resource = resource.clone();
                move || read(resource.clone())
            });
            tabs.append_page(&view.root, Some(&gtk::Label::new(Some(name))));
            evidence.push((resource, view));
        }
        let terminal = TerminalView::new(callbacks.action.clone());
        let terminal_page = gtk::Box::new(gtk::Orientation::Vertical, 8);
        terminal_page.append(&terminal.root);
        let terminal_index =
            tabs.append_page(&terminal_page, Some(&gtk::Label::new(Some("Terminal"))));
        attach.connect_clicked({
            let callback = callbacks.attach;
            let key = key.clone();
            let tabs = tabs.downgrade();
            move |_| {
                if let Some(tabs) = tabs.upgrade() {
                    tabs.set_current_page(Some(terminal_index));
                }
                callback(key.clone())
            }
        });
        // Attach is offered only while nothing is attached here.
        let latest: Rc<RefCell<Option<AgentPresentation>>> = Rc::new(RefCell::new(None));
        let live = Rc::new(Cell::new(false));
        terminal.connect_attached({
            let attach = attach.downgrade();
            let latest = latest.clone();
            let live = live.clone();
            move |attached| {
                if let Some(attach) = attach.upgrade() {
                    let had_focus = attach.is_focus();
                    attach.set_sensitive(
                        !attached && live.get() && available(&latest.borrow(), ActionKind::Attach),
                    );
                    // Once attached, focus goes on to the terminal's controls
                    // rather than being lost with Attach.
                    if had_focus && !attach.is_sensitive() {
                        crate::shell::focus_past(
                            attach.upcast_ref(),
                            gtk::DirectionType::TabForward,
                        );
                    }
                }
            }
        });
        controls.push(Control {
            kind: ActionKind::Attach,
            button: attach,
            reason: None,
        });
        let screen = ResourceKey {
            entity: key.clone(),
            kind: ResourceKind::Output,
            options: ReadOptions::Output {
                lines: 300,
                screen: true,
                ansi: false,
            },
        };
        let screen_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let screen_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        screen_bar.set_margin_top(12);
        screen_bar.set_margin_start(24);
        screen_bar.set_margin_end(24);
        let screen_note = common::muted(
            "A bounded read of the host pane. Send prompt types into the agent's terminal; it is not a team message.",
        );
        screen_note.set_hexpand(true);
        screen_bar.append(&screen_note);
        let prompt = gtk::Button::with_label("Send prompt…");
        prompt.set_valign(gtk::Align::Center);
        prompt.connect_clicked({
            let callback = callbacks.prompt;
            let key = key.clone();
            move |_| callback(key.clone())
        });
        screen_bar.append(&prompt);
        screen_page.append(&screen_bar);
        controls.push(Control {
            kind: ActionKind::AgentInput,
            button: prompt,
            reason: None,
        });
        let screen_view = EvidenceView::new({
            let read = callbacks.read.clone();
            let resource = screen.clone();
            move || read(resource.clone())
        });
        screen_page.append(&screen_view.root);
        evidence.push((screen.clone(), screen_view));
        tabs.append_page(&screen_page, Some(&gtk::Label::new(Some("Host screen"))));
        tabs.connect_switch_page({
            let evidence: Vec<_> = evidence.iter().map(|(k, _)| k.clone()).collect();
            let read = callbacks.read;
            move |_, _, index| {
                let slot = match index {
                    1..=3 => Some(index as usize - 1),
                    5 => Some(3),
                    _ => None,
                };
                if let Some(key) = slot.and_then(|i| evidence.get(i)) {
                    read(key.clone());
                }
            }
        });
        root.append(&tabs);
        Self {
            root,
            terminal,
            key,
            title,
            chip,
            subtitle,
            warnings,
            facts,
            process,
            work,
            tasks,
            messages,
            reasons,
            controls: Rc::new(controls),
            evidence,
            latest,
            live,
        }
    }
    pub fn update(&self, data: &AgentDetailData, live: bool) {
        *self.latest.borrow_mut() = data.agent.clone();
        let Some(member) = &data.agent else {
            self.subtitle
                .set_label("This agent is no longer part of the team.");
            self.subtitle.set_visible(true);
            self.chip.set_visible(false);
            self.set_live(false);
            return;
        };
        let a = &member.agent;
        self.title.set_label(&a.id);
        common::set_status_chip(&self.chip, &member.status_label, member.tone);
        self.chip.set_visible(true);
        let mut subtitle = vec![a.role.clone(), a.runtime.to_string()];
        if a.name != a.id && !a.name.is_empty() {
            subtitle.insert(0, a.name.clone());
        }
        if let Some(model) = a.model_label() {
            subtitle.push(model);
        }
        self.subtitle.set_label(&subtitle.join(" · "));
        self.subtitle.set_visible(true);
        common::clear(&self.warnings);
        for warning in &member.warnings {
            self.warnings.append(&common::warning(&warning.text));
        }
        self.warnings.set_visible(!member.warnings.is_empty());
        let current = member.current_work.as_ref().map(|work| {
            format!(
                "{}{}{}",
                work.task_id,
                if work.done { " · completed" } else { "" },
                work.elapsed_ms
                    .map_or(String::new(), |ms| format!(" · {}", common::duration(ms)))
            )
        });
        self.facts.set(&[
            ("Current work", current),
            (
                "Waiting",
                (!member.waiting_task_ids.is_empty()).then(|| member.waiting_task_ids.join(", ")),
            ),
            ("Activity", Some(a.activity.clone())),
            ("Progress", a.progress.map(|p| format!("{p}%"))),
            (
                "Context",
                a.context
                    .as_ref()
                    .map(|c| format!("{}% of the window", c.percent)),
            ),
            ("Branch", a.branch.clone()),
            (
                "Mailbox",
                Some(format!(
                    "{} unread · {} received · {} sent",
                    a.inbox_unread, a.inbox_total, a.outbox_total
                )),
            ),
            ("Subagent of", member.satellite_parent_id.clone()),
            (
                "Subagents",
                (!member.subagent_ids.is_empty()).then(|| member.subagent_ids.join(", ")),
            ),
        ]);
        self.process.set(&[
            ("Workspace", Some(a.workdir.clone())),
            ("Session", a.session_id.clone()),
            ("Backend", Some(a.backend.clone())),
            ("Process", a.pid.map(|pid| pid.to_string())),
            ("Restarts", (a.restarts > 0).then(|| a.restarts.to_string())),
            ("Last activity", Some(common::timestamp(a.last_activity_at))),
        ]);
        let scope = &self.key.scope;
        let task_row = |kind: &str, id: &String| {
            let item = data
                .assigned_tasks
                .iter()
                .chain(&data.review_tasks)
                .find(|item| &item.task.id == id);
            let body = inset_row();
            body.append(&common::row_title(
                &item.map_or_else(|| id.clone(), |item| item.task.title.clone()),
                item.map(|item| {
                    common::status_chip(
                        item.task.status.as_str(),
                        common::task_tone(item.task.status),
                    )
                }),
            ));
            let detail = format!("{kind} · {id}");
            body.append(&common::muted(&detail));
            let label = format!(
                "{detail}{}",
                item.map_or(String::new(), |item| format!(
                    ", {}, {}",
                    item.task.title, item.task.status
                ))
            );
            (
                EntityKey {
                    scope: scope.clone(),
                    entity: Entity::Task(id.clone()),
                },
                body.upcast::<gtk::Widget>(),
                label,
            )
        };
        self.work.replace(
            member
                .current_work
                .iter()
                .map(|work| {
                    (
                        if work.done { "Completed" } else { "Current" },
                        &work.task_id,
                    )
                })
                .chain(member.waiting_task_ids.iter().map(|id| ("Waiting", id)))
                .map(|(kind, id)| task_row(kind, id))
                .collect(),
        );
        self.tasks.replace(
            data.assigned_tasks
                .iter()
                .map(|t| task_row("Assigned", &t.task.id))
                .chain(
                    data.review_tasks
                        .iter()
                        .map(|t| task_row("Review", &t.task.id)),
                )
                .collect(),
        );
        self.messages.replace(
            data.recent_messages
                .items
                .iter()
                .map(|m| {
                    let body = inset_row();
                    body.append(&common::muted(&format!(
                        "{} to {} · {}",
                        m.from, m.to, m.kind
                    )));
                    let preview = common::text(&m.body);
                    preview.set_lines(3);
                    preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    body.append(&preview);
                    let label = format!("{} to {}, {}, {}", m.from, m.to, m.kind, m.body);
                    (
                        EntityKey {
                            scope: scope.clone(),
                            entity: Entity::Message(m.id.clone()),
                        },
                        body.upcast(),
                        label,
                    )
                })
                .collect(),
        );
        self.set_live(live);
        for state in &data.resources {
            self.resource(state);
        }
    }
    pub fn resource(&self, state: &ResourceState) {
        for (key, view) in &self.evidence {
            // Default read aliases are canonicalized by the shared planner.
            if key.entity == state.key.entity && key.kind == state.key.kind {
                view.update(state);
            }
        }
    }
    pub fn set_live(&self, live: bool) {
        self.live.set(live);
        let latest = self.latest.borrow();
        let paused = latest.as_ref().is_some_and(|agent| agent.agent.paused);
        let mut reasons = Vec::new();
        for control in self.controls.iter() {
            match control.kind {
                ActionKind::Pause => control.button.set_visible(!paused),
                ActionKind::Resume => control.button.set_visible(paused),
                _ => (),
            }
            let availability = latest
                .as_ref()
                .and_then(|agent| agent.actions.iter().find(|a| a.action == control.kind));
            let attached = control.kind == ActionKind::Attach && self.terminal.is_active();
            let sensitive = live && !attached && availability.is_some_and(|a| a.available);
            control.button.set_sensitive(sensitive);
            let reason = availability
                .and_then(|a| a.reason.as_ref())
                .map(|reason| reason.message.as_str())
                .filter(|_| !sensitive && live);
            control.button.set_tooltip_text(reason);
            match (&control.reason, reason) {
                (Some(label), reason) => {
                    label.set_label(reason.unwrap_or_default());
                    // The reason sits beside its item and goes with it.
                    label.set_visible(control.button.is_visible() && reason.is_some());
                }
                (None, Some(reason)) => reasons.push(format!(
                    "{}: {reason}",
                    control
                        .button
                        .label()
                        .unwrap_or_default()
                        .trim_end_matches('…')
                )),
                (None, None) => (),
            }
        }
        if !live {
            reasons.insert(0, "Last known data · reconnect to act".into());
            self.terminal.end("Disconnected or stale · read-only");
        }
        self.reasons.set_label(&reasons.join("\n"));
    }
}

fn available(agent: &Option<AgentPresentation>, kind: ActionKind) -> bool {
    agent
        .as_ref()
        .and_then(|agent| agent.actions.iter().find(|a| a.action == kind))
        .is_some_and(|a| a.available)
}

/// A row inside an already padded pane: indented under its section title
/// rather than at the page edge.
fn inset_row() -> gtk::Box {
    let row = common::row();
    row.set_margin_start(12);
    row.set_margin_end(12);
    row
}
