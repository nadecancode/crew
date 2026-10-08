//! The toolbox calls (notes/toolboxes.md §Secrets, §Orchestrator tools,
//! §Display): how `crew toolbox mcp` reaches a member's server, what it
//! reports back, members' toolboxes as `crew toolbox` and `agents_list`
//! show them, and the bundles an agent may choose for another. What a member
//! runs with, and how a toolbox maps onto a runtime, are toolbox-launch's
//! (daemon/toolbox.rs); these calls only read them.

use std::collections::{BTreeMap, BTreeSet};

use crew_core::config::TeamConfig;
use crew_core::rpc::{
    MemberToolbox, Redaction, RpcError, ToolboxReload, ToolboxReportKind, ToolboxServerLaunch,
    Upstream,
};
use crew_core::toolbox::{Content, Item, ItemKind, Resolved, Toolbox};

use crate::agents::toolbox::ActiveToolbox;
use crew_core::{Agent, RuntimeKind};

use super::Daemon;
use crate::persistence::NewEvent;

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
        None => (
            EMPTY.get_or_init(Toolbox::default),
            format!("agent '{id}'.toolbox"),
        ),
    }
}

impl Daemon {
    /// A member's toolbox as its configuration says now: what a subagent
    /// was given, else its own spec resolved against the running team.
    fn configured_toolbox(&self, agent: &Agent) -> Result<Resolved, RpcError> {
        let model = self.model();
        let Some(team) = model.session.team.as_ref() else {
            return Ok(Resolved::default());
        };
        let resolved = match model.local.get(&agent.id) {
            Some(local) => match &local.toolbox_inherited {
                Some(inherited) => return Ok(inherited.resolved.clone()),
                None => team.resolve_toolbox(
                    &local.spec.toolbox,
                    &format!("agent '{}'.toolbox", agent.id),
                ),
            },
            None => {
                let (level, path) = member_level(team, agent);
                team.resolve_toolbox(level, &path)
            }
        };
        resolved.map_err(|e| RpcError::invalid(format!("{e:#}")))
    }

    /// `Call::ToolboxServer`: how agent `id`'s `crew toolbox mcp <name>`
    /// reaches the server, references resolved. Only a server of the
    /// toolbox its session was launched with, and only with the values
    /// captured for that launch; nothing of it is logged.
    pub(crate) fn toolbox_server_launch(
        &self,
        id: &str,
        name: &str,
        _preflight: bool,
    ) -> Result<ToolboxServerLaunch, RpcError> {
        let agent = self
            .agent(id)
            .ok_or_else(|| RpcError::not_found(format!("no agent '{id}'")))?;
        let (server, refresh) = self.toolbox_item(id, name)?;
        let references = self.toolbox_references(id, name)?;
        if let Some(missing) = references.missing.iter().next() {
            return Err(RpcError::invalid(format!("Needs ${missing}")));
        }
        let expose =
            |map: &BTreeMap<String, crate::endpoints::Secret>| -> BTreeMap<String, String> {
                map.iter()
                    .map(|(k, v)| (k.clone(), v.expose().to_string()))
                    .collect()
            };
        let redact = references
            .redactions
            .iter()
            .map(|(name, value)| Redaction {
                name: name.clone(),
                value: value.expose().to_string(),
            })
            .collect();
        // Every secret name of the member, not only this server's: no server
        // inherits another's credential from the session.
        let mut scrub: BTreeSet<String> = references.scrub.clone();
        if let Some(active) = self.active_toolbox(id) {
            scrub.extend(active.scrub_env);
        }
        let upstream = match &server.url {
            Some(url) => Upstream::Http {
                url: url.clone(),
                transport: server.transport.unwrap_or_default(),
                headers: expose(&references.headers),
            },
            None => Upstream::Stdio {
                command: server.command.clone(),
                cwd: server.cwd.as_ref().map(|cwd| {
                    std::path::Path::new(&agent.workdir)
                        .join(cwd)
                        .to_string_lossy()
                        .into_owned()
                }),
                env: expose(&references.env),
            },
        };
        Ok(ToolboxServerLaunch {
            name: name.to_string(),
            upstream,
            redact,
            scrub: scrub.into_iter().collect(),
            definition: crate::mcp::proxy::definition_hash(&server),
            snapshot: crate::mcp::proxy::snapshot_path(&self.agent_dir(id), name)
                .to_string_lossy()
                .into_owned(),
            refresh,
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
                self.toolbox_failed_in_session(id, name, &detail);
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

    /// A member's toolbox as it shows. Without `runtime`, the one its session
    /// runs with when it has run (`Agent.toolbox`). With `runtime`, or before
    /// it has run, how its configuration would map onto that runtime (its own
    /// when left out) at the next launch.
    pub(crate) fn toolbox_view(
        &self,
        agent: &Agent,
        runtime: Option<RuntimeKind>,
    ) -> Result<MemberToolbox, RpcError> {
        if let (None, Some(toolbox)) = (runtime, &agent.toolbox) {
            return Ok(MemberToolbox {
                agent: agent.id.clone(),
                runtime: agent.runtime,
                toolbox: toolbox.clone(),
                stops: Vec::new(),
            });
        }
        let runtime = runtime.unwrap_or(agent.runtime);
        let resolved = self.configured_toolbox(agent)?;
        self.toolbox_preview(&agent.id, agent.parent.as_deref(), runtime, &resolved)
    }

    /// `Call::ToolboxCheck`: every member of `team` (a team file the CLI
    /// loaded), mapped as its next launch would be, without starting
    /// anything.
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
                self.toolbox_check_preview(
                    team,
                    &id,
                    team.agents.iter().find(|spec| spec.id == id),
                    runtime,
                    &resolved,
                )
            })
            .collect()
    }

    /// `Call::ToolboxReload`: the team file's toolboxes, staged for each
    /// member's next restart.
    pub(crate) fn toolbox_reload_call(&self, team: &TeamConfig) -> Result<ToolboxReload, RpcError> {
        self.toolbox_reload(team)
    }
}

impl Daemon {
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
        let mut bundles: Vec<&str> = model
            .session
            .team
            .as_ref()
            .map(|t| t.toolboxes.keys().map(String::as_str).collect())
            .unwrap_or_default();
        if !bundles.contains(&crew_core::toolbox::COMPACT_CLI) {
            bundles.push(crew_core::toolbox::COMPACT_CLI);
            bundles.sort_unstable();
        }
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

impl Daemon {
    /// What a new subagent of `parent` gets (notes/toolboxes.md, Orchestrator
    /// tools, `agent_spawn`): by default a copy of the parent's active
    /// toolbox; with `selection`, only those bundles, resolved against the
    /// team's current definitions, each item equal to the parent's (the same
    /// definition and, for a file, the same content). A worker cannot grant
    /// more than it has. Checked before the subagent is created.
    pub(crate) fn subagent_toolbox(
        &self,
        parent: &str,
        selection: Option<Vec<String>>,
    ) -> Result<Option<ActiveToolbox>, RpcError> {
        let Some(active) = self.active_toolbox(parent) else {
            return match selection {
                Some(names) if !names.is_empty() => Err(RpcError::invalid(
                    "you run without a toolbox: there is nothing to give a subagent",
                )),
                _ => Ok(None),
            };
        };
        let Some(names) = selection else {
            return Ok(Some(ActiveToolbox {
                // The child's own launch fingerprints and pins what it gets.
                fingerprint: None,
                definition_digest: None,
                refresh: false,
                ..active
            }));
        };
        let requested = {
            let model = self.model();
            let team = model
                .session
                .team
                .as_ref()
                .ok_or_else(|| RpcError::unavailable("no team is loaded"))?;
            team.resolve_toolbox(
                &Toolbox {
                    include: names,
                    reset: true,
                    ..Toolbox::default()
                },
                "agent_spawn.toolbox",
            )
            .map_err(|e| RpcError::invalid(format!("{e:#}")))?
        };
        let store = std::cell::OnceCell::new();
        let mut out = ActiveToolbox {
            resolved: Resolved {
                items: BTreeMap::new(),
                sources: active.resolved.sources.clone(),
                strict: active.resolved.strict,
            },
            snapshot_owner: active.snapshot_owner.clone(),
            ..ActiveToolbox::default()
        };
        for (item, entry) in &requested.items {
            let Some(own) = active.resolved.items.get(item) else {
                return Err(RpcError::invalid(format!(
                    "{item} is not in your toolbox, so you cannot give it to a subagent"
                )));
            };
            if own.item != entry.item {
                return Err(RpcError::invalid(format!(
                    "{item} is defined differently now than in the toolbox you were started with; \
a subagent can only get what you have"
                )));
            }
            if let (Item::File(file), Some(Content::Sha256(had))) =
                (&entry.item, active.content.get(item))
            {
                let store = match store.get() {
                    Some(store) => store,
                    None => {
                        let opened = crate::agents::toolbox::store::Store::open(&self.state_dir)
                            .map_err(|e| RpcError::internal(format!("{e:#}")))?;
                        store.get_or_init(|| opened)
                    }
                };
                let now = store
                    .snapshot(std::path::Path::new(&file.path))
                    .map(|s| s.hash)
                    .unwrap_or_default();
                if now != *had {
                    return Err(RpcError::invalid(format!(
                        "{item}'s files changed since you were started; a subagent can only get \
what you have"
                    )));
                }
            }
            out.resolved.items.insert(item.clone(), own.clone());
            if let Some(content) = active.content.get(item) {
                out.content.insert(item.clone(), content.clone());
            }
            if let Some(file) = active.files.get(item) {
                out.files.insert(item.clone(), file.clone());
            }
            if item.kind == ItemKind::Mcp && active.servers.contains(&item.name) {
                out.servers.insert(item.name.clone());
                if let Some(snapshot) = active.mcp_snapshots.get(&item.name) {
                    out.mcp_snapshots
                        .insert(item.name.clone(), snapshot.clone());
                }
            }
        }
        Ok(Some(out))
    }

    /// Hand a new subagent the immutable snapshot references captured by
    /// subagent_toolbox, before agent creation or any await. The launch uses
    /// those bytes; it never consults the parent's mutable pin files.
    pub(crate) fn give_subagent_toolbox(
        &self,
        child: &str,
        toolbox: Option<ActiveToolbox>,
    ) -> Result<(), RpcError> {
        match toolbox {
            Some(toolbox) => self.inherit_toolbox(child, toolbox),
            None => Ok(()),
        }
    }
}

/// T11's subagent cases (notes/toolboxes.md, Orchestrator tools): what a
/// subagent gets from a parent whose session runs with a toolbox, and that
/// it can never get more.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::AgentLocal;

    pub(super) const TEAM: &str = r#"
project: t
objective: t
toolboxes:
  web:
    mcp:
      context7: { command: [ctx, --stdio] }
  docs:
    mcp:
      manual: { command: [manual, --stdio] }
agents:
  - id: parent
    role: worker
"#;

    pub(super) async fn daemon() -> (tempfile::TempDir, crate::daemon::Running) {
        let temp = tempfile::tempdir().unwrap();
        let running = crate::daemon::start(crate::daemon::DaemonOptions {
            workspace: temp.path().into(),
            crew_bin: std::env::current_exe().unwrap(),
            pin: None,
        })
        .await
        .unwrap();
        (temp, running)
    }

    /// The parent runs with bundles `web` and `docs` as the team defined
    /// them when it started.
    pub(super) fn parent_runs_with(daemon: &Daemon, team: TeamConfig) -> ActiveToolbox {
        let resolved = team
            .resolve_toolbox(
                &Toolbox {
                    include: vec!["web".into(), "docs".into()],
                    ..Toolbox::default()
                },
                "agents[0].toolbox",
            )
            .unwrap();
        let active = ActiveToolbox {
            resolved,
            servers: ["context7".to_string(), "manual".to_string()].into(),
            definition_digest: Some("digest".into()),
            scrub_env: ["SHOP_TOKEN".to_string()].into(),
            snapshot_owner: Some("parent".into()),
            ..ActiveToolbox::default()
        };
        let mut model = daemon.model();
        model.session.team = Some(team);
        model.local.insert(
            "parent".into(),
            AgentLocal {
                toolbox_active: Some(active.clone()),
                ..AgentLocal::default()
            },
        );
        model.local.insert("child".into(), AgentLocal::default());
        active
    }

    pub(super) fn team(yaml: &str) -> TeamConfig {
        TeamConfig::from_yaml(yaml).unwrap()
    }

    #[tokio::test]
    async fn a_default_subagent_gets_a_copy_of_its_parent_s_active_toolbox() {
        let (_temp, running) = daemon().await;
        let daemon = &running.daemon;
        let active = parent_runs_with(daemon, team(TEAM));
        let copy = daemon.subagent_toolbox("parent", None).unwrap().unwrap();
        assert_eq!(copy.resolved, active.resolved);
        assert_eq!(copy.content, active.content);
        assert_eq!(copy.servers, active.servers);
        // Its launch pins the parent's snapshots and fingerprints them anew.
        assert_eq!(copy.snapshot_owner.as_deref(), Some("parent"));
        assert_eq!(copy.definition_digest, None);
        daemon
            .give_subagent_toolbox("child", Some(copy.clone()))
            .unwrap();
        let inherited = daemon.model().local["child"].toolbox_inherited.clone();
        assert_eq!(inherited.unwrap().resolved, copy.resolved);
    }

    #[tokio::test]
    async fn a_selection_gives_only_those_bundles_and_never_more() {
        let (_temp, running) = daemon().await;
        let daemon = &running.daemon;
        let active = parent_runs_with(daemon, team(TEAM));
        let docs = daemon
            .subagent_toolbox("parent", Some(vec!["docs".into()]))
            .unwrap()
            .unwrap();
        let names: Vec<String> = docs.resolved.items.keys().map(|k| k.to_string()).collect();
        assert_eq!(names, ["mcp.manual"]);
        assert_eq!(docs.servers, ["manual".to_string()].into());
        assert_eq!(docs.snapshot_owner.as_deref(), Some("parent"));
        let manual = crew_core::toolbox::ItemRef::new(ItemKind::Mcp, "manual");
        assert_eq!(docs.resolved.items[&manual], active.resolved.items[&manual]);

        // A reload widens `web` while the parent still runs the old one: a
        // default subagent gets the old items, and asking for `web` is
        // refused, naming the first new item.
        let widened = TEAM.replace(
            "      context7: { command: [ctx, --stdio] }",
            "      context7: { command: [ctx, --stdio] }\n      search: { command: [search] }",
        );
        daemon.model().session.team = Some(team(&widened));
        let copy = daemon.subagent_toolbox("parent", None).unwrap().unwrap();
        assert_eq!(copy.resolved, active.resolved);
        let refused = daemon
            .subagent_toolbox("parent", Some(vec!["web".into()]))
            .unwrap_err();
        assert!(
            refused
                .message
                .contains("mcp.search is not in your toolbox"),
            "{}",
            refused.message
        );

        // Nor a bundle whose definition changed since the parent started.
        let changed = TEAM.replace("[ctx, --stdio]", "[ctx, --http]");
        daemon.model().session.team = Some(team(&changed));
        let refused = daemon
            .subagent_toolbox("parent", Some(vec!["web".into()]))
            .unwrap_err();
        assert!(
            refused
                .message
                .contains("mcp.context7 is defined differently"),
            "{}",
            refused.message
        );
    }

    #[tokio::test]
    async fn a_parent_without_a_toolbox_has_nothing_to_give() {
        let (_temp, running) = daemon().await;
        let daemon = &running.daemon;
        daemon.model().session.team = Some(team(TEAM));
        assert!(daemon.subagent_toolbox("parent", None).unwrap().is_none());
        let refused = daemon
            .subagent_toolbox("parent", Some(vec!["web".into()]))
            .unwrap_err();
        assert!(
            refused.message.contains("nothing to give"),
            "{}",
            refused.message
        );
    }
}

#[cfg(test)]
mod frozen_snapshot_tests {
    use super::tests::*;
    use super::*;
    use serde_json::json;

    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(parent) = std::fs::File::open(self.0.join("toolbox")) {
                let _ =
                    crate::agents::toolbox::store::remove(&parent, std::path::Path::new("store"));
            }
        }
    }

    fn freeze(daemon: &Daemon, store: &crate::agents::toolbox::store::Store, name: &str) {
        let snapshot = store
            .snapshot(&crate::mcp::proxy::snapshot_path(
                &daemon.agent_dir("parent"),
                name,
            ))
            .unwrap();
        daemon
            .model()
            .local
            .get_mut("parent")
            .unwrap()
            .toolbox_active
            .as_mut()
            .unwrap()
            .mcp_snapshots
            .insert(name.into(), snapshot);
    }

    fn listing(description: &str) -> serde_json::Value {
        json!({
            "initialize": { "protocolVersion": "2025-06-18", "capabilities": { "tools": {} } },
            "tools": [{ "name": "search", "description": description, "inputSchema": { "type": "object" } }],
        })
    }

    /// T11 / T07: a subagent's catalog is the one its parent had when it was
    /// spawned. The parent pinning another (a restart, a reload) changes
    /// nothing the subagent serves, now or at its own restarts.
    #[tokio::test]
    async fn a_subagent_keeps_the_catalog_its_parent_had_at_spawn() {
        for selection in [None, Some(vec!["web".to_string()])] {
            let (_temp, running) = daemon().await;
            let daemon = &running.daemon;
            let _cleanup = Cleanup(daemon.state_dir.clone());
            let store = crate::agents::toolbox::store::Store::open(&daemon.state_dir).unwrap();
            let active = parent_runs_with(daemon, team(TEAM));
            let server = |name: &str| match &active.resolved.items
                [&crew_core::toolbox::ItemRef::new(ItemKind::Mcp, name)]
                .item
            {
                Item::Mcp(server) => crate::mcp::proxy::definition_hash(server),
                _ => unreachable!(),
            };
            let parent_dir = daemon.agent_dir("parent");
            daemon.agent_root("parent").unwrap();
            daemon.agent_root("child").unwrap();
            crate::mcp::proxy::pin(
                &parent_dir,
                "context7",
                &server("context7"),
                &listing("Old."),
                true,
            )
            .unwrap();
            freeze(daemon, &store, "context7");
            crate::mcp::proxy::pin(
                &parent_dir,
                "manual",
                &server("manual"),
                &listing("Manual."),
                true,
            )
            .unwrap();
            freeze(daemon, &store, "manual");
            // Both the default and selected-bundle paths capture immutable refs
            // before create_agent can await. Replace the parent's live pin in
            // that precise gap, before give_subagent_toolbox.
            let subset = selection.is_some();
            let copy = daemon
                .subagent_toolbox("parent", selection)
                .unwrap()
                .unwrap();
            assert_eq!(copy.mcp_snapshots.len(), if subset { 1 } else { 2 });
            assert_eq!(copy.mcp_snapshots.contains_key("manual"), !subset);
            crate::mcp::proxy::pin(
                &parent_dir,
                "context7",
                &server("context7"),
                &listing("New."),
                true,
            )
            .unwrap();
            freeze(daemon, &store, "context7");
            daemon.give_subagent_toolbox("child", Some(copy)).unwrap();
            let inherited = daemon.model().local["child"]
                .toolbox_inherited
                .clone()
                .unwrap();
            crate::agents::toolbox::restore_inherited_mcp(
                &store,
                &daemon.agent_dir("child"),
                "context7",
                &server("context7"),
                &inherited,
            )
            .unwrap();
            let child_pin =
                crate::mcp::proxy::snapshot_path(&daemon.agent_dir("child"), "context7");
            let frozen = std::fs::read_to_string(&child_pin).unwrap();
            assert!(frozen.contains("Old."), "{frozen}");
            // Restoring one server never copies another server implicitly.
            assert!(
                !crate::mcp::proxy::snapshot_path(&daemon.agent_dir("child"), "manual").exists()
            );

            // A different parent definition cannot break the child's valid pin
            // or change what a later child crash/resume serves.
            crate::mcp::proxy::pin(
                &parent_dir,
                "context7",
                &"f".repeat(64),
                &listing("Different server."),
                true,
            )
            .unwrap();
            let resumed: ActiveToolbox =
                serde_json::from_slice(&serde_json::to_vec(&inherited).unwrap()).unwrap();
            assert_eq!(
                crate::agents::toolbox::restore_inherited_mcp(
                    &store,
                    &daemon.agent_dir("child"),
                    "context7",
                    &server("context7"),
                    &resumed
                )
                .unwrap(),
                crate::mcp::proxy::PinOutcome::Kept
            );
            assert_eq!(std::fs::read_to_string(&child_pin).unwrap(), frozen);
        }
    }

    #[tokio::test]
    async fn an_inherited_snapshot_for_another_definition_is_refused() {
        let (_temp, running) = daemon().await;
        let daemon = &running.daemon;
        let _cleanup = Cleanup(daemon.state_dir.clone());
        let store = crate::agents::toolbox::store::Store::open(&daemon.state_dir).unwrap();
        let active = parent_runs_with(daemon, team(TEAM));
        let expected = match &active.resolved.items
            [&crew_core::toolbox::ItemRef::new(ItemKind::Mcp, "context7")]
            .item
        {
            Item::Mcp(server) => crate::mcp::proxy::definition_hash(server),
            _ => unreachable!(),
        };
        daemon.agent_root("parent").unwrap();
        daemon.agent_root("child").unwrap();
        crate::mcp::proxy::pin(
            &daemon.agent_dir("parent"),
            "context7",
            &"f".repeat(64),
            &listing("Other."),
            true,
        )
        .unwrap();
        freeze(daemon, &store, "context7");
        let copy = daemon.subagent_toolbox("parent", None).unwrap().unwrap();
        let refused = crate::agents::toolbox::restore_inherited_mcp(
            &store,
            &daemon.agent_dir("child"),
            "context7",
            &expected,
            &copy,
        )
        .unwrap_err()
        .to_string();
        assert!(
            refused.contains("does not match context7's definition"),
            "{refused}"
        );
    }
}
