//! Pools: members crew makes as a workflow's slices can start, and retires
//! when their slice is done (gsd-pi's worker per slice). A team declares a
//! pool as an agent with `pool: N` — a template, not a member — and names it
//! as a slice step's crew; before each plan, a member is made from it for
//! every slice card that can start and has no member free, up to N at a
//! time (`<pool>-1`, `<pool>-2`, … the lowest numbers not taken), and after
//! each plan a member with no open card leaves once no card of its pool
//! waits for one. Members are temporary: they are not saved with the
//! roster, and the orchestrator is not asked to find them more to do.

use std::collections::{BTreeSet, HashMap};

use crew_core::config::{pool_member_number, AgentSpec};
use crew_core::{AgentStatus, TaskStatus};

use super::roster::work_of;
use super::{Daemon, Model};
use crate::persistence::NewEvent;
use crate::tasks::occupies_agent;

impl Daemon {
    /// Before the plan: a member of the pool for every slice card that can
    /// start now and has no member free, up to the pool's size. Each is
    /// added like a member the orchestrator adds, temporary.
    pub(crate) async fn staff_pools(&self) {
        let wanted: Vec<AgentSpec> = {
            let model = self.model();
            let Some(team) = model.session.team.as_ref() else {
                return;
            };
            let cards = pool_cards_in(&model);
            if cards.is_empty() {
                return;
            }
            let done = |id: &str| model.task(id).is_some_and(|t| t.status == TaskStatus::Done);
            let mut wanted = Vec::new();
            for template in team.agents.iter().filter(|a| a.pool.is_some()) {
                let size = template.pool.unwrap_or(0) as usize;
                let mut taken: BTreeSet<String> = model
                    .agents
                    .iter()
                    .map(|a| a.id.clone())
                    .chain(model.reserved.iter().cloned())
                    .filter(|id| pool_member_number(&template.id, id).is_some())
                    .collect();
                let free = model
                    .agents
                    .iter()
                    .filter(|a| taken.contains(&a.id) && !a.status.is_terminal())
                    .filter(|a| {
                        !model.tasks.iter().any(|t| {
                            t.assigned_agent.as_deref() == Some(a.id.as_str()) && occupies_agent(t)
                        })
                    })
                    .count();
                let pending = model
                    .tasks
                    .iter()
                    .filter(|t| cards.get(&t.id) == Some(&template.id))
                    .filter(|t| {
                        t.assigned_agent.is_none()
                            && matches!(t.status, TaskStatus::Queued | TaskStatus::Ready)
                            && t.dependencies.iter().all(|d| done(d))
                    })
                    .count();
                let make = pending
                    .saturating_sub(free)
                    .min(size.saturating_sub(taken.len()));
                let mut n = 1;
                for _ in 0..make {
                    while taken.contains(&format!("{}-{n}", template.id)) {
                        n += 1;
                    }
                    let spec = template.pool_member(n);
                    taken.insert(spec.id.clone());
                    wanted.push(spec);
                }
            }
            wanted
        };
        for spec in wanted {
            let pool = spec
                .id
                .rsplit_once('-')
                .map(|(pool, _)| pool.to_string())
                .unwrap_or_default();
            match self.add_agent(spec.clone(), true).await {
                Ok(_) => self.emit(
                    NewEvent::new(
                        "scheduler",
                        "workflow.pool",
                        format!("{} joins from pool {pool} for a slice", spec.id),
                    )
                    .agent(&spec.id),
                ),
                Err(e) => self.emit(
                    NewEvent::new(
                        "scheduler",
                        "workflow.pool",
                        format!(
                            "{} could not be made from pool {pool}: {}; the slice waits",
                            spec.id, e.message
                        ),
                    )
                    .warn(),
                ),
            }
        }
    }

    /// After the plan: a pool's member with no open card leaves once no
    /// card of its pool waits for a member (one whose slice waits on
    /// another keeps a member rather than making one again).
    pub(crate) async fn retire_pool_members(&self) {
        let leaving: Vec<String> = {
            let model = self.model();
            let cards = pool_cards_in(&model);
            model
                .agents
                .iter()
                .filter(|a| a.status != AgentStatus::Spawning)
                .filter_map(|a| pool_of_in(&model, &a.id).map(|pool| (a, pool)))
                .filter(|(a, _)| !work_of(&model.tasks, &a.id).0)
                .filter(|(_, pool)| {
                    !model.tasks.iter().any(|t| {
                        cards.get(&t.id) == Some(pool)
                            && t.assigned_agent.is_none()
                            && matches!(t.status, TaskStatus::Queued | TaskStatus::Ready)
                    })
                })
                .map(|(a, _)| a.id.clone())
                .collect()
        };
        for id in leaving {
            if let Err(e) = self
                .remove_agent(&id, ": its slice is done (a pool's member)")
                .await
            {
                tracing::warn!("retiring {id}: {}", e.message);
            }
        }
    }
}

/// The pool `id` is a member of, if any.
pub(super) fn pool_of_in(model: &Model, id: &str) -> Option<String> {
    model
        .session
        .team
        .as_ref()?
        .pool_of(id)
        .map(|template| template.id.clone())
}

/// The cards on the board that belong to a pool: each slice card of a
/// slice step whose crew is a pool, by the pool's name.
pub(super) fn pool_cards_in(model: &Model) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(team) = model.session.team.as_ref() else {
        return out;
    };
    let Some(workflow) = team.workflow.as_ref() else {
        return out;
    };
    for run in &model.session.workflow_runs {
        for slice in &run.slices {
            for (step_id, task) in &slice.cards {
                let owner = workflow
                    .steps
                    .iter()
                    .find(|s| &s.id == step_id)
                    .and_then(|s| s.agent.as_deref());
                if let Some(owner) = owner.filter(|o| team.is_pool(o)) {
                    out.insert(task.clone(), owner.to_string());
                }
            }
        }
    }
    out
}
