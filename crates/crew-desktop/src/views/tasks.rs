use crew_client::api::*;
use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};

use super::common::{self, EntityList, Select};
use super::graph::{Edge, EdgeKind, Graph, Node};

#[derive(Clone)]
pub struct TasksView {
    pub root: gtk::Box,
    list: EntityList,
    empty: gtk::ScrolledWindow,
    graph: Graph,
}

impl TasksView {
    pub fn new(select: Select) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // A list selection also marks the graph, so switching presentation
        // keeps the same selected task.
        let marked: Rc<RefCell<Option<Graph>>> = Rc::new(RefCell::new(None));
        let list = EntityList::new(
            "Task board",
            Rc::new({
                let marked = marked.clone();
                let select = select.clone();
                move |key: EntityKey| {
                    if let Some(graph) = marked.borrow().as_ref() {
                        graph.set_selected(Some(&key));
                    }
                    select(key);
                }
            }),
        );
        let graph = Graph::new("Task dependencies", select, |layer| {
            Some(if layer == 0 {
                "No dependencies".into()
            } else {
                format!("Layer {layer}")
            })
        });
        *marked.borrow_mut() = Some(graph.clone());
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
    pub fn graph(&self) -> &Graph {
        &self.graph
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
        let (nodes, edges) = topology(scope, data);
        self.graph.update(nodes, edges);
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
                        task_label(task),
                    )
                })
                .collect(),
        );
    }
}

fn task_label(task: &crew_protocol::Task) -> String {
    format!(
        "{}, {}, {}, {}",
        task.id,
        task.title,
        task.status,
        task.assigned_agent.as_deref().unwrap_or("unassigned")
    )
}

/// Dependency graph: columns are the shared dependency layers, rows keep the
/// board order, and each edge runs from a dependency to the task waiting on it.
/// Unresolved dependencies stay explicit on the dependent node.
fn topology(scope: &ClientScope, data: &TasksData) -> (Vec<Node>, Vec<Edge>) {
    let index = |id: &str| data.tasks.iter().position(|item| item.task.id == id);
    let mut edges = Vec::new();
    let mut required_by: Vec<Vec<&str>> = vec![Vec::new(); data.tasks.len()];
    for (to, item) in data.tasks.iter().enumerate() {
        for dependency in &item.dependencies {
            if let Some(from) = index(&dependency.id) {
                edges.push(Edge {
                    from,
                    to,
                    kind: EdgeKind::Dependency,
                });
                required_by[from].push(&item.task.id);
            }
        }
    }
    let nodes = data
        .tasks
        .iter()
        .zip(required_by)
        .map(|(item, required_by)| {
            let task = &item.task;
            let mut lines = vec![format!(
                "{} · {}",
                task.id,
                task.assigned_agent.as_deref().unwrap_or("Unassigned")
            )];
            if let Some(progress) = task.progress {
                lines.push(format!("{progress}%"));
            }
            let missing: Vec<String> = item
                .dependencies
                .iter()
                .filter(|d| d.status.is_none())
                .map(|d| format!("‘{}’", d.id))
                .collect();
            if !missing.is_empty() {
                lines.push(format!(
                    "Depends on {} (not in this workspace)",
                    missing.join(", ")
                ));
            }
            if item.in_cycle {
                lines.push("Dependency cycle".into());
            }
            let mut relations = Vec::new();
            if !item.dependencies.is_empty() {
                relations.push(format!(
                    "Depends on {}",
                    item.dependencies
                        .iter()
                        .map(|d| format!(
                            "{} ({})",
                            d.id,
                            d.status.map_or("missing", |status| status.as_str())
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !required_by.is_empty() {
                relations.push(format!("Required by {}", required_by.join(", ")));
            }
            if item.in_cycle {
                relations.push("Part of a dependency cycle".into());
            }
            Node {
                key: Some(EntityKey {
                    scope: scope.clone(),
                    entity: Entity::Task(task.id.clone()),
                }),
                title: task.title.clone(),
                status: Some((
                    task.status.as_str().to_owned(),
                    common::task_tone(task.status),
                )),
                lines,
                layer: item.layer,
                label: task_label(task),
                relations: if relations.is_empty() {
                    "No dependencies".into()
                } else {
                    relations.join(". ")
                },
            }
        })
        .collect();
    (nodes, edges)
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
            body.append(&common::text(&artifact.description));
        }
    }
    if let Some(error) = &data.card.error {
        body.append(&common::text(&error.message));
    }
    common::scroll(&body)
}
