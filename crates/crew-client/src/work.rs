//! Current work selection with the terminal dashboard's existing semantics.

use crew_protocol::{Task, TaskStatus};
use serde::{Deserialize, Serialize};

/// The task to show as an agent's current work, with caller-supplied time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentWork {
    pub task_id: String,
    pub done: bool,
    pub elapsed_ms: Option<i64>,
}

/// Executing work precedes parked cards and other started work.
///
/// Equal active rank/start values keep board order. Completed fallback preserves
/// the TUI's `Option` ordering (unknown below known) and last entry on a tie.
/// The current web port has different completed fallback rules; this seed does
/// not change either client's behavior.
pub fn current_work(tasks: &[Task], agent_id: &str, server_now_ms: i64) -> Option<CurrentWork> {
    let mine = || {
        tasks
            .iter()
            .filter(move |t| t.assigned_agent.as_deref() == Some(agent_id))
    };
    let rank = |t: &Task| match t.status {
        TaskStatus::Working => 2,
        TaskStatus::Blocked if t.started_at.is_some() => 2,
        TaskStatus::Waiting => 1,
        _ => 0,
    };
    let current = mine()
        .filter(|t| !t.status.is_terminal() && (rank(t) > 0 || t.started_at.is_some()))
        .reduce(|current, task| {
            if (rank(task), task.started_at) > (rank(current), current.started_at) {
                task
            } else {
                current
            }
        });
    if let Some(task) = current {
        return Some(CurrentWork {
            task_id: task.id.clone(),
            done: false,
            elapsed_ms: task.started_at.map(|at| (server_now_ms - at).max(0)),
        });
    }
    mine()
        .filter(|t| t.status == TaskStatus::Done)
        .max_by_key(|t| t.completed_at)
        .map(|task| CurrentWork {
            task_id: task.id.clone(),
            done: true,
            elapsed_ms: task.completed_at.map(|at| (server_now_ms - at).max(0)),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: TaskStatus, start: Option<i64>, end: Option<i64>) -> Task {
        Task {
            id: id.into(),
            title: id.into(),
            description: String::new(),
            assigned_agent: Some("a".into()),
            status,
            dependencies: Vec::new(),
            created_at: 0,
            started_at: start,
            completed_at: end,
            artifacts: Vec::new(),
            reviewer: None,
            result: None,
            progress: None,
            note: None,
            timeout_secs: None,
        }
    }

    fn work(id: &str, done: bool, elapsed_ms: Option<i64>) -> Option<CurrentWork> {
        Some(CurrentWork {
            task_id: id.into(),
            done,
            elapsed_ms,
        })
    }

    #[test]
    fn current_work_covers_the_existing_twenty_web_projection_cases() {
        use TaskStatus::*;
        let mut other = task("A", Working, Some(100), None);
        other.assigned_agent = Some("z".into());
        let cases = [
            (
                "resumed older Working beats newer Waiting",
                vec![
                    task("A", Working, Some(100), None),
                    task("B", Waiting, Some(200), None),
                ],
                work("A", false, Some(900)),
            ),
            (
                "Working beats newer Review",
                vec![
                    task("A", Review, Some(300), None),
                    task("B", Working, Some(200), None),
                ],
                work("B", false, Some(800)),
            ),
            (
                "latest started Waiting wins",
                vec![
                    task("A", Waiting, Some(100), None),
                    task("B", Waiting, Some(200), None),
                ],
                work("B", false, Some(800)),
            ),
            (
                "Waiting ties keep first board entry",
                vec![
                    task("A", Waiting, Some(100), None),
                    task("B", Waiting, Some(100), None),
                ],
                work("A", false, Some(900)),
            ),
            (
                "started Blocked beats Waiting",
                vec![
                    task("A", Blocked, Some(100), None),
                    task("B", Waiting, Some(200), None),
                ],
                work("A", false, Some(900)),
            ),
            (
                "unstarted Blocked yields to Waiting",
                vec![
                    task("A", Blocked, None, None),
                    task("B", Waiting, Some(200), None),
                ],
                work("B", false, Some(800)),
            ),
            (
                "unstarted Blocked yields to last Done",
                vec![
                    task("A", Blocked, None, None),
                    task("B", Done, Some(1), Some(50)),
                ],
                work("B", true, Some(950)),
            ),
            (
                "Working with unknown start beats Waiting",
                vec![
                    task("A", Working, None, None),
                    task("B", Waiting, Some(200), None),
                ],
                work("A", false, None),
            ),
            (
                "Waiting beats newer Review",
                vec![
                    task("A", Review, Some(300), None),
                    task("B", Waiting, Some(200), None),
                ],
                work("B", false, Some(800)),
            ),
            (
                "started Ready remains a display fallback",
                vec![task("A", Ready, Some(100), None)],
                work("A", false, Some(900)),
            ),
            (
                "unstarted Ready and Queued are not current",
                vec![task("A", Ready, None, None), task("B", Queued, None, None)],
                None,
            ),
            (
                "latest completion wins among Done",
                vec![
                    task("A", Done, Some(1), Some(10)),
                    task("B", Done, Some(2), Some(20)),
                ],
                work("B", true, Some(980)),
            ),
            (
                "Failed is ignored",
                vec![
                    task("A", Failed, Some(100), None),
                    task("B", Waiting, Some(50), None),
                ],
                work("B", false, Some(950)),
            ),
            (
                "other agent's work is ignored",
                vec![other, task("B", Waiting, Some(50), None)],
                work("B", false, Some(950)),
            ),
            (
                "unknown start preserves unknown elapsed",
                vec![task("A", Working, None, None)],
                work("A", false, None),
            ),
            (
                "elapsed measures chosen executing card",
                vec![task("A", Working, Some(400), None)],
                work("A", false, Some(600)),
            ),
            (
                "multiple Working snapshots keep latest start",
                vec![
                    task("A", Working, Some(100), None),
                    task("B", Working, Some(200), None),
                ],
                work("B", false, Some(800)),
            ),
            (
                "zero is a known Blocked start",
                vec![
                    task("A", Blocked, Some(0), None),
                    task("B", Waiting, Some(200), None),
                ],
                work("A", false, Some(1000)),
            ),
            (
                "status priority does not depend on timestamp magnitude",
                vec![
                    task("A", Working, Some(-100), None),
                    task("B", Waiting, Some(3_000_000_000_000_000), None),
                ],
                work("A", false, Some(1100)),
            ),
            (
                "Waiting without start keeps first board tie",
                vec![
                    task("A", Waiting, None, None),
                    task("B", Waiting, None, None),
                ],
                work("A", false, None),
            ),
        ];
        for (name, tasks, expected) in cases {
            assert_eq!(current_work(&tasks, "a", 1000), expected, "{name}");
        }
    }

    #[test]
    fn completed_fallback_preserves_tui_ties_and_optional_time_order() {
        use TaskStatus::Done;
        for (tasks, expected) in [
            (
                vec![
                    task("A", Done, None, Some(20)),
                    task("B", Done, None, Some(10)),
                ],
                work("A", true, Some(980)),
            ),
            (
                vec![task("A", Done, None, Some(5)), task("B", Done, None, None)],
                work("A", true, Some(995)),
            ),
            (
                vec![
                    task("A", Done, None, Some(50)),
                    task("B", Done, None, Some(50)),
                ],
                work("B", true, Some(950)),
            ),
            (
                vec![task("A", Done, None, None), task("B", Done, None, None)],
                work("B", true, None),
            ),
            (
                vec![task("A", Done, None, None), task("B", Done, None, Some(0))],
                work("B", true, Some(1000)),
            ),
            (
                vec![task("A", Done, None, None), task("B", Done, None, Some(-1))],
                work("B", true, Some(1001)),
            ),
        ] {
            assert_eq!(current_work(&tasks, "a", 1000), expected);
        }
    }

    #[test]
    fn elapsed_clamps_future_times_and_empty_or_unassigned_work_is_absent() {
        assert_eq!(
            current_work(
                &[task("A", TaskStatus::Working, Some(1500), None)],
                "a",
                1000
            ),
            work("A", false, Some(0))
        );
        assert_eq!(
            current_work(&[task("A", TaskStatus::Done, None, Some(1500))], "a", 1000),
            work("A", true, Some(0))
        );
        assert!(current_work(&[], "a", 1000).is_none());
        let mut unassigned = task("A", TaskStatus::Working, Some(10), None);
        unassigned.assigned_agent = None;
        assert!(current_work(&[unassigned], "a", 1000).is_none());
    }
}
