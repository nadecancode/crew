//! The deterministic half of the orchestrator.
//!
//! `plan` is a pure function from the task graph and the agents' availability
//! to the next actions. The daemon applies those actions (persisting them and
//! sending the task messages) and calls `plan` again whenever something
//! changes, so scheduling is event-driven and trivially testable.

use std::collections::{HashMap, HashSet};

use crew_core::{Task, TaskStatus};

use crate::tasks::occupies_agent;

/// What the scheduler needs to know about an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentView {
    pub id: String,
    pub orchestrator: bool,
    /// The process is alive and not paused, so it can accept a task.
    pub runnable: bool,
    /// A turn is running or a message is on its way to the agent: something
    /// may still happen even if no task is formally "working".
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// All dependencies are done.
    MakeReady { task: String },
    /// A blocked task's dependencies recovered but are not finished yet.
    Requeue { task: String },
    /// Give an unassigned task to a free worker.
    Assign { task: String, agent: String },
    /// Hand a ready task to its agent.
    Dispatch { task: String, agent: String },
    /// The task cannot run (a dependency failed).
    Block { task: String, reason: String },
    /// Nothing is running and nothing can start: a deadlock or a stuck team.
    Stall { tasks: Vec<String>, reason: String },
    /// Every task is done.
    AllDone,
}

pub fn plan(tasks: &[Task], agents: &[AgentView]) -> Vec<Action> {
    let mut actions = Vec::new();
    if tasks.is_empty() {
        return actions;
    }
    // Work on a scratch copy so later steps see the effect of earlier ones.
    let mut tasks: Vec<Task> = tasks.to_vec();
    let status: HashMap<String, TaskStatus> =
        tasks.iter().map(|t| (t.id.clone(), t.status)).collect();

    // 1. Dependency resolution.
    for task in tasks.iter_mut() {
        let unstarted_block = task.status == TaskStatus::Blocked && task.started_at.is_none();
        if task.status != TaskStatus::Queued && !unstarted_block {
            continue;
        }
        let failed = task
            .dependencies
            .iter()
            .find(|d| matches!(status.get(d.as_str()), Some(TaskStatus::Failed) | None));
        if let Some(dep) = failed {
            if task.status != TaskStatus::Blocked {
                let reason = if status.contains_key(dep.as_str()) {
                    format!("dependency '{dep}' failed")
                } else {
                    format!("dependency '{dep}' does not exist")
                };
                actions.push(Action::Block {
                    task: task.id.clone(),
                    reason,
                });
                task.status = TaskStatus::Blocked;
            }
            continue;
        }
        let all_done = task
            .dependencies
            .iter()
            .all(|d| status.get(d.as_str()) == Some(&TaskStatus::Done));
        if all_done {
            actions.push(Action::MakeReady {
                task: task.id.clone(),
            });
            task.status = TaskStatus::Ready;
        } else if unstarted_block {
            actions.push(Action::Requeue {
                task: task.id.clone(),
            });
            task.status = TaskStatus::Queued;
        }
    }

    // 2. Assignment and dispatch, in creation order, one task per free agent.
    let mut busy: HashSet<String> = tasks
        .iter()
        .filter(|t| occupies_agent(t))
        .filter_map(|t| t.assigned_agent.clone())
        .collect();
    let mut load: HashMap<String, usize> = HashMap::new();
    for task in &tasks {
        if let (Some(agent), false) = (&task.assigned_agent, task.status.is_terminal()) {
            *load.entry(agent.clone()).or_default() += 1;
        }
    }
    let runnable: HashSet<&str> = agents
        .iter()
        .filter(|a| a.runnable)
        .map(|a| a.id.as_str())
        .collect();

    for task in tasks.iter_mut() {
        if task.status != TaskStatus::Ready {
            continue;
        }
        let agent = match &task.assigned_agent {
            Some(agent) => agent.clone(),
            None => {
                let candidate = agents
                    .iter()
                    .filter(|a| a.runnable && !a.orchestrator && !busy.contains(&a.id))
                    .min_by_key(|a| (load.get(&a.id).copied().unwrap_or(0), a.id.clone()));
                let Some(candidate) = candidate else { continue };
                actions.push(Action::Assign {
                    task: task.id.clone(),
                    agent: candidate.id.clone(),
                });
                *load.entry(candidate.id.clone()).or_default() += 1;
                task.assigned_agent = Some(candidate.id.clone());
                candidate.id.clone()
            }
        };
        if runnable.contains(agent.as_str()) && !busy.contains(&agent) {
            actions.push(Action::Dispatch {
                task: task.id.clone(),
                agent: agent.clone(),
            });
            busy.insert(agent);
            task.status = TaskStatus::Working;
        }
    }

    // 3. Completion and stall detection.
    if tasks.iter().all(|t| t.status == TaskStatus::Done) {
        actions.push(Action::AllDone);
        return actions;
    }
    // A review is progress only while its reviewer can do it.
    let reviewable = |t: &Task| t.reviewer.as_deref().is_some_and(|r| runnable.contains(r));
    // A waiting task is not stuck while the teammate it waits on is busy
    // (for instance answering its question).
    let progressing = tasks.iter().any(|t| match t.status {
        TaskStatus::Working => true,
        TaskStatus::Review => reviewable(t),
        _ => false,
    }) || agents.iter().any(|a| a.runnable && a.active);
    if !progressing {
        let mut stuck = Vec::new();
        let mut reasons = Vec::new();
        for task in &tasks {
            match task.status {
                TaskStatus::Done | TaskStatus::Failed => {}
                TaskStatus::Ready => {
                    stuck.push(task.id.clone());
                    match &task.assigned_agent {
                        Some(agent) => reasons
                            .push(format!("'{}' is ready but {agent} is not running", task.id)),
                        None => {
                            reasons.push(format!("'{}' is ready but no worker is free", task.id))
                        }
                    }
                }
                TaskStatus::Waiting => {
                    stuck.push(task.id.clone());
                    reasons.push(format!("'{}' is waiting", task.id));
                }
                TaskStatus::Blocked => {
                    stuck.push(task.id.clone());
                    let why = task.note.as_deref().unwrap_or("blocked");
                    reasons.push(format!("'{}' is blocked ({why})", task.id));
                }
                TaskStatus::Queued => {
                    stuck.push(task.id.clone());
                }
                TaskStatus::Review if !reviewable(task) => {
                    stuck.push(task.id.clone());
                    let reviewer = task.reviewer.as_deref().unwrap_or("nobody");
                    reasons.push(format!(
                        "'{}' awaits review by {reviewer}, who is not running",
                        task.id
                    ));
                }
                TaskStatus::Working | TaskStatus::Review => {}
            }
        }
        if !stuck.is_empty() {
            if reasons.is_empty() {
                reasons.push("remaining tasks wait on dependencies that cannot finish".into());
            }
            actions.push(Action::Stall {
                tasks: stuck,
                reason: reasons.join("; "),
            });
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, agent: Option<&str>, deps: &[&str], status: TaskStatus) -> Task {
        Task {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            assigned_agent: agent.map(str::to_string),
            status,
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
            created_at: 0,
            started_at: None,
            completed_at: None,
            artifacts: vec![],
            reviewer: None,
            result: None,
            progress: None,
            note: None,
            timeout_secs: None,
        }
    }

    fn agents(ids: &[&str]) -> Vec<AgentView> {
        ids.iter()
            .map(|id| AgentView {
                id: id.to_string(),
                orchestrator: *id == "orchestrator",
                runnable: true,
                active: false,
            })
            .collect()
    }

    fn dispatch(task: &str, agent: &str) -> Action {
        Action::Dispatch {
            task: task.into(),
            agent: agent.into(),
        }
    }

    fn ready(task: &str) -> Action {
        Action::MakeReady { task: task.into() }
    }

    #[test]
    fn independent_tasks_start_together() {
        let tasks = vec![
            task("a", Some("x"), &[], TaskStatus::Queued),
            task("b", Some("y"), &[], TaskStatus::Queued),
        ];
        assert_eq!(
            plan(&tasks, &agents(&["x", "y"])),
            vec![
                ready("a"),
                ready("b"),
                dispatch("a", "x"),
                dispatch("b", "y")
            ]
        );
    }

    #[test]
    fn dependent_task_waits_for_all_dependencies() {
        let mut tasks = vec![
            task("a", Some("x"), &[], TaskStatus::Done),
            task("b", Some("y"), &[], TaskStatus::Working),
            task("c", Some("z"), &["a", "b"], TaskStatus::Queued),
        ];
        // C depends on A and B; B is still working, so nothing happens.
        assert_eq!(plan(&tasks, &agents(&["x", "y", "z"])), vec![]);
        tasks[1].status = TaskStatus::Done;
        assert_eq!(
            plan(&tasks, &agents(&["x", "y", "z"])),
            vec![ready("c"), dispatch("c", "z")]
        );
    }

    #[test]
    fn an_agent_gets_one_task_at_a_time() {
        let mut tasks = vec![
            task("a", Some("x"), &[], TaskStatus::Queued),
            task("b", Some("x"), &[], TaskStatus::Queued),
        ];
        assert_eq!(
            plan(&tasks, &agents(&["x"])),
            vec![ready("a"), ready("b"), dispatch("a", "x")]
        );
        tasks[0].status = TaskStatus::Working;
        tasks[1].status = TaskStatus::Ready;
        assert_eq!(plan(&tasks, &agents(&["x"])), vec![]);
        tasks[0].status = TaskStatus::Done;
        assert_eq!(plan(&tasks, &agents(&["x"])), vec![dispatch("b", "x")]);
    }

    #[test]
    fn a_task_under_review_frees_its_author() {
        let tasks = vec![
            task("a", Some("x"), &[], TaskStatus::Review),
            task("b", Some("x"), &[], TaskStatus::Ready),
        ];
        assert_eq!(plan(&tasks, &agents(&["x"])), vec![dispatch("b", "x")]);
    }

    #[test]
    fn unassigned_tasks_go_to_the_least_loaded_free_worker() {
        let tasks = vec![
            task("a", Some("x"), &[], TaskStatus::Working),
            task("b", None, &[], TaskStatus::Queued),
        ];
        let actions = plan(&tasks, &agents(&["orchestrator", "x", "y"]));
        assert_eq!(
            actions,
            vec![
                ready("b"),
                Action::Assign {
                    task: "b".into(),
                    agent: "y".into()
                },
                dispatch("b", "y"),
            ]
        );
    }

    #[test]
    fn failed_dependency_blocks_dependents_and_stalls() {
        let tasks = vec![
            task("a", Some("x"), &[], TaskStatus::Failed),
            task("b", Some("y"), &["a"], TaskStatus::Queued),
        ];
        let actions = plan(&tasks, &agents(&["x", "y"]));
        assert_eq!(
            actions[0],
            Action::Block {
                task: "b".into(),
                reason: "dependency 'a' failed".into()
            }
        );
        assert!(matches!(&actions[1], Action::Stall { tasks, .. } if tasks == &["b"]));
    }

    #[test]
    fn blocked_task_recovers_when_its_dependency_is_retried() {
        let mut blocked = task("b", Some("y"), &["a"], TaskStatus::Blocked);
        blocked.note = Some("dependency 'a' failed".into());
        let mut tasks = vec![task("a", Some("x"), &[], TaskStatus::Working), blocked];
        assert_eq!(
            plan(&tasks, &agents(&["x", "y"])),
            vec![Action::Requeue { task: "b".into() }]
        );
        tasks[0].status = TaskStatus::Done;
        assert_eq!(
            plan(&tasks, &agents(&["x", "y"])),
            vec![ready("b"), dispatch("b", "y")]
        );
    }

    #[test]
    fn dead_agent_stalls_instead_of_dispatching() {
        let tasks = vec![task("a", Some("x"), &[], TaskStatus::Ready)];
        let mut views = agents(&["x"]);
        views[0].runnable = false;
        let actions = plan(&tasks, &views);
        assert!(
            matches!(&actions[..], [Action::Stall { reason, .. }] if reason.contains("x is not running")),
            "{actions:?}"
        );
    }

    #[test]
    fn mutual_waiting_is_reported_as_a_stall() {
        let mut a = task("a", Some("x"), &[], TaskStatus::Waiting);
        let mut b = task("b", Some("y"), &[], TaskStatus::Waiting);
        a.started_at = Some(1);
        b.started_at = Some(1);
        let actions = plan(&[a.clone(), b.clone()], &agents(&["x", "y"]));
        assert!(matches!(&actions[..], [Action::Stall { tasks, .. }] if tasks.len() == 2));
        // ...but not while a teammate is busy, e.g. answering the question
        // the task is waiting on.
        let mut views = agents(&["x", "y", "z"]);
        views[2].active = true;
        assert_eq!(plan(&[a, b], &views), vec![]);
    }

    #[test]
    fn a_review_nobody_can_do_is_a_stall() {
        let mut a = task("a", Some("x"), &[], TaskStatus::Review);
        a.reviewer = Some("r".into());
        let tasks = vec![a];
        // A running reviewer is progress.
        assert_eq!(plan(&tasks, &agents(&["x", "r"])), vec![]);
        // A stopped one, or one no longer on the team, is not.
        let mut views = agents(&["x", "r"]);
        views[1].runnable = false;
        for views in [views, agents(&["x"])] {
            let actions = plan(&tasks, &views);
            assert!(
                matches!(&actions[..], [Action::Stall { tasks, reason }]
                    if tasks == &["a"] && reason.contains("review by r")),
                "{actions:?}"
            );
        }
    }

    #[test]
    fn completion_is_reported_once_everything_is_done() {
        let tasks = vec![
            task("a", Some("x"), &[], TaskStatus::Done),
            task("b", Some("y"), &["a"], TaskStatus::Done),
        ];
        assert_eq!(plan(&tasks, &agents(&["x", "y"])), vec![Action::AllDone]);
        assert_eq!(plan(&[], &agents(&["x"])), vec![]);
    }
}
