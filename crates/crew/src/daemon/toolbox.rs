//! Active toolbox state. Configuration reloads never mutate a running
//! member's definitions; the next launch applies the new configuration.

use crew_core::rpc::RpcError;
use crew_core::toolbox::{Item, ItemKind, ItemRef, McpServer};
use crew_core::{ToolboxItemCode, ToolboxItemKind, ToolboxItemStatus};

use super::Daemon;
use crate::agents::toolbox::ActiveToolbox;
use crate::persistence::NewEvent;

impl Daemon {
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
