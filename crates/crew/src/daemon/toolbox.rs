//! Active toolbox state. Configuration reloads never mutate a running
//! member's definitions; the next launch applies the new configuration.

use crew_core::config::TeamConfig;
use crew_core::rpc::{MemberToolbox, PendingToolbox, RpcError, ToolboxReload};
use crew_core::toolbox::{Item, ItemKind, ItemRef, McpServer, Resolved};
use crew_core::{RuntimeKind, ToolboxItemCode, ToolboxItemKind, ToolboxItemStatus};
use serde_json::Value;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::time::Duration;

use super::Daemon;
use crate::agents::toolbox::prepare::{self, Context};
use crate::agents::toolbox::ActiveToolbox;
use crate::persistence::NewEvent;

impl Daemon {
    pub(crate) async fn prepare_toolbox_launch(
        &self,
        agent: &crew_core::Agent,
        resolved: Resolved,
        inherited: Option<ActiveToolbox>,
        endpoint: &crate::endpoints::Endpoint,
        refresh: bool,
        since: u64,
    ) -> anyhow::Result<crate::agents::toolbox::RuntimeToolbox> {
        let context = self.toolbox_context(&agent.id, agent.runtime, false, endpoint);
        let previous = self.active_toolbox(&agent.id);
        let workspace = self.workspace.clone();
        let (id, parent, runtime) = (agent.id.clone(), agent.parent.clone(), agent.runtime);
        let inherited_for_plan = inherited.clone();
        let mut plan = tokio::task::spawn_blocking(move || {
            let layers = crate::endpoints::Layers::load(&workspace);
            prepare::prepare(&context, resolved, inherited_for_plan.as_ref(), |server| {
                layers.toolbox_references(runtime, &id, parent.as_deref(), server)
            })
        })
        .await??;
        if self.stops(&agent.id) != since {
            anyhow::bail!("toolbox preparation was superseded by a stop");
        }
        plan.active.refresh = refresh;
        plan.active.snapshot_owner = Some(agent.id.clone());
        {
            let mut model = self.model();
            model.rt(&agent.id).toolbox_references = plan
                .references
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            let local = model.local.entry(agent.id.clone()).or_default();
            local.settings.toolbox = plan.active.resolved.clone();
            local.toolbox_active = Some(plan.active.clone());
        }
        self.save_local(&agent.id);
        self.update_agent(&agent.id, |member| member.toolbox = Some(plan.view.clone()));
        if !plan.stops.is_empty() {
            anyhow::bail!("toolbox prevents launch: {}", plan.stops.join("; "));
        }
        for name in plan.runtime.servers.clone() {
            if self.stops(&agent.id) != since {
                anyhow::bail!("toolbox preflight was superseded by a stop");
            }
            let server = match &plan.active.resolved.items[&ItemRef::new(ItemKind::Mcp, &name)].item
            {
                Item::Mcp(server) => server.clone(),
                _ => continue,
            };
            let definition = crate::mcp::proxy::definition_hash(&server);
            if let Some(owner) = inherited
                .as_ref()
                .and_then(|active| active.snapshot_owner.as_deref())
            {
                let source = self.agent_root(owner)?;
                let relative = PathBuf::from("toolbox/mcp").join(format!("{name}.json"));
                let file = crew_core::paths::open_beneath(source.as_fd(), &relative)?;
                let snapshot: Value =
                    serde_json::from_reader(std::io::Read::take(file, 16 * 1024 * 1024))?;
                if snapshot.get("definition").and_then(Value::as_str) != Some(definition.as_str()) {
                    anyhow::bail!("inherited MCP snapshot does not match {name}'s definition");
                }
                crate::mcp::proxy::pin(
                    &self.agent_dir(&agent.id),
                    &name,
                    &definition,
                    &snapshot,
                    true,
                )?;
            }
            match self
                .toolbox_preflight(agent, &name, &plan.runtime.scrub_env, since)
                .await
            {
                Ok(listing) => {
                    let pinned = crate::mcp::proxy::pin(
                        &self.agent_dir(&agent.id),
                        &name,
                        &definition,
                        &listing,
                        refresh && inherited.is_none(),
                    )?;
                    if pinned.outcome == crate::mcp::proxy::PinOutcome::KeptDiffers {
                        self.emit(NewEvent::new("daemon", "toolbox.listing", format!("{name}: upstream definitions differ; keeping the pinned snapshot")).agent(&agent.id));
                    }
                    if let Some(item) = plan
                        .view
                        .items
                        .iter_mut()
                        .find(|item| item.kind == ToolboxItemKind::Mcp && item.name == name)
                    {
                        item.status = prepare::status(ToolboxItemCode::Active, "Active");
                    }
                }
                Err(error) => {
                    let reason = format!("{error:#}");
                    let blocked = self.sandboxed()
                        && (reason.contains("cannot be sandboxed")
                            || reason.contains("No such file")
                            || reason.contains("Permission denied"));
                    plan.fail_server(&name, reason, blocked);
                }
            }
        }
        if self.stops(&agent.id) != since {
            anyhow::bail!("toolbox preflight was superseded by a stop");
        }
        if let Some(digest) =
            crate::mcp::proxy::definition_digest(&self.agent_dir(&agent.id), &plan.runtime.servers)
        {
            plan.active.definition_digest = Some(digest.sha256.clone());
            plan.view.definition_digest = Some(digest.sha256);
            plan.view.tool_count = Some(digest.tools);
            plan.view.definition_bytes = Some(digest.bytes);
        }
        {
            let mut model = self.model();
            model
                .local
                .entry(agent.id.clone())
                .or_default()
                .toolbox_active = Some(plan.active.clone());
        }
        self.save_local(&agent.id);
        self.update_agent(&agent.id, |member| member.toolbox = Some(plan.view.clone()));
        if !plan.stops.is_empty() {
            anyhow::bail!("toolbox prevents launch: {}", plan.stops.join("; "));
        }
        let changed = previous.as_ref().is_some_and(|previous| {
            previous.fingerprint != plan.active.fingerprint
                || previous.definition_digest != plan.active.definition_digest
        });
        if changed {
            self.emit(NewEvent::new("daemon", "toolbox.changed", format!("{}: applying changed toolbox", agent.id)).agent(&agent.id).data(serde_json::json!({"from":previous.as_ref().and_then(|active| active.fingerprint.as_ref()),"to":plan.view.fingerprint,"definition_digest":plan.view.definition_digest})));
        }
        self.emit(
            NewEvent::new(
                "daemon",
                "toolbox.applied",
                format!(
                    "{}: toolbox {} applied",
                    agent.id,
                    plan.view.short_fingerprint()
                ),
            )
            .agent(&agent.id)
            .data(serde_json::to_value(&plan.view)?),
        );
        Ok(plan.runtime)
    }

    async fn toolbox_preflight(
        &self,
        agent: &crew_core::Agent,
        name: &str,
        scrub: &[String],
        since: u64,
    ) -> anyhow::Result<Value> {
        use std::os::unix::process::CommandExt;
        use tokio::io::AsyncReadExt;
        let program = match agent.runtime {
            RuntimeKind::Claude => "claude".to_string(),
            RuntimeKind::Codex => "codex".to_string(),
            RuntimeKind::Pi => crate::agents::pi::program()
                .map_err(anyhow::Error::msg)?
                .to_string_lossy()
                .into_owned(),
            RuntimeKind::Shell => self.crew_bin.to_string_lossy().into_owned(),
        };
        let env = vec![(
            crew_core::paths::IDENTITY_ENV.to_string(),
            self.agent_dir(&agent.id)
                .join("identity.json")
                .to_string_lossy()
                .into_owned(),
        )];
        let run = super::sandbox::Run {
            agent_id: agent.id.clone(),
            program,
            env: env.clone(),
            sidecar_dir: None,
            terminal: None,
        };
        let sandbox = self
            .agent_sandbox(agent, run)
            .await?
            .map(|sandbox| sandbox.spec);
        let record = sandbox
            .as_ref()
            .map(|_| self.ledger.start(&agent.id, "toolbox-preflight"))
            .transpose()?;
        let sandbox = sandbox.map(|mut sandbox| {
            if let Some(record) = &record {
                sandbox.done = Some(record.done().to_path_buf());
            }
            sandbox
        });
        let argv = vec![
            self.crew_bin.to_string_lossy().into_owned(),
            "toolbox".into(),
            "mcp".into(),
            name.into(),
            "--identity".into(),
            self.agent_dir(&agent.id)
                .join("identity.json")
                .to_string_lossy()
                .into_owned(),
            "--preflight".into(),
        ];
        let mut command = super::sandbox::command(sandbox.as_ref(), &argv, &env)?;
        for name in scrub {
            command.env_remove(name);
        }
        command
            .current_dir(&agent.workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if sandbox.is_none() {
            command.as_std_mut().process_group(0);
        }
        let spawned = command.spawn();
        if let (Err(_), Some(record)) = (&spawned, &record) {
            self.ledger.never_started(record);
        }
        let mut child = spawned?;
        let pid = child.id();
        if let (Some(record), Some(pid)) = (&record, pid) {
            record.update(|entry| entry.pid = Some(pid));
            if let Some(process) = super::retire::Proc::of(pid) {
                let init = super::retire::discover(process, super::retire::DISCOVER_WITHIN).await?;
                record.update(|entry| entry.init = init);
            }
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("preflight stdout is missing"))?;
        let reader = tokio::spawn(async move {
            let mut bytes = Vec::new();
            stdout
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .await
                .map(|_| bytes)
        });
        let wait = async {
            loop {
                if self.stops(&agent.id) != since {
                    anyhow::bail!("preflight stopped");
                }
                if let Some(status) = child.try_wait()? {
                    return Ok(status);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        let status = tokio::time::timeout(
            crate::mcp::proxy::PREFLIGHT_WITHIN + Duration::from_secs(1),
            wait,
        )
        .await;
        let success = matches!(&status, Ok(Ok(_)));
        if !success {
            if sandbox.is_none() {
                if let Some(pid) = pid {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
            }
            let _ = child.kill().await;
            if sandbox.is_some() {
                self.ledger.end_orphans(&agent.id);
            }
        }
        if sandbox.is_some() {
            self.ledger
                .retire(&agent.id, Duration::from_secs(10))
                .await?;
        }
        let status = status
            .map_err(|_| anyhow::anyhow!("MCP preflight did not finish within 30 seconds"))??;
        let bytes = tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .map_err(|_| anyhow::anyhow!("MCP preflight output did not close"))???;
        if bytes.len() > 16 * 1024 * 1024 {
            anyhow::bail!("MCP preflight response exceeds 16 MiB");
        }
        let result: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("MCP preflight returned no valid result"))?;
        if !status.success() || result.get("ok") != Some(&Value::Bool(true)) {
            anyhow::bail!(
                "{}",
                result
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("MCP preflight failed")
            );
        }
        result
            .get("listing")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP preflight returned no listing"))
    }

    /// Proxies use values captured for this launch. After daemon recovery,
    /// re-read credentials only when their source names are still covered by
    /// the running member's recorded environment scrub set.
    pub(crate) fn toolbox_references(
        &self,
        agent: &str,
        name: &str,
    ) -> Result<crate::endpoints::ToolboxReferences, RpcError> {
        if let Some(references) = self.model().rt(agent).toolbox_references.get(name).cloned() {
            return Ok(references);
        }
        let member = self
            .agent(agent)
            .ok_or_else(|| RpcError::not_found("no such member"))?;
        let (server, _) = self.toolbox_item(agent, name)?;
        let references = crate::endpoints::Layers::load(&self.workspace)
            .toolbox_references(member.runtime, agent, member.parent.as_deref(), &server)
            .map_err(|error| RpcError::invalid(format!("{error:#}")))?;
        let active = self
            .active_toolbox(agent)
            .ok_or_else(|| RpcError::unavailable("the member has no active toolbox"))?;
        if !references.scrub.is_subset(&active.scrub_env) {
            return Err(RpcError::unavailable(
                "toolbox credential sources changed; restart the member to apply them",
            ));
        }
        self.model()
            .rt(agent)
            .toolbox_references
            .insert(name.to_string(), references.clone());
        Ok(references)
    }

    pub(crate) fn toolbox_runtime_check(
        &self,
        agent: &crew_core::Agent,
        runtime: RuntimeKind,
    ) -> Result<(), RpcError> {
        if runtime == agent.runtime {
            return Ok(());
        }
        let resolved = {
            let model = self.model();
            let local = model
                .local
                .get(&agent.id)
                .ok_or_else(|| RpcError::not_found("member settings are missing"))?;
            if let Some(inherited) = &local.toolbox_inherited {
                inherited.resolved.clone()
            } else if let Some(team) = &model.session.team {
                team.resolve_toolbox(
                    &local.spec.toolbox,
                    &format!("agent '{}'.toolbox", agent.id),
                )
                .map_err(|error| RpcError::invalid(format!("{error:#}")))?
            } else {
                return Ok(());
            }
        };
        let preview =
            self.toolbox_preview(&agent.id, agent.parent.as_deref(), runtime, &resolved)?;
        if preview.stops.is_empty() {
            Ok(())
        } else {
            Err(RpcError::invalid(preview.stops.join("; ")))
        }
    }

    pub(crate) fn toolbox_context(
        &self,
        id: &str,
        runtime: RuntimeKind,
        preview: bool,
        endpoint: &crate::endpoints::Endpoint,
    ) -> Context {
        let model = self.model();
        let settings = model.local.get(id).map(|local| &local.settings);
        let defaults = model.session.team.as_ref().map(|team| &team.defaults);
        let workdir = model
            .agent(id)
            .map(|agent| PathBuf::from(&agent.workdir))
            .unwrap_or_else(|| self.workspace.clone());
        Context {
            runtime,
            state_dir: self.state_dir.clone(),
            agent_dir: self.agent_dir(id),
            workdir,
            private_home: model
                .session
                .team
                .as_ref()
                .is_some_and(|team| {
                    team.workspace.sandbox.mode == crew_core::config::SandboxMode::Required
                })
                .then(|| self.agent_dir(id).join("rw/home")),
            operator: crate::agents::home::Operator::detect(endpoint),
            operator_home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/")),
            socket_delivery: settings
                .map(|settings| settings.socket_delivery)
                .unwrap_or_else(|| {
                    defaults.is_none_or(|defaults| {
                        defaults.delivery != crew_core::config::DeliveryChoice::Terminal
                    })
                }),
            auto_trust: settings
                .map(|settings| settings.auto_trust)
                .unwrap_or_else(|| defaults.is_some_and(|defaults| defaults.auto_trust)),
            preview,
        }
    }

    pub(crate) fn toolbox_preview(
        &self,
        id: &str,
        parent: Option<&str>,
        runtime: RuntimeKind,
        resolved: &Resolved,
    ) -> Result<MemberToolbox, RpcError> {
        let layers = crate::endpoints::Layers::load(&self.workspace);
        let endpoint = layers
            .resolve(runtime, id, parent)
            .map_err(|error| RpcError::invalid(format!("{error:#}")))?;
        let context = self.toolbox_context(id, runtime, true, &endpoint);
        let inherited = self
            .model()
            .local
            .get(id)
            .and_then(|local| local.toolbox_inherited.clone());
        let plan = prepare::prepare(&context, resolved.clone(), inherited.as_ref(), |server| {
            layers.toolbox_references(runtime, id, parent, server)
        })
        .map_err(|error| RpcError::invalid(format!("{error:#}")))?;
        Ok(MemberToolbox {
            agent: id.into(),
            runtime,
            toolbox: plan.view,
            stops: plan.stops,
        })
    }

    /// Only toolbox fields are accepted from the supplied team file. Prepare
    /// every member first, then swap the daemon's configuration atomically.
    pub(crate) fn toolbox_reload(&self, incoming: &TeamConfig) -> Result<ToolboxReload, RpcError> {
        incoming
            .validate()
            .map_err(|error| RpcError::invalid(format!("{error:#}")))?;
        let (before, agents, locals) = {
            let model = self.model();
            (
                model
                    .session
                    .team
                    .clone()
                    .ok_or_else(|| RpcError::unavailable("no team is loaded"))?,
                model.agents.clone(),
                model.local.clone(),
            )
        };
        let mut next = before.clone();
        next.toolboxes = incoming.toolboxes.clone();
        next.defaults.toolbox = incoming.defaults.toolbox.clone();
        next.defaults.inherit_mcp = incoming.defaults.inherit_mcp;
        next.orchestrator.toolbox = incoming.orchestrator.toolbox.clone();
        for spec in &mut next.agents {
            if let Some(replacement) = incoming.agents.iter().find(|other| other.id == spec.id) {
                spec.toolbox = replacement.toolbox.clone();
            }
        }
        next.validate()
            .map_err(|error| RpcError::invalid(format!("{error:#}")))?;
        let mut prepared = Vec::new();
        for agent in &agents {
            let Some(local) = locals.get(&agent.id) else {
                continue;
            };
            let mut spec = local.spec.clone();
            if agent.orchestrator {
                spec.toolbox = next.orchestrator.toolbox.clone();
            } else if let Some(replacement) =
                incoming.agents.iter().find(|spec| spec.id == agent.id)
            {
                spec.toolbox = replacement.toolbox.clone();
            }
            let resolved = match &local.toolbox_inherited {
                Some(inherited) => inherited.resolved.clone(),
                None => next
                    .resolve_toolbox(&spec.toolbox, &format!("agent '{}'.toolbox", agent.id))
                    .map_err(|error| RpcError::invalid(format!("{error:#}")))?,
            };
            let preview =
                self.toolbox_preview(&agent.id, agent.parent.as_deref(), agent.runtime, &resolved)?;
            prepared.push((agent.id.clone(), spec, resolved, preview.toolbox));
        }
        let mut comparison = incoming.clone();
        comparison.toolboxes = before.toolboxes.clone();
        comparison.defaults.toolbox = before.defaults.toolbox.clone();
        comparison.defaults.inherit_mcp = before.defaults.inherit_mcp;
        comparison.orchestrator.toolbox = before.orchestrator.toolbox.clone();
        for spec in &mut comparison.agents {
            if let Some(old) = before.agents.iter().find(|old| old.id == spec.id) {
                spec.toolbox = old.toolbox.clone();
            }
        }
        let old = serde_json::to_value(&before).map_err(super::handlers::internal)?;
        let compared = serde_json::to_value(&comparison).map_err(super::handlers::internal)?;
        let ignored = old
            .as_object()
            .into_iter()
            .flat_map(|map| map.keys())
            .filter(|key| old.get(*key) != compared.get(*key))
            .cloned()
            .collect();
        {
            let mut model = self.model();
            if model.session.team.as_ref() != Some(&before) {
                return Err(RpcError::conflict(
                    "the team changed while preparing the reload; try again",
                ));
            }
            model.session.team = Some(next);
            for (id, spec, resolved, _) in &prepared {
                if let Some(local) = model.local.get_mut(id) {
                    local.spec.toolbox = spec.toolbox.clone();
                    local.settings.toolbox = resolved.clone();
                }
            }
        }
        self.save_session();
        let mut pending = Vec::new();
        for (id, _, _, preview) in prepared {
            self.save_local(&id);
            let from = self.active_toolbox(&id).and_then(|active| {
                active
                    .fingerprint
                    .map(|fingerprint| fingerprint.as_str().to_string())
            });
            let changed = from.as_deref() != Some(preview.fingerprint.as_str());
            self.update_agent(&id, |agent| {
                if let Some(active) = agent.toolbox.as_mut() {
                    active.pending_fingerprint = changed.then(|| preview.fingerprint.clone());
                } else {
                    agent.toolbox = Some(preview.clone());
                }
            });
            if changed {
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "toolbox.pending",
                        format!("{id}: toolbox changed; restart to apply"),
                    )
                    .agent(&id)
                    .data(serde_json::json!({"from":from,"to":preview.fingerprint})),
                );
                pending.push(PendingToolbox {
                    agent: id,
                    from,
                    to: preview.fingerprint,
                });
            }
        }
        self.persist_roster();
        Ok(ToolboxReload { pending, ignored })
    }

    pub(crate) fn active_toolbox(&self, agent: &str) -> Option<ActiveToolbox> {
        self.model()
            .local
            .get(agent)
            .and_then(|local| local.toolbox_active.clone())
    }

    /// Called after creating a subagent and before starting its process.
    /// The parent's pinned files and definitions are copied, not re-resolved
    /// against bundles that may have changed during the parent's turn.
    pub(crate) fn inherit_toolbox(
        &self,
        agent: &str,
        active: ActiveToolbox,
    ) -> Result<(), RpcError> {
        {
            let mut model = self.model();
            let local = model
                .local
                .get_mut(agent)
                .ok_or_else(|| RpcError::not_found(format!("no agent '{agent}'")))?;
            local.settings.toolbox = active.resolved.clone();
            local.toolbox_inherited = Some(active);
        }
        self.save_local(agent);
        Ok(())
    }

    pub(crate) fn toolbox_item(
        &self,
        agent: &str,
        name: &str,
    ) -> Result<(McpServer, bool), RpcError> {
        let active = self
            .active_toolbox(agent)
            .ok_or_else(|| RpcError::unavailable("the member has no active toolbox"))?;
        if !active.servers.contains(name) {
            return Err(RpcError::forbidden(format!(
                "MCP server '{name}' is not available to this member"
            )));
        }
        match active
            .resolved
            .items
            .get(&ItemRef::new(ItemKind::Mcp, name))
            .map(|entry| &entry.item)
        {
            Some(Item::Mcp(server)) => Ok((server.clone(), active.refresh)),
            _ => Err(RpcError::not_found(format!("no MCP server '{name}'"))),
        }
    }

    pub(crate) fn toolbox_failed_in_session(&self, agent: &str, name: &str, reason: &str) {
        self.update_agent(agent, |member| {
            if let Some(item) = member.toolbox.as_mut().and_then(|toolbox| {
                toolbox
                    .items
                    .iter_mut()
                    .find(|item| item.kind == ToolboxItemKind::Mcp && item.name == name)
            }) {
                item.status = ToolboxItemStatus {
                    code: ToolboxItemCode::FailedInSession,
                    text: "Failed in session".to_string(),
                };
                item.detail = Some(reason.to_string());
            }
        });
        self.emit(
            NewEvent::new(
                "daemon",
                "toolbox.failed",
                format!("MCP server {name} failed in session: {reason}"),
            )
            .agent(agent)
            .warn(),
        );
    }
}
