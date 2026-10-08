//! Task graph helpers: construction, validation and queries.

use std::collections::HashMap;

use crew_core::config::{find_cycle, validate_timer, TaskSpec};
use crew_core::util::{is_valid_id, one_line, truncate_chars};
use crew_core::{Task, TaskStatus};

/// Build a queued task from its declaration.
pub fn task_from_spec(spec: &TaskSpec, now: i64) -> Task {
    let title = match &spec.title {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => {
            let first = spec.description.lines().find(|l| !l.trim().is_empty());
            match first {
                Some(line) => truncate_chars(&one_line(line), 60),
                None => spec.id.clone(),
            }
        }
    };
    Task {
        epic: None,
        id: spec.id.clone(),
        title,
        description: spec.description.trim().to_string(),
        assigned_agent: spec.agent.clone(),
        status: TaskStatus::Queued,
        dependencies: spec.depends_on.clone(),
        created_at: now,
        started_at: None,
        completed_at: None,
        artifacts: Vec::new(),
        reviewer: spec.reviewer.clone(),
        result: None,
        progress: None,
        note: None,
        timeout_secs: spec.timeout_secs,
    }
}

/// Check that a task can be added to an existing graph.
pub fn validate_new(existing: &[Task], spec: &TaskSpec) -> Result<(), String> {
    if !is_valid_id(&spec.id) {
        return Err(format!(
            "invalid task id '{}' (expected [a-z][a-z0-9_-]{{0,31}})",
            spec.id
        ));
    }
    if existing.iter().any(|t| t.id == spec.id) {
        return Err(format!("task '{}' already exists", spec.id));
    }
    check_reviewer(spec)?;
    // As in a team file (M7).
    if let Some(timeout) = spec.timeout_secs {
        validate_timer(&format!("task '{}'.timeout_secs", spec.id), timeout)
            .map_err(|e| e.to_string())?;
    }
    for dep in &spec.depends_on {
        if dep == &spec.id {
            return Err(format!("task '{}' cannot depend on itself", spec.id));
        }
        if !existing.iter().any(|t| &t.id == dep) {
            return Err(format!(
                "task '{}' depends on unknown task '{dep}'",
                spec.id
            ));
        }
    }
    // A new node whose dependencies all exist cannot close a cycle, but check
    // anyway so the invariant holds even if the graph was edited by hand.
    let mut graph: HashMap<&str, Vec<&str>> = existing
        .iter()
        .map(|t| {
            (
                t.id.as_str(),
                t.dependencies.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    graph.insert(
        spec.id.as_str(),
        spec.depends_on.iter().map(String::as_str).collect(),
    );
    if let Some(cycle) = find_cycle(&graph) {
        return Err(format!("dependency cycle: {}", cycle.join(" -> ")));
    }
    Ok(())
}

/// A review is someone else's look at the work.
pub fn check_reviewer(spec: &TaskSpec) -> Result<(), String> {
    match (&spec.agent, &spec.reviewer) {
        (Some(agent), Some(reviewer)) if agent == reviewer => Err(format!(
            "task '{}' cannot be reviewed by its own agent '{agent}'",
            spec.id
        )),
        _ => Ok(()),
    }
}

/// A task occupies its agent while the agent is expected to act on it.
pub fn occupies_agent(task: &Task) -> bool {
    match task.status {
        TaskStatus::Working => true,
        // Blocked after it was started: the agent itself is stuck on it.
        TaskStatus::Blocked => task.started_at.is_some(),
        _ => false,
    }
}

/// The task occupying an agent's execution slot, if any. Waiting cards are
/// parked: they keep their owner without preventing independent work.
pub fn executing_task<'a>(tasks: &'a [Task], agent: &str) -> Option<&'a Task> {
    tasks
        .iter()
        .find(|t| t.assigned_agent.as_deref() == Some(agent) && occupies_agent(t))
}

/// The executing card, otherwise the most recently started waiting card.
/// Equal start times keep board order. This is also the default target for
/// task calls when no task id is supplied.
pub fn active_task<'a>(tasks: &'a [Task], agent: &str) -> Option<&'a Task> {
    executing_task(tasks, agent).or_else(|| {
        tasks
            .iter()
            .filter(|t| {
                t.assigned_agent.as_deref() == Some(agent) && t.status == TaskStatus::Waiting
            })
            .fold(None, |selected: Option<&Task>, task| match selected {
                Some(current) if current.started_at >= task.started_at => Some(current),
                _ => Some(task),
            })
    })
}

/// `(done, total)` across all tasks.
pub fn done_counts(tasks: &[Task]) -> (usize, usize) {
    (
        tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count(),
        tasks.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_task_needs_a_timeout_a_team_file_would_accept() {
        let with = |timeout| TaskSpec {
            timeout_secs: Some(timeout),
            ..spec("t", &[])
        };
        for bad in [0, u64::MAX] {
            let error = validate_new(&[], &with(bad)).expect_err("accepted a bad timeout");
            assert!(error.contains("timeout_secs"), "{error}");
        }
        assert!(validate_new(&[], &with(3600)).is_ok());
    }

    fn spec(id: &str, deps: &[&str]) -> TaskSpec {
        TaskSpec {
            id: id.into(),
            description: format!("do {id}\nsecond line"),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            ..TaskSpec::default()
        }
    }

    #[test]
    fn builds_tasks_with_derived_titles() {
        let t = task_from_spec(&spec("a", &[]), 7);
        assert_eq!(t.title, "do a");
        assert_eq!(t.status, TaskStatus::Queued);
        assert_eq!(t.created_at, 7);
        let mut s = spec("b", &[]);
        s.title = Some("  Explicit ".into());
        assert_eq!(task_from_spec(&s, 0).title, "Explicit");
        s.title = None;
        s.description = String::new();
        assert_eq!(task_from_spec(&s, 0).title, "b");
    }

    #[test]
    fn validates_additions() {
        let existing = vec![task_from_spec(&spec("a", &[]), 0)];
        assert!(validate_new(&existing, &spec("b", &["a"])).is_ok());
        assert!(validate_new(&existing, &spec("a", &[])).is_err());
        assert!(validate_new(&existing, &spec("b", &["zzz"])).is_err());
        assert!(validate_new(&existing, &spec("b", &["b"])).is_err());
        assert!(validate_new(&existing, &spec("Bad Id", &[])).is_err());
    }

    #[test]
    fn occupancy() {
        let mut t = task_from_spec(&spec("a", &[]), 0);
        t.assigned_agent = Some("x".into());
        assert!(active_task(std::slice::from_ref(&t), "x").is_none());
        t.status = TaskStatus::Working;
        assert!(active_task(std::slice::from_ref(&t), "x").is_some());
        assert!(active_task(std::slice::from_ref(&t), "y").is_none());
        t.status = TaskStatus::Blocked;
        assert!(!occupies_agent(&t), "blocked before start does not occupy");
        t.started_at = Some(1);
        assert!(occupies_agent(&t));
        t.status = TaskStatus::Review;
        assert!(!occupies_agent(&t), "author is free while under review");
    }

    #[test]
    fn selection_prefers_execution_then_latest_waiting_with_stable_ties() {
        let card = |id: &str, status, started| {
            let mut task = task_from_spec(&spec(id, &[]), 0);
            task.assigned_agent = Some("x".into());
            task.status = status;
            task.started_at = Some(started);
            task
        };
        let mut cards = vec![
            card("older", TaskStatus::Waiting, 1),
            card("executing", TaskStatus::Working, 2),
            card("newer", TaskStatus::Waiting, 3),
            card("tie", TaskStatus::Waiting, 3),
        ];
        assert_eq!(active_task(&cards, "x").unwrap().id, "executing");
        cards[1].status = TaskStatus::Blocked;
        assert_eq!(active_task(&cards, "x").unwrap().id, "executing");
        cards[1].status = TaskStatus::Done;
        assert_eq!(active_task(&cards, "x").unwrap().id, "newer");
        assert!(executing_task(&cards, "x").is_none());
        assert!(!occupies_agent(&cards[0]));
    }
}
