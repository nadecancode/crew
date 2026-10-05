//! Shared relationship facts. Hosts own graph geometry and interaction.

use std::collections::HashMap;

use crew_protocol::{Agent, Task};

use crate::api::{
    AgentRelation, ClientError, DependencyState, TaskLayer, TaskPresentation, TasksData,
};
use crate::satellite_parent_id;

/// Actual satellite relationships follow the same known-worker rule as ordering.
/// Orphan/nested/orchestrator children remain ordinary members, as in the TUI.
pub fn parent_relations(agents: &[Agent]) -> Vec<AgentRelation> {
    agents
        .iter()
        .filter_map(|agent| {
            satellite_parent_id(agents, agent).map(|parent_id| AgentRelation {
                parent_id: parent_id.to_owned(),
                child_id: agent.id.clone(),
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Depth {
    Unknown,
    Visiting,
    Known(u32),
}

struct Frame {
    index: usize,
    next_dependency: usize,
    depth: u32,
}

/// Preserve App::task_layers depth, board order, missing-edge and cycle-cut
/// behavior. Explicit DFS frames avoid depending on the host thread's stack.
fn depths(tasks: &[Task], indices: &HashMap<&str, usize>) -> Result<Vec<u32>, ClientError> {
    let mut memo = vec![Depth::Unknown; tasks.len()];
    for root in 0..tasks.len() {
        if matches!(memo[root], Depth::Known(_)) {
            continue;
        }
        memo[root] = Depth::Visiting;
        let mut stack = vec![Frame {
            index: root,
            next_dependency: 0,
            depth: 0,
        }];
        while let Some(frame) = stack.last_mut() {
            if let Some(dependency) = tasks[frame.index].dependencies.get(frame.next_dependency) {
                frame.next_dependency += 1;
                let Some(&child) = indices.get(dependency.as_str()) else {
                    continue;
                };
                match memo[child] {
                    Depth::Unknown => {
                        memo[child] = Depth::Visiting;
                        stack.push(Frame {
                            index: child,
                            next_dependency: 0,
                            depth: 0,
                        });
                    }
                    Depth::Visiting => frame.depth = frame.depth.max(1),
                    Depth::Known(depth) => frame.depth = frame.depth.max(next_depth(depth)?),
                }
            } else {
                let frame = stack.pop().expect("nonempty traversal");
                memo[frame.index] = Depth::Known(frame.depth);
                if let Some(parent) = stack.last_mut() {
                    parent.depth = parent.depth.max(next_depth(frame.depth)?);
                }
            }
        }
    }
    Ok(memo
        .into_iter()
        .map(|depth| match depth {
            Depth::Known(value) => value,
            _ => unreachable!("completed depth traversal"),
        })
        .collect())
}

fn next_depth(depth: u32) -> Result<u32, ClientError> {
    depth
        .checked_add(1)
        .ok_or_else(|| ClientError::InternalFailure {
            code: "task_layer_overflow".into(),
        })
}

/// Cycle membership is separate from the legacy depth traversal. A task which
/// depends on a cycle is not itself reported as participating in that cycle.
fn cycle_members(edges: &[Vec<usize>]) -> Vec<bool> {
    let count = edges.len();
    let mut seen = vec![false; count];
    let mut finished = Vec::with_capacity(count);
    for root in 0..count {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![(root, 0)];
        while let Some((node, next)) = stack.last_mut() {
            if let Some(&child) = edges[*node].get(*next) {
                *next += 1;
                if !seen[child] {
                    seen[child] = true;
                    stack.push((child, 0));
                }
            } else {
                finished.push(stack.pop().expect("nonempty traversal").0);
            }
        }
    }
    let mut reversed = vec![Vec::new(); count];
    for (node, children) in edges.iter().enumerate() {
        for &child in children {
            reversed[child].push(node);
        }
    }
    seen.fill(false);
    let mut cyclic = vec![false; count];
    for root in finished.into_iter().rev() {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![root];
        let mut component = Vec::new();
        while let Some(node) = stack.pop() {
            component.push(node);
            for &child in &reversed[node] {
                if !seen[child] {
                    seen[child] = true;
                    stack.push(child);
                }
            }
        }
        if component.len() > 1 || edges[root].contains(&root) {
            for member in component {
                cyclic[member] = true;
            }
        }
    }
    cyclic
}

pub fn task_data(tasks: &[Task]) -> Result<TasksData, ClientError> {
    // A malformed duplicate id retains the TUI's last-index lookup behavior.
    let indices: HashMap<_, _> = tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.as_str(), index))
        .collect();
    let depth = depths(tasks, &indices)?;
    let edges: Vec<Vec<_>> = tasks
        .iter()
        .map(|task| {
            task.dependencies
                .iter()
                .filter_map(|id| indices.get(id.as_str()).copied())
                .collect()
        })
        .collect();
    let cyclic = cycle_members(&edges);
    let mut layers: Vec<TaskLayer> = Vec::new();
    let mut presentations = Vec::with_capacity(tasks.len());
    for (index, task) in tasks.iter().enumerate() {
        let layer = depth[index];
        let layer_index = usize::try_from(layer).map_err(|_| ClientError::InternalFailure {
            code: "task_layer_width".into(),
        })?;
        while layers.len() <= layer_index {
            layers.push(TaskLayer {
                index: u32::try_from(layers.len()).map_err(|_| ClientError::InternalFailure {
                    code: "task_layer_overflow".into(),
                })?,
                task_ids: Vec::new(),
            });
        }
        layers[layer_index].task_ids.push(task.id.clone());
        presentations.push(TaskPresentation {
            task: task.clone(),
            dependencies: task
                .dependencies
                .iter()
                .map(|id| DependencyState {
                    id: id.clone(),
                    status: indices.get(id.as_str()).map(|&index| tasks[index].status),
                })
                .collect(),
            layer,
            in_cycle: cyclic[index],
        });
    }
    Ok(TasksData {
        tasks: presentations,
        layers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_protocol::TaskStatus;

    fn task(id: &str, deps: &[&str]) -> Task {
        Task {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            assigned_agent: None,
            status: TaskStatus::Queued,
            dependencies: deps.iter().map(|id| (*id).to_owned()).collect(),
            created_at: 0,
            started_at: None,
            completed_at: None,
            artifacts: Vec::new(),
            reviewer: None,
            result: None,
            progress: None,
            note: None,
            timeout_secs: None,
        }
    }

    #[test]
    fn parent_edges_match_the_existing_satellite_grouping() {
        let make = |id: &str, parent: Option<&str>| {
            serde_json::from_value::<Agent>(serde_json::json!({
                "id":id,"name":id,"role":"","status":"idle","runtime":"codex",
                "workdir":"","backend":"pty","last_activity_at":0,"parent":parent,
            }))
            .unwrap()
        };
        let agents = [
            make("worker", None),
            make("child", Some("worker")),
            make("nested", Some("child")),
            make("orphan", Some("missing")),
            make("orchestrator", None),
            make("orch-child", Some("orchestrator")),
        ];
        assert_eq!(
            parent_relations(&agents),
            [AgentRelation {
                parent_id: "worker".into(),
                child_id: "child".into()
            }]
        );
    }

    #[test]
    fn missing_edges_and_board_order_preserve_existing_layers() {
        let data = task_data(&[
            task("child", &["root", "missing"]),
            task("peer", &[]),
            task("root", &[]),
        ])
        .unwrap();
        assert_eq!(
            data.layers
                .iter()
                .map(|layer| layer.task_ids.clone())
                .collect::<Vec<_>>(),
            [
                vec!["peer".to_owned(), "root".to_owned()],
                vec!["child".to_owned()]
            ]
        );
        assert_eq!(
            data.tasks[0].dependencies[1],
            DependencyState {
                id: "missing".into(),
                status: None
            }
        );
        assert!(data.tasks.iter().all(|task| !task.in_cycle));
    }

    #[test]
    fn cycles_do_not_change_legacy_depth_or_mark_downstream_tasks() {
        let data = task_data(&[
            task("a", &["b"]),
            task("b", &["a"]),
            task("tail", &["a"]),
            task("self", &["self"]),
        ])
        .unwrap();
        assert_eq!(
            data.tasks.iter().map(|task| task.layer).collect::<Vec<_>>(),
            [2, 1, 3, 1]
        );
        assert_eq!(
            data.tasks
                .iter()
                .map(|task| task.in_cycle)
                .collect::<Vec<_>>(),
            [true, true, false, true]
        );
    }

    #[test]
    fn long_chains_do_not_require_recursive_host_stack() {
        let tasks: Vec<_> = (0..20_000)
            .map(|index| task(&index.to_string(), &[]))
            .collect();
        let mut tasks = tasks;
        for index in 0..tasks.len() - 1 {
            tasks[index].dependencies.push((index + 1).to_string());
        }
        let data = task_data(&tasks).unwrap();
        assert_eq!(data.tasks[0].layer, 19_999);
        assert_eq!(data.tasks.last().unwrap().layer, 0);
        assert!(data.tasks.iter().all(|task| !task.in_cycle));
    }
}
