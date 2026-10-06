use crew_client::api::*;
use gtk::prelude::*;

use super::common::{self, EntityList, Select};

#[derive(Clone)]
pub struct TasksView {
    pub root: gtk::Box,
    list: EntityList,
    empty: gtk::ScrolledWindow,
    graph: super::graph::Graph,
}

impl TasksView {
    pub fn new(select: Select) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let list = EntityList::new("Task board", select.clone());
        let graph = super::graph::Graph::new("Task dependencies", select);
        let stack = gtk::Stack::new();
        stack.add_titled(list.widget(), Some("list"), "List");
        stack.add_titled(&graph.root, Some("graph"), "Graph");
        let switcher = gtk::StackSwitcher::builder().stack(&stack).build();
        root.append(&switcher);
        let empty = super::empty_state(
            "view-list-symbolic",
            "No tasks loaded",
            "The workspace has not reported a task board yet.",
        );
        root.append(&stack);
        root.append(&empty);
        Self {
            root,
            list,
            empty,
            graph,
        }
    }
    pub fn update(&self, scope: Option<&ClientScope>, data: &TasksData) {
        let Some(scope) = scope else {
            self.list.replace(Vec::new());
            self.graph.update(Vec::new(), Vec::new());
            self.list.root.set_visible(false);
            self.empty.set_visible(true);
            return;
        };
        self.empty.set_visible(data.tasks.is_empty());
        self.graph.update(
            data.tasks
                .iter()
                .enumerate()
                .map(|(i, item)| super::graph::Node {
                    key: EntityKey {
                        scope: scope.clone(),
                        entity: Entity::Task(item.task.id.clone()),
                    },
                    label: format!(
                        "{} · {}\n{}\n{}{}",
                        item.task.id,
                        item.task.title,
                        item.task.status,
                        item.task.assigned_agent.as_deref().unwrap_or("Unassigned"),
                        if item.in_cycle {
                            " · Dependency cycle"
                        } else {
                            ""
                        }
                    ),
                    column: item.layer,
                    row: i as u32,
                })
                .collect(),
            data.tasks
                .iter()
                .flat_map(|item| {
                    item.dependencies
                        .iter()
                        .map(move |d| (d.id.clone(), item.task.id.clone()))
                })
                .collect(),
        );
        self.list.root.set_visible(!data.tasks.is_empty());
        self.list.replace(
            data.tasks
                .iter()
                .map(|item| {
                    let task = &item.task;
                    let body = common::column();
                    body.append(&common::heading(&task.title));
                    body.append(&common::text(&format!(
                        "{} · {} · {}",
                        task.id,
                        task.status,
                        task.assigned_agent.as_deref().unwrap_or("Unassigned")
                    )));
                    if let Some(progress) = task.progress {
                        body.append(&common::text(&format!("{progress}%")));
                    }
                    if let Some(note) = &task.note {
                        body.append(&common::text(note));
                    }
                    if !item.dependencies.is_empty() {
                        body.append(&common::text(&format!(
                            "Dependencies: {}",
                            item.dependencies
                                .iter()
                                .map(|dependency| format!(
                                    "{} ({})",
                                    dependency.id,
                                    dependency
                                        .status
                                        .map_or("missing".to_owned(), |status| status.to_string())
                                ))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )));
                    }
                    if item.in_cycle {
                        body.append(&common::text("Dependency cycle"));
                    }
                    (
                        EntityKey {
                            scope: scope.clone(),
                            entity: Entity::Task(task.id.clone()),
                        },
                        body.upcast(),
                        format!(
                            "{}, {}, {}, {}",
                            task.id,
                            task.title,
                            task.status,
                            task.assigned_agent.as_deref().unwrap_or("unassigned")
                        ),
                    )
                })
                .collect(),
        );
    }
}

pub fn detail(data: &TaskDetailData) -> gtk::ScrolledWindow {
    let body = common::column();
    let Some(item) = &data.task else {
        body.append(&common::heading("Task no longer present"));
        return common::scroll(&body);
    };
    let task = &item.task;
    body.append(&common::heading(&task.title));
    common::fact(&body, "Task", &task.id);
    common::fact(&body, "State", task.status.as_str());
    common::fact(
        &body,
        "Assigned to",
        task.assigned_agent.as_deref().unwrap_or("Unassigned"),
    );
    common::fact(
        &body,
        "Reviewer",
        task.reviewer.as_deref().unwrap_or("None"),
    );
    common::fact(&body, "Description", &task.description);
    if let Some(note) = &task.note {
        common::fact(&body, "Note", note);
    }
    if let Some(result) = &task.result {
        common::fact(&body, "Result", result);
    }
    for dependency in &item.dependencies {
        common::fact(
            &body,
            "Dependency",
            &format!(
                "{} · {}",
                dependency.id,
                dependency
                    .status
                    .map_or("Missing".to_owned(), |status| status.to_string())
            ),
        );
    }
    if item.in_cycle {
        body.append(&common::text(
            "This task participates in a dependency cycle.",
        ));
    }
    if let Some(timeout) = task.timeout_secs {
        common::fact(&body, "Timeout", &format!("{timeout} seconds"));
    }
    for artifact in &task.artifacts {
        common::fact(&body, &artifact.kind, &artifact.value);
        if !artifact.description.is_empty() {
            body.append(&common::selectable(&artifact.description));
        }
    }
    if let Some(error) = &data.card.error {
        body.append(&common::text(&error.message));
    }
    common::scroll(&body)
}
