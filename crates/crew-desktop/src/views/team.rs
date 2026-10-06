use crew_client::api::*;
use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};

use super::common::{self, EntityList, Select};
use super::graph::{Edge, EdgeKind, Graph, Node};

#[derive(Clone)]
pub struct TeamView {
    pub root: gtk::Box,
    list: EntityList,
    empty: gtk::ScrolledWindow,
    graph: Graph,
    pairs: gtk::Box,
    pairs_group: gtk::Expander,
    pair: Rc<dyn Fn(ClientScope, String, String)>,
}

impl TeamView {
    pub fn new(select: Select, pair: Rc<dyn Fn(ClientScope, String, String)>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // A list selection also marks the graph, so switching presentation
        // keeps the same selected agent.
        let marked: Rc<RefCell<Option<Graph>>> = Rc::new(RefCell::new(None));
        let list = EntityList::new(
            "Team agents",
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
        let graph = Graph::new("Agent relationships", select);
        *marked.borrow_mut() = Some(graph.clone());
        let stack = gtk::Stack::new();
        stack.add_titled(list.widget(), Some("list"), "List");
        stack.add_titled(&graph.root, Some("graph"), "Graph");
        let switcher = gtk::StackSwitcher::builder().stack(&stack).build();
        root.append(&switcher);
        // Pair navigation is collapsed and height-bounded so a large team or
        // enlarged text cannot push the agent list out of view.
        let pairs = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let pairs_scroll = gtk::ScrolledWindow::builder()
            .child(&pairs)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(180)
            .build();
        let pairs_group = gtk::Expander::builder()
            .label("Conversations between related agents")
            .child(&pairs_scroll)
            .visible(false)
            .build();
        root.append(&pairs_group);
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
            pairs_group,
            pair,
        }
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn update(&self, scope: Option<&ClientScope>, data: &TeamData) {
        self.pairs_group
            .set_visible(scope.is_some() && !data.relations.is_empty());
        self.pairs_group.set_label(Some(&format!(
            "Conversations between related agents ({})",
            data.relations.len()
        )));
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
        let (nodes, edges) = topology(scope, data);
        self.graph.update(nodes, edges);
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
                    let label = agent_label(member);
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

fn agent_label(member: &AgentPresentation) -> String {
    format!(
        "{}, {}, {}, current task {}",
        member.agent.id,
        member.agent.role,
        member.status_label,
        member
            .current_work
            .as_ref()
            .map_or("none", |work| &work.task_id)
    )
}

/// Team topology in three layers: the orchestrator agent, or the scheduler
/// when the team has none; its members; then subagents beside their parents.
/// Membership and parents come from the shared projection; within a layer the
/// list order is kept.
fn topology(scope: &ClientScope, data: &TeamData) -> (Vec<Node>, Vec<Edge>) {
    if data.agents.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let orchestrator = data.agents.iter().position(|m| m.agent.orchestrator);
    let members: Vec<&str> = data
        .agents
        .iter()
        .filter(|m| !m.agent.orchestrator && m.satellite_parent_id.is_none())
        .map(|m| m.agent.id.as_str())
        .collect();
    let hub_name = orchestrator.map_or("the scheduler", |i| data.agents[i].agent.id.as_str());
    let mut nodes = Vec::with_capacity(data.agents.len() + 1);
    if orchestrator.is_none() {
        nodes.push(Node {
            key: None,
            title: "Scheduler".into(),
            status: None,
            lines: vec![format!("Coordinates {}", plural(members.len(), "agent"))],
            layer: 0,
            label: format!("Scheduler, coordinates {}", plural(members.len(), "agent")),
            relations: format!("Coordinates {}", members.join(", ")),
        });
    }
    let offset = nodes.len();
    for member in &data.agents {
        let agent = &member.agent;
        let mut lines = Vec::new();
        if let Some(work) = &member.current_work {
            lines.push(format!(
                "{}: {}",
                if work.done { "Completed" } else { "Current" },
                work.task_id
            ));
        }
        if !member.waiting_task_ids.is_empty() {
            lines.push(format!("Waiting: {}", member.waiting_task_ids.join(", ")));
        }
        let mut relations = Vec::new();
        if agent.orchestrator {
            relations.push(format!("Coordinates {}", members.join(", ")));
        } else if let Some(parent) = &member.satellite_parent_id {
            relations.push(format!("Subagent of {parent}"));
        } else {
            relations.push(format!("Coordinated by {hub_name}"));
        }
        if !member.subagent_ids.is_empty() {
            relations.push(format!("Subagents {}", member.subagent_ids.join(", ")));
        }
        nodes.push(Node {
            key: Some(EntityKey {
                scope: scope.clone(),
                entity: Entity::Agent(agent.id.clone()),
            }),
            title: format!("{} · {}", agent.id, agent.role),
            status: Some((member.status_label.clone(), member.tone)),
            lines,
            layer: if agent.orchestrator {
                0
            } else if member.satellite_parent_id.is_some() {
                2
            } else {
                1
            },
            label: agent_label(member),
            relations: relations.join(". "),
        });
    }
    let index = |id: &str| {
        data.agents
            .iter()
            .position(|m| m.agent.id == id)
            .map(|i| i + offset)
    };
    let hub = orchestrator.map_or(0, |i| i + offset);
    let mut edges: Vec<Edge> = members
        .iter()
        .filter_map(|id| index(id))
        .map(|to| Edge {
            from: hub,
            to,
            kind: EdgeKind::Coordination,
        })
        .collect();
    edges.extend(data.relations.iter().filter_map(|relation| {
        Some(Edge {
            from: index(&relation.parent_id)?,
            to: index(&relation.child_id)?,
            kind: EdgeKind::Subagent,
        })
    }));
    (nodes, edges)
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}
