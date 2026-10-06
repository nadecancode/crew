use super::{common, evidence::EvidenceView};
use crew_client::api::*;
use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub struct TaskInspector {
    pub root: gtk::Box,
    summary: gtk::Label,
    artifacts: gtk::Box,
    last_artifacts: Rc<RefCell<Vec<crew_protocol::Artifact>>>,
    evidence: EvidenceView,
}
impl TaskInspector {
    pub fn new(key: EntityKey, read: Rc<dyn Fn(ResourceKey)>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let body = common::column();
        let summary = common::selectable("Loading task…");
        body.append(&summary);
        let artifacts = gtk::Box::new(gtk::Orientation::Vertical, 8);
        body.append(&artifacts);
        let tabs = gtk::Notebook::new();
        tabs.set_scrollable(true);
        tabs.set_vexpand(true);
        tabs.append_page(&common::scroll(&body), Some(&gtk::Label::new(Some("Task"))));
        let resource = ResourceKey {
            entity: key,
            kind: ResourceKind::TaskCard,
            options: ReadOptions::Default,
        };
        let evidence = EvidenceView::new(move || read(resource.clone()));
        tabs.append_page(
            &evidence.root,
            Some(&gtk::Label::new(Some("Complete card"))),
        );
        root.append(&tabs);
        Self {
            root,
            summary,
            artifacts,
            evidence,
            last_artifacts: Rc::new(RefCell::new(Vec::new())),
        }
    }
    pub fn update(&self, data: &TaskDetailData) {
        let Some(item) = &data.task else {
            self.summary.set_label("Task no longer present");
            return;
        };
        let task = &item.task;
        let text = format!("{} · {}\n{}\nAssigned: {}\nReviewer: {}\nProgress: {}\n\n{}\n\nNote: {}\nResult: {}\nTimeout: {}\nCreated: {}\nStarted: {}\nCompleted: {}\n\nDependencies:\n{}{}",
            task.id, task.title, task.status, task.assigned_agent.as_deref().unwrap_or("Unassigned"), task.reviewer.as_deref().unwrap_or("None"),
            task.progress.map_or("Not reported".into(), |p| format!("{p}%")), task.description, task.note.as_deref().unwrap_or("None"), task.result.as_deref().unwrap_or("None"),
            task.timeout_secs.map_or("Not reported".into(), |s| format!("{s} seconds")), common::timestamp(task.created_at), task.started_at.map_or("Not reported".into(), common::timestamp), task.completed_at.map_or("Not reported".into(), common::timestamp),
            item.dependencies.iter().map(|d| format!("{} · {}", d.id, d.status.map_or("Missing".into(), |s| s.to_string()))).collect::<Vec<_>>().join("\n"), if item.in_cycle { "\nDependency cycle" } else { "" });
        if self.summary.label().as_str() != text {
            self.summary.set_label(&text);
        }
        if *self.last_artifacts.borrow() != task.artifacts {
            common::clear(&self.artifacts);
            for artifact in &task.artifacts {
                self.artifacts.append(&common::selectable(&format!(
                    "{}: {}\n{}",
                    artifact.kind, artifact.value, artifact.description
                )));
                let copy = gtk::Button::with_label(&format!("Copy {}", artifact.kind));
                let value = artifact.value.clone();
                copy.connect_clicked(move |button| button.clipboard().set_text(&value));
                self.artifacts.append(&copy);
            }
            *self.last_artifacts.borrow_mut() = task.artifacts.clone();
        }
        self.evidence.update(&data.card);
    }
    pub fn resource(&self, state: &ResourceState) {
        self.evidence.update(state);
    }
}
