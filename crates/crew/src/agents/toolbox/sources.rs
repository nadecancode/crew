//! Runtime source discovery and the controls that are known to exclude it.
//! Exclusion failures stay visible; strict launch policy is applied by the caller.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use crew_core::toolbox::{Resolved, SourceKind};
use crew_core::{
    RuntimeKind, ToolboxSource, ToolboxSourceCode as Code, ToolboxSourceOrigin as Origin,
    ToolboxSourceStatus,
};
use serde_json::{json, Value};

use super::prepare::Context;
use super::RuntimeToolbox;

const GROUP: &[SourceKind] = &[
    SourceKind::Skills,
    SourceKind::Plugins,
    SourceKind::Subagents,
    SourceKind::Hooks,
    SourceKind::Settings,
    SourceKind::Memory,
];
const MAX_CONFIG: u64 = 1024 * 1024;

pub(super) fn prepare(
    context: &Context,
    resolved: &Resolved,
    launch: &mut RuntimeToolbox,
) -> Result<Vec<ToolboxSource>> {
    if context.runtime == RuntimeKind::Shell {
        return Ok(Vec::new());
    }
    let mut sources = Vec::new();
    match context.runtime {
        RuntimeKind::Claude => claude(context, resolved, launch, &mut sources)?,
        RuntimeKind::Codex => codex(context, resolved, launch, &mut sources)?,
        RuntimeKind::Pi => pi(context, resolved, launch, &mut sources)?,
        RuntimeKind::Shell => {}
    }
    Ok(sources)
}

fn row(
    origin: Origin,
    kind: SourceKind,
    runtime: RuntimeKind,
    code: Code,
    mut names: Vec<String>,
    detail: Option<String>,
    earlier: bool,
) -> ToolboxSource {
    names.sort();
    names.dedup();
    let source = match origin {
        Origin::Operator if earlier => "Earlier sessions",
        Origin::Operator => "Your",
        Origin::Project => "Project",
        Origin::System => "System",
        Origin::Managed => "Managed",
        Origin::Unknown => "Other",
    };
    let text = match code {
        Code::NotUsed => "Not used".into(),
        Code::Used => format!("Used: {}", kind.as_str()),
        Code::Disabled => format!("Disabled: {}", names.join(", ")),
        Code::NotAvailableSeparately => format!(
            "Not available separately on {}",
            crate::models::runtime_label(runtime)
        ),
        Code::CannotBeExcluded => format!(
            "Cannot be excluded on {}",
            crate::models::runtime_label(runtime)
        ),
        Code::Unknown => "Unknown".into(),
    };
    ToolboxSource {
        origin,
        kind: Some(kind.as_str().into()),
        label: format!(
            "{source} {} {}",
            crate::models::runtime_label(runtime),
            kind.as_str()
        ),
        status: ToolboxSourceStatus { code, text },
        names,
        detail,
    }
}

fn claude(
    context: &Context,
    resolved: &Resolved,
    launch: &mut RuntimeToolbox,
    sources: &mut Vec<ToolboxSource>,
) -> Result<()> {
    let directory = context
        .private_home
        .as_ref()
        .map(|home| home.join("claude"))
        .unwrap_or_else(|| context.operator.claude.clone());
    let settings = read_json(&directory.join("settings.json"))?;
    let mut enabled = Vec::new();
    let operator_all = GROUP
        .iter()
        .all(|kind| resolved.sources.operator.contains(kind));
    let project_all = GROUP
        .iter()
        .all(|kind| resolved.sources.project.contains(kind));
    if operator_all {
        enabled.extend(["user", "local"]);
    }
    if project_all {
        enabled.push("project");
    }
    launch.claude_sources = enabled.join(",");
    launch.claude_preferences = crate::agents::home::claude_preferences(&context.operator);
    for (origin, root, allowed, all) in [
        (
            Origin::Operator,
            directory.clone(),
            &resolved.sources.operator,
            operator_all,
        ),
        (
            Origin::Project,
            context.workdir.join(".claude"),
            &resolved.sources.project,
            project_all,
        ),
    ] {
        let config = read_json(&root.join("settings.json"))?;
        for kind in GROUP {
            let names = match kind {
                SourceKind::Skills => skill_paths(&[root.join("skills")])?
                    .into_iter()
                    .map(|path| path.display().to_string())
                    .collect(),
                SourceKind::Subagents => files_in(&root.join("agents"), "md")?,
                SourceKind::Plugins => object_keys(config.get("enabledPlugins")),
                SourceKind::Hooks => object_keys(config.get("hooks")),
                SourceKind::Memory => existing(&[root.join("CLAUDE.md"), root.join("AGENTS.md")]),
                SourceKind::Settings => {
                    if config.as_object().is_some_and(|map| !map.is_empty()) {
                        vec![root.join("settings.json").display().to_string()]
                    } else {
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            };
            let code = if all {
                Code::Used
            } else if allowed.contains(kind) {
                Code::NotAvailableSeparately
            } else {
                Code::NotUsed
            };
            sources.push(row(origin, *kind, context.runtime, code, names, (code == Code::NotAvailableSeparately).then(|| "Claude Code loads these settings-source kinds together; use all to inherit them together".into()), context.private_home.is_some() && origin == Origin::Operator));
        }
    }
    let mut operator_mcp = Vec::new();
    for path in [
        directory.join(".claude.json"),
        directory
            .parent()
            .unwrap_or(&directory)
            .join(".claude.json"),
    ] {
        let state = read_json(&path)?;
        operator_mcp.extend(object_keys(state.get("mcpServers")));
        if let Some(project) = state
            .get("projects")
            .and_then(|projects| projects.get(context.workdir.to_string_lossy().as_ref()))
        {
            operator_mcp.extend(object_keys(project.get("mcpServers")));
        }
    }
    let inherit_mcp = resolved.sources.operator.contains(&SourceKind::Mcp);
    launch.claude_inherit_mcp = inherit_mcp && operator_all;
    sources.push(row(
        Origin::Operator,
        SourceKind::Mcp,
        context.runtime,
        if launch.claude_inherit_mcp {
            Code::Used
        } else if inherit_mcp {
            Code::NotAvailableSeparately
        } else {
            Code::NotUsed
        },
        operator_mcp,
        if launch.claude_inherit_mcp { Some("Inherited servers are outside the cache rules".into()) } else if inherit_mcp { Some("User MCP servers need the user settings source; use all to inherit its other kinds together".into()) } else { None },
        context.private_home.is_some(),
    ));
    let project_mcp = object_keys(read_json(&context.workdir.join(".mcp.json"))?.get("mcpServers"));
    let project_code = if launch.claude_inherit_mcp && project_all && !project_mcp.is_empty() {
        Code::CannotBeExcluded
    } else {
        Code::NotUsed
    };
    sources.push(row(
        Origin::Project,
        SourceKind::Mcp,
        context.runtime,
        project_code,
        project_mcp,
        (project_code == Code::CannotBeExcluded)
            .then(|| "Disabling strict MCP loading also permits project MCP configuration".into()),
        false,
    ));
    if inherit_mcp && !operator_all && !object_keys(settings.get("enabledPlugins")).is_empty() {
        sources.push(row(
            Origin::Operator,
            SourceKind::Mcp,
            context.runtime,
            Code::NotAvailableSeparately,
            object_keys(settings.get("enabledPlugins")),
            Some("MCP servers bundled with excluded user plugins are not loaded".into()),
            false,
        ));
    }
    Ok(())
}

fn codex(
    context: &Context,
    resolved: &Resolved,
    launch: &mut RuntimeToolbox,
    sources: &mut Vec<ToolboxSource>,
) -> Result<()> {
    let directory = context
        .private_home
        .as_ref()
        .map(|home| home.join("codex"))
        .unwrap_or_else(|| context.operator.codex.clone());
    let home = context
        .private_home
        .clone()
        .unwrap_or_else(|| context.operator_home.clone());
    let settings = read_toml(&directory.join("config.toml"))?;
    // CLI array overrides replace arrays. Preserve existing native exclusions
    // while adding ours, including project-layer entries.
    let mut disabled = settings
        .pointer("/skills/config")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut project_servers = BTreeSet::new();
    for root in project_roots(&context.workdir) {
        let project = read_toml(&root.join(".codex/config.toml"))?;
        if let Some(entries) = project.pointer("/skills/config").and_then(Value::as_array) {
            disabled.extend(entries.iter().cloned());
        }
        project_servers.extend(object_keys(project.get("mcp_servers")));
    }
    launch.codex_project_servers = project_servers.clone();
    let operator_servers = object_keys(settings.get("mcp_servers"));
    if resolved.sources.operator.contains(&SourceKind::Mcp) {
        launch.codex_inherit_servers.extend(
            operator_servers
                .iter()
                .filter(|name| !project_servers.contains(*name))
                .cloned(),
        );
    }
    sources.push(row(
        Origin::Operator,
        SourceKind::Mcp,
        context.runtime,
        if resolved.sources.operator.contains(&SourceKind::Mcp) {
            Code::Used
        } else if operator_servers.is_empty() {
            Code::NotUsed
        } else {
            Code::Disabled
        },
        operator_servers,
        resolved
            .sources
            .operator
            .contains(&SourceKind::Mcp)
            .then(|| "Inherited servers are outside the cache rules".into()),
        context.private_home.is_some(),
    ));
    let project_mcp_detail = (!project_servers.is_empty()).then(|| {
        "Codex can load project MCP servers after its trust prompt, after crew's launch inventory; their exclusion is not guaranteed".into()
    });
    sources.push(row(
        Origin::Project,
        SourceKind::Mcp,
        context.runtime,
        if project_servers.is_empty() {
            Code::NotUsed
        } else {
            Code::CannotBeExcluded
        },
        project_servers.into_iter().collect(),
        project_mcp_detail,
        false,
    ));
    for (origin, roots, allowed) in [
        (
            Origin::Operator,
            vec![home.join(".agents/skills"), directory.join("skills")],
            &resolved.sources.operator,
        ),
        (
            Origin::Project,
            project_roots(&context.workdir)
                .into_iter()
                .map(|root| root.join(".agents/skills"))
                .collect(),
            &resolved.sources.project,
        ),
    ] {
        let mut skills = skill_paths(&roots)?;
        // The terminal-delivery directory is replaced by crew, not inherited.
        if context.private_home.is_some() {
            skills.retain(|path| !path.starts_with(home.join(".agents/skills")));
        }
        let names = skills
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        if !allowed.contains(&SourceKind::Skills) {
            disabled.extend(
                skills
                    .into_iter()
                    .map(|path| json!({"path":path,"enabled":false})),
            );
        }
        sources.push(row(
            origin,
            SourceKind::Skills,
            context.runtime,
            if allowed.contains(&SourceKind::Skills) {
                Code::Used
            } else if names.is_empty() {
                Code::NotUsed
            } else {
                Code::Disabled
            },
            names,
            None,
            context.private_home.is_some() && origin == Origin::Operator,
        ));
    }
    if !disabled.is_empty() {
        // Path order is stable; for duplicate entries an exclusion wins.
        let mut entries = std::collections::BTreeMap::new();
        for entry in disabled {
            if let Some(path) = entry.get("path").and_then(Value::as_str) {
                let previous = entries
                    .entry(path.to_string())
                    .or_insert_with(|| entry.clone());
                if entry.get("enabled") == Some(&Value::Bool(false)) {
                    *previous = entry;
                }
            }
        }
        launch.codex_overrides.push((
            "skills.config".into(),
            Value::Array(entries.into_values().collect()),
        ));
    }
    for (kind, feature, relative) in [
        (SourceKind::Hooks, "features.hooks", "hooks.json"),
        (SourceKind::Subagents, "features.multi_agent", "agents"),
        (SourceKind::Plugins, "features.plugins", "plugins"),
    ] {
        let allow_operator = resolved.sources.operator.contains(&kind);
        let allow_project = resolved.sources.project.contains(&kind);
        let selected_roles = kind == SourceKind::Subagents
            && context.private_home.is_some()
            && resolved
                .items
                .keys()
                .any(|key| key.kind == crew_core::toolbox::ItemKind::Subagents);
        let enabled = allow_operator
            || selected_roles
            || (context.private_home.is_some() && allow_project && kind != SourceKind::Plugins);
        if !enabled {
            launch.codex_overrides.push((feature.into(), json!(false)));
        } else if selected_roles {
            launch.codex_overrides.push((feature.into(), json!(true)));
        }
        let operator_names = match kind {
            SourceKind::Subagents => {
                if context.private_home.is_some() {
                    Vec::new()
                } else {
                    codex_roles(&directory, &settings)?
                }
            }
            SourceKind::Plugins => object_keys(settings.get("plugins")),
            _ => {
                let mut names = existing(&[directory.join(relative)]);
                names.extend(object_keys(settings.get("hooks")));
                names
            }
        };
        let operator_code = if allow_operator {
            Code::Used
        } else if (enabled || kind == SourceKind::Subagents) && !operator_names.is_empty() {
            Code::CannotBeExcluded
        } else {
            Code::NotUsed
        };
        sources.push(row(
            Origin::Operator,
            kind,
            context.runtime,
            operator_code,
            operator_names,
            (kind == SourceKind::Subagents && operator_code == Code::CannotBeExcluded).then(|| {
                "features.multi_agent=false does not exclude roles from Codex code-mode children".into()
            }),
            context.private_home.is_some(),
        ));
        let mut project_names = Vec::new();
        for root in project_roots(&context.workdir) {
            let directory = root.join(".codex");
            if kind == SourceKind::Subagents {
                project_names.extend(codex_roles(
                    &directory,
                    &read_toml(&directory.join("config.toml"))?,
                )?);
            } else {
                project_names.extend(existing(&[directory.join(relative)]));
            }
        }
        let project_code = if allow_project && (enabled || kind == SourceKind::Subagents) {
            Code::Used
        } else if !allow_project
            && (enabled || kind == SourceKind::Subagents)
            && !project_names.is_empty()
        {
            Code::CannotBeExcluded
        } else if allow_project && !enabled {
            Code::NotAvailableSeparately
        } else {
            Code::NotUsed
        };
        sources.push(row(
            Origin::Project,
            kind,
            context.runtime,
            project_code,
            project_names,
            if kind == SourceKind::Subagents && project_code == Code::CannotBeExcluded {
                Some("features.multi_agent=false does not exclude roles from Codex code-mode children".into())
            } else {
                (project_code == Code::NotAvailableSeparately).then(|| {
                    "The runtime switch applies to user and project sources together".into()
                })
            },
            false,
        ));
    }
    let memory = existing(&[
        directory.join("AGENTS.md"),
        directory.join("AGENTS.override.md"),
    ]);
    let memory_code = if resolved.sources.operator.contains(&SourceKind::Memory) {
        Code::Used
    } else if memory.is_empty() {
        Code::NotUsed
    } else {
        Code::CannotBeExcluded
    };
    sources.push(row(
        Origin::Operator,
        SourceKind::Memory,
        context.runtime,
        memory_code,
        memory,
        (memory_code == Code::CannotBeExcluded)
            .then(|| "No verified independent exclusion of native user instruction files".into()),
        context.private_home.is_some(),
    ));
    let project_memory = resolved.sources.project.contains(&SourceKind::Memory);
    if !project_memory {
        launch
            .codex_overrides
            .push(("project_doc_max_bytes".into(), json!(0)));
    }
    sources.push(row(
        Origin::Project,
        SourceKind::Memory,
        context.runtime,
        if project_memory {
            Code::Used
        } else {
            Code::NotUsed
        },
        project_roots(&context.workdir)
            .into_iter()
            .flat_map(|root| existing(&[root.join("AGENTS.md"), root.join("AGENTS.override.md")]))
            .collect(),
        None,
        false,
    ));
    let known = [
        "model",
        "model_provider",
        "model_providers",
        "model_reasoning_effort",
        "model_reasoning_summary",
        "model_verbosity",
        "model_context_window",
        "model_auto_compact_token_limit",
        "personality",
        "developer_instructions",
        "mcp_servers",
        "skills",
        "plugins",
        "agents",
        "hooks",
        "notify",
        "projects",
        "features",
    ];
    let remaining = settings
        .as_object()
        .map(|map| {
            map.keys()
                .filter(|key| !known.contains(&key.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    sources.push(row(
        Origin::Operator,
        SourceKind::Settings,
        context.runtime,
        if resolved.sources.operator.contains(&SourceKind::Settings) {
            Code::Used
        } else if remaining.is_empty() {
            Code::NotUsed
        } else {
            Code::CannotBeExcluded
        },
        remaining,
        Some("Model and display preferences are retained".into()),
        context.private_home.is_some(),
    ));
    sources.push(row(
        Origin::System,
        SourceKind::Skills,
        context.runtime,
        Code::Used,
        vec!["Bundled system skills".into()],
        None,
        false,
    ));
    Ok(())
}

fn pi(
    context: &Context,
    resolved: &Resolved,
    launch: &mut RuntimeToolbox,
    sources: &mut Vec<ToolboxSource>,
) -> Result<()> {
    let directory = context
        .private_home
        .as_ref()
        .map(|home| home.join("pi"))
        .unwrap_or_else(|| context.operator.pi.clone());
    let home = context
        .private_home
        .clone()
        .unwrap_or_else(|| context.operator_home.clone());
    let settings = read_json(&directory.join("settings.json"))?;
    launch.pi_inherit_extensions = resolved.sources.operator.contains(&SourceKind::Plugins)
        && resolved.sources.operator.contains(&SourceKind::Hooks);
    // Native project MCP cannot be switched independently of other project files.
    launch.pi_approve = false;
    let operator_servers = object_keys(read_json(&directory.join("mcp.json"))?.get("mcpServers"));
    let inherit_mcp = resolved.sources.operator.contains(&SourceKind::Mcp);
    let mcp_code = if inherit_mcp {
        Code::Used
    } else if operator_servers.is_empty() {
        Code::NotUsed
    } else {
        Code::CannotBeExcluded
    };
    sources.push(row(
        Origin::Operator,
        SourceKind::Mcp,
        context.runtime,
        mcp_code,
        operator_servers,
        (mcp_code == Code::CannotBeExcluded)
            .then(|| "Native MCP cannot exclude this source while retaining crew's server".into()),
        context.private_home.is_some(),
    ));
    let mut project_skill_roots = vec![context.workdir.join(".pi/skills")];
    project_skill_roots.extend(
        project_roots(&context.workdir)
            .into_iter()
            .map(|root| root.join(".agents/skills")),
    );
    for (origin, roots, allowed) in [
        (
            Origin::Operator,
            vec![directory.join("skills"), home.join(".agents/skills")],
            &resolved.sources.operator,
        ),
        (
            Origin::Project,
            project_skill_roots,
            &resolved.sources.project,
        ),
    ] {
        let skills = skill_paths(&roots)?;
        let used = allowed.contains(&SourceKind::Skills)
            && (origin != Origin::Project || context.auto_trust);
        let names = skills
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        if used {
            launch.pi_skills.extend(skills);
        }
        sources.push(row(
            origin,
            SourceKind::Skills,
            context.runtime,
            if used { Code::Used } else { Code::NotUsed },
            names,
            None,
            context.private_home.is_some() && origin == Origin::Operator,
        ));
    }
    for kind in [SourceKind::Plugins, SourceKind::Hooks] {
        let mut names = object_keys(settings.get("packages"));
        names.extend(existing(&[directory.join("extensions")]));
        let wanted = resolved.sources.operator.contains(&kind);
        let code = if launch.pi_inherit_extensions {
            Code::Used
        } else if wanted {
            Code::NotAvailableSeparately
        } else {
            Code::NotUsed
        };
        sources.push(row(
            Origin::Operator,
            kind,
            context.runtime,
            code,
            names,
            Some("pi extensions are loaded together; built-in MCP is explicitly retained".into()),
            context.private_home.is_some(),
        ));
    }
    for kind in [SourceKind::Plugins, SourceKind::Hooks, SourceKind::Settings] {
        sources.push(row(
            Origin::Project,
            kind,
            context.runtime,
            if resolved.sources.project.contains(&kind) && context.auto_trust {
                Code::NotAvailableSeparately
            } else {
                Code::NotUsed
            },
            existing(&[context.workdir.join(".pi")]),
            Some(
                "Project configuration is not approved because its MCP servers are excluded".into(),
            ),
            false,
        ));
    }
    let memory = existing(&[
        directory.join("AGENTS.md"),
        directory.join("AGENTS.override.md"),
        directory.join("CLAUDE.md"),
    ]);
    let operator_memory = resolved.sources.operator.contains(&SourceKind::Memory);
    let project_memory = resolved.sources.project.contains(&SourceKind::Memory);
    launch.pi_context_files = operator_memory || project_memory;
    let project_system = context.workdir.join(".pi/SYSTEM.md");
    let operator_system = directory.join("SYSTEM.md");
    launch.pi_system_prompt = if project_memory && context.auto_trust && project_system.is_file() {
        project_system.to_string_lossy().into_owned()
    } else if operator_memory && operator_system.is_file() {
        operator_system.to_string_lossy().into_owned()
    } else {
        String::new()
    };
    let remaining = if launch.pi_context_files {
        memory
    } else {
        Vec::new()
    };
    sources.push(row(
        Origin::Operator,
        SourceKind::Memory,
        context.runtime,
        if operator_memory {
            Code::Used
        } else if remaining.is_empty() {
            Code::NotUsed
        } else {
            Code::CannotBeExcluded
        },
        remaining,
        (!operator_memory && launch.pi_context_files).then(|| {
            "User context files cannot be excluded while project context files are kept".into()
        }),
        context.private_home.is_some(),
    ));
    let project_context = project_roots(&context.workdir)
        .into_iter()
        .flat_map(|root| {
            existing(&[
                root.join("AGENTS.md"),
                root.join("AGENTS.override.md"),
                root.join("CLAUDE.md"),
            ])
        })
        .collect::<Vec<_>>();
    let project_code = if project_memory {
        Code::Used
    } else if launch.pi_context_files && !project_context.is_empty() {
        Code::CannotBeExcluded
    } else {
        Code::NotUsed
    };
    sources.push(row(
        Origin::Project,
        SourceKind::Memory,
        context.runtime,
        project_code,
        project_context,
        (project_code == Code::CannotBeExcluded).then(|| {
            "Project context files cannot be excluded while user context files are kept".into()
        }),
        false,
    ));
    let preferences = [
        "defaultProvider",
        "defaultModel",
        "defaultThinkingLevel",
        "enabledModels",
        "thinkingBudgets",
        "theme",
        "extensions",
        "packages",
        "skills",
    ];
    let remaining = settings
        .as_object()
        .map(|map| {
            map.keys()
                .filter(|key| !preferences.contains(&key.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    sources.push(row(
        Origin::Operator,
        SourceKind::Settings,
        context.runtime,
        if resolved.sources.operator.contains(&SourceKind::Settings) {
            Code::Used
        } else if remaining.is_empty() {
            Code::NotUsed
        } else {
            Code::CannotBeExcluded
        },
        remaining,
        None,
        context.private_home.is_some(),
    ));
    sources.push(row(
        Origin::Operator,
        SourceKind::Subagents,
        context.runtime,
        Code::NotUsed,
        Vec::new(),
        Some("No native Markdown subagent discovery".into()),
        context.private_home.is_some(),
    ));
    sources.push(row(
        Origin::Project,
        SourceKind::Mcp,
        context.runtime,
        Code::NotUsed,
        object_keys(read_json(&context.workdir.join(".pi/mcp.json"))?.get("mcpServers")),
        None,
        false,
    ));
    Ok(())
}

pub(super) fn read_json(path: &Path) -> Result<Value> {
    match read(path)? {
        Some(text) => serde_json::from_str(&text)
            .map_err(|_| anyhow!("cannot inspect JSON configuration {}", path.display())),
        None => Ok(json!({})),
    }
}

fn read_toml(path: &Path) -> Result<Value> {
    match read(path)? {
        Some(text) => {
            let table: toml::Table = toml::from_str(&text)
                .map_err(|_| anyhow!("cannot inspect TOML configuration {}", path.display()))?;
            Ok(serde_json::to_value(table)?)
        }
        None => Ok(json!({})),
    }
}

fn read(path: &Path) -> Result<Option<String>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        bail!("configuration {} is not a regular file", path.display());
    }
    let mut text = String::new();
    file.take(MAX_CONFIG + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_CONFIG {
        bail!("configuration {} is too large to inspect", path.display());
    }
    Ok(Some(text))
}

fn object_keys(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Object(map)) => map
            .iter()
            .filter(|(_, value)| value.as_bool() != Some(false))
            .map(|(key, _)| key.clone())
            .collect(),
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn existing(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| path.exists())
        .map(|path| path.display().to_string())
        .collect()
}

fn files_in(directory: &Path, extension: &str) -> Result<Vec<String>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == extension) {
            names.push(entry.path().display().to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Native roles may come from a directory or an `agents.<name>` table whose
/// config_file lives elsewhere. Scalar scheduler settings are not roles.
fn codex_roles(directory: &Path, settings: &Value) -> Result<Vec<String>> {
    let mut names = files_in(&directory.join("agents"), "toml")?;
    if let Some(roles) = settings.get("agents").and_then(Value::as_object) {
        names.extend(
            roles
                .iter()
                .filter(|(_, v)| v.is_object())
                .map(|(name, _)| {
                    format!("{}: agents.{name}", directory.join("config.toml").display())
                }),
        );
    }
    Ok(names)
}

pub(super) fn skill_paths(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut found = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut pending = roots
        .iter()
        .cloned()
        .map(|path| (path, 0))
        .collect::<Vec<_>>();
    while let Some((path, depth)) = pending.pop() {
        if depth > 32 || seen.len() > 8192 {
            bail!("skill sources exceed the discovery limit");
        }
        if path.file_name().is_some_and(|name| name == ".system") {
            continue;
        }
        let canonical = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if path.is_dir() {
            let skill = path.join("SKILL.md");
            if skill.is_file() {
                found.insert(skill.clone());
                if let Ok(canonical) = skill.canonicalize() {
                    found.insert(canonical);
                }
            }
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                if entry.path().is_dir() {
                    pending.push((entry.path(), depth + 1));
                }
            }
        } else if path.file_name().is_some_and(|name| name == "SKILL.md") {
            found.insert(path);
            found.insert(canonical);
        }
    }
    Ok(found.into_iter().collect())
}

fn project_roots(cwd: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for directory in cwd.ancestors() {
        roots.push(directory.to_path_buf());
        if directory.join(".git").exists() || directory.join(".jj").exists() {
            break;
        }
    }
    roots
}
