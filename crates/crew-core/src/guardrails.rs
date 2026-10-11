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
    (
        "workflow",
        &["workflow_run", "workflow_slices", "workflow_phases"],
    ),
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
/// `workflow_run(phase, title, brief)` lays the chain on the board. Its
/// gates are deterministic and held by the daemon: a step's `produces`
/// must be in the card's commit before the card is done, a step's
/// `verdict` is read from a file it produced and decides whether the next
/// unit may run (`order: sequential`), and `review_rounds` bounds how often
/// a card goes back to its author before the orchestrator decides.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Whether units run one at a time.
    #[serde(default, skip_serializing_if = "Order::is_default")]
    pub order: Order,
    /// Changes requested this many times on one card of a run: the card is
    /// blocked for the orchestrator to decide instead of going back to its
    /// author again (0: no limit).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review_rounds: u32,
    /// crew runs the units queued with `workflow_phases` itself, each when
    /// the one before lets it (`order: sequential`), and a follow-up run
    /// after a redo verdict; the orchestrator decides only what the
    /// workflow cannot.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Whether units of work run one at a time.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Order {
    /// Units may run side by side.
    #[default]
    Free,
    /// One unit at a time: `workflow_run` is refused while the last unit
    /// has an open card, or while its verdict (if a step gives one) is not
    /// a pass; after a redo verdict, only a follow-up run of the same unit
    /// (an id starting with `<phase>-`) may come.
    Sequential,
}

impl Order {
    pub fn is_default(&self) -> bool {
        *self == Order::default()
    }
}

/// A verdict a step leaves in a file it produces: a `<key>: <value>` line
/// (front matter or body), read when the card is done.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    /// The file, with the same placeholders as `produces`.
    pub file: String,
    /// The key of the line; `status` when left out.
    #[serde(default = "default_verdict_key")]
    pub key: String,
    /// Values that let the next unit run.
    pub pass: Vec<String>,
    /// Values after which only a follow-up run of the same unit may come.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redo: Vec<String>,
}

fn default_verdict_key() -> String {
    "status".to_string()
}

/// What a verdict's value means for the next unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Redo,
    /// Neither: the team cannot go on from it by itself.
    Other,
}

impl Verdict {
    /// The value of the first `<key>: <value>` line of `text`, if any.
    pub fn read(&self, text: &str) -> Option<String> {
        text.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(self.key.as_str())?;
            let value = rest.trim_start().strip_prefix(':')?.trim();
            let value = value.trim_matches(|c| c == '"' || c == '\'').trim();
            (!value.is_empty()).then(|| value.to_string())
        })
    }

    pub fn outcome(&self, value: &str) -> Outcome {
        if self.pass.iter().any(|v| v == value) {
            Outcome::Pass
        } else if self.redo.iter().any(|v| v == value) {
            Outcome::Redo
        } else {
            Outcome::Other
        }
    }

    /// `passed, or gaps_found (a follow-up run)`: what the line may say.
    pub fn choices(&self) -> String {
        let mut out = self.pass.join(", ");
        if !self.redo.is_empty() {
            out.push_str(&format!(", or {} (a follow-up run)", self.redo.join(", ")));
        }
        out
    }
}

/// Whether a step runs once per unit, or once per slice of it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// One card per unit.
    #[default]
    Phase,
    /// One card per slice of the unit, side by side, laid out once the
    /// unit's slices are declared (`workflow_slices`). A phase step that
    /// depends on a slice step waits for every slice's card.
    Slice,
}

impl Scope {
    pub fn is_default(&self) -> bool {
        *self == Scope::default()
    }
}

/// One step of a workflow: the card it becomes for a unit of work. In
/// `title`, `description` and `produces`, `{phase}` is the unit's id,
/// `{title}` its title and `{brief}` what the orchestrator wrote when it
/// ran the workflow; a slice step also has `{slice}`, `{slice_title}` and
/// `{slice_brief}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    /// The card's title; `<id>: {title}` (`<id>: {title} / {slice_title}`
    /// for a slice step) when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The member the card goes to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// More members the cards are spread over in turn: a slice step's
    /// slices run side by side on them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// Steps of the same run this one waits for (their cards, reviews
    /// included).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Scope::is_default")]
    pub scope: Scope,
    /// The card's text.
    #[serde(default)]
    pub description: String,
    /// What the step leaves behind (files, by path), named on its card and
    /// required in its commit before the card is done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub produces: Vec<String>,
    /// A verdict the step leaves in one of those files, read when the card
    /// is done; the unit's last step with one gives the unit's verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

impl Step {
    /// The members this step's cards go to, in turn.
    pub fn owners(&self) -> Vec<&str> {
        self.agent
            .iter()
            .chain(self.agents.iter())
            .map(String::as_str)
            .collect()
    }

    /// The owner of the step's `n`th card.
    pub fn owner_of(&self, n: usize) -> String {
        let owners = self.owners();
        owners[n % owners.len().max(1)].to_string()
    }
}

/// A slice of a unit of work, as declared once its plan is known.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct SliceSpec {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub brief: String,
    /// Slices of the same unit this one waits for; the rest run side by
    /// side with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends: Vec<String>,
    /// The paths this slice writes (files or directories, relative to the
    /// workspace). Two slices that touch the same path do not run side by
    /// side: the one declared later waits for the earlier, as if it had
    /// named it in `depends`. A slice that names none may write anything,
    /// so it takes its turn alone: after every slice declared before it,
    /// and every slice declared after it waits for it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touches: Vec<String>,
}

/// `path` as a slice's `touches` entry is compared: no leading `./`, no
/// trailing `/` or `/*`.
pub fn touched_path(path: &str) -> String {
    let mut path = path.trim().trim_start_matches("./");
    loop {
        let shorter = path
            .strip_suffix("/**")
            .or_else(|| path.strip_suffix("/*"))
            .or_else(|| path.strip_suffix('/'))
            .unwrap_or(path);
        if shorter == path {
            return path.to_string();
        }
        path = shorter;
    }
}

/// Whether two `touches` entries name the same file or directory, or one
/// lies under the other.
pub fn touches_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (touched_path(a), touched_path(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b
        || a.strip_prefix(b.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(a.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A unit of work queued for the workflow (`workflow_phases`), run by crew
/// itself under `auto` when the one before lets it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct PhaseSpec {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub brief: String,
}

/// Most slices one unit may have.
pub const MAX_SLICES: usize = 16;

/// Most paths one slice may say it touches.
pub const MAX_TOUCHES: usize = 64;

/// `template` with its placeholders filled in.
pub fn fill(template: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in pairs {
        out = out.replace(&format!("{{{key}}}"), value.trim());
    }
    out
}

/// The placeholders of a unit's cards.
fn unit_pairs<'a>(phase: &'a str, title: &'a str, brief: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![("phase", phase), ("title", title), ("brief", brief)]
}

fn check_unit_id(what: &str, id: &str, room: usize, shape: &str) -> Result<()> {
    let chars = id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if id.is_empty() || !chars || id.len() > room {
        bail!(
            "'{id}' is not a {what} id (expected [a-z0-9_-]+, at most {room} characters here, \
since each card is named {shape})"
        );
    }
    Ok(())
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
            let owners = step.owners();
            if owners.is_empty() {
                bail!("workflow step '{}' names no agent", step.id);
            }
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for who in owners.iter().copied() {
                if !seen.insert(who) {
                    bail!("workflow step '{}' names '{who}' twice", step.id);
                }
            }
            for (field, who) in owners
                .iter()
                .map(|who| ("agent", *who))
                .chain(step.reviewer.as_deref().map(|r| ("reviewer", r)))
            {
                if who == ORCHESTRATOR_ID {
                    bail!(
                        "workflow step '{}'.{field} names the orchestrator, which does no cards",
                        step.id
                    );
                }
                if !agents.iter().any(|a| a.id == who) {
                    bail!(
                        "workflow step '{}'.{field} names '{who}', which is not in `agents`",
                        step.id
                    );
                }
            }
            if step
                .reviewer
                .as_deref()
                .is_some_and(|r| owners.contains(&r))
            {
                bail!("workflow step '{}' is reviewed by its own agent", step.id);
            }
            if step.description.trim().is_empty() {
                bail!("workflow step '{}' has no description", step.id);
            }
            if let Some(verdict) = &step.verdict {
                if step.scope == Scope::Slice {
                    bail!(
                        "workflow step '{}' is a slice step: a verdict is a unit's, so it goes \
on a phase step",
                        step.id
                    );
                }
                if verdict.file.trim().is_empty() {
                    bail!("workflow step '{}'.verdict needs a file", step.id);
                }
                if verdict.key.is_empty()
                    || !verdict
                        .key
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                {
                    bail!("workflow step '{}'.verdict.key is not a key", step.id);
                }
                if verdict.pass.is_empty() {
                    bail!("workflow step '{}'.verdict.pass names no value", step.id);
                }
                if let Some(both) = verdict.pass.iter().find(|v| verdict.redo.contains(v)) {
                    bail!(
                        "workflow step '{}'.verdict: '{both}' is both a pass and a redo",
                        step.id
                    );
                }
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

    /// Whether any step runs per slice.
    pub fn has_slices(&self) -> bool {
        self.steps.iter().any(|s| s.scope == Scope::Slice)
    }

    /// Whether step `index` comes after the slices: it is a slice step, or
    /// depends on one, however far back.
    pub fn after_slices(&self, index: usize) -> bool {
        let Some(step) = self.steps.get(index) else {
            return false;
        };
        if step.scope == Scope::Slice {
            return true;
        }
        step.depends_on.iter().any(|dep| {
            self.steps
                .iter()
                .position(|s| &s.id == dep)
                .is_some_and(|i| self.after_slices(i))
        })
    }

    /// The id of step `step`'s card for a unit, or for one of its slices.
    pub fn card_id(step: &Step, phase: &str, slice: Option<&str>) -> String {
        match slice {
            Some(slice) => format!("{}-{phase}-{slice}", step.id),
            None => format!("{}-{phase}", step.id),
        }
    }

    fn longest_step(&self, scope: Option<Scope>) -> usize {
        self.steps
            .iter()
            .filter(|s| scope.is_none_or(|scope| s.scope == scope))
            .map(|s| s.id.len())
            .max()
            .unwrap_or(0)
    }

    /// The card of step `index` for a unit (`slice`: one of its slices),
    /// its text filled in. `n` numbers the card among the step's for the
    /// owner it goes to.
    fn card(
        &self,
        index: usize,
        phase: &str,
        title: &str,
        brief: &str,
        slice: Option<(usize, &SliceSpec)>,
    ) -> TaskSpec {
        let step = &self.steps[index];
        let mut pairs = unit_pairs(phase, title, brief);
        if let Some((_, s)) = slice {
            pairs.push(("slice", &s.id));
            pairs.push(("slice_title", &s.title));
            pairs.push(("slice_brief", &s.brief));
        }
        let fill = |template: &str| fill(template, &pairs);
        let mut description = fill(step.description.trim());
        if !step.produces.is_empty() {
            let produces: Vec<String> = step.produces.iter().map(|p| fill(p)).collect();
            description.push_str(&format!("\n\nProduces: {}", produces.join(", ")));
        }
        let slice_id = slice.map(|(_, s)| s.id.as_str());
        let depends_on = step
            .depends_on
            .iter()
            .flat_map(|dep| {
                let dep_step = self
                    .steps
                    .iter()
                    .find(|s| &s.id == dep)
                    .expect("validated dependency");
                match (dep_step.scope, slice_id) {
                    // A slice step waits for its own slice's earlier step.
                    (Scope::Slice, Some(slice)) => {
                        vec![Self::card_id(dep_step, phase, Some(slice))]
                    }
                    // A phase step after the slices waits for every slice.
                    (Scope::Slice, None) => Vec::new(),
                    (Scope::Phase, _) => vec![Self::card_id(dep_step, phase, None)],
                }
            })
            .collect();
        TaskSpec {
            id: Self::card_id(step, phase, slice_id),
            title: Some(match (&step.title, slice) {
                (Some(template), _) => fill(template),
                (None, Some((_, s))) => format!("{}: {title} / {}", step.id, s.title.trim()),
                (None, None) => format!("{}: {title}", step.id),
            }),
            description,
            agent: Some(step.owner_of(slice.map_or(0, |(n, _)| n))),
            depends_on,
            reviewer: step.reviewer.clone(),
            timeout_secs: None,
        }
    }

    /// The cards laid when a unit is run: its phase steps up to the slices.
    /// `phase` names the unit (each card is `<step>-<phase>`), `title` and
    /// `brief` fill the templates in.
    pub fn phase_cards(&self, phase: &str, title: &str, brief: &str) -> Result<Vec<TaskSpec>> {
        // Room for a slice id behind the phase id when there are slice steps.
        let room = if self.has_slices() {
            32 - 1 - self.longest_step(Some(Scope::Slice)) - 1 - 1
        } else {
            32 - 1 - self.longest_step(None)
        };
        check_unit_id("phase", phase, room, "<step>-<phase>")?;
        let title = title.trim();
        if title.is_empty() {
            bail!("the run needs a title");
        }
        Ok((0..self.steps.len())
            .filter(|i| !self.after_slices(*i))
            .map(|i| self.card(i, phase, title, brief, None))
            .collect())
    }

    /// A unit's slices checked and in the order their cards are laid: every
    /// slice after the ones it waits for, else as declared. `depends` comes
    /// back complete: a slice that touches a path an earlier-declared slice
    /// touches waits for that slice too, so two slices never write the
    /// same files side by side (gsd-pi's conflict filter, settled when the
    /// slices are declared rather than at each dispatch).
    pub fn slice_order(slices: &[SliceSpec]) -> Result<Vec<SliceSpec>> {
        if slices.is_empty() {
            bail!("declare at least one slice");
        }
        if slices.len() > MAX_SLICES {
            bail!(
                "a unit has at most {MAX_SLICES} slices, found {}",
                slices.len()
            );
        }
        let mut seen = BTreeSet::new();
        for slice in slices {
            if slice.id.is_empty()
                || !slice
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            {
                bail!("'{}' is not a slice id (expected [a-z0-9_-]+)", slice.id);
            }
            if !seen.insert(slice.id.as_str()) {
                bail!("slice '{}' is declared twice", slice.id);
            }
            if slice.title.trim().is_empty() {
                bail!("slice '{}' needs a title", slice.id);
            }
            if slice.touches.len() > MAX_TOUCHES {
                bail!(
                    "slice '{}' touches more than {MAX_TOUCHES} paths; name directories",
                    slice.id
                );
            }
            if let Some(bad) = slice.touches.iter().find(|p| {
                let p = touched_path(p);
                p.is_empty() || p.starts_with('/') || p.split('/').any(|part| part == "..")
            }) {
                bail!(
                    "slice '{}' touches '{bad}', which is not a path inside the workspace",
                    slice.id
                );
            }
        }
        for slice in slices {
            for dep in &slice.depends {
                if dep == &slice.id {
                    bail!("slice '{}' depends on itself", slice.id);
                }
                if !seen.contains(dep.as_str()) {
                    bail!("slice '{}' depends on unknown slice '{dep}'", slice.id);
                }
            }
        }
        let graph: HashMap<&str, Vec<&str>> = slices
            .iter()
            .map(|s| {
                (
                    s.id.as_str(),
                    s.depends.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        if let Some(cycle) = find_cycle(&graph) {
            bail!("slices form a cycle: {}", cycle.join(" -> "));
        }
        // Whether `from` waits for `to`, directly or through other slices.
        let waits_for = |from: &str, to: &str| -> bool {
            let mut stack = vec![from];
            let mut visited = BTreeSet::new();
            while let Some(id) = stack.pop() {
                if !visited.insert(id) {
                    continue;
                }
                for dep in graph.get(id).into_iter().flatten() {
                    if *dep == to {
                        return true;
                    }
                    stack.push(dep);
                }
            }
            false
        };
        let mut slices: Vec<SliceSpec> = slices
            .iter()
            .map(|s| SliceSpec {
                id: s.id.clone(),
                title: s.title.trim().to_string(),
                brief: s.brief.trim().to_string(),
                depends: s.depends.clone(),
                touches: s.touches.iter().map(|p| touched_path(p)).collect(),
            })
            .collect();
        for later in 1..slices.len() {
            for earlier in 0..later {
                let (a, b) = (&slices[earlier], &slices[later]);
                // Nothing named: it may write anything.
                let overlap = a.touches.is_empty()
                    || b.touches.is_empty()
                    || a.touches
                        .iter()
                        .any(|p| b.touches.iter().any(|q| touches_overlap(p, q)));
                if overlap && !waits_for(&b.id, &a.id) && !waits_for(&a.id, &b.id) {
                    let dep = a.id.clone();
                    slices[later].depends.push(dep);
                }
            }
        }
        // Dependencies first, else as declared.
        let mut ordered: Vec<SliceSpec> = Vec::with_capacity(slices.len());
        let mut placed: BTreeSet<String> = BTreeSet::new();
        while ordered.len() < slices.len() {
            let next = slices
                .iter()
                .position(|s| {
                    !placed.contains(&s.id) && s.depends.iter().all(|d| placed.contains(d))
                })
                .expect("an acyclic graph has a slice whose dependencies are placed");
            placed.insert(slices[next].id.clone());
            ordered.push(slices[next].clone());
        }
        Ok(ordered)
    }

    /// The cards laid when a unit's slices are declared: each slice step
    /// once per slice (its owners taking turns), then the phase steps after
    /// the slices, each waiting for every slice's card of the steps it
    /// depends on.
    pub fn slice_cards(
        &self,
        phase: &str,
        title: &str,
        brief: &str,
        slices: &[SliceSpec],
    ) -> Result<Vec<TaskSpec>> {
        if !self.has_slices() {
            bail!("this workflow has no slice steps");
        }
        if slices.is_empty() {
            bail!("declare at least one slice");
        }
        if slices.len() > MAX_SLICES {
            bail!(
                "a unit has at most {MAX_SLICES} slices, found {}",
                slices.len()
            );
        }
        let room = 32 - 1 - self.longest_step(Some(Scope::Slice)) - 1 - phase.len();
        for slice in slices {
            check_unit_id("slice", &slice.id, room, "<step>-<phase>-<slice>")?;
        }
        let slices = &Self::slice_order(slices)?;
        let mut cards = Vec::new();
        for (i, step) in self.steps.iter().enumerate() {
            match step.scope {
                Scope::Slice => {
                    for (n, slice) in slices.iter().enumerate() {
                        let mut card = self.card(i, phase, title, brief, Some((n, slice)));
                        // A slice waits for the same step of the slices it
                        // depends on; the rest run beside it.
                        card.depends_on.extend(
                            slice
                                .depends
                                .iter()
                                .map(|dep| Self::card_id(step, phase, Some(dep))),
                        );
                        cards.push(card);
                    }
                }
                Scope::Phase if self.after_slices(i) => {
                    let mut card = self.card(i, phase, title, brief, None);
                    // The slice steps it waits for: every slice's card.
                    for dep in &step.depends_on {
                        let dep_step = self
                            .steps
                            .iter()
                            .find(|s| &s.id == dep)
                            .expect("validated dependency");
                        if dep_step.scope == Scope::Slice {
                            card.depends_on.extend(
                                slices
                                    .iter()
                                    .map(|s| Self::card_id(dep_step, phase, Some(&s.id))),
                            );
                        }
                    }
                    cards.push(card);
                }
                Scope::Phase => {}
            }
        }
        Ok(cards)
    }

    /// The files step `index` must leave in its commit for the unit
    /// (`phase`, `title`; `slice` for a slice step's card), paths filled in.
    pub fn produces_of(
        &self,
        index: usize,
        phase: &str,
        title: &str,
        slice: Option<&SliceSpec>,
    ) -> Vec<String> {
        let mut pairs = unit_pairs(phase, title, "");
        if let Some(s) = slice {
            pairs.push(("slice", &s.id));
            pairs.push(("slice_title", &s.title));
            pairs.push(("slice_brief", &s.brief));
        }
        self.steps
            .get(index)
            .map(|step| step.produces.iter().map(|p| fill(p, &pairs)).collect())
            .unwrap_or_default()
    }

    /// The verdict step `index` gives, with its file filled in.
    pub fn verdict_of(&self, index: usize, phase: &str, title: &str) -> Option<Verdict> {
        let verdict = self.steps.get(index)?.verdict.as_ref()?;
        Some(Verdict {
            file: fill(&verdict.file, &unit_pairs(phase, title, "")),
            ..verdict.clone()
        })
    }

    /// The step whose verdict is the unit's: the last with one.
    pub fn verdict_step(&self) -> Option<usize> {
        self.steps.iter().rposition(|s| s.verdict.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_is_read_off_a_key_line_and_classified() {
        let verdict = Verdict {
            file: ".planning/{phase}/VERIFICATION.md".into(),
            key: "status".into(),
            pass: vec!["passed".into()],
            redo: vec!["gaps_found".into()],
        };
        assert_eq!(
            verdict.read("---\nphase: 3\nstatus: passed\n---\n# Verification"),
            Some("passed".into())
        );
        assert_eq!(
            verdict.read("Status line\n  status: \"gaps_found\" \n"),
            Some("gaps_found".into())
        );
        assert_eq!(verdict.read("status_code: 1\nnothing"), None);
        assert_eq!(verdict.read("status:\n"), None);
        assert_eq!(verdict.outcome("passed"), Outcome::Pass);
        assert_eq!(verdict.outcome("gaps_found"), Outcome::Redo);
        assert_eq!(verdict.outcome("human_needed"), Outcome::Other);
        assert_eq!(verdict.choices(), "passed, or gaps_found (a follow-up run)");
        let workflow = Workflow {
            steps: vec![Step {
                id: "verify".into(),
                agent: Some("v".into()),
                description: "x".into(),
                produces: vec![".planning/{phase}/VERIFICATION.md".into()],
                verdict: Some(verdict),
                ..Step::default()
            }],
            ..Workflow::default()
        };
        assert_eq!(
            workflow.produces_of(0, "03-auth", "Auth", None),
            [".planning/03-auth/VERIFICATION.md"]
        );
        assert_eq!(
            workflow.verdict_of(0, "03-auth", "Auth").unwrap().file,
            ".planning/03-auth/VERIFICATION.md"
        );
        assert_eq!(workflow.verdict_step(), Some(0));
        assert!(workflow.produces_of(5, "p", "t", None).is_empty());
        // Checked with the rest of the chain.
        let team = agents(&["v"]);
        workflow.validate(&team).unwrap();
        let mut bad = workflow.clone();
        bad.steps[0].verdict.as_mut().unwrap().pass.clear();
        assert!(bad
            .validate(&team)
            .unwrap_err()
            .to_string()
            .contains("names no value"));
        let mut bad = workflow.clone();
        bad.steps[0].verdict.as_mut().unwrap().redo = vec!["passed".into()];
        assert!(bad
            .validate(&team)
            .unwrap_err()
            .to_string()
            .contains("both a pass and a redo"));
        let parsed: Workflow = serde_yaml::from_str(
            "order: sequential\nreview_rounds: 3\nsteps:\n  - id: a\n    agent: v\n    description: d\n    verdict: { file: F, pass: [ok] }\n",
        )
        .unwrap();
        assert_eq!(parsed.order, Order::Sequential);
        assert_eq!(parsed.review_rounds, 3);
        assert_eq!(parsed.steps[0].verdict.as_ref().unwrap().key, "status");
    }

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
            agent: Some(agent.into()),
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
            ..Workflow::default()
        };
        workflow.validate(&team).unwrap();

        let bad = |mutate: &dyn Fn(&mut Workflow)| {
            let mut w = workflow.clone();
            mutate(&mut w);
            w.validate(&team).unwrap_err().to_string()
        };
        assert!(bad(&|w| w.steps.clear()).contains("empty"));
        assert!(bad(&|w| w.steps[1].agent = Some("nobody".into())).contains("not in `agents`"));
        assert!(bad(&|w| w.steps[1].agent = Some(ORCHESTRATOR_ID.into())).contains("orchestrator"));
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
            .phase_cards("03-auth", "Authentication", "sessions and tokens")
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
            .phase_cards("a-phase-id-that-is-far-too-long-for-a-card", "t", "")
            .unwrap_err()
            .to_string();
        assert!(error.contains("at most 23 characters"), "{error}");
        assert!(workflow.phase_cards("03", " ", "").is_err());
        assert!(workflow.phase_cards("Phase 3", "t", "").is_err());
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

    #[test]
    fn slices_that_touch_the_same_paths_take_turns_and_dependencies_come_first() {
        let slice = |id: &str, depends: &[&str], touches: &[&str]| SliceSpec {
            id: id.into(),
            title: id.to_uppercase(),
            brief: String::new(),
            depends: depends.iter().map(|s| s.to_string()).collect(),
            touches: touches.iter().map(|s| s.to_string()).collect(),
        };
        assert!(touches_overlap("src/api", "src/api/users.rs"));
        assert!(touches_overlap("src/api/", "./src/api/*"));
        assert!(touches_overlap("src/api", "src/api"));
        assert!(!touches_overlap("src/api", "src/apiary"));
        assert!(!touches_overlap("src/api", "src/ui"));
        assert!(!touches_overlap("", "src"));
        // db and ui are disjoint; api writes under db's paths, so it waits
        // for db although it named nothing; docs waits for ui by name,
        // which puts it after ui whatever the declared order.
        let ordered = Workflow::slice_order(&[
            slice("docs", &["ui"], &["docs/"]),
            slice("db", &[], &["src/db", "migrations"]),
            slice("ui", &[], &["src/ui"]),
            slice("api", &[], &["src/db/queries.rs", "src/api"]),
        ])
        .unwrap();
        let ids: Vec<&str> = ordered.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["db", "ui", "docs", "api"]);
        assert_eq!(ordered[3].depends, ["db"]);
        assert_eq!(ordered[2].depends, ["ui"]);
        assert!(ordered[0].depends.is_empty());
        assert_eq!(ordered[0].touches, ["src/db", "migrations"]);
        // A declared dependency the other way is not doubled by the paths.
        let ordered =
            Workflow::slice_order(&[slice("a", &["b"], &["x"]), slice("b", &[], &["x/y"])])
                .unwrap();
        assert_eq!(ordered[0].id, "b");
        assert_eq!(ordered[1].depends, ["b"]);
        // A slice that names nothing takes its turn alone.
        let ordered = Workflow::slice_order(&[
            slice("a", &[], &["x"]),
            slice("b", &[], &[]),
            slice("c", &[], &["y"]),
        ])
        .unwrap();
        assert!(ordered[0].depends.is_empty());
        assert_eq!(ordered[1].depends, ["a"]);
        assert_eq!(ordered[2].depends, ["b"]);
        for (bad, why) in [
            (vec![slice("a", &[], &["/etc"])], "not a path inside"),
            (vec![slice("a", &[], &["../x"])], "not a path inside"),
            (vec![slice("a", &["b"], &[])], "unknown slice"),
            (
                vec![slice("a", &["b"], &[]), slice("b", &["a"], &[])],
                "cycle",
            ),
            (vec![slice("A", &[], &[])], "not a slice id"),
        ] {
            let e = Workflow::slice_order(&bad).unwrap_err().to_string();
            assert!(e.contains(why), "{e}");
        }
    }
}
