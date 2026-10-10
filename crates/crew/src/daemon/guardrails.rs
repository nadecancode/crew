//! The team's guardrails as the daemon enforces them
//! (`crew_core::guardrails`): the crew tools a member may call, whom it may
//! write to, and the workflow laid on the board. Declared in the work style,
//! refused here whatever a session asks for: a member's MCP server offers
//! only what it has (its identity file lists it), and a call that gets past
//! that, through `crew ctl` or a stale list, is refused the same way.

use crew_core::guardrails::{self, Messaging};
use crew_core::rpc::{Call, RpcError};
use crew_core::{Task, BROADCAST, OPERATOR_ID};

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

    /// Lay the team's workflow on the board for one unit of work: a card
    /// per step, chained as the steps are. All or none: the ids are checked
    /// before the first card is made.
    pub(crate) fn run_workflow(
        &self,
        phase: &str,
        title: &str,
        brief: &str,
    ) -> Result<Vec<Task>, RpcError> {
        let workflow = self
            .model()
            .session
            .team
            .as_ref()
            .and_then(|t| t.workflow.clone())
            .ok_or_else(|| {
                RpcError::invalid("this team has no workflow: its style declares none")
            })?;
        let specs = workflow
            .cards(phase, title, brief)
            .map_err(|e| RpcError::invalid(e.to_string()))?;
        {
            let model = self.model();
            // Each card is checked against the board as it will be when
            // its turn comes: the cards before it in the chain included.
            let mut board = model.tasks.clone();
            for spec in &specs {
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
        }
        let mut created = Vec::with_capacity(specs.len());
        for spec in specs {
            created.push(self.create_task(spec)?);
        }
        self.emit(
            NewEvent::new(
                "scheduler",
                "workflow.run",
                format!(
                    "workflow laid out for {phase} ({title}): {}",
                    created
                        .iter()
                        .map(|t| t.id.as_str())
                        .collect::<Vec<_>>()
                        .join(" → ")
                ),
            )
            .data(serde_json::json!({ "phase": phase, "tasks": created.iter().map(|t| t.id.clone()).collect::<Vec<_>>() })),
        );
        Ok(created)
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
                        agent: "a".into(),
                        description: "look into {title}: {brief}".into(),
                        ..Step::default()
                    },
                    Step {
                        id: "build".into(),
                        agent: "b".into(),
                        reviewer: Some("a".into()),
                        depends_on: vec!["research".into()],
                        description: "build {phase}".into(),
                        ..Step::default()
                    },
                ],
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
        // Run again: the chain exists, and nothing is added.
        let before = d.model().tasks.len();
        let refused = d.run_workflow("03-auth", "Again", "").unwrap_err();
        assert!(
            refused.message.contains("already exists"),
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
}
