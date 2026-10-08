use super::{
    common::{self, EntityList, Facts, Select},
    evidence::EvidenceView,
};
use crate::terminal::TerminalView;
use crew_client::api::*;
use crew_protocol::{
    toolbox_origin_label, AgentToolbox, ToolboxItem, ToolboxItemCode, ToolboxSource,
    ToolboxSourceCode, TOOLBOX_PENDING, TOOLBOX_UNPINNED,
};
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

/// The member's effective toolbox (notes/toolboxes.md §Display): the summary,
/// a pending change with Restart, the items by kind, then the runtime's
/// sources. Hidden for a member without one. Groups are rebuilt only when the
/// toolbox changes, so a pushed update does not move a reader's selection.
#[derive(Clone)]
struct ToolboxView {
    root: gtk::Box,
    summary: gtk::Label,
    pending: gtk::Box,
    unpinned: gtk::Label,
    groups: gtk::Box,
    definitions: Facts,
    shown: Rc<RefCell<Option<AgentToolbox>>>,
}

impl ToolboxView {
    fn new(restart: &gtk::Button, reason: &gtk::Label) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.append(&common::heading_level("Toolbox", 2));
        let summary = common::selectable("");
        root.append(&summary);
        let pending = gtk::Box::new(gtk::Orientation::Vertical, 8);
        pending.append(&common::warning(TOOLBOX_PENDING));
        restart.set_halign(gtk::Align::Start);
        pending.append(restart);
        pending.append(reason);
        root.append(&pending);
        let unpinned = common::muted(TOOLBOX_UNPINNED);
        root.append(&unpinned);
        let groups = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.append(&groups);
        let definitions = Facts::new();
        root.append(
            &gtk::Expander::builder()
                .label("Definitions")
                .child(&definitions.root)
                .build(),
        );
        root.set_visible(false);
        Self {
            root,
            summary,
            pending,
            unpinned,
            groups,
            definitions,
            shown: Rc::default(),
        }
    }

    fn update(&self, toolbox: Option<&AgentToolbox>) {
        if self.shown.borrow().as_ref() == toolbox {
            return;
        }
        *self.shown.borrow_mut() = toolbox.cloned();
        self.root.set_visible(toolbox.is_some());
        let Some(toolbox) = toolbox else {
            return;
        };
        // `Toolbox 3f9a0c21e7b4 · 4 items · 1 not available`, the fingerprint
        // monospaced, the whole of it in the tooltip.
        let short = toolbox.short_fingerprint();
        let summary = toolbox.summary();
        let rest = summary
            .strip_prefix(&format!("Toolbox {short}"))
            .unwrap_or_default();
        self.summary.set_markup(&format!(
            "Toolbox <span font_family=\"monospace\">{}</span>{}",
            gtk::glib::markup_escape_text(short),
            gtk::glib::markup_escape_text(rest)
        ));
        self.summary.set_tooltip_text(Some(&toolbox.fingerprint));
        self.pending
            .set_visible(toolbox.pending_fingerprint.is_some());
        self.unpinned.set_visible(toolbox.definitions_unpinned());
        common::clear(&self.groups);
        let mut kinds: Vec<_> = toolbox.items.iter().map(|item| item.kind).collect();
        kinds.sort();
        kinds.dedup();
        for kind in kinds {
            self.groups
                .append(&common::heading_level(kind.heading(), 3));
            for item in toolbox.items.iter().filter(|item| item.kind == kind) {
                self.groups.append(&toolbox_item(item));
            }
        }
        if !toolbox.sources.is_empty() {
            self.groups.append(&common::heading_level("Sources", 3));
            for source in &toolbox.sources {
                self.groups.append(&toolbox_source(source));
            }
        }
        let or_unknown = |value: Option<String>| value.unwrap_or_else(|| "unknown".into());
        self.definitions.set(&[
            ("Fingerprint", Some(toolbox.fingerprint.clone())),
            ("After restart", toolbox.pending_fingerprint.clone()),
            (
                "Definition digest",
                Some(
                    toolbox
                        .definition_digest
                        .clone()
                        .unwrap_or_else(|| "unavailable".into()),
                ),
            ),
            (
                "Tools",
                Some(or_unknown(toolbox.tool_count.map(|n| n.to_string()))),
            ),
            (
                "Definition bytes",
                Some(or_unknown(
                    toolbox.definition_bytes.map(|n| format!("{n} bytes")),
                )),
            ),
        ]);
    }
}

/// One item: its name, then origin and status in words (a warning when it is
/// not usable), then its command, URL and references, wrapped and copyable.
fn toolbox_item(item: &ToolboxItem) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.set_margin_start(12);
    row.append(&common::text(&item.name));
    let mut facts = format!(
        "{} · {}",
        toolbox_origin_label(&item.origin),
        item.status.text
    );
    if item.required {
        facts.push_str(" · required");
    }
    match item.status.code {
        ToolboxItemCode::Active | ToolboxItemCode::Planned | ToolboxItemCode::NotApplicable => {
            row.append(&common::muted(&facts))
        }
        _ => row.append(&common::warning(&facts)),
    }
    if let Some(detail) = &item.detail {
        let detail = common::selectable(detail);
        detail.add_css_class("crew-muted");
        detail.add_css_class("monospace");
        row.append(&detail);
    }
    row
}

/// One runtime source: `Your Codex MCP servers: Disabled (ambient, docs-local)`
/// and its reason below.
fn toolbox_source(source: &ToolboxSource) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.set_margin_start(12);
    let mut line = format!("{}: {}", source.label, source.status.text);
    if !source.names.is_empty() {
        line.push_str(&format!(" ({})", source.names.join(", ")));
    }
    if source.status.code == ToolboxSourceCode::CannotBeExcluded {
        row.append(&common::warning(&line));
    } else {
        row.append(&common::text(&line));
    }
    if let Some(detail) = &source.detail {
        row.append(&common::muted(detail));
    }
    row
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
    toolbox: ToolboxView,
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
        // Six sections: the tab row wraps at narrow widths rather than
        // scrolling tabs out of reach.
        let tabs = common::Tabs::new("Agent sections");
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
            (
                "Unpause",
                ActionKind::Resume,
                Some(AgentAction::Resume),
                false,
            ),
            (
                "Compact",
                ActionKind::Compact,
                Some(AgentAction::Compact),
                false,
            ),
            (
                "Restart",
                ActionKind::Restart,
                Some(AgentAction::Restart),
                false,
            ),
            ("Model and CLI…", ActionKind::SetModel, None, false),
            (
                "Jump to host pane",
                ActionKind::Focus,
                Some(AgentAction::Focus),
                false,
            ),
            (
                "Restart fresh…",
                ActionKind::RestartFresh,
                Some(AgentAction::RestartFresh),
                true,
            ),
            ("Kill…", ActionKind::Kill, Some(AgentAction::Kill), true),
        ] {
            // The reason sits right under its item, inside the menu's width.
            let item = gtk::Box::new(gtk::Orientation::Vertical, 0);
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
            reason.set_max_width_chars(36);
            reason.set_margin_start(12);
            reason.set_margin_bottom(6);
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
        // A pending toolbox change offers the plain restart: the same action,
        // availability and reason as the Actions menu's Restart.
        let restart = gtk::Button::with_label("Restart");
        restart.connect_clicked({
            let callback = callbacks.action.clone();
            let key = key.clone();
            move |_| {
                let Entity::Agent(id) = &key.entity else {
                    return;
                };
                callback(ActionIntent::Agent {
                    scope: key.scope.clone(),
                    id: id.clone(),
                    action: AgentAction::Restart,
                })
            }
        });
        let restart_reason = common::reason();
        let toolbox = ToolboxView::new(&restart, &restart_reason);
        controls.push(Control {
            kind: ActionKind::Restart,
            button: restart,
            reason: Some(restart_reason),
        });
        overview.append(&toolbox.root);
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
        tabs.append("Overview", &common::scroll(&overview));
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
            tabs.append(name, &view.root);
            evidence.push((resource, view));
        }
        let terminal = TerminalView::new(callbacks.action.clone());
        let terminal_page = gtk::Box::new(gtk::Orientation::Vertical, 8);
        terminal_page.append(&terminal.root);
        let terminal_index = tabs.append("Terminal", &terminal_page);
        attach.connect_clicked({
            let callback = callbacks.attach;
            let key = key.clone();
            let tabs = tabs.clone();
            move |_| {
                tabs.select(terminal_index);
                callback(key.clone())
            }
        });
        // Attach is offered only while nothing is attached here.
        let latest: Rc<RefCell<Option<AgentPresentation>>> = Rc::new(RefCell::new(None));
        let live = Rc::new(Cell::new(false));
        // An attach the operator asked for here takes focus to the terminal;
        // one that ends while its controls have focus gives it back to Attach.
        let requested = Rc::new(Cell::new(false));
        attach.connect_clicked({
            let requested = requested.clone();
            move |_| requested.set(true)
        });
        terminal.connect_attached({
            let attach = attach.downgrade();
            // Weak: the listener is stored in the terminal view itself.
            let panel = terminal.root.downgrade();
            let latest = latest.clone();
            let live = live.clone();
            move |attached| {
                let Some(attach) = attach.upgrade() else {
                    return;
                };
                let Some(panel) = panel.upgrade() else {
                    return;
                };
                let in_terminal = gtk::prelude::WidgetExt::root(&attach)
                    .and_then(|root| root.focus())
                    .is_some_and(|focus| focus.is_ancestor(&panel));
                keep_focus_on_disable(
                    attach.upcast_ref(),
                    !attached && live.get() && available(&latest.borrow(), ActionKind::Attach),
                );
                if attached && requested.replace(false) {
                    // After the panel has adopted the renderer that attached.
                    let panel = panel.downgrade();
                    gtk::glib::idle_add_local_once(move || {
                        if let Some(terminal) =
                            panel.upgrade().and_then(|p| shown_terminal(p.upcast_ref()))
                        {
                            focus_when_shown(terminal.upcast_ref());
                        }
                    });
                } else if !attached && in_terminal && attach.is_sensitive() {
                    attach.grab_focus();
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
        tabs.append("Host screen", &screen_page);
        tabs.connect_switched({
            let evidence: Vec<_> = evidence.iter().map(|(k, _)| k.clone()).collect();
            let read = callbacks.read;
            move |index| {
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
        root.append(&tabs.root);
        Self {
            root,
            terminal,
            key,
            title,
            chip,
            subtitle,
            warnings,
            facts,
            toolbox,
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
        // The Toolbox section below says the same with its Restart.
        let warnings: Vec<_> = member
            .warnings
            .iter()
            .filter(|warning| warning.code != TOOLBOX_WARNING)
            .collect();
        for warning in &warnings {
            self.warnings.append(&common::warning(&warning.text));
        }
        self.warnings.set_visible(!warnings.is_empty());
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
        self.toolbox.update(a.toolbox.as_ref());
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
            let shown = match control.kind {
                ActionKind::Pause => !paused,
                ActionKind::Resume => paused,
                _ => true,
            };
            control.button.set_visible(shown);
            let availability = latest
                .as_ref()
                .and_then(|agent| agent.actions.iter().find(|a| a.action == control.kind));
            let attached = control.kind == ActionKind::Attach && self.terminal.is_active();
            let sensitive = live && !attached && availability.is_some_and(|a| a.available);
            keep_focus_on_disable(control.button.upcast_ref(), sensitive);
            let reason = availability
                .and_then(|a| a.reason.as_ref())
                .map(|reason| reason.message.as_str())
                .filter(|_| !sensitive && live);
            control.button.set_tooltip_text(reason);
            match (&control.reason, reason) {
                (Some(label), reason) => {
                    label.set_label(reason.unwrap_or_default());
                    // The reason sits under its item and goes with it.
                    label.set_visible(shown && reason.is_some());
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

/// A focused control that becomes unavailable (Attach once attached, an
/// action Core no longer offers) hands focus on to the next control instead
/// of leaving nothing focused.
fn keep_focus_on_disable(control: &gtk::Widget, sensitive: bool) {
    let had_focus = gtk::prelude::WidgetExt::root(control)
        .and_then(|root| root.focus())
        .is_some_and(|focus| focus == *control || focus.is_ancestor(control));
    control.set_sensitive(sensitive);
    if had_focus && !sensitive {
        crate::shell::focus_past(control, gtk::DirectionType::TabForward);
    }
}

/// Focus `widget` now, or once it is shown (a page that is switching in).
fn focus_when_shown(widget: &gtk::Widget) {
    if widget.is_mapped() {
        widget.grab_focus();
        return;
    }
    let handler = Rc::new(RefCell::new(None));
    *handler.borrow_mut() = Some(widget.connect_map({
        let handler = handler.clone();
        move |widget| {
            widget.grab_focus();
            if let Some(id) = handler.borrow_mut().take() {
                widget.disconnect(id);
            }
        }
    }));
}

/// The terminal a panel shows: the adopted renderer replaces the panel's own.
fn shown_terminal(panel: &gtk::Widget) -> Option<vte4::Terminal> {
    let mut child = panel.first_child();
    while let Some(widget) = child {
        if let Ok(terminal) = widget.clone().downcast::<vte4::Terminal>() {
            return Some(terminal);
        }
        if let Some(found) = shown_terminal(&widget) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}
