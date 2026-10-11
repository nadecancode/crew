//! The team's guardrails as the daemon enforces them
//! (`crew_core::guardrails`): the crew tools a member may call, whom it may
//! write to, and the workflow laid on the board. Declared in the work style,
//! refused here whatever a session asks for: a member's MCP server offers
//! only what it has (its identity file lists it), and a call that gets past
//! that, through `crew ctl` or a stale list, is refused the same way.

use crew_core::guardrails::{self, Messaging, Order, Outcome, Workflow};
use crew_core::rpc::{Call, RpcError};
use crew_core::{Artifact, MessageType, Task, TaskStatus, BROADCAST, OPERATOR_ID};

use super::server::Who;
use super::Daemon;
use crate::persistence::NewEvent;
use crate::tasks;

impl Daemon {
    /// The `crew_tools` selection of `id`: the orchestrator's from the team,
    /// a member's from its declaration. `None` leaves it every tool.
    pub(crate) fn crew_tools_of(&self, id: &str) -> Option<Vec<String>> {
        let model = self.model();
        if model.agent(id).is_some_and(|a| a.orchestrator) {
            model
                .session
                .team
                .as_ref()
                .and_then(|t| t.orchestrator.crew_tools.clone())
        } else {
            model.local.get(id).and_then(|l| l.spec.crew_tools.clone())
        }
    }

    /// The crew tools `id` gets, by name, when the style takes some away
    /// (what its identity file carries, and its MCP server offers).
    pub(crate) fn tool_names_of(&self, id: &str, orchestrator: bool) -> Option<Vec<String>> {
        let selection = self.crew_tools_of(id)?;
        Some(
            crate::mcp::tools::definitions()
                .into_iter()
                .filter(|t| orchestrator || !t.orchestrator_only)
                .filter(|t| guardrails::crew_tool_allowed(Some(&selection), t.name))
                .map(|t| t.name.to_string())
                .collect(),
        )
    }

    /// Refuse a call outside the caller's crew tools.
    pub(crate) fn check_crew_tool(&self, who: &Who, call: &Call) -> Result<(), RpcError> {
        let Who::Agent(id) = who else {
            return Ok(());
        };
        let tool = match call {
            Call::Send(params) if params.to == BROADCAST => "agent_broadcast",
            Call::Send(_) => "agent_send",
            other => other.method(),
        };
        if !guardrails::is_governed(tool) {
            return Ok(());
        }
        let Some(selection) = self.crew_tools_of(id) else {
            return Ok(());
        };
        if guardrails::crew_tool_allowed(Some(&selection), tool) {
            return Ok(());
        }
        Err(RpcError::forbidden(format!(
            "{tool} is not among the crew tools this team's style gives {id} (crew_tools: {})",
            if selection.is_empty() {
                "none beyond the basic ones".to_string()
            } else {
                selection.join(", ")
            }
        )))
    }

    /// Whom `from` writes to instead of a teammate under hub messaging: its
    /// parent for a subagent, else the orchestrator, else the operator.
    pub(crate) fn hub_of(&self, from: &str) -> String {
        let model = self.model();
        if let Some(parent) = model.agent(from).and_then(|a| a.parent.clone()) {
            return parent;
        }
        model
            .orchestrator()
            .map(|a| a.id.clone())
            .unwrap_or_else(|| OPERATOR_ID.to_string())
    }

    /// Whether `from` (an agent; anyone else may) may write to `to` under
    /// the team's messaging rule. Under `hub`, a member writes to the
    /// orchestrator (the operator, when there is none) and to its own
    /// subagents, a subagent to its parent; the orchestrator to anyone. The
    /// error says where to write instead.
    pub(crate) fn may_write(&self, from: &str, to: &str) -> Result<(), String> {
        let model = self.model();
        let hub = model
            .session
            .team
            .as_ref()
            .is_some_and(|t| t.guardrails.messaging == Messaging::Hub);
        if !hub {
            return Ok(());
        }
        let Some(sender) = model.agent(from) else {
            return Ok(());
        };
        if sender.orchestrator {
            return Ok(());
        }
        let orchestrator = model.orchestrator().map(|a| a.id.as_str());
        let allowed = match &sender.parent {
            Some(parent) => to == parent,
            None if to == BROADCAST => false,
            None => {
                Some(to) == orchestrator
                    || (to == OPERATOR_ID && orchestrator.is_none())
                    || model
                        .agent(to)
                        .is_some_and(|a| a.parent.as_deref() == Some(from))
            }
        };
        if allowed {
            return Ok(());
        }
        // Not hub_of: the model is locked here.
        let hub = match &sender.parent {
            Some(parent) => parent.clone(),
            None => orchestrator.unwrap_or(OPERATOR_ID).to_string(),
        };
        Err(match &sender.parent {
            Some(_) => format!(
                "this team's members write to whoever they report to only (guardrails.messaging: \
hub): you report to {hub}, so write to it, and it carries on what {to} needs"
            ),
            None if to == BROADCAST => format!(
                "this team's members write to {hub} only (guardrails.messaging: hub): nobody \
broadcasts but the orchestrator; tell {hub} what the team needs to know"
            ),
            None => format!(
                "this team's members write to {hub} only (guardrails.messaging: hub): put what \
{to} needs to {hub}, which carries it on, or on your card (task_complete's result, your commits)"
            ),
        })
    }

    fn team_workflow(&self) -> Result<Workflow, RpcError> {
        self.model()
            .session
            .team
            .as_ref()
            .and_then(|t| t.workflow.clone())
            .ok_or_else(|| RpcError::invalid("this team has no workflow: its style declares none"))
    }

    /// Every card checked against the board as it will be when its turn
    /// comes (the cards before it in the chain included, the cards in
    /// `except` gone), and its owners on the team: all or none.
    fn check_new_cards(
        &self,
        specs: &[crew_core::config::TaskSpec],
        except: &[String],
    ) -> Result<(), RpcError> {
        let model = self.model();
        let mut board: Vec<Task> = model
            .tasks
            .iter()
            .filter(|t| !except.contains(&t.id))
            .cloned()
            .collect();
        for spec in specs {
            tasks::validate_new(&board, spec).map_err(RpcError::invalid)?;
            for who in [&spec.agent, &spec.reviewer].into_iter().flatten() {
                if model.agent(who).is_none() {
                    return Err(RpcError::not_found(format!(
                        "the workflow names '{who}', which is not on the team"
                    )));
                }
            }
            board.push(tasks::task_from_spec(spec, crew_core::util::now_ms()));
        }
        Ok(())
    }

    fn lay_cards(&self, specs: Vec<crew_core::config::TaskSpec>) -> Result<Vec<Task>, RpcError> {
        let mut created = Vec::with_capacity(specs.len());
        for spec in specs {
            created.push(self.create_task(spec)?);
        }
        Ok(created)
    }

    /// Lay the team's workflow on the board for one unit of work: its
    /// phase steps up to the slices, chained as the steps are. All or none.
    pub(crate) fn run_workflow(
        &self,
        phase: &str,
        title: &str,
        brief: &str,
    ) -> Result<Vec<Task>, RpcError> {
        let workflow = self.team_workflow()?;
        let specs = workflow
            .phase_cards(phase, title, brief)
            .map_err(|e| RpcError::invalid(e.to_string()))?;
        if self
            .model()
            .session
            .workflow_runs
            .iter()
            .any(|r| r.phase == phase)
        {
            return Err(RpcError::conflict(format!("phase {phase} was run already")));
        }
        if workflow.order == Order::Sequential {
            self.hold_sequence(&workflow, phase)?;
        }
        self.check_new_cards(&specs, &[])?;
        let created = self.lay_cards(specs)?;
        // The run is part of the session: the dashboards show which phase
        // the team is on from it, and it survives a daemon restart.
        let mut cards = std::collections::BTreeMap::new();
        for task in &created {
            if let Some(step) = workflow
                .steps
                .iter()
                .find(|s| Workflow::card_id(s, phase, None) == task.id)
            {
                cards.insert(step.id.clone(), task.id.clone());
            }
        }
        self.model()
            .session
            .workflow_runs
            .push(crew_core::workflow::WorkflowPhase {
                phase: phase.to_string(),
                title: title.trim().to_string(),
                brief: brief.trim().to_string(),
                laid_at: crew_core::util::now_ms(),
                cards,
                slices: Vec::new(),
                verdict: None,
                landed: std::collections::BTreeMap::new(),
            });
        self.save_session();
        self.emit(
            NewEvent::new(
                "scheduler",
                "workflow.run",
                format!(
                    "workflow laid out for {phase} ({title}): {}{}",
                    created.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(" → "),
                    if workflow.has_slices() { " → (slices)" } else { "" }
                ),
            )
            .data(serde_json::json!({ "phase": phase, "tasks": created.iter().map(|t| t.id.clone()).collect::<Vec<_>>() })),
        );
        Ok(created)
    }

    /// Declare a unit's slices: its slice steps' cards, one per slice,
    /// side by side, and the phase steps after them. All or none. Declared
    /// again while none of those cards has started (a review changed the
    /// plan's split), the cards are taken off the board and laid anew;
    /// once a slice has begun, they stay.
    pub(crate) fn declare_slices(
        &self,
        phase: &str,
        slices: Vec<crew_core::guardrails::SliceSpec>,
    ) -> Result<Vec<Task>, RpcError> {
        let workflow = self.team_workflow()?;
        let (run, title, brief, old) = {
            let model = self.model();
            let (run, p) = model
                .session
                .workflow_runs
                .iter()
                .enumerate()
                .find(|(_, r)| r.phase == phase)
                .ok_or_else(|| RpcError::not_found(format!("no phase '{phase}' was run")))?;
            // The cards the earlier declaration laid: the slices' and the
            // phase steps' after them.
            let old: Vec<String> = p
                .slices
                .iter()
                .flat_map(|s| s.cards.values().cloned())
                .chain(p.cards.iter().filter_map(|(step, id)| {
                    workflow
                        .steps
                        .iter()
                        .position(|s| &s.id == step)
                        .filter(|i| workflow.after_slices(*i))
                        .map(|_| id.clone())
                }))
                .collect();
            let started = old.iter().find(|id| {
                model.task(id).is_none_or(|t| {
                    t.started_at.is_some()
                        || !matches!(t.status, TaskStatus::Queued | TaskStatus::Ready)
                })
            });
            if let Some(started) = started {
                return Err(RpcError::conflict(format!(
                    "phase {phase}'s slices are declared already ({}), and {started} has \
started: slices are laid out again only before any of them begins",
                    p.slices
                        .iter()
                        .map(|s| s.slice.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            (run, p.title.clone(), p.brief.clone(), old)
        };
        let mut specs = workflow
            .slice_cards(phase, &title, &brief, &slices)
            .map_err(|e| RpcError::invalid(e.to_string()))?;
        // A slice step whose crew is a pool: its cards wait for a member
        // of the pool, made as each can start (daemon::pools).
        {
            let model = self.model();
            if let Some(team) = model.session.team.as_ref() {
                for spec in specs.iter_mut() {
                    if spec.agent.as_deref().is_some_and(|a| team.is_pool(a)) {
                        spec.agent = None;
                    }
                }
            }
        }
        // As laid: dependencies first, `depends` complete (the slices that
        // touch the same paths included).
        let slices =
            Workflow::slice_order(&slices).map_err(|e| RpcError::invalid(e.to_string()))?;
        self.check_new_cards(&specs, &old)?;
        let again = !old.is_empty();
        if again {
            self.remove_unstarted_tasks(&old)?;
        }
        let created = self.lay_cards(specs)?;
        {
            let mut model = self.model();
            let Some(p) = model.session.workflow_runs.get_mut(run) else {
                return Err(RpcError::not_found("the run vanished"));
            };
            p.cards.retain(|_, id| !old.contains(id));
            p.slices = slices
                .iter()
                .map(|s| crew_core::workflow::WorkflowSlice {
                    slice: s.id.clone(),
                    title: s.title.clone(),
                    brief: s.brief.clone(),
                    depends: s.depends.clone(),
                    touches: s.touches.clone(),
                    cards: std::collections::BTreeMap::new(),
                })
                .collect();
            for task in &created {
                for step in &workflow.steps {
                    if Workflow::card_id(step, phase, None) == task.id {
                        p.cards.insert(step.id.clone(), task.id.clone());
                    }
                    for slice in p.slices.iter_mut() {
                        if Workflow::card_id(step, phase, Some(&slice.slice)) == task.id {
                            slice.cards.insert(step.id.clone(), task.id.clone());
                        }
                    }
                }
            }
        }
        self.save_session();
        self.emit(
            NewEvent::new(
                "scheduler",
                "workflow.slices",
                format!(
                    "phase {phase}: {} slice(s) laid out{} ({})",
                    slices.len(),
                    if again { " again" } else { "" },
                    slices
                        .iter()
                        .map(|s| {
                            if s.depends.is_empty() {
                                s.id.clone()
                            } else {
                                format!("{} after {}", s.id, s.depends.join("+"))
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .data(serde_json::json!({ "phase": phase, "tasks": created.iter().map(|t| t.id.clone()).collect::<Vec<_>>() })),
        );
        Ok(created)
    }

    /// Whether the team lands its cards: it has a workflow, and its members
    /// work in worktrees of their own, so there is a branch per member and
    /// an integration branch to land on.
    pub(crate) fn lands_cards(&self) -> bool {
        let model = self.model();
        model.session.isolation == "worktree"
            && model
                .session
                .team
                .as_ref()
                .is_some_and(|t| t.workflow.is_some())
    }

    /// In a team that lands its cards, a card is done only once its author's
    /// branch is merged into the integration branch (`agents/integration`),
    /// the line every card starts from, so that whatever comes next — a
    /// slice that waits for this one, the step after the slices, the next
    /// unit — builds on it without merging by hand. The merge is crew's,
    /// never forced: one that conflicts leaves the card open, and the author
    /// takes the integration branch into its worktree, resolves, and
    /// completes again. Recorded on the run for a workflow card.
    pub(crate) async fn hold_landing(
        &self,
        task: &Task,
        author: &str,
        new_artifacts: &[Artifact],
    ) -> Result<(), RpcError> {
        if !self.lands_cards() {
            return Ok(());
        }
        // A card without a commit (a verdict given, a question answered)
        // leaves nothing to land; the merges its worktree took are not work.
        if !new_artifacts
            .iter()
            .chain(task.artifacts.iter())
            .any(|a| a.kind == "commit")
        {
            return Ok(());
        }
        if self
            .model()
            .agent(author)
            .is_none_or(|a| a.branch.is_none())
        {
            return Ok(());
        }
        let (_, steps) = self
            .integrate_agents(vec![author.to_string()])
            .await
            .map_err(|e| {
                RpcError::invalid(format!(
                    "task {} is not done: its work could not be landed on {}: {}",
                    task.id,
                    crate::git::INTEGRATION_BRANCH,
                    e.message
                ))
            })?;
        // No step: nothing of the author's is ahead of the integration
        // branch, so there is nothing new to land.
        let Some(step) = steps.into_iter().find(|s| s.agent == author) else {
            return Ok(());
        };
        if !step.outcome.ok {
            let files = step
                .outcome
                .conflict
                .as_ref()
                .map(|c| c.files.join(", "))
                .filter(|f| !f.is_empty())
                .unwrap_or_else(|| "files the merge reported".to_string());
            return Err(RpcError::invalid(format!(
                "task {} is not done: its work does not merge into {}, the line every card \
starts from (conflicts in {files}). git_merge(from_agent: \"{}\"), resolve the conflicts, \
git_commit, then task_complete again",
                task.id,
                crate::git::INTEGRATION_BRANCH,
                crate::daemon::gitops::INTEGRATION_PEER
            )));
        }
        let sha = step.outcome.commit.as_ref().map(|c| c.sha.clone());
        if let (Some(sha), Some((run, ..))) = (&sha, self.workflow_card(&task.id)) {
            if let Some(p) = self.model().session.workflow_runs.get_mut(run) {
                p.landed.insert(task.id.clone(), sha.clone());
            }
            self.save_session();
        }
        self.emit(
            NewEvent::new(
                "scheduler",
                "workflow.landed",
                format!(
                    "task {} landed on {}{}",
                    task.id,
                    crate::git::INTEGRATION_BRANCH,
                    sha.as_deref()
                        .map(|s| format!(" as {}", crew_core::util::truncate_chars(s, 12)))
                        .unwrap_or_default()
                ),
            )
            .task(&task.id)
            .agent(author),
        );
        Ok(())
    }

    /// A slice card's commits held against what the slice said it touches:
    /// a file outside those paths (and outside what the step produces, and
    /// where it produces it) is reported — the plan's split was wrong, or
    /// the executor strayed — so the orchestrator knows, the slices running
    /// beside it are warned of what may conflict when they land, and the
    /// planner can declare better next time. Deterministic; holds nothing.
    pub(crate) async fn report_strays(
        &self,
        task: &Task,
        author: &str,
        new_artifacts: &[Artifact],
    ) {
        let Some((run, step, Some(n), workflow)) = self.workflow_card(&task.id) else {
            return;
        };
        let (phase, title, slice, dir) = {
            let model = self.model();
            if model.session.isolation != "worktree" {
                return;
            }
            let p = &model.session.workflow_runs[run];
            let Some(slice) = p.slices.get(n).cloned() else {
                return;
            };
            let Some(dir) = model
                .agent(author)
                .map(|a| std::path::PathBuf::from(&a.workdir))
            else {
                return;
            };
            (p.phase.clone(), p.title.clone(), slice, dir)
        };
        if slice.touches.is_empty() {
            return;
        }
        let commits: Vec<String> = new_artifacts
            .iter()
            .chain(task.artifacts.iter())
            .filter(|a| a.kind == "commit")
            .map(|a| a.value.clone())
            .collect();
        if commits.is_empty() {
            return;
        }
        let spec = crew_core::guardrails::SliceSpec {
            id: slice.slice.clone(),
            title: slice.title.clone(),
            brief: slice.brief.clone(),
            ..Default::default()
        };
        let mut allowed = slice.touches.clone();
        for produced in workflow.produces_of(step, &phase, &title, Some(&spec)) {
            if let Some((dir, _)) = produced.rsplit_once('/') {
                allowed.push(dir.to_string());
            }
            allowed.push(produced);
        }
        let vcs = self.vcs();
        let files = self
            .agent_git(author, move || {
                let mut files = std::collections::BTreeSet::new();
                for commit in commits {
                    files.extend(vcs.files_of(&dir, &commit)?);
                }
                Ok(files)
            })
            .await;
        let files = match files {
            Ok(files) => files,
            Err(e) => {
                tracing::debug!("the files of {}'s commits: {}", task.id, e.message);
                return;
            }
        };
        let strays: Vec<String> = files
            .into_iter()
            .filter(|f| {
                !allowed
                    .iter()
                    .any(|a| crew_core::guardrails::touches_overlap(a, f))
            })
            .collect();
        if strays.is_empty() {
            return;
        }
        let text = format!(
            "slice {} ({}) wrote outside what it touches ({}): {}",
            slice.slice,
            task.id,
            slice.touches.join(", "),
            strays.join(", ")
        );
        self.emit(
            NewEvent::new("scheduler", "workflow.strayed", text.clone())
                .task(&task.id)
                .agent(author)
                .warn(),
        );
        self.notify_orchestrator(&format!(
            "{text}. The plan's split was wrong, or the executor strayed: a slice running beside \
it may conflict with those files when it lands, and the planner should name what each slice \
writes next time."
        ))
        .await;
    }

    /// Before a card is handed out in a team that lands its cards: the
    /// integration branch is merged into the owner's worktree, so the card
    /// starts from every card landed before it. What could not be merged
    /// (a conflict, uncommitted changes) does not hold the card: it is said
    /// on the card's message, with what to do first.
    pub(crate) async fn sync_card(&self, agent: &str) -> Option<String> {
        if !self.lands_cards() {
            return None;
        }
        self.model().agent(agent)?.branch.as_ref()?;
        let branch = crate::git::INTEGRATION_BRANCH;
        let peer = crate::daemon::gitops::INTEGRATION_PEER;
        let why = match self.sync_from_integration(agent).await {
            Ok(outcome) if outcome.ok => return None,
            Ok(outcome) => format!(
                "conflicts in {}",
                outcome
                    .conflict
                    .as_ref()
                    .map(|c| c.files.join(", "))
                    .filter(|f| !f.is_empty())
                    .unwrap_or_else(|| "files the merge reported".to_string())
            ),
            // Nothing has landed yet: the card starts from the base.
            Err(e) if e.code == "not_found" => return None,
            Err(e) => e.message,
        };
        Some(format!(
            "Your worktree did not take {branch}, where every completed card lands and your card \
builds on: {why}. First git_merge(from_agent: \"{peer}\"), settle what it reports, and \
git_commit; then the card."
        ))
    }

    /// `order: sequential`: a new unit may start only once the last one is
    /// done (its slices declared and every card done), and, where a step
    /// gives a verdict, gone on from: a pass lets anything follow, a redo
    /// only a follow-up run of the same unit.
    fn hold_sequence(&self, workflow: &Workflow, next: &str) -> Result<(), RpcError> {
        let (last, open) = {
            let model = self.model();
            let Some(last) = model.session.workflow_runs.last().cloned() else {
                return Ok(());
            };
            let describe = |step: &str, id: &str| {
                let status = model
                    .task(id)
                    .map(|t| t.status.to_string())
                    .unwrap_or_else(|| "gone".to_string());
                format!("{step} ({id}: {status})")
            };
            let not_done = |id: &str| !model.task(id).is_some_and(|t| t.status == TaskStatus::Done);
            let open = workflow.steps.iter().find_map(|step| {
                if step.scope == crew_core::guardrails::Scope::Slice {
                    if last.slices.is_empty() {
                        return Some(format!(
                            "{}: its slices are not declared yet (workflow_slices)",
                            step.id
                        ));
                    }
                    last.slices.iter().find_map(|s| {
                        s.cards
                            .get(&step.id)
                            .filter(|id| not_done(id))
                            .map(|id| describe(&step.id, id))
                    })
                } else {
                    match last.cards.get(&step.id) {
                        Some(id) if not_done(id) => Some(describe(&step.id, id)),
                        Some(_) => None,
                        None => Some(format!("{}: not laid out yet", step.id)),
                    }
                }
            });
            (last, open)
        };
        if let Some(open) = open {
            return Err(RpcError::conflict(format!(
                "phase {} is still on {open}: one unit at a time (workflow.order: sequential)",
                last.phase
            )));
        }
        let Some(step) = workflow.verdict_step() else {
            return Ok(());
        };
        let verdict = workflow
            .verdict_of(step, &last.phase, &last.title)
            .expect("the verdict step has a verdict");
        let step_id = &workflow.steps[step].id;
        let Some(value) = last.verdict.as_deref() else {
            return Err(RpcError::conflict(format!(
                "phase {} has no verdict recorded: task_retry its {step_id} card so {} is \
written and read again",
                last.phase, verdict.file
            )));
        };
        match verdict.outcome(value) {
            Outcome::Pass => Ok(()),
            Outcome::Redo if next.starts_with(&format!("{}-", last.phase)) => Ok(()),
            Outcome::Redo => Err(RpcError::conflict(format!(
                "phase {p}'s verdict is {value}: only a follow-up run of it may come next, with \
an id starting with '{p}-' (say '{p}-gaps') and what it found in the brief",
                p = last.phase
            ))),
            Outcome::Other => Err(RpcError::conflict(format!(
                "phase {}'s verdict is {value}, which the workflow does not go on from (it goes \
on from {}). Settle it with the operator (operator_ask), then task_retry the {step_id} card \
so {} is written and read again",
                last.phase,
                verdict.choices(),
                verdict.file
            ))),
        }
    }

    /// The run, step and (for a slice card) slice a card belongs to, when
    /// it is a workflow card.
    pub(crate) fn workflow_card(
        &self,
        task_id: &str,
    ) -> Option<(usize, usize, Option<usize>, Workflow)> {
        let model = self.model();
        let workflow = model.session.team.as_ref()?.workflow.clone()?;
        let step_index = |step: &str| workflow.steps.iter().position(|s| s.id == step);
        for (run, phase) in model.session.workflow_runs.iter().enumerate() {
            if let Some((step, _)) = phase.cards.iter().find(|(_, id)| *id == task_id) {
                return step_index(step).map(|s| (run, s, None, workflow.clone()));
            }
            for (n, slice) in phase.slices.iter().enumerate() {
                if let Some((step, _)) = slice.cards.iter().find(|(_, id)| *id == task_id) {
                    return step_index(step).map(|s| (run, s, Some(n), workflow.clone()));
                }
            }
        }
        None
    }

    /// A workflow card is done only with its step's `produces` in its
    /// commit (the newest commit on the card), and its verdict, if the step
    /// gives one, readable there. Deterministic, held whatever the result
    /// says; the verdict is recorded on the run.
    pub(crate) async fn hold_workflow_gates(
        &self,
        task: &Task,
        author: &str,
        new_artifacts: &[Artifact],
    ) -> Result<(), RpcError> {
        let Some((run, step, slice, workflow)) = self.workflow_card(&task.id) else {
            return Ok(());
        };
        let (phase, title, slice) = {
            let model = self.model();
            let p = &model.session.workflow_runs[run];
            let slice =
                slice
                    .and_then(|n| p.slices.get(n))
                    .map(|s| crew_core::guardrails::SliceSpec {
                        id: s.slice.clone(),
                        title: s.title.clone(),
                        brief: s.brief.clone(),
                        ..Default::default()
                    });
            (p.phase.clone(), p.title.clone(), slice)
        };
        let step_id = workflow.steps[step].id.clone();
        let produces = workflow.produces_of(step, &phase, &title, slice.as_ref());
        let verdict = workflow.verdict_of(step, &phase, &title);
        if produces.is_empty() && verdict.is_none() {
            return Ok(());
        }
        let mut wanted: Vec<String> = produces.clone();
        if let Some(v) = &verdict {
            if !wanted.contains(&v.file) {
                wanted.push(v.file.clone());
            }
        }
        let shared = self.model().session.isolation == "shared";
        // What is there: read from the tree everyone shares, or from the
        // card's newest commit in the author's workspace.
        let found: Vec<(String, Option<String>)> = if shared {
            let root = self.workspace.clone();
            wanted
                .iter()
                .map(|path| (path.clone(), std::fs::read_to_string(root.join(path)).ok()))
                .collect()
        } else {
            let commit = new_artifacts
                .iter()
                .rev()
                .chain(task.artifacts.iter().rev())
                .find(|a| a.kind == "commit")
                .map(|a| a.value.clone());
            let Some(commit) = commit else {
                return Err(RpcError::invalid(format!(
                    "task {} is not done: {step_id} leaves {} in a commit, and the card has \
none. git_commit, then task_complete again",
                    task.id,
                    produces.join(", ")
                )));
            };
            let dir = std::path::PathBuf::from(
                self.model()
                    .agent(author)
                    .map(|a| a.workdir.clone())
                    .ok_or_else(|| RpcError::not_found(format!("no agent '{author}'")))?,
            );
            let vcs = self.vcs();
            let paths = wanted.clone();
            let rev = commit.clone();
            self.agent_git(author, move || {
                let mut found: Vec<(String, Option<String>)> = Vec::new();
                for path in paths {
                    let text = vcs.file_at(&dir, &rev, &path)?;
                    found.push((path, text));
                }
                Ok(found)
            })
            .await
            .map_err(|e| {
                RpcError::invalid(format!(
                    "task {} cannot be checked against commit {}: {}",
                    task.id,
                    crew_core::util::truncate_chars(&commit, 12),
                    e.message
                ))
            })?
        };
        let missing: Vec<&str> = produces
            .iter()
            .filter(|p| {
                found
                    .iter()
                    .any(|(path, text)| path == *p && text.is_none())
            })
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(RpcError::invalid(format!(
                "task {} is not done: {} {} not in {}; {step_id} produces {}. Write {}, {}then \
task_complete again",
                task.id,
                missing.join(", "),
                if missing.len() == 1 { "is" } else { "are" },
                if shared {
                    "the workspace"
                } else {
                    "the card's newest commit"
                },
                produces.join(", "),
                if missing.len() == 1 { "it" } else { "them" },
                if shared { "" } else { "git_commit, " },
            )));
        }
        if let Some(verdict) = verdict {
            let text = found
                .iter()
                .find(|(path, _)| *path == verdict.file)
                .and_then(|(_, text)| text.clone())
                .ok_or_else(|| {
                    RpcError::invalid(format!(
                        "task {} is not done: {} is missing; it must say `{}: <value>`, one of {}",
                        task.id,
                        verdict.file,
                        verdict.key,
                        verdict.choices()
                    ))
                })?;
            let Some(value) = verdict.read(&text) else {
                return Err(RpcError::invalid(format!(
                    "task {} is not done: {} has no `{}:` line; it must say one of {}",
                    task.id,
                    verdict.file,
                    verdict.key,
                    verdict.choices()
                )));
            };
            let outcome = verdict.outcome(&value);
            {
                let mut model = self.model();
                if let Some(p) = model.session.workflow_runs.get_mut(run) {
                    p.verdict = Some(value.clone());
                }
            }
            self.save_session();
            let event = NewEvent::new(
                "scheduler",
                "workflow.verdict",
                format!(
                    "phase {phase} ({title}): {step_id} says {}: {value}{}",
                    verdict.key,
                    match outcome {
                        Outcome::Pass => "",
                        Outcome::Redo => " — a follow-up run comes next",
                        Outcome::Other => " — the team cannot go on from this by itself",
                    }
                ),
            )
            .task(&task.id);
            self.emit(if outcome == Outcome::Other {
                event.warn()
            } else {
                event
            });
            if outcome == Outcome::Other {
                self.notify_orchestrator(&format!(
                    "phase {phase}'s {step_id} says {}: {value}, which the workflow does not go on from by itself (it goes on from {}). Settle it with the operator (operator_ask), then task_retry the {step_id} card so {} is written and read again.",
                    verdict.key,
                    verdict.choices(),
                    verdict.file
                ))
                .await;
            }
        }
        Ok(())
    }

    /// Who may lay out a unit's slices or queue units: the orchestrator and
    /// the operator; a member for the unit its open card belongs to
    /// (slices), or with any open card (phases: the roadmap's author).
    pub(crate) fn workflow_caller(&self, who: &Who, phase: Option<&str>) -> Result<(), RpcError> {
        let Who::Agent(id) = who else {
            return Ok(());
        };
        if self.is_orchestrator(id) {
            return Ok(());
        }
        let card = crate::tasks::active_task(&self.model().tasks, id)
            .cloned()
            .ok_or_else(|| {
                RpcError::forbidden("you have no open card; the orchestrator lays the workflow out")
            })?;
        let Some(phase) = phase else {
            return Ok(());
        };
        let owned = self
            .workflow_card(&card.id)
            .map(|(run, _, _, _)| self.model().session.workflow_runs[run].phase.clone());
        match owned {
            Some(p) if p == phase => Ok(()),
            Some(p) => Err(RpcError::forbidden(format!(
                "your open card belongs to phase {p}, not {phase}"
            ))),
            None => Err(RpcError::forbidden(format!(
                "your open card ({}) is not a card of this workflow; only the orchestrator \
declares {phase}'s slices",
                card.id
            ))),
        }
    }

    /// Queue units for the workflow, after what is queued. Under `auto`
    /// crew runs them in turn; the ids that are queued come back.
    pub(crate) fn queue_phases(
        &self,
        phases: Vec<crew_core::guardrails::PhaseSpec>,
    ) -> Result<Vec<String>, RpcError> {
        let workflow = self.team_workflow()?;
        if phases.is_empty() {
            return Err(RpcError::invalid("queue at least one phase"));
        }
        {
            let model = self.model();
            let mut seen: Vec<&str> = model
                .session
                .workflow_queue
                .iter()
                .map(|p| p.id.as_str())
                .chain(model.session.workflow_runs.iter().map(|r| r.phase.as_str()))
                .collect();
            for phase in &phases {
                workflow
                    .phase_cards(&phase.id, &phase.title, &phase.brief)
                    .map_err(|e| RpcError::invalid(e.to_string()))?;
                if seen.contains(&phase.id.as_str()) {
                    return Err(RpcError::conflict(format!(
                        "phase {} is queued or run already",
                        phase.id
                    )));
                }
                seen.push(&phase.id);
            }
        }
        let queued: Vec<String> = {
            let mut model = self.model();
            model.session.workflow_queue.extend(phases.iter().cloned());
            model
                .session
                .workflow_queue
                .iter()
                .map(|p| p.id.clone())
                .collect()
        };
        self.save_session();
        self.emit(NewEvent::new(
            "scheduler",
            "workflow.queued",
            format!(
                "{} phase(s) queued: {}",
                phases.len(),
                phases
                    .iter()
                    .map(|p| p.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
        self.auto_advance();
        Ok(queued)
    }

    /// Under `auto`, run the next queued unit when the sequence lets it,
    /// or a follow-up run after a redo verdict (three at most). What the
    /// workflow cannot go on from was said when the verdict was read.
    pub(crate) fn auto_advance(&self) {
        let Ok(workflow) = self.team_workflow() else {
            return;
        };
        if !workflow.auto {
            return;
        }
        // A run lays cards, which replans, which may land here again.
        {
            let mut model = self.model();
            if model.auto_advancing {
                return;
            }
            model.auto_advancing = true;
        }
        self.advance_once(&workflow);
        self.model().auto_advancing = false;
    }

    fn advance_once(&self, workflow: &Workflow) {
        let (last, next) = {
            let model = self.model();
            (
                model.session.workflow_runs.last().cloned(),
                model.session.workflow_queue.first().cloned(),
            )
        };
        // A redo verdict: a follow-up run of the same unit, named after it.
        if let (Some(last), Some(step)) = (&last, workflow.verdict_step()) {
            let verdict = workflow
                .verdict_of(step, &last.phase, &last.title)
                .expect("the verdict step has a verdict");
            if last
                .verdict
                .as_deref()
                .is_some_and(|v| verdict.outcome(v) == Outcome::Redo)
            {
                let base = last
                    .phase
                    .trim_end_matches(|c: char| c.is_ascii_digit())
                    .trim_end_matches("-gaps")
                    .to_string();
                let rounds = self
                    .model()
                    .session
                    .workflow_runs
                    .iter()
                    .filter(|r| r.phase.starts_with(&format!("{base}-gaps")))
                    .count();
                if rounds >= 3 {
                    self.emit(
                        NewEvent::new(
                            "scheduler",
                            "workflow.auto_stopped",
                            format!(
                                "phase {base} found gaps three times; the workflow does not run \
a fourth follow-up by itself"
                            ),
                        )
                        .warn(),
                    );
                    return;
                }
                let id = if rounds == 0 {
                    format!("{base}-gaps")
                } else {
                    format!("{base}-gaps{}", rounds + 1)
                };
                let title = format!("{} (gaps, round {})", last.title, rounds + 1);
                let brief = format!(
                    "Follow-up of phase {}: close the gaps its {} found. Read it first; the \
earlier cards' commits carry the work so far.",
                    last.phase, verdict.file
                );
                if let Err(e) = self.run_workflow(&id, &title, &brief) {
                    self.emit(
                        NewEvent::new(
                            "scheduler",
                            "workflow.auto_stopped",
                            format!(
                                "the follow-up run of {} could not start: {}",
                                last.phase, e.message
                            ),
                        )
                        .warn(),
                    );
                }
                return;
            }
        }
        let Some(next) = next else {
            return;
        };
        match self.run_workflow(&next.id, &next.title, &next.brief) {
            Ok(_) => {
                self.model()
                    .session
                    .workflow_queue
                    .retain(|p| p.id != next.id);
                self.save_session();
            }
            // The sequence holds it: again when a card is done.
            Err(e) if e.code == "conflict" => {}
            Err(e) => self.emit(
                NewEvent::new(
                    "scheduler",
                    "workflow.auto_stopped",
                    format!("phase {} could not start: {}", next.id, e.message),
                )
                .warn(),
            ),
        }
    }

    /// Whether this rejection is the last the workflow allows on `task`:
    /// its round and the limit, when `review_rounds` is set for a workflow
    /// card and reached. Earlier rounds are counted off the bus.
    pub(crate) fn review_rounds_exhausted(
        &self,
        task: &Task,
        reviewer: &str,
    ) -> Option<(u32, u32)> {
        let (_, _, _, workflow) = self.workflow_card(&task.id)?;
        let limit = workflow.review_rounds;
        if limit == 0 {
            return None;
        }
        let earlier = self
            .bus
            .history(Some(reviewer), 1000)
            .unwrap_or_default()
            .iter()
            .filter(|m| {
                m.kind == MessageType::ReviewResult
                    && m.metadata.get("task_id").and_then(|v| v.as_str()) == Some(&task.id)
                    && m.metadata.get("approved").and_then(|v| v.as_bool()) == Some(false)
            })
            .count() as u32;
        let round = earlier + 1;
        (round >= limit).then_some((round, limit))
    }
}

#[cfg(test)]
mod tests {
    use crew_core::config::{AgentSpec, OrchestratorConfig, TeamConfig};
    use crew_core::guardrails::{Guardrails, Messaging, Step, Workflow};
    use crew_core::rpc::{Call, SendParams};
    use crew_core::{MessageType, TaskStatus};

    use super::super::server::Who;
    use super::super::{start, DaemonOptions, Running};

    /// A daemon holding a hub team: the orchestrator, members a and b (a's
    /// crew tools cut to messaging and review), and a's subagent a-s1.
    async fn hub_team(messaging: Messaging) -> (tempfile::TempDir, Running) {
        let temp = tempfile::tempdir().unwrap();
        let running = start(DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        let daemon = &running.daemon;
        let mut m = daemon.model();
        for (id, orchestrator, parent) in [
            ("orchestrator", true, None),
            ("a", false, None),
            ("b", false, None),
            ("a-s1", false, Some("a")),
        ] {
            let agent: crew_core::Agent = serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "role": "work", "status": "working", "runtime": "shell",
                "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0,
                "orchestrator": orchestrator, "parent": parent,
            }))
            .unwrap();
            m.agents.push(agent);
        }
        m.local.entry("a".into()).or_default().spec = AgentSpec {
            id: "a".into(),
            crew_tools: Some(vec!["messaging".into(), "review".into()]),
            ..AgentSpec::default()
        };
        let team = TeamConfig {
            project: "t".into(),
            guardrails: Guardrails { messaging },
            orchestrator: OrchestratorConfig {
                crew_tools: Some(vec!["workflow".into(), "task_edit".into()]),
                ..OrchestratorConfig::default()
            },
            workflow: Some(Workflow {
                steps: vec![
                    Step {
                        id: "research".into(),
                        agent: Some("a".into()),
                        description: "look into {title}: {brief}".into(),
                        ..Step::default()
                    },
                    Step {
                        id: "build".into(),
                        agent: Some("b".into()),
                        reviewer: Some("a".into()),
                        depends_on: vec!["research".into()],
                        description: "build {phase}".into(),
                        ..Step::default()
                    },
                ],
                ..Workflow::default()
            }),
            ..TeamConfig::from_yaml("project: t\norchestrator:\n  agent: true\n").unwrap()
        };
        m.session.team = Some(team);
        drop(m);
        (temp, running)
    }

    fn send(to: &str) -> Call {
        Call::Send(SendParams {
            to: to.into(),
            kind: MessageType::Message,
            body: "hi".into(),
            correlation_id: None,
            reply_to: None,
            metadata: serde_json::Value::Null,
            id: None,
        })
    }

    #[tokio::test]
    async fn under_hub_messaging_a_member_writes_to_whoever_it_reports_to_only() {
        let (_temp, running) = hub_team(Messaging::Hub).await;
        let d = &running.daemon;
        assert!(d.may_write("orchestrator", "b").is_ok());
        assert!(d.may_write("orchestrator", crew_core::BROADCAST).is_ok());
        assert!(d.may_write("a", "orchestrator").is_ok());
        assert!(d.may_write("a", "a-s1").is_ok(), "its own subagent");
        assert!(d.may_write("a-s1", "a").is_ok(), "its parent");
        let refused = d.may_write("a", "b").unwrap_err();
        assert!(refused.contains("write to orchestrator only"), "{refused}");
        assert!(
            refused.contains("put what b needs to orchestrator"),
            "{refused}"
        );
        assert!(d
            .may_write("a", crew_core::BROADCAST)
            .unwrap_err()
            .contains("nobody broadcasts but the orchestrator"));
        assert!(d.may_write("a", crew_core::OPERATOR_ID).is_err());
        assert!(d
            .may_write("a-s1", "b")
            .unwrap_err()
            .contains("you report to a"));
        assert!(d.may_write("a-s1", "orchestrator").is_err());
        // The scheduler and the operator are not members.
        assert!(d.may_write("scheduler", "b").is_ok());
        assert_eq!(d.hub_of("a"), "orchestrator");
        assert_eq!(d.hub_of("a-s1"), "a");
        // Without an orchestrator agent, the operator is the hub.
        d.model().agents.retain(|a| !a.orchestrator);
        assert!(d.may_write("a", crew_core::OPERATOR_ID).is_ok());
        assert_eq!(d.hub_of("a"), crew_core::OPERATOR_ID);
        assert!(d.may_write("a", "b").is_err());

        let (_temp, open) = hub_team(Messaging::Open).await;
        assert!(open.daemon.may_write("a", "b").is_ok());
        assert!(open.daemon.may_write("a-s1", "b").is_ok());
    }

    #[tokio::test]
    async fn a_members_crew_tools_hold_at_the_daemon_whatever_asked() {
        let (_temp, running) = hub_team(Messaging::Open).await;
        let d = &running.daemon;
        let a = Who::Agent("a".into());
        assert!(d.check_crew_tool(&a, &send("b")).is_ok());
        assert!(d.check_crew_tool(&a, &send(crew_core::BROADCAST)).is_ok());
        assert!(
            d.check_crew_tool(&a, &Call::TasksList).is_ok(),
            "never governed"
        );
        let refused = d
            .check_crew_tool(&a, &Call::GitStatus { agent: None })
            .unwrap_err();
        assert!(
            refused
                .message
                .contains("git_status is not among the crew tools"),
            "{}",
            refused.message
        );
        assert!(refused.message.contains("crew_tools: messaging, review"));
        assert!(d
            .check_crew_tool(
                &a,
                &Call::AgentSpawn {
                    id: None,
                    role: String::new(),
                    task: String::new(),
                    runtime: None,
                    model: None,
                    toolbox: None,
                }
            )
            .is_err());
        // b's declaration says nothing: every tool.
        let b = Who::Agent("b".into());
        assert!(d
            .check_crew_tool(&b, &Call::GitStatus { agent: None })
            .is_ok());
        // The orchestrator's selection comes from the team.
        let lead = Who::Agent("orchestrator".into());
        assert!(d
            .check_crew_tool(
                &lead,
                &Call::WorkflowRun {
                    phase: "p".into(),
                    title: "t".into(),
                    brief: String::new()
                }
            )
            .is_ok());
        let refused = d
            .check_crew_tool(
                &lead,
                &Call::TaskCreate {
                    spec: crew_core::config::TaskSpec::default(),
                },
            )
            .unwrap_err();
        assert!(refused.message.contains("task_create is not among"));
        assert!(d
            .check_crew_tool(
                &Who::Operator,
                &Call::TaskCreate {
                    spec: crew_core::config::TaskSpec::default()
                }
            )
            .is_ok());
        // What the identity file carries: the names of a's tools.
        let names = d.tool_names_of("a", false).unwrap();
        assert!(names.contains(&"agent_send".to_string()));
        assert!(names.contains(&"review_submit".to_string()));
        assert!(names.contains(&"task_complete".to_string()));
        assert!(!names.contains(&"git_commit".to_string()));
        assert!(
            !names.contains(&"task_create".to_string()),
            "a worker's list"
        );
        assert_eq!(d.tool_names_of("b", false), None);
        let lead_names = d.tool_names_of("orchestrator", true).unwrap();
        assert!(lead_names.contains(&"workflow_run".to_string()));
        assert!(lead_names.contains(&"operator_ask".to_string()));
        assert!(!lead_names.contains(&"agent_add".to_string()));
    }

    #[tokio::test]
    async fn a_workflow_run_lays_its_cards_as_a_chain_or_none_at_all() {
        let (_temp, running) = hub_team(Messaging::Hub).await;
        let d = &running.daemon;
        let cards = d
            .run_workflow("03-auth", "Authentication", "tokens")
            .unwrap();
        let ids: Vec<&str> = cards.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["research-03-auth", "build-03-auth"]);
        assert_eq!(cards[0].description, "look into Authentication: tokens");
        assert_eq!(cards[0].assigned_agent.as_deref(), Some("a"));
        assert_eq!(cards[1].dependencies, ["research-03-auth"]);
        assert_eq!(cards[1].reviewer.as_deref(), Some("a"));
        assert_eq!(cards[1].status, TaskStatus::Queued);
        // The run is part of the session: the dashboards read the phase
        // the team is on from it and the cards.
        let info = d.session_info();
        let workflow = info.workflow.as_ref().unwrap();
        assert_eq!(workflow.step_ids(), ["research", "build"]);
        assert_eq!(workflow.phases.len(), 1);
        assert_eq!(
            (
                workflow.phases[0].phase.as_str(),
                workflow.phases[0].title.as_str()
            ),
            ("03-auth", "Authentication")
        );
        assert_eq!(workflow.phases[0].cards["research"], "research-03-auth");
        assert_eq!(workflow.phases[0].cards["build"], "build-03-auth");
        assert_eq!(
            workflow.headline(&d.model().tasks).as_deref(),
            Some("phase 1/1 · 03-auth (Authentication): research ○ · build ○")
        );
        // Run again: the chain exists, and nothing is added.
        let before = d.model().tasks.len();
        let refused = d.run_workflow("03-auth", "Again", "").unwrap_err();
        assert!(
            refused.message.contains("was run already"),
            "{}",
            refused.message
        );
        assert_eq!(d.model().tasks.len(), before);
        assert!(d
            .run_workflow("Phase 3", "t", "")
            .unwrap_err()
            .message
            .contains("not a phase id"));
        d.model().session.team.as_mut().unwrap().workflow = None;
        assert!(d
            .run_workflow("04", "t", "")
            .unwrap_err()
            .message
            .contains("no workflow"));
    }

    #[tokio::test]
    async fn slices_are_laid_side_by_side_and_a_sequential_workflow_waits_for_them() {
        use crew_core::guardrails::{Order, Scope, SliceSpec};
        let (_temp, running) = hub_team(Messaging::Open).await;
        let d = &running.daemon;
        {
            let mut m = d.model();
            let team = m.session.team.as_mut().unwrap();
            team.workflow = Some(Workflow {
                order: Order::Sequential,
                steps: vec![
                    Step {
                        id: "research".into(),
                        agent: Some("a".into()),
                        description: "look".into(),
                        ..Step::default()
                    },
                    Step {
                        id: "build".into(),
                        scope: Scope::Slice,
                        agent: Some("a".into()),
                        agents: vec!["b".into()],
                        depends_on: vec!["research".into()],
                        description: "build {slice} ({slice_title}): {slice_brief}".into(),
                        ..Step::default()
                    },
                    Step {
                        id: "verify".into(),
                        agent: Some("b".into()),
                        depends_on: vec!["build".into()],
                        description: "check {phase}".into(),
                        ..Step::default()
                    },
                ],
                ..Workflow::default()
            });
        }
        // The run lays the steps up to the slices; the rest waits for them.
        let laid = d.run_workflow("p1", "First", "go").unwrap();
        assert_eq!(
            laid.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["research-p1"]
        );
        let refused = d.run_workflow("p2", "Second", "").unwrap_err();
        assert!(
            refused.message.contains("still on research"),
            "{}",
            refused.message
        );
        d.update_task("research-p1", |t| t.status = TaskStatus::Done);
        let refused = d.run_workflow("p2", "Second", "").unwrap_err();
        assert!(
            refused.message.contains("slices are not declared yet"),
            "{}",
            refused.message
        );
        // Each slice says what it writes, so they may run side by side.
        let slice = |id: &str, title: &str| SliceSpec {
            id: id.into(),
            title: title.into(),
            brief: format!("do {id}"),
            touches: vec![format!("src/{id}")],
            ..SliceSpec::default()
        };
        // A first split, replaced before any of its cards starts: the old
        // cards leave the board, the new ones take their place.
        let first = d.declare_slices("p1", vec![slice("one", "One")]).unwrap();
        assert_eq!(
            first.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["build-p1-one", "verify-p1"]
        );
        let cards = d
            .declare_slices("p1", vec![slice("api", "API"), slice("ui", "UI")])
            .unwrap();
        let ids: Vec<&str> = cards.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["build-p1-api", "build-p1-ui", "verify-p1"]);
        assert!(
            d.model().task("build-p1-one").is_none(),
            "taken off the board"
        );
        assert!(!d
            .store
            .load_tasks()
            .unwrap()
            .iter()
            .any(|t| t.id == "build-p1-one"));
        assert!(d
            .store
            .recent_events(None, 200)
            .unwrap()
            .iter()
            .any(
                |e| e.event_type == "task.removed" && e.task_id.as_deref() == Some("build-p1-one")
            ));
        // The slices take turns over the step's members; the verify waits
        // for every slice.
        assert_eq!(cards[0].assigned_agent.as_deref(), Some("a"));
        assert_eq!(cards[1].assigned_agent.as_deref(), Some("b"));
        assert_eq!(cards[0].description, "build api (API): do api");
        assert_eq!(cards[0].dependencies, ["research-p1"]);
        assert_eq!(cards[1].dependencies, ["research-p1"]);
        assert_eq!(cards[2].dependencies, ["build-p1-api", "build-p1-ui"]);
        let run = d.session_info().workflow.unwrap();
        assert_eq!(run.phases[0].slices.len(), 2);
        assert_eq!(run.phases[0].slices[1].cards["build"], "build-p1-ui");
        assert_eq!(run.phases[0].cards["verify"], "verify-p1");
        assert_eq!(
            run.headline(&d.model().tasks).as_deref(),
            Some("phase 1/1 · p1 (First): research ✓ · build [api ○ · ui ○] · verify ○")
        );
        // Once a slice has started, the split stays.
        d.update_task("build-p1-api", |t| {
            t.status = TaskStatus::Working;
            t.started_at = Some(1);
        });
        let refused = d.declare_slices("p1", vec![slice("x", "X")]).unwrap_err();
        assert!(
            refused.message.contains("declared already")
                && refused.message.contains("build-p1-api has started"),
            "{}",
            refused.message
        );
        assert!(d.model().task("build-p1-api").is_some());
        let refused = d.run_workflow("p2", "Second", "").unwrap_err();
        assert!(
            refused.message.contains("still on build"),
            "{}",
            refused.message
        );
        for id in ["build-p1-api", "build-p1-ui", "verify-p1"] {
            d.update_task(id, |t| t.status = TaskStatus::Done);
        }
        // Done, and no verdict step: the next unit may run.
        assert!(d.run_workflow("p2", "Second", "").is_ok());
        // Under auto, a queued unit runs by itself once the sequence lets it;
        // a slice with depends waits for that slice's card.
        d.model()
            .session
            .team
            .as_mut()
            .unwrap()
            .workflow
            .as_mut()
            .unwrap()
            .auto = true;
        let queued = d
            .queue_phases(vec![crew_core::guardrails::PhaseSpec {
                id: "p3".into(),
                title: "Third".into(),
                brief: "later".into(),
            }])
            .unwrap();
        assert_eq!(queued, ["p3"]);
        assert!(d.model().task("research-p3").is_none(), "p2 is still open");
        d.update_task("research-p2", |t| t.status = TaskStatus::Done);
        // api names db; db names nothing, but ui, declared before it,
        // writes under db's paths, so db takes its turn after ui, and the
        // run says so.
        let cards = d
            .declare_slices(
                "p2",
                vec![
                    SliceSpec {
                        touches: vec!["src/ui".into(), "src/db/schema.sql".into()],
                        ..slice("ui", "UI")
                    },
                    SliceSpec {
                        touches: vec!["src/db/".into()],
                        ..slice("db", "DB")
                    },
                    SliceSpec {
                        depends: vec!["db".into()],
                        touches: vec!["src/api".into()],
                        ..slice("api", "API")
                    },
                ],
            )
            .unwrap();
        let ids: Vec<&str> = cards.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            ["build-p2-ui", "build-p2-db", "build-p2-api", "verify-p2"]
        );
        assert_eq!(cards[0].dependencies, ["research-p2"]);
        assert_eq!(cards[1].dependencies, ["research-p2", "build-p2-ui"]);
        assert_eq!(cards[2].dependencies, ["research-p2", "build-p2-db"]);
        let run = d.session_info().workflow.unwrap();
        let p2 = &run.phases[1];
        assert_eq!(p2.slices[0].slice, "ui");
        assert!(p2.slices[0].depends.is_empty());
        assert_eq!(p2.slices[0].touches, ["src/ui", "src/db/schema.sql"]);
        assert_eq!(p2.slices[1].depends, ["ui"]);
        assert_eq!(p2.slices[1].touches, ["src/db"]);
        assert_eq!(p2.slices[2].depends, ["db"]);
        assert!(d
            .model()
            .task("build-p2-api")
            .unwrap()
            .dependencies
            .contains(&"build-p2-db".to_string()));
        for id in ["build-p2-ui", "build-p2-db", "build-p2-api", "verify-p2"] {
            d.update_task(id, |t| t.status = TaskStatus::Done);
        }
        d.auto_advance();
        assert!(
            d.model().task("research-p3").is_some(),
            "p3 laid out by itself"
        );
        assert!(d.model().session.workflow_queue.is_empty());
        assert!(d
            .queue_phases(vec![crew_core::guardrails::PhaseSpec {
                id: "p3".into(),
                title: "Again".into(),
                brief: String::new(),
            }])
            .unwrap_err()
            .message
            .contains("queued or run already"));
    }
}
