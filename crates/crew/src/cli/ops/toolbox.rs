//! `crew toolbox`, `crew toolbox check` and `crew toolbox reload`: members'
//! toolboxes as every client shows them (notes/toolboxes.md §Display).

use std::path::Path;

use anyhow::{bail, Result};
use crew_core::config::TeamConfig;
use crew_core::rpc::{Call, MemberToolbox, ToolboxReload};
use crew_core::types::{toolbox_origin_label, ToolboxItemKind, TOOLBOX_PENDING, TOOLBOX_UNPINNED};
use crew_core::RuntimeKind;

use super::Target;

/// `crew toolbox [<agent>] [--runtime <kind>] [--json]`.
pub fn toolbox(
    target: &Target,
    agent: Option<String>,
    runtime: Option<RuntimeKind>,
    json: bool,
) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let members: Vec<MemberToolbox> = client.call(Call::Toolbox { agent, runtime })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&members)?);
    } else {
        print!("{}", render(&members));
    }
    Ok(())
}

/// `crew toolbox check <file>`: exits non-zero when a member's toolbox would
/// stop its launch.
pub fn toolbox_check(target: &Target, file: &Path, json: bool) -> Result<()> {
    // Loading validates the file: a bad toolbox fails here, with its path.
    let config = TeamConfig::load(file)?;
    let (mut client, _) = target.connect("cli")?;
    let members: Vec<MemberToolbox> = client.call(Call::ToolboxCheck {
        config: Box::new(config),
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&members)?);
    } else {
        print!("{}", render(&members));
    }
    let stops: Vec<String> = members
        .iter()
        .flat_map(|m| m.stops.iter().map(move |s| format!("{}: {s}", m.agent)))
        .collect();
    if !stops.is_empty() {
        bail!(
            "{} would not start:\n  {}",
            if stops.len() == 1 { "a member" } else { "members" },
            stops.join("\n  ")
        );
    }
    Ok(())
}

/// `crew toolbox reload <file>`.
pub fn toolbox_reload(target: &Target, file: &Path) -> Result<()> {
    let config = TeamConfig::load(file)?;
    let (mut client, _) = target.connect("cli")?;
    let reload: ToolboxReload = client.call(Call::ToolboxReload {
        config: Box::new(config),
    })?;
    if reload.pending.is_empty() {
        println!("No member's toolbox changes.");
    }
    for pending in &reload.pending {
        let from = pending.from.as_deref().map_or("none", short);
        println!(
            "{}: toolbox {from} → {} · {TOOLBOX_PENDING}",
            pending.agent,
            short(&pending.to)
        );
    }
    if !reload.ignored.is_empty() {
        println!(
            "Not applied (only toolboxes are reloaded): {}",
            reload.ignored.join(", ")
        );
    }
    Ok(())
}

fn short(fingerprint: &str) -> &str {
    fingerprint.get(..12).unwrap_or(fingerprint)
}

fn runtime_name(runtime: RuntimeKind) -> &'static str {
    crate::models::runtime_label(runtime)
}

/// Members' toolboxes as text: a summary line each, then the groups.
fn render(members: &[MemberToolbox]) -> String {
    let mut out = String::new();
    for (i, member) in members.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let toolbox = &member.toolbox;
        out.push_str(&format!(
            "{} ({}) · {}\n",
            member.agent,
            runtime_name(member.runtime),
            toolbox.summary()
        ));
        if let Some(attention) = toolbox.attention() {
            out.push_str(&format!("  {attention}\n"));
        }
        if toolbox.definitions_unpinned() {
            out.push_str(&format!("  {TOOLBOX_UNPINNED}\n"));
        }
        for kind in [
            ToolboxItemKind::Mcp,
            ToolboxItemKind::Skills,
            ToolboxItemKind::Plugins,
            ToolboxItemKind::Subagents,
            ToolboxItemKind::Hooks,
            ToolboxItemKind::Unknown,
        ] {
            let items: Vec<_> = toolbox.items.iter().filter(|i| i.kind == kind).collect();
            if items.is_empty() {
                continue;
            }
            out.push_str(&format!("  {}\n", kind.heading()));
            for item in items {
                out.push_str(&format!(
                    "    {} · {} · {}{}\n",
                    item.name,
                    toolbox_origin_label(&item.origin),
                    item.status.text,
                    if item.required { " (required)" } else { "" }
                ));
                if let Some(detail) = &item.detail {
                    out.push_str(&format!("      {detail}\n"));
                }
            }
        }
        if !toolbox.sources.is_empty() {
            out.push_str("  Sources\n");
            for source in &toolbox.sources {
                out.push_str(&format!("    {}: {}", source.label, source.status.text));
                if !source.names.is_empty() {
                    out.push_str(&format!(" ({})", source.names.join(", ")));
                }
                out.push('\n');
                if let Some(detail) = &source.detail {
                    out.push_str(&format!("      {detail}\n"));
                }
            }
        }
        if let Some(digest) = &toolbox.definition_digest {
            let mut line = format!("  Definition digest {}", short(digest));
            if let Some(tools) = toolbox.tool_count {
                line.push_str(&format!(" · {tools} tools"));
            }
            if let Some(bytes) = toolbox.definition_bytes {
                line.push_str(&format!(" · {bytes} bytes"));
            }
            out.push_str(&line);
            out.push('\n');
        }
        for stop in &member.stops {
            out.push_str(&format!("  Would not start: {stop}\n"));
        }
    }
    out
}
