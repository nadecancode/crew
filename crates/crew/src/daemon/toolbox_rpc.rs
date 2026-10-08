//! The toolbox calls (notes/toolboxes.md §Secrets, §Orchestrator tools,
//! §Display): how `crew toolbox mcp` reaches a member's server, what it
//! reports back, and members' toolboxes as `crew toolbox` and `agents_list`
//! show them.
//!
//! Until toolbox-launch keeps each member's active toolbox, a member's
//! definitions are resolved here from the running team's configuration and
//! its references from the daemon's environment: `member_toolbox`,
//! `reference_value` and `secret_names` are the seams that change then.

use std::collections::{BTreeMap, BTreeSet};

use crew_core::config::TeamConfig;
use crew_core::rpc::{
    MemberToolbox, Redaction, RpcError, ToolboxReportKind, ToolboxServerLaunch, Upstream,
};
use crew_core::toolbox::{Content, Fingerprint, Item, ItemKind, Reference, Resolved, Toolbox};
use crew_core::types::{
    AgentToolbox, ToolboxItem, ToolboxItemCode, ToolboxItemKind, ToolboxItemStatus,
};
use crew_core::{Agent, RuntimeKind};

use super::Daemon;
use crate::mcp::proxy;
use crate::persistence::NewEvent;

/// The version of how a toolbox is mapped onto a runtime, part of every
/// fingerprint.
const MAPPING_VERSION: u32 = 1;

/// The team-file level that holds a member's own toolbox.
fn member_level<'a>(team: &'a TeamConfig, agent: &Agent) -> (&'a Toolbox, String) {
    static EMPTY: std::sync::OnceLock<Toolbox> = std::sync::OnceLock::new();
    if agent.orchestrator {
        return (&team.orchestrator.toolbox, "orchestrator.toolbox".into());
    }
    // A subagent has its parent's toolbox.
    let id = agent.parent.as_deref().unwrap_or(&agent.id);
    match team.agents.iter().position(|a| a.id == id) {
        Some(i) => (&team.agents[i].toolbox, format!("agents[{i}].toolbox")),
        None => (EMPTY.get_or_init(Toolbox::default), format!("agent '{id}'.toolbox")),
    }
}

/// What a reference resolves to, or the name of the variable it needs.
fn resolve_reference(reference: &Reference) -> Result<String, String> {
    match reference {
        Reference::Value(value) => Ok(value.clone()),
        Reference::Env { name, format } => {
            let value = std::env::var(name).map_err(|_| name.clone())?;
            Ok(match format {
                Some(format) => format.replacen("{}", &value, 1),
                None => value,
            })
        }
    }
}

fn env_names(server: &crew_core::toolbox::McpServer) -> impl Iterator<Item = &str> {
    server
        .env
        .values()
        .chain(server.headers.values())
        .filter_map(|r| match r {
            Reference::Env { name, .. } => Some(name.as_str()),
            Reference::Value(_) => None,
        })
}

impl Daemon {
    /// A member's effective toolbox definitions.
    fn member_toolbox(&self, agent: &Agent) -> Result<Resolved, RpcError> {
        let model = self.model();
        let Some(team) = model.session.team.as_ref() else {
            return Ok(Resolved::default());
        };
        let (level, path) = member_level(team, agent);
        team.resolve_toolbox(level, &path)
            .map_err(|e| RpcError::invalid(format!("{e:#}")))
    }

    /// The member's secret names: every variable its toolbox references
    /// (removed from what its servers inherit).
    fn secret_names(&self, resolved: &Resolved) -> Vec<String> {
        let names: BTreeSet<&str> = resolved
            .items
            .values()
            .filter_map(|r| match &r.item {
                Item::Mcp(server) => Some(env_names(server)),
                _ => None,
            })
            .flatten()
            .collect();
        names.into_iter().map(str::to_string).collect()
    }

    /// `Call::ToolboxServer`: how agent `id`'s `crew toolbox mcp <name>`
    /// reaches the server, references resolved. Only for the caller's own
    /// toolbox; nothing of it is logged.
    pub(crate) fn toolbox_server_launch(
        &self,
        id: &str,
        name: &str,
        _preflight: bool,
    ) -> Result<ToolboxServerLaunch, RpcError> {
        let agent = self
            .agent(id)
            .ok_or_else(|| RpcError::not_found(format!("no agent '{id}'")))?;
        let resolved = self.member_toolbox(&agent)?;
        let item = crew_core::toolbox::ItemRef::new(ItemKind::Mcp, name);
        let Some(Item::Mcp(server)) = resolved.items.get(&item).map(|r| &r.item) else {
            return Err(RpcError::not_found(format!(
                "{id}'s toolbox has no MCP server '{name}'"
            )));
        };
        let mut redact = Vec::new();
        let mut resolve_map = |map: &BTreeMap<String, Reference>| -> Result<BTreeMap<String, String>, RpcError> {
            let mut out = BTreeMap::new();
            for (key, reference) in map {
                let value = resolve_reference(reference)
                    .map_err(|missing| RpcError::invalid(format!("Needs ${missing}")))?;
                if let Reference::Env { name, .. } = reference {
                    redact.push(Redaction {
                        name: name.clone(),
                        value: value.clone(),
                    });
                }
                out.insert(key.clone(), value);
            }
            Ok(out)
        };
        let upstream = match &server.url {
            Some(url) => Upstream::Http {
                url: url.clone(),
                transport: server.transport.unwrap_or_default(),
                headers: resolve_map(&server.headers)?,
            },
            None => Upstream::Stdio {
                command: server.command.clone(),
                cwd: server.cwd.as_ref().map(|cwd| {
                    std::path::Path::new(&agent.workdir)
                        .join(cwd)
                        .to_string_lossy()
                        .into_owned()
                }),
                env: resolve_map(&server.env)?,
            },
        };
        Ok(ToolboxServerLaunch {
            name: name.to_string(),
            upstream,
            redact,
            scrub: self.secret_names(&resolved),
            definition: proxy::definition_hash(server),
            snapshot: proxy::snapshot_path(&self.agent_dir(id), name)
                .to_string_lossy()
                .into_owned(),
            refresh: false,
        })
    }

    /// `Call::ToolboxReport`: what agent `id`'s proxy saw of server `name`.
    pub(crate) async fn toolbox_report(
        &self,
        id: &str,
        name: &str,
        kind: ToolboxReportKind,
        detail: &str,
    ) {
        let detail = crew_core::util::truncate_chars(&crew_core::util::one_line(detail), 400);
        match kind {
            ToolboxReportKind::Failed => {
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "toolbox.failed",
                        format!("{id}'s toolbox server {name} failed in session: {detail}"),
                    )
                    .agent(id)
                    .warn(),
                );
                self.notify_orchestrator(&format!(
                    "{id}'s toolbox server {name} failed in session ({detail}). Its session keeps \
running without it; a restart starts it again."
                ))
                .await;
            }
            ToolboxReportKind::ListingDiffers | ToolboxReportKind::NewTools => {
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "toolbox.listing",
                        format!("{id}'s toolbox server {name}: {detail}"),
                    )
                    .agent(id),
                );
            }
        }
    }

    /// A member's toolbox as it shows: the one its session runs with when it
    /// has run (`Agent.toolbox`), else how its configuration would map onto
    /// `runtime` (its own when left out).
    pub(crate) fn toolbox_view(
        &self,
        agent: &Agent,
        runtime: Option<RuntimeKind>,
    ) -> Result<MemberToolbox, RpcError> {
        if runtime.is_none_or(|r| r == agent.runtime) {
            if let Some(toolbox) = &agent.toolbox {
                return Ok(MemberToolbox {
                    agent: agent.id.clone(),
                    runtime: agent.runtime,
                    toolbox: toolbox.clone(),
                    stops: Vec::new(),
                });
            }
        }
        let runtime = runtime.unwrap_or(agent.runtime);
        let resolved = self.member_toolbox(agent)?;
        Ok(planned(&agent.id, runtime, &resolved))
    }

    /// `Call::ToolboxCheck`: every member of `team` (a team file the CLI
    /// loaded), resolved against this daemon's environment.
    pub(crate) fn toolbox_check(&self, team: &TeamConfig) -> Result<Vec<MemberToolbox>, RpcError> {
        let mut members: Vec<(String, RuntimeKind, &Toolbox, String)> = Vec::new();
        if team.orchestrator.agent {
            members.push((
                crew_core::ORCHESTRATOR_ID.to_string(),
                team.orchestrator.runtime.unwrap_or(team.defaults.runtime),
                &team.orchestrator.toolbox,
                "orchestrator.toolbox".into(),
            ));
        }
        for (i, spec) in team.agents.iter().enumerate() {
            members.push((
                spec.id.clone(),
                spec.runtime.unwrap_or(team.defaults.runtime),
                &spec.toolbox,
                format!("agents[{i}].toolbox"),
            ));
        }
        members
            .into_iter()
            .map(|(id, runtime, level, path)| {
                let resolved = team
                    .resolve_toolbox(level, &path)
                    .map_err(|e| RpcError::invalid(format!("{e:#}")))?;
                Ok(planned(&id, runtime, &resolved))
            })
            .collect()
    }
}

/// A toolbox as it would map before anything runs: every item Planned, or
/// why it would not load.
fn planned(id: &str, runtime: RuntimeKind, resolved: &Resolved) -> MemberToolbox {
    let mut stops = Vec::new();
    let mut content = BTreeMap::new();
    let items = resolved
        .items
        .iter()
        .map(|(item, r)| {
            let required = resolved.strict || r.item.required();
            let applies = runtime != RuntimeKind::Shell
                && (r.item.runtimes().is_empty() || r.item.runtimes().contains(&runtime));
            if item.kind.is_file() {
                content.insert(item.clone(), Content::NotLoaded);
            }
            let missing = match &r.item {
                Item::Mcp(server) if applies => env_names(server)
                    .find(|name| std::env::var(name).is_err())
                    .map(str::to_string),
                _ => None,
            };
            let (code, text) = match (applies, &missing) {
                (false, _) => (ToolboxItemCode::NotApplicable, "Not applicable".to_string()),
                (true, Some(name)) => (ToolboxItemCode::Needs, format!("Needs ${name}")),
                (true, None) => (ToolboxItemCode::Planned, "Planned".to_string()),
            };
            if required && code == ToolboxItemCode::Needs {
                stops.push(format!("{item}: {text}"));
            }
            ToolboxItem {
                kind: ToolboxItemKind::from(item.kind),
                name: item.name.clone(),
                origin: r.origin.to_string(),
                required: required && applies,
                status: ToolboxItemStatus { code, text },
                detail: detail(&r.item),
            }
        })
        .collect();
    let fingerprint = resolved
        .canonical(&content)
        .map(|canonical| Fingerprint::of(&canonical, runtime, MAPPING_VERSION).as_str().to_string())
        .unwrap_or_default();
    MemberToolbox {
        agent: id.to_string(),
        runtime,
        toolbox: AgentToolbox {
            fingerprint,
            items,
            ..AgentToolbox::default()
        },
        stops,
    }
}

/// An item's detail: an MCP server's command or URL and its references,
/// `$NAME` or `$NAME (not set)`. Never a value.
fn detail(item: &Item) -> Option<String> {
    let Item::Mcp(server) = item else {
        return None;
    };
    let mut out = match &server.url {
        Some(url) => url.clone(),
        None => server.command.join(" "),
    };
    let refs: Vec<String> = env_names(server)
        .map(|name| match std::env::var(name) {
            Ok(_) => format!("${name}"),
            Err(_) => format!("${name} (not set)"),
        })
        .collect();
    if !refs.is_empty() {
        out.push_str(" · ");
        out.push_str(&refs.join(", "));
    }
    Some(out)
}

impl Daemon {
    /// Whether `agent` may move to `runtime`: refused when its toolbox maps
    /// there with a required item that would not load, with that item's
    /// reason.
    pub(crate) fn toolbox_switch_check(
        &self,
        agent: &Agent,
        runtime: RuntimeKind,
    ) -> Result<(), RpcError> {
        let view = self.toolbox_view(agent, Some(runtime))?;
        match view.stops.first() {
            Some(stop) => Err(RpcError::invalid(format!(
                "{} cannot move to {}: {stop}",
                agent.id,
                crate::models::runtime_label(runtime)
            ))),
            None => Ok(()),
        }
    }

    /// The bundles an agent may name for a new member (`agent_add`): names
    /// of the team's `toolboxes`, nothing else.
    pub(crate) fn check_bundle_selection(&self, toolbox: &Toolbox) -> Result<(), RpcError> {
        let selection_only = Toolbox {
            include: toolbox.include.clone(),
            ..Toolbox::default()
        };
        if *toolbox != selection_only {
            return Err(RpcError::forbidden(
                "an agent chooses bundles by name; toolbox items are defined in the team file",
            ));
        }
        let model = self.model();
        let bundles: Vec<&str> = model
            .session
            .team
            .as_ref()
            .map(|t| t.toolboxes.keys().map(String::as_str).collect())
            .unwrap_or_default();
        for name in &toolbox.include {
            if !bundles.contains(&name.as_str()) {
                return Err(RpcError::invalid(if bundles.is_empty() {
                    format!("Unknown toolbox \"{name}\". This team defines no toolboxes.")
                } else {
                    format!(
                        "Unknown toolbox \"{name}\". Toolboxes in this team: {}.",
                        bundles.join(", ")
                    )
                }));
            }
        }
        Ok(())
    }
}
