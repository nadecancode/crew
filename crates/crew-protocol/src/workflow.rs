//! The team's workflow as laid on the board: the style's steps, and the
//! units of work (phases) `workflow_run` has laid out, each with its cards
//! and, once declared, its slices. Where a phase stands — which step it is
//! on — is read off those cards, which every client already has, so nothing
//! here changes as work moves.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::{Task, TaskStatus};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct WorkflowInfo {
    /// The steps, in order.
    pub steps: Vec<StepInfo>,
    /// The units laid on the board, oldest first.
    #[serde(default)]
    pub phases: Vec<WorkflowPhase>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct StepInfo {
    pub id: String,
    /// Runs once per slice of a unit, side by side.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub slice: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct WorkflowPhase {
    pub phase: String,
    pub title: String,
    /// What the orchestrator wrote when it ran the unit.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub brief: String,
    pub laid_at: i64,
    /// The unit's phase-step cards, by step id. A phase step after the
    /// slices has none until the slices are declared.
    #[serde(default)]
    pub cards: BTreeMap<String, String>,
    /// The unit's slices, once declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slices: Vec<WorkflowSlice>,
    /// The unit's verdict, read from the file its verdict step produced
    /// when that card was done (`workflow.steps[].verdict`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// The cards whose work landed on the integration branch
    /// (`agents/integration`) when they were completed, and the commit
    /// each landed as. Under worktree isolation a team with a workflow
    /// lands every completed card there, and every card starts from it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub landed: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct WorkflowSlice {
    pub slice: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub brief: String,
    /// The slices this one waits for: those it named, and those declared
    /// before it that touch the same paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<String>,
    /// The paths the slice said it writes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touches: Vec<String>,
    /// The slice's cards, by step id.
    #[serde(default)]
    pub cards: BTreeMap<String, String>,
}

/// Where one unit stands: each step with its cards.
pub struct PhaseProgress<'a> {
    pub phase: &'a WorkflowPhase,
    pub steps: Vec<StepProgress<'a>>,
}

/// One step of a unit: its card, or one card per slice.
pub struct StepProgress<'a> {
    pub id: &'a str,
    pub slice: bool,
    /// The cards: `(slice id, card)`; a phase step has one entry with no
    /// slice. Empty for a slice step while no slice is declared, and for a
    /// phase step after the slices that is not on the board yet.
    pub cards: Vec<(Option<&'a str>, Option<&'a Task>)>,
}

impl StepProgress<'_> {
    pub fn done(&self) -> bool {
        !self.cards.is_empty()
            && self
                .cards
                .iter()
                .all(|(_, task)| task.is_some_and(|t| t.status == TaskStatus::Done))
    }

    /// `plan ◐ checker`, or `execute [api ✓ · db ● executor-2 · ui ○]`,
    /// or `execute ⋯` while its cards are not laid out yet.
    pub fn text(&self) -> String {
        if self.cards.is_empty() {
            return format!("{} ⋯", self.id);
        }
        if !self.slice {
            return format!("{} {}", self.id, card_text(self.cards[0].1));
        }
        let slices: Vec<String> = self
            .cards
            .iter()
            .map(|(slice, task)| format!("{} {}", slice.unwrap_or("?"), card_text(*task)))
            .collect();
        format!("{} [{}]", self.id, slices.join(" · "))
    }
}

/// `✓`, or `● planner` for a card at work (the owner), `◐ checker` for one
/// in review (the reviewer).
fn card_text(task: Option<&Task>) -> String {
    let who = task
        .and_then(|t| match t.status {
            TaskStatus::Review => t.reviewer.as_deref(),
            TaskStatus::Working | TaskStatus::Waiting => t.assigned_agent.as_deref(),
            _ => None,
        })
        .map(|who| format!(" {who}"))
        .unwrap_or_default();
    format!("{}{who}", mark(task))
}

impl PhaseProgress<'_> {
    /// Every card done, every step laid out.
    pub fn done(&self) -> bool {
        self.steps.iter().all(StepProgress::done)
    }

    /// The step the unit is on: the first not done.
    pub fn current(&self) -> Option<usize> {
        self.steps.iter().position(|s| !s.done())
    }

    /// `03-auth (Authentication): research ✓ · plan ● planner · verify ○`
    pub fn line(&self) -> String {
        let steps: Vec<String> = self.steps.iter().map(StepProgress::text).collect();
        let verdict = self
            .phase
            .verdict
            .as_deref()
            .map(|v| format!(" → {v}"))
            .unwrap_or_default();
        format!(
            "{} ({}): {}{verdict}",
            self.phase.phase,
            self.phase.title,
            steps.join(" · ")
        )
    }
}

/// A card's mark in a workflow line.
pub fn mark(task: Option<&Task>) -> &'static str {
    match task.map(|t| t.status) {
        None => "?",
        Some(TaskStatus::Done) => "✓",
        Some(TaskStatus::Working) => "●",
        Some(TaskStatus::Review) => "◐",
        Some(TaskStatus::Waiting) => "◔",
        Some(TaskStatus::Blocked | TaskStatus::Failed) => "✗",
        Some(TaskStatus::Queued | TaskStatus::Ready) => "○",
    }
}

impl WorkflowInfo {
    pub fn progress<'a>(&'a self, tasks: &'a [Task]) -> Vec<PhaseProgress<'a>> {
        let find = |id: &str| tasks.iter().find(|t| t.id == id);
        self.phases
            .iter()
            .map(|phase| PhaseProgress {
                phase,
                steps: self
                    .steps
                    .iter()
                    .map(|step| StepProgress {
                        id: &step.id,
                        slice: step.slice,
                        cards: if step.slice {
                            phase
                                .slices
                                .iter()
                                .filter_map(|s| {
                                    s.cards
                                        .get(&step.id)
                                        .map(|id| (Some(s.slice.as_str()), find(id)))
                                })
                                .collect()
                        } else {
                            phase
                                .cards
                                .get(&step.id)
                                .map(|id| vec![(None, find(id))])
                                .unwrap_or_default()
                        },
                    })
                    .collect(),
            })
            .collect()
    }

    /// The unit the team is on (its index and progress): the oldest not
    /// done, else the last one.
    pub fn current<'a>(&'a self, tasks: &'a [Task]) -> Option<(usize, PhaseProgress<'a>)> {
        let mut phases = self.progress(tasks);
        if phases.is_empty() {
            return None;
        }
        let at = phases
            .iter()
            .position(|p| !p.done())
            .unwrap_or(phases.len() - 1);
        Some((at, phases.swap_remove(at)))
    }

    /// One line for the whole workflow, for a status bar: the unit the
    /// team is on with its place, or that every unit is done.
    pub fn headline(&self, tasks: &[Task]) -> Option<String> {
        let (at, phase) = self.current(tasks)?;
        let n = self.phases.len();
        Some(if phase.done() {
            format!("all {n} done · last {}", phase.line())
        } else {
            format!("phase {}/{n} · {}", at + 1, phase.line())
        })
    }

    /// One line per unit, oldest first.
    pub fn lines(&self, tasks: &[Task]) -> Vec<String> {
        self.progress(tasks)
            .iter()
            .map(PhaseProgress::line)
            .collect()
    }

    /// The steps' ids, in order.
    pub fn step_ids(&self) -> Vec<&str> {
        self.steps.iter().map(|s| s.id.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: TaskStatus, agent: &str, reviewer: Option<&str>) -> Task {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": id, "description": "", "status": status,
            "assigned_agent": agent, "reviewer": reviewer, "created_at": 0
        }))
        .unwrap()
    }

    fn cards(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn info() -> WorkflowInfo {
        WorkflowInfo {
            steps: vec![
                StepInfo {
                    id: "research".into(),
                    slice: false,
                },
                StepInfo {
                    id: "plan".into(),
                    slice: false,
                },
                StepInfo {
                    id: "execute".into(),
                    slice: true,
                },
                StepInfo {
                    id: "verify".into(),
                    slice: false,
                },
            ],
            phases: vec![
                WorkflowPhase {
                    phase: "01-setup".into(),
                    title: "Setup".into(),
                    brief: String::new(),
                    laid_at: 1,
                    cards: cards(&[
                        ("research", "research-01-setup"),
                        ("plan", "plan-01-setup"),
                        ("verify", "verify-01-setup"),
                    ]),
                    slices: vec![WorkflowSlice {
                        slice: "all".into(),
                        title: "All".into(),
                        cards: cards(&[("execute", "execute-01-setup-all")]),
                        ..WorkflowSlice::default()
                    }],
                    verdict: Some("passed".into()),
                    ..WorkflowPhase::default()
                },
                WorkflowPhase {
                    phase: "02-auth".into(),
                    title: "Authentication".into(),
                    brief: "tokens".into(),
                    laid_at: 2,
                    cards: cards(&[("research", "research-02-auth"), ("plan", "plan-02-auth")]),
                    ..WorkflowPhase::default()
                },
            ],
        }
    }

    #[test]
    fn where_each_phase_stands_is_read_off_its_cards_slices_included() {
        let mut tasks = vec![
            task("research-01-setup", TaskStatus::Done, "r", None),
            task("plan-01-setup", TaskStatus::Done, "p", Some("c")),
            task("execute-01-setup-all", TaskStatus::Done, "e", None),
            task("verify-01-setup", TaskStatus::Done, "v", None),
            task("research-02-auth", TaskStatus::Done, "r", None),
            task("plan-02-auth", TaskStatus::Review, "p", Some("c")),
        ];
        let mut info = info();
        assert_eq!(
            info.lines(&tasks),
            [
                "01-setup (Setup): research ✓ · plan ✓ · execute [all ✓] · verify ✓ → passed",
                "02-auth (Authentication): research ✓ · plan ◐ c · execute ⋯ · verify ⋯",
            ]
        );
        let (at, current) = info.current(&tasks).unwrap();
        assert_eq!((at, current.current()), (1, Some(1)));
        assert_eq!(
            info.headline(&tasks).as_deref(),
            Some(
                "phase 2/2 · 02-auth (Authentication): research ✓ · plan ◐ c · execute ⋯ · \
verify ⋯"
            )
        );
        // The slices declared: their cards side by side, the verify after them.
        tasks[5].status = TaskStatus::Done;
        info.phases[1].slices = vec![
            WorkflowSlice {
                slice: "api".into(),
                title: "API".into(),
                cards: cards(&[("execute", "execute-02-auth-api")]),
                ..WorkflowSlice::default()
            },
            WorkflowSlice {
                slice: "ui".into(),
                title: "UI".into(),
                cards: cards(&[("execute", "execute-02-auth-ui")]),
                ..WorkflowSlice::default()
            },
        ];
        info.phases[1]
            .cards
            .insert("verify".into(), "verify-02-auth".into());
        tasks.push(task("execute-02-auth-api", TaskStatus::Done, "e1", None));
        tasks.push(task("execute-02-auth-ui", TaskStatus::Working, "e2", None));
        tasks.push(task("verify-02-auth", TaskStatus::Queued, "v", None));
        assert_eq!(
            info.lines(&tasks)[1],
            "02-auth (Authentication): research ✓ · plan ✓ · execute [api ✓ · ui ● e2] · verify ○"
        );
        assert_eq!(info.current(&tasks).unwrap().1.current(), Some(2));
        // A missing card is marked; every phase done shows the last.
        tasks.pop();
        assert!(info.lines(&tasks)[1].ends_with("verify ?"));
        tasks[7].status = TaskStatus::Done;
        tasks.push(task("verify-02-auth", TaskStatus::Done, "v", None));
        assert!(info
            .headline(&tasks)
            .unwrap()
            .starts_with("all 2 done · last 02-auth"));
        assert!(WorkflowInfo::default().headline(&tasks).is_none());
        assert_eq!(info.step_ids(), ["research", "plan", "execute", "verify"]);
    }
}
