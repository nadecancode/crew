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
    pairs_group: gtk::Expander,
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
        switcher.set_halign(gtk::Align::Start);
        switcher.set_margin_start(24);
        switcher.set_margin_bottom(8);
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
                    let body = common::row();
                    body.append(&common::row_title(
                        &agent.id,
                        Some(common::status_chip(&member.status_label, member.tone)),
                    ));
                    let mut identity = vec![agent.role.clone(), agent.runtime.to_string()];
                    if agent.name != agent.id && !agent.name.is_empty() {
                        identity.insert(0, agent.name.clone());
                    }
                    body.append(&common::muted(&identity.join(" · ")));
                    let progress = agent
                        .progress
                        .map_or(String::new(), |progress| format!(" · {progress}%"));
                    let work = match &member.current_work {
                        Some(work) if work.done => Some(format!("Completed {}", work.task_id)),
                        Some(work) => Some(format!("Working on {}{progress}", work.task_id)),
                        None => agent.task.clone(),
                    };
                    if let Some(work) = work {
                        body.append(&common::text(&work));
                    }
                    if !member.waiting_task_ids.is_empty() {
                        body.append(&common::muted(&format!(
                            "Waiting: {}",
                            member.waiting_task_ids.join(", ")
                        )));
                    }
                    if let Some(parent) = &member.satellite_parent_id {
                        body.append(&common::muted(&format!("Subagent of {parent}")));
                    }
                    for warning in &member.warnings {
                        body.append(&common::warning(&warning.text));
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
