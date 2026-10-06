use super::{
    common::{self, EntityList, Select},
    evidence::EvidenceView,
};
use crate::terminal::TerminalView;
use crew_client::api::*;
use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};

pub struct AgentCallbacks {
    pub action: Rc<dyn Fn(ActionIntent)>,
    pub select: Select,
    pub compose: Rc<dyn Fn(String)>,
    pub model: Rc<dyn Fn(EntityKey)>,
    pub read: Rc<dyn Fn(ResourceKey)>,
    pub attach: Rc<dyn Fn(EntityKey)>,
    pub prompt: Rc<dyn Fn(EntityKey)>,
}

#[derive(Clone)]
pub struct AgentInspector {
    pub root: gtk::Box,
    pub terminal: TerminalView,
    key: EntityKey,
    summary: gtk::Label,
    reasons: gtk::Label,
    tasks: EntityList,
    messages: EntityList,
    controls: Vec<(ActionKind, gtk::Button)>,
    evidence: Vec<(ResourceKey, EvidenceView)>,
    latest: Rc<RefCell<Option<AgentPresentation>>>,
}
impl AgentInspector {
    pub fn new(key: EntityKey, callbacks: AgentCallbacks) -> Self {
        let Entity::Agent(id) = &key.entity else {
            panic!("agent inspector key")
        };
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let tabs = gtk::Notebook::new();
        tabs.set_scrollable(true);
        tabs.set_hexpand(true);
        tabs.set_vexpand(true);
        let overview = common::column();
        let summary = common::text("Loading agent…");
        overview.append(&summary);
        let actions = gtk::FlowBox::new();
        actions.set_selection_mode(gtk::SelectionMode::None);
        actions.set_homogeneous(false);
        actions.set_max_children_per_line(3);
        actions.set_min_children_per_line(1);
        let mut controls = Vec::new();
        for (name, kind, action) in [
            ("Pause", ActionKind::Pause, AgentAction::Pause),
            ("Unpause", ActionKind::Resume, AgentAction::Resume),
            ("Compact", ActionKind::Compact, AgentAction::Compact),
            ("Restart", ActionKind::Restart, AgentAction::Restart),
            (
                "Restart fresh",
                ActionKind::RestartFresh,
                AgentAction::RestartFresh,
            ),
            ("Kill", ActionKind::Kill, AgentAction::Kill),
            ("Jump to host pane", ActionKind::Focus, AgentAction::Focus),
        ] {
            let button = gtk::Button::with_label(name);
            button.connect_clicked({
                let callback = callbacks.action.clone();
                let scope = key.scope.clone();
                let id = id.clone();
                move |_| {
                    callback(ActionIntent::Agent {
                        scope: scope.clone(),
                        id: id.clone(),
                        action,
                    })
                }
            });
            actions.insert(&button, -1);
            controls.push((kind, button));
        }
        let model = gtk::Button::with_label("Model and CLI");
        model.connect_clicked({
            let callback = callbacks.model;
            let key = key.clone();
            move |_| callback(key.clone())
        });
        actions.insert(&model, -1);
        controls.push((ActionKind::SetModel, model));
        let compose = gtk::Button::with_label("Message agent");
        compose.connect_clicked({
            let callback = callbacks.compose;
            let id = id.clone();
            move |_| callback(id.clone())
        });
        actions.insert(&compose, -1);
        controls.push((ActionKind::SendMessage, compose));
        overview.append(&actions);
        let reasons = common::text("");
        reasons.add_css_class("crew-muted");
        overview.append(&reasons);
        overview.append(&common::heading("Assigned and review tasks"));
        let tasks = EntityList::new("Assigned and review tasks", callbacks.select.clone());
        tasks.root.set_min_content_height(160);
        overview.append(&tasks.root);
        overview.append(&common::heading("Related messages"));
        let messages = EntityList::new("Agent messages", callbacks.select);
        messages.root.set_min_content_height(160);
        overview.append(&messages.root);
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
        let attach = gtk::Button::with_label("Attach PTY terminal");
        attach.connect_clicked({
            let callback = callbacks.attach;
            let key = key.clone();
            move |_| callback(key.clone())
        });
        terminal_page.append(&attach);
        controls.push((ActionKind::Attach, attach));
        terminal_page.append(&terminal.root);
        tabs.append_page(&terminal_page, Some(&gtk::Label::new(Some("Terminal"))));
        let screen = ResourceKey {
            entity: key.clone(),
            kind: ResourceKind::Output,
            options: ReadOptions::Output {
                lines: 300,
                screen: true,
                ansi: false,
            },
        };
        let screen_page = gtk::Box::new(gtk::Orientation::Vertical, 8);
        screen_page.append(&common::text("Herdr screen is a bounded read. Send prompt submits explicitly to the host agent; it is separate from a team message."));
        let prompt = gtk::Button::with_label("Send prompt");
        prompt.connect_clicked({
            let callback = callbacks.prompt;
            let key = key.clone();
            move |_| callback(key.clone())
        });
        screen_page.append(&prompt);
        controls.push((ActionKind::AgentInput, prompt));
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
            summary,
            reasons,
            tasks,
            messages,
            controls,
            evidence,
            latest: Rc::new(RefCell::new(None)),
        }
    }
    pub fn update(&self, data: &AgentDetailData, live: bool) {
        *self.latest.borrow_mut() = data.agent.clone();
        let Some(member) = &data.agent else {
            self.summary.set_label("Agent no longer present");
            self.set_live(false);
            return;
        };
        let a = &member.agent;
        let current = member.current_work.as_ref().map_or("None".into(), |work| {
            format!(
                "{}{} · {}",
                work.task_id,
                if work.done { " (completed)" } else { "" },
                work.elapsed_ms
                    .map_or("Start time not reported".into(), |ms| format!(
                        "{} seconds elapsed",
                        ms / 1000
                    ))
            )
        });
        let mut summary = format!("{} · {}\n{} · {}\n\nCurrent work: {}\nWaiting cards: {}\nActivity: {}\nModel: {}\nProgress: {}\nContext: {}\n\nRuntime: {}\nBranch: {}\nWorkspace: {}\nMailbox: {} unread / {} received / {} sent\nSubagents: {}\nSession: {}\nBackend: {}\nProcess: {}\nRestarts: {}\nLast activity: {}", a.name, a.id, a.role, member.status_label, current, member.waiting_task_ids.join(", "), a.activity,
            a.model_label().unwrap_or("Not reported".into()), a.progress.map_or("Not reported".into(), |p| format!("{p}%")),
            a.context.as_ref().map_or("Not reported".into(), |c| format!("{}%", c.percent)), a.runtime,
            a.branch.as_deref().unwrap_or("None"), a.workdir, a.inbox_unread, a.inbox_total, a.outbox_total, a.subagents,
            a.session_id.as_deref().unwrap_or("Not reported"), a.backend, a.pid.map_or("Not reported".into(), |p| p.to_string()), a.restarts, common::timestamp(a.last_activity_at));
        for warning in &member.warnings {
            summary.push_str(&format!("\n{}", warning.text));
        }
        if let Some(parent) = &member.satellite_parent_id {
            summary.push_str(&format!("\nSubagent of {parent}"));
        }
        self.summary.set_label(&summary);
        self.set_live(live);
        self.tasks.replace(
            data.assigned_tasks
                .iter()
                .chain(&data.review_tasks)
                .map(|t| {
                    let label = format!("{} · {} · {}", t.task.id, t.task.status, t.task.title);
                    (
                        EntityKey {
                            scope: self.key.scope.clone(),
                            entity: Entity::Task(t.task.id.clone()),
                        },
                        common::text(&label).upcast(),
                        label,
                    )
                })
                .collect(),
        );
        self.messages.replace(
            data.recent_messages
                .items
                .iter()
                .map(|m| {
                    let label = format!("{} to {} · {}\n{}", m.from, m.to, m.kind, m.body);
                    (
                        EntityKey {
                            scope: self.key.scope.clone(),
                            entity: Entity::Message(m.id.clone()),
                        },
                        common::text(&label).upcast(),
                        label,
                    )
                })
                .collect(),
        );
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
        let latest = self.latest.borrow();
        let mut reasons = Vec::new();
        for (kind, button) in &self.controls {
            let availability = latest
                .as_ref()
                .and_then(|agent| agent.actions.iter().find(|a| a.action == *kind));
            button.set_sensitive(live && availability.is_some_and(|a| a.available));
            if let Some(reason) = availability.and_then(|a| a.reason.as_ref()) {
                button.set_tooltip_text(Some(&reason.message));
                reasons.push(format!(
                    "{}: {}",
                    button.label().unwrap_or_default(),
                    reason.message
                ));
            } else {
                button.set_tooltip_text(None);
            }
        }
        if !live {
            reasons.insert(
                0,
                "Retained data · reconnect and synchronize before acting.".into(),
            );
            self.terminal.end("Disconnected or stale · read-only");
        }
        self.reasons.set_label(&reasons.join("\n"));
    }
}
