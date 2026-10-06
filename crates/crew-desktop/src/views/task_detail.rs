use super::{
    common::{self, Facts},
    evidence::EvidenceView,
};
use crew_client::api::*;
use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub struct TaskInspector {
    pub root: gtk::Box,
    title: gtk::Label,
    chip: gtk::Label,
    facts: Facts,
    description: gtk::Label,
    outcome: Facts,
    dependencies: gtk::Label,
    cycle: gtk::Box,
    artifacts: gtk::Box,
    last_artifacts: Rc<RefCell<Vec<crew_protocol::Artifact>>>,
    evidence: EvidenceView,
}
impl TaskInspector {
    pub fn new(key: EntityKey, read: Rc<dyn Fn(ResourceKey)>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let body = common::column();
        body.set_spacing(24);
        let header = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let identity = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let title = common::title("Loading task…");
        title.set_hexpand(true);
        identity.append(&title);
        let chip = common::status_chip("", Tone::Neutral);
        chip.set_visible(false);
        identity.append(&chip);
        header.append(&identity);
        let facts = Facts::new();
        header.append(&facts.root);
        body.append(&header);
        let description = common::selectable("");
        description.set_visible(false);
        body.append(&description);
        let outcome = Facts::new();
        body.append(&outcome.root);
        let dependencies = common::selectable("");
        dependencies.set_visible(false);
        body.append(&dependencies);
        let cycle = common::warning("This task is part of a dependency cycle.");
        cycle.set_visible(false);
        body.append(&cycle);
        let artifacts = gtk::Box::new(gtk::Orientation::Vertical, 8);
        body.append(&artifacts);
        let tabs = common::Tabs::new("Task sections");
        tabs.append("Task", &common::scroll(&body));
        let resource = ResourceKey {
            entity: key,
            kind: ResourceKind::TaskCard,
            options: ReadOptions::Default,
        };
        let evidence = EvidenceView::new(move || read(resource.clone()));
        tabs.append("Complete card", &evidence.root);
        root.append(&tabs.root);
        Self {
            root,
            title,
            chip,
            facts,
            description,
            outcome,
            dependencies,
            cycle,
            artifacts,
            evidence,
            last_artifacts: Rc::new(RefCell::new(Vec::new())),
        }
    }
    pub fn update(&self, data: &TaskDetailData) {
        let Some(item) = &data.task else {
            self.title.set_label("This task is no longer on the board");
            self.chip.set_visible(false);
            return;
        };
        let task = &item.task;
        self.title.set_label(&task.title);
        common::set_status_chip(
            &self.chip,
            task.status.as_str(),
            common::task_tone(task.status),
        );
        self.chip.set_visible(true);
        self.facts.set(&[
            ("Task", Some(task.id.clone())),
            (
                "Assigned to",
                Some(
                    task.assigned_agent
                        .clone()
                        .unwrap_or_else(|| "Unassigned".into()),
                ),
            ),
            ("Reviewer", task.reviewer.clone()),
            ("Progress", task.progress.map(|p| format!("{p}%"))),
            ("Timeout", task.timeout_secs.map(|s| format!("{s} seconds"))),
            ("Created", Some(common::timestamp(task.created_at))),
            ("Started", task.started_at.map(common::timestamp)),
            ("Completed", task.completed_at.map(common::timestamp)),
        ]);
        self.description.set_label(&task.description);
        self.description.set_visible(!task.description.is_empty());
        self.outcome
            .set(&[("Note", task.note.clone()), ("Result", task.result.clone())]);
        let dependencies = item
            .dependencies
            .iter()
            .map(|d| {
                format!(
                    "{} ({})",
                    d.id,
                    d.status.map_or("missing".into(), |s| s.to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.dependencies
            .set_label(&format!("Depends on {dependencies}"));
        self.dependencies.set_visible(!item.dependencies.is_empty());
        self.cycle.set_visible(item.in_cycle);
        if *self.last_artifacts.borrow() != task.artifacts {
            common::clear(&self.artifacts);
            if !task.artifacts.is_empty() {
                self.artifacts.append(&common::heading("Artifacts"));
            }
            for artifact in &task.artifacts {
                let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
                row.append(&common::muted(&artifact.kind));
                row.append(&common::selectable(&artifact.value));
                if !artifact.description.is_empty() {
                    row.append(&common::selectable(&artifact.description));
                }
                let copy = gtk::Button::with_label(&format!("Copy {}", artifact.kind));
                copy.set_halign(gtk::Align::Start);
                let value = artifact.value.clone();
                copy.connect_clicked(move |button| button.clipboard().set_text(&value));
                row.append(&copy);
                self.artifacts.append(&row);
            }
            *self.last_artifacts.borrow_mut() = task.artifacts.clone();
        }
        self.evidence.update(&data.card);
    }
    pub fn resource(&self, state: &ResourceState) {
        self.evidence.update(state);
    }
}
