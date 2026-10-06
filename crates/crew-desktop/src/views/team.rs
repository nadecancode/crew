use crew_client::api::*;
use gtk::prelude::*;
use std::rc::Rc;

use super::common::{self, EntityList, Select};

#[derive(Clone)]
pub struct TeamView {
    pub root: gtk::Box,
    list: EntityList,
    empty: gtk::ScrolledWindow,
    graph: super::graph::Graph,
    pairs: gtk::Box,
    pair: Rc<dyn Fn(ClientScope, String, String)>,
}

impl TeamView {
    pub fn new(select: Select, pair: Rc<dyn Fn(ClientScope, String, String)>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let list = EntityList::new("Team agents", select.clone());
        let graph = super::graph::Graph::new("Agent relationships", select);
        let stack = gtk::Stack::new();
        stack.add_titled(list.widget(), Some("list"), "List");
        stack.add_titled(&graph.root, Some("graph"), "Graph");
        let switcher = gtk::StackSwitcher::builder().stack(&stack).build();
        root.append(&switcher);
        let pairs = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.append(&pairs);
        let empty = super::empty_state(
            "system-run-symbolic",
            "No agents loaded",
            "This workspace has not reported a team yet.",
        );
        root.append(&stack);
        root.append(&empty);
        Self {
            root,
            list,
            empty,
            graph,
            pairs,
            pair,
        }
    }

    pub fn update(&self, scope: Option<&ClientScope>, data: &TeamData) {
        let Some(scope) = scope else {
            common::clear(&self.pairs);
            self.list.replace(Vec::new());
            self.graph.update(Vec::new(), Vec::new());
            self.list.root.set_visible(false);
            self.empty.set_visible(true);
            return;
        };
        self.empty.set_visible(data.agents.is_empty());
        common::clear(&self.pairs);
        for relation in &data.relations {
            let button = gtk::Button::with_label(&format!(
                "Messages between {} and {}",
                relation.parent_id, relation.child_id
            ));
            button.connect_clicked({
                let pair = self.pair.clone();
                let scope = scope.clone();
                let first = relation.parent_id.clone();
                let second = relation.child_id.clone();
                move |_| pair(scope.clone(), first.clone(), second.clone())
            });
            self.pairs.append(&button);
        }
        self.graph.update(
            data.agents
                .iter()
                .enumerate()
                .map(|(i, member)| super::graph::Node {
                    key: EntityKey {
                        scope: scope.clone(),
                        entity: Entity::Agent(member.agent.id.clone()),
                    },
                    label: format!(
                        "{} · {}\n{}\nCurrent: {}\nWaiting: {}",
                        member.agent.id,
                        member.agent.role,
                        member.status_label,
                        member.current_work.as_ref().map_or("none", |w| &w.task_id),
                        member.waiting_task_ids.join(", ")
                    ),
                    column: u32::from(member.satellite_parent_id.is_some()),
                    row: i as u32,
                })
                .collect(),
            data.relations
                .iter()
                .map(|r| (r.parent_id.clone(), r.child_id.clone()))
                .collect(),
        );
        self.list.root.set_visible(!data.agents.is_empty());
        self.list.replace(
            data.agents
                .iter()
                .map(|member| {
                    let agent = &member.agent;
                    let body = common::column();
                    body.append(&common::heading(&format!("{} · {}", agent.name, agent.id)));
                    body.append(&common::text(&format!(
                        "{} · {} · {}",
                        agent.role, agent.runtime, member.status_label
                    )));
                    if let Some(work) = &member.current_work {
                        body.append(&common::text(&format!(
                            "{} task {}",
                            if work.done { "Completed" } else { "Current" },
                            work.task_id
                        )));
                        if work.elapsed_ms.is_none() {
                            body.append(&common::text("Start time not reported"));
                        }
                    } else if let Some(task) = &agent.task {
                        body.append(&common::text(task));
                    }
                    if !member.waiting_task_ids.is_empty() {
                        body.append(&common::text(&format!(
                            "Waiting: {}",
                            member.waiting_task_ids.join(", ")
                        )));
                    }
                    if let Some(parent) = &member.satellite_parent_id {
                        body.append(&common::text(&format!("Subagent of {parent}")));
                    }
                    for warning in &member.warnings {
                        body.append(&common::text(&warning.text));
                    }
                    let label = format!(
                        "{}, {}, {}, current task {}",
                        agent.id,
                        agent.role,
                        member.status_label,
                        member
                            .current_work
                            .as_ref()
                            .map_or("none", |work| &work.task_id)
                    );
                    (
                        EntityKey {
                            scope: scope.clone(),
                            entity: Entity::Agent(agent.id.clone()),
                        },
                        body.upcast(),
                        label,
                    )
                })
                .collect(),
        );
    }
}
