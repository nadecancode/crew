//! Work-style guardrails: what a team file (or a style) fixes about how its
//! members may act. Whom they write to (`guardrails.messaging`), which of
//! their runtime's own tools they get (`agents[].tools`), which of crew's
//! tools (`crew_tools`), and the workflow: the chain of cards one unit of
//! work goes through (`workflow.steps`), laid on the board by `workflow_run`.
//!
//! What is declared here is enforced, not only told: the daemon refuses the
//! calls a member's `crew_tools` leave out and the messages its team's
//! `messaging` does not allow, and the runtimes are launched with their own
//! tools cut down (`crates/crew/src/agents`). A model that reasons past an
//! instruction is stopped all the same. The briefing says what the limits
//! are, so no member tries what it cannot do.
//!
//! Everything here is pure: the daemon and the launchers apply it.

use std::collections::{BTreeSet, HashMap};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::config::{find_cycle, AgentSpec, TaskSpec};
use crate::types::ORCHESTRATOR_ID;
use crate::util::is_valid_id;

/// Most steps a workflow may chain.
pub const MAX_STEPS: usize = 16;

/// Whom members may write to.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Messaging {
    /// Anyone: teammates write to one another directly.
    #[default]
    Open,
    /// Hub and spoke: members write to the orchestrator only (a subagent to
    /// its parent, a parent to its subagents), and the orchestrator to
    /// anyone. A line a member addresses to a teammate in a reply is carried
    /// to the orchestrator instead; a message sent to one is refused.
    /// Reviews come from the board: the orchestrator sets a card's reviewer.
    Hub,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Guardrails {
    #[serde(default)]
    pub messaging: Messaging,
}

impl Guardrails {
    pub fn is_default(&self) -> bool {
        *self == Guardrails::default()
    }
}

/// A class of a runtime's built-in tools. What each runtime makes of a
/// class is the launcher's business; a class a runtime cannot take away is
/// reported at launch and told to the member.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    /// Read files, search them, list directories.
    Read,
    /// Create and edit files.
    Write,
    /// Run commands.
    Shell,
    /// Fetch pages and search the web.
    Web,
    /// Launch in-session subagents of the runtime's own.
    Agents,
}

impl ToolClass {
    pub const ALL: [ToolClass; 5] = [
        ToolClass::Read,
        ToolClass::Write,
        ToolClass::Shell,
        ToolClass::Web,
        ToolClass::Agents,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ToolClass::Read => "read",
            ToolClass::Write => "write",
            ToolClass::Shell => "shell",
            ToolClass::Web => "web",
            ToolClass::Agents => "agents",
        }
    }

    /// What the class lets a member do, for its briefing.
    pub fn describe(self) -> &'static str {
        match self {
            ToolClass::Read => "read and search files",
            ToolClass::Write => "create and edit files",
            ToolClass::Shell => "run commands",
            ToolClass::Web => "fetch pages and search the web",
            ToolClass::Agents => "launch subagents of your runtime's own",
        }
    }
}

/// The classes of `ALL` that `tools` leaves out, in order.
pub fn missing_classes(tools: &[ToolClass]) -> Vec<ToolClass> {
    ToolClass::ALL
        .into_iter()
        .filter(|class| !tools.contains(class))
        .collect()
}

/// Crew's tools a team may take from a member, in groups: a `crew_tools`
/// list names groups or single tools. What is not in any group (reading the
/// board and one's own card, the inbox, status and progress, finishing or
/// giving up a card, declining, compacting, and the orchestrator's
/// `operator_ask` and `session_complete`) is always a member's.
pub const CREW_TOOL_GROUPS: &[(&str, &[&str])] = &[
    ("messaging", &["agent_send", "agent_broadcast"]),
    (
        "git",
        &[
            "git_commit",
            "git_diff",
            "git_status",
            "git_cherry_pick",
            "git_merge",
        ],
    ),
    ("review", &["review_request", "review_submit"]),
    ("spawn", &["agent_spawn"]),
    ("staff", &["agent_add", "agent_remove", "agent_set_model"]),
    (
        "board",
        &["task_create", "task_edit", "task_assign", "task_retry"],
    ),
    ("workflow", &["workflow_run"]),
];

/// Every tool a `crew_tools` list governs.
pub fn governed_tools() -> impl Iterator<Item = &'static str> {
    CREW_TOOL_GROUPS
        .iter()
        .flat_map(|(_, tools)| tools.iter().copied())
}

/// Whether `tool` is one a `crew_tools` list governs.
pub fn is_governed(tool: &str) -> bool {
    governed_tools().any(|t| t == tool)
}

/// The governed tools `selection` allows: each entry a group or a tool
/// name. An entry that is neither is an error naming the choices.
pub fn crew_tools_allowed(selection: &[String]) -> Result<BTreeSet<&'static str>> {
    let mut allowed = BTreeSet::new();
    for entry in selection {
        let entry = entry.trim();
        if let Some((_, tools)) = CREW_TOOL_GROUPS.iter().find(|(group, _)| *group == entry) {
            allowed.extend(tools.iter().copied());
        } else if let Some(tool) = governed_tools().find(|t| *t == entry) {
            allowed.insert(tool);
        } else {
            let groups: Vec<&str> = CREW_TOOL_GROUPS.iter().map(|(group, _)| *group).collect();
            bail!(
                "'{entry}' is not a crew tool group or a tool a team can take away (groups: {}; \
tools: {})",
                groups.join(", "),
                governed_tools().collect::<Vec<_>>().join(", ")
            );
        }
    }
    Ok(allowed)
}

/// Whether `tool` may be called under `selection` (`None`: everything).
pub fn crew_tool_allowed(selection: Option<&[String]>, tool: &str) -> bool {
    match selection {
        None => true,
        Some(selection) => {
            !is_governed(tool)
                || crew_tools_allowed(selection).is_ok_and(|allowed| allowed.contains(tool))
        }
    }
}

/// A workflow: the chain of cards one unit of work (a phase, a feature)
/// goes through, each step a card for one member, in dependency order.
/// `workflow_run(phase, title, brief)` lays the chain on the board.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// One step of a workflow: the card it becomes for a unit of work. In
/// `title` and `description`, `{phase}` is the unit's id, `{title}` its
/// title and `{brief}` what the orchestrator wrote when it ran the workflow.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    /// The card's title; `<id>: {title}` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The member the card goes to.
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// Steps of the same run this one waits for (their cards, reviews
    /// included).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// The card's text.
    #[serde(default)]
    pub description: String,
    /// What the step leaves behind (files, by path), named on its card.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub produces: Vec<String>,
}

impl Workflow {
    /// Check the chain against the team's roster.
    pub fn validate(&self, agents: &[AgentSpec]) -> Result<()> {
        if self.steps.is_empty() {
            bail!("workflow.steps is empty");
        }
        if self.steps.len() > MAX_STEPS {
            bail!(
                "a workflow has at most {MAX_STEPS} steps, found {}",
                self.steps.len()
            );
        }
        let mut ids = BTreeSet::new();
        for step in &self.steps {
            if !is_valid_id(&step.id) || step.id.len() > 16 {
                bail!(
                    "workflow step id '{}' is not valid (expected [a-z][a-z0-9_-]{{0,15}})",
                    step.id
                );
            }
            if !ids.insert(step.id.as_str()) {
                bail!("duplicate workflow step '{}'", step.id);
            }
            for (field, who) in [
                ("agent", Some(&step.agent)),
                ("reviewer", step.reviewer.as_ref()),
            ] {
                let Some(who) = who else { continue };
                if who == ORCHESTRATOR_ID {
                    bail!(
                        "workflow step '{}'.{field} names the orchestrator, which does no cards",
                        step.id
                    );
                }
                if !agents.iter().any(|a| &a.id == who) {
                    bail!(
                        "workflow step '{}'.{field} names '{who}', which is not in `agents`",
                        step.id
                    );
                }
            }
            if step.reviewer.as_ref() == Some(&step.agent) {
                bail!("workflow step '{}' is reviewed by its own agent", step.id);
            }
            if step.description.trim().is_empty() {
                bail!("workflow step '{}' has no description", step.id);
            }
        }
        // Steps are listed in order: a step waits for steps before it, so
        // the cards can be made in that order, each dependency on the board
        // before the card that names it.
        let mut before: BTreeSet<&str> = BTreeSet::new();
        for step in &self.steps {
            for dep in &step.depends_on {
                if !ids.contains(dep.as_str()) {
                    bail!(
                        "workflow step '{}' depends on unknown step '{dep}'",
                        step.id
                    );
                }
                if !before.contains(dep.as_str()) {
                    bail!(
                        "workflow step '{}' depends on '{dep}', which is listed after it: list \
steps in the order they run",
                        step.id
                    );
                }
            }
            before.insert(&step.id);
        }
        let graph: HashMap<&str, Vec<&str>> = self
            .steps
            .iter()
            .map(|s| {
                (
                    s.id.as_str(),
                    s.depends_on.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        if let Some(cycle) = find_cycle(&graph) {
            bail!("workflow steps form a cycle: {}", cycle.join(" -> "));
        }
        Ok(())
    }

    /// The cards of one run: `phase` names the unit (each card is
    /// `<step>-<phase>`), `title` and `brief` fill the templates in.
    pub fn cards(&self, phase: &str, title: &str, brief: &str) -> Result<Vec<TaskSpec>> {
        let longest = self.steps.iter().map(|s| s.id.len()).max().unwrap_or(0);
        let phase_chars = phase
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if phase.is_empty() || !phase_chars || phase.len() + 1 + longest > 32 {
            bail!(
                "'{phase}' is not a phase id (expected [a-z0-9_-]+, at most {} characters here, \
since each card is named <step>-<phase>)",
                32 - 1 - longest
            );
        }
        let title = title.trim();
        if title.is_empty() {
            bail!("the run needs a title");
        }
        let fill = |template: &str| {
            template
                .replace("{phase}", phase)
                .replace("{title}", title)
                .replace("{brief}", brief.trim())
        };
        Ok(self
            .steps
            .iter()
            .map(|step| {
                let mut description = fill(step.description.trim());
                if !step.produces.is_empty() {
                    let produces: Vec<String> = step.produces.iter().map(|p| fill(p)).collect();
                    description.push_str(&format!("\n\nProduces: {}", produces.join(", ")));
                }
                TaskSpec {
                    id: format!("{}-{phase}", step.id),
                    title: Some(match &step.title {
                        Some(template) => fill(template),
                        None => format!("{}: {title}", step.id),
                    }),
                    description,
                    agent: Some(step.agent.clone()),
                    depends_on: step
                        .depends_on
                        .iter()
                        .map(|dep| format!("{dep}-{phase}"))
                        .collect(),
                    reviewer: step.reviewer.clone(),
                    timeout_secs: None,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents(ids: &[&str]) -> Vec<AgentSpec> {
        ids.iter()
            .map(|id| AgentSpec {
                id: id.to_string(),
                ..AgentSpec::default()
            })
            .collect()
    }

    fn step(id: &str, agent: &str, deps: &[&str]) -> Step {
        Step {
            id: id.into(),
            agent: agent.into(),
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
            description: format!("do {id} for {{phase}}: {{brief}}"),
            ..Step::default()
        }
    }

    #[test]
    fn a_crew_tools_list_names_groups_or_single_tools_and_nothing_else() {
        let allowed = crew_tools_allowed(&["git".into(), "review_submit".into()]).unwrap();
        assert!(allowed.contains("git_commit") && allowed.contains("git_merge"));
        assert!(allowed.contains("review_submit") && !allowed.contains("review_request"));
        assert!(!allowed.contains("agent_send"));
        let error = crew_tools_allowed(&["task_update".into()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("'task_update' is not"), "{error}");
        assert!(error.contains("groups: messaging, git"), "{error}");

        let selection = vec!["git".to_string()];
        // Ungoverned tools are always allowed; governed ones by the list.
        assert!(crew_tool_allowed(Some(&selection), "task_complete"));
        assert!(crew_tool_allowed(Some(&selection), "git_diff"));
        assert!(!crew_tool_allowed(Some(&selection), "agent_broadcast"));
        assert!(crew_tool_allowed(None, "agent_broadcast"));
        assert!(crew_tool_allowed(Some(&[]), "task_fail"));
        assert!(!crew_tool_allowed(Some(&[]), "agent_spawn"));
    }

    #[test]
    fn a_workflow_is_checked_against_the_roster_and_for_cycles() {
        let team = agents(&["researcher", "planner", "checker"]);
        let mut workflow = Workflow {
            steps: vec![
                step("research", "researcher", &[]),
                Step {
                    reviewer: Some("checker".into()),
                    ..step("plan", "planner", &["research"])
                },
            ],
        };
        workflow.validate(&team).unwrap();

        let bad = |mutate: &dyn Fn(&mut Workflow)| {
            let mut w = workflow.clone();
            mutate(&mut w);
            w.validate(&team).unwrap_err().to_string()
        };
        assert!(bad(&|w| w.steps.clear()).contains("empty"));
        assert!(bad(&|w| w.steps[1].agent = "nobody".into()).contains("not in `agents`"));
        assert!(bad(&|w| w.steps[1].agent = ORCHESTRATOR_ID.into()).contains("orchestrator"));
        assert!(bad(&|w| w.steps[1].reviewer = Some("planner".into())).contains("its own agent"));
        assert!(bad(&|w| w.steps[1].depends_on = vec!["verify".into()]).contains("unknown step"));
        assert!(bad(&|w| w.steps[0].depends_on = vec!["plan".into()]).contains("listed after it"));
        assert!(bad(&|w| w.steps.swap(0, 1)).contains("listed after it"));
        assert!(bad(&|w| w.steps[1].id = "research".into()).contains("duplicate"));
        assert!(bad(&|w| w.steps[1].id = "Plan".into()).contains("not valid"));
        assert!(bad(&|w| w.steps[1].description = " ".into()).contains("no description"));
        workflow.steps[0].title = Some("Look into {title}".into());
        workflow.steps[0].produces = vec![".planning/{phase}/RESEARCH.md".into()];

        let cards = workflow
            .cards("03-auth", "Authentication", "sessions and tokens")
            .unwrap();
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].id, "research-03-auth");
        assert_eq!(cards[0].title.as_deref(), Some("Look into Authentication"));
        assert_eq!(
            cards[0].description,
            "do research for 03-auth: sessions and tokens\n\nProduces: .planning/03-auth/RESEARCH.md"
        );
        assert_eq!(cards[0].agent.as_deref(), Some("researcher"));
        assert_eq!(cards[1].id, "plan-03-auth");
        assert_eq!(cards[1].title.as_deref(), Some("plan: Authentication"));
        assert_eq!(cards[1].depends_on, ["research-03-auth"]);
        assert_eq!(cards[1].reviewer.as_deref(), Some("checker"));
        // A phase id must leave room for the step in the card id.
        let error = workflow
            .cards("a-phase-id-that-is-far-too-long-for-a-card", "t", "")
            .unwrap_err()
            .to_string();
        assert!(error.contains("at most 23 characters"), "{error}");
        assert!(workflow.cards("03", " ", "").is_err());
        assert!(workflow.cards("Phase 3", "t", "").is_err());
    }

    #[test]
    fn the_classes_a_member_lacks_are_listed_in_order() {
        assert_eq!(
            missing_classes(&[ToolClass::Shell, ToolClass::Read]),
            [ToolClass::Write, ToolClass::Web, ToolClass::Agents]
        );
        assert!(missing_classes(&ToolClass::ALL).is_empty());
        let class: ToolClass = serde_yaml::from_str("web").unwrap();
        assert_eq!(class, ToolClass::Web);
        assert!(serde_yaml::from_str::<ToolClass>("browser").is_err());
    }
}
