//! The Team page header: which project this is, what it is for, how the team
//! stands, who is waiting on the operator, and the session's warnings.

use std::rc::Rc;

use crew_client::api::OverviewData;
use gtk::prelude::*;

use super::common;

#[derive(Clone)]
pub struct OverviewHeader {
    pub root: gtk::Box,
    title: gtk::Label,
    path: gtk::Label,
    objective: gtk::Label,
    counts: gtk::Label,
    stale: gtk::Label,
    live: std::rc::Rc<std::cell::Cell<bool>>,
    asking: gtk::Box,
    question: gtk::Label,
    pub answer: gtk::Button,
    warnings: gtk::Box,
}

impl OverviewHeader {
    pub fn new(answer: Rc<dyn Fn()>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(24);
        root.set_margin_bottom(12);
        root.set_margin_start(24);
        root.set_margin_end(24);
        let title = common::text("");
        title.add_css_class("title-2");
        root.append(&title);
        let path = common::muted("");
        path.set_wrap(false);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        root.append(&path);
        let objective = common::text("");
        objective.set_max_width_chars(90);
        objective.set_visible(false);
        root.append(&objective);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.set_margin_top(4);
        // Retained figures are marked until the next live snapshot.
        let stale = common::status_chip("Last known", crew_client::api::Tone::Warning);
        stale.set_visible(false);
        line.append(&stale);
        let counts = common::text("");
        counts.set_hexpand(true);
        counts.set_valign(gtk::Align::Center);
        line.append(&counts);
        root.append(&line);
        // Who is waiting on the operator, with the way to answer beside it.
        let asking = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let question = common::text("");
        question.add_css_class("crew-attention");
        question.set_hexpand(true);
        question.set_valign(gtk::Align::Center);
        asking.append(&question);
        let answer_button = gtk::Button::with_label("Answer");
        answer_button.add_css_class("suggested-action");
        answer_button.set_valign(gtk::Align::Center);
        answer_button.connect_clicked(move |_| answer());
        asking.append(&answer_button);
        asking.set_visible(false);
        root.append(&asking);
        let warnings = gtk::Box::new(gtk::Orientation::Vertical, 4);
        warnings.set_visible(false);
        root.append(&warnings);
        Self {
            root,
            title,
            path,
            objective,
            counts,
            stale,
            live: std::rc::Rc::new(std::cell::Cell::new(true)),
            asking,
            question,
            answer: answer_button,
            warnings,
        }
    }

    pub fn update(&self, overview: &OverviewData) {
        let session = overview.session.as_ref();
        let workspace = session.map_or("", |s| s.workspace.as_str());
        let project = session
            .and_then(|s| s.project.as_deref())
            .filter(|project| !project.is_empty())
            .or_else(|| workspace.rsplit('/').find(|part| !part.is_empty()))
            .unwrap_or("No workspace loaded");
        self.title.set_label(project);
        self.path.set_label(workspace);
        self.path.set_tooltip_text(Some(workspace));
        self.path.set_visible(!workspace.is_empty());
        let objective = session.map_or("", |s| s.objective.trim());
        self.objective.set_label(objective);
        self.objective.set_visible(!objective.is_empty());
        self.counts.set_label(&format!(
            "{} · {} working · {} of {} done",
            plural(overview.agent_count, "agent", "agents"),
            overview.working_count,
            overview.done_count,
            plural(overview.task_count, "task", "tasks"),
        ));
        self.set_live(self.live.get());
        common::clear(&self.warnings);
        for warning in &overview.warnings {
            self.warnings.append(&common::warning(&warning.text));
        }
        self.warnings.set_visible(!overview.warnings.is_empty());
    }

    /// Mark the counts and warnings as last known while not live.
    pub fn set_live(&self, live: bool) {
        self.live.set(live);
        self.stale
            .set_visible(!live && !self.counts.label().is_empty());
    }

    /// Nothing loaded: only a title saying so.
    pub fn clear(&self, title: &str) {
        self.title.set_label(title);
        for label in [&self.path, &self.objective] {
            label.set_visible(false);
        }
        self.counts.set_label("");
        self.stale.set_visible(false);
        self.asking.set_visible(false);
        common::clear(&self.warnings);
        self.warnings.set_visible(false);
    }

    /// Name who is asking; with no open question, show nothing at all.
    pub fn set_questions(&self, questions: &[crew_protocol::Message]) {
        let mut askers: Vec<String> = Vec::new();
        for question in questions {
            let asker = question.asker();
            if !askers.contains(&asker) {
                askers.push(asker);
            }
        }
        let text = match questions.len() {
            0 => String::new(),
            1 => format!("{} asked a question", askers[0]),
            count => format!("{count} questions from {}", askers.join(", ")),
        };
        self.question.set_label(&text);
        self.asking.set_visible(!questions.is_empty());
    }
}

fn plural(count: u32, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}
