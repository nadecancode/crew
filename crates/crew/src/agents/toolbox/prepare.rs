//! Resolve runtime support, snapshot file items, and prepare credential-free
//! adapter inputs. MCP readiness is completed by the daemon's preflight.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use crew_core::toolbox::{Content, Fingerprint, Item, ItemKind, ItemRef, Resolved, SourceKind};
use crew_core::{AgentToolbox, RuntimeKind, ToolboxItem, ToolboxItemCode, ToolboxItemStatus};
use serde_json::{json, Value};

use super::store::{self, BTreeFiles, Snapshot, Store};
use super::{ActiveToolbox, RuntimeToolbox, MAPPING_VERSION};
use crate::agents::home::Operator;
use crate::endpoints::ToolboxReferences;

#[derive(Clone, Debug)]
pub struct Context {
    pub runtime: RuntimeKind,
    pub state_dir: PathBuf,
    pub agent_dir: PathBuf,
    pub workdir: PathBuf,
    pub private_home: Option<PathBuf>,
    pub operator: Operator,
    pub operator_home: PathBuf,
    pub socket_delivery: bool,
    pub auto_trust: bool,
    /// Read-only preview: no member files are rendered and no CLI is launched.
    pub preview: bool,
}

pub struct Plan {
    pub active: ActiveToolbox,
    pub view: AgentToolbox,
    pub runtime: RuntimeToolbox,
    pub stops: Vec<String>,
    pub references: BTreeMap<String, ToolboxReferences>,
}

impl Plan {
    pub fn finish_fingerprint(&mut self, runtime: RuntimeKind) -> Result<()> {
        let canonical = self.active.resolved.canonical(&self.active.content)?;
        let fingerprint = Fingerprint::of(&canonical, runtime, MAPPING_VERSION);
        self.view.fingerprint = fingerprint.as_str().to_string();
        self.active.fingerprint = Some(fingerprint);
        Ok(())
    }

    pub fn fail_server(&mut self, name: &str, reason: String, blocked: bool) {
        self.active.servers.remove(name);
        self.runtime.servers.retain(|server| server != name);
        if let Some(item) = self
            .view
            .items
            .iter_mut()
            .find(|item| item.kind == crew_core::ToolboxItemKind::Mcp && item.name == name)
        {
            item.status = if blocked {
                status(ToolboxItemCode::Blocked, "Blocked by the sandbox")
            } else {
                status(ToolboxItemCode::FailedPreflight, "Failed preflight")
            };
            item.detail = Some(reason.clone());
            if item.required {
                self.stops.push(format!("mcp.{name}: {reason}"));
            }
        }
    }
}

pub fn applies(runtime: RuntimeKind, item: &Item) -> bool {
    runtime != RuntimeKind::Shell
        && (item.runtimes().is_empty() || item.runtimes().contains(&runtime))
}

/// A format is never silently translated by dropping a capability.
pub fn unsupported(context: &Context, kind: ItemKind) -> Option<&'static str> {
    match (context.runtime, kind) {
        (RuntimeKind::Codex | RuntimeKind::Pi, ItemKind::Plugins) => {
            Some("Claude Code plugin format")
        }
        (RuntimeKind::Codex | RuntimeKind::Pi, ItemKind::Hooks) => {
            Some("selected hook loading is not available on this runtime")
        }
        (RuntimeKind::Pi, ItemKind::Subagents) => {
            Some("pi needs an extension to load subagent definitions")
        }
        (RuntimeKind::Codex, ItemKind::Subagents) if context.private_home.is_none() => {
            Some("outside the sandbox")
        }
        (RuntimeKind::Codex, ItemKind::Skills)
            if !context.socket_delivery && context.private_home.is_none() =>
        {
            Some("without crew's app-server")
        }
        _ => None,
    }
}

pub fn prepare(
    context: &Context,
    resolved: Resolved,
    inherited: Option<&ActiveToolbox>,
    references: impl Fn(&crew_core::toolbox::McpServer) -> Result<ToolboxReferences>,
) -> Result<Plan> {
    let mut plan = Plan {
        active: ActiveToolbox {
            resolved,
            ..ActiveToolbox::default()
        },
        view: AgentToolbox::default(),
        runtime: RuntimeToolbox::default(),
        stops: Vec::new(),
        references: BTreeMap::new(),
    };
    plan.runtime.inherit_memory = plan
        .active
        .resolved
        .sources
        .operator
        .contains(&SourceKind::Memory);
    let store = Store::open(&context.state_dir)?;
    for (key, entry) in &plan.active.resolved.items {
        let applicable = applies(context.runtime, &entry.item);
        let required = applicable && (plan.active.resolved.strict || entry.item.required());
        let mut item = ToolboxItem {
            kind: key.kind.into(),
            name: key.name.clone(),
            origin: entry.origin.to_string(),
            required,
            status: status(ToolboxItemCode::Planned, "Planned"),
            detail: detail(&entry.item),
        };
        if key.kind.is_file() {
            plan.active.content.insert(key.clone(), Content::NotLoaded);
        }
        if !applicable {
            item.status = status(ToolboxItemCode::NotApplicable, "Not applicable");
        } else if let Some(reason) = unsupported(context, key.kind) {
            item.status = status(
                ToolboxItemCode::NotAvailable,
                &format!(
                    "Not available on {}",
                    crate::models::runtime_label(context.runtime)
                ),
            );
            item.detail = Some(reason.to_string());
        } else {
            match &entry.item {
                Item::Mcp(server) => {
                    let values = references(server)?;
                    plan.runtime.scrub_env.extend(values.scrub.iter().cloned());
                    if values.missing.is_empty() {
                        // Readiness is not claimed until the daemon finishes preflight.
                        plan.active.servers.insert(key.name.clone());
                        plan.runtime.servers.push(key.name.clone());
                    } else {
                        let missing = values
                            .missing
                            .iter()
                            .map(|name| format!("${name}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        item.status = status(ToolboxItemCode::Needs, &format!("Needs {missing}"));
                        item.detail = Some(format!("{missing} (not set)"));
                    }
                    plan.references.insert(key.name.clone(), values);
                }
                Item::File(file) => {
                    let snapshot =
                        match inherited.and_then(|active| active.files.get(key)) {
                            Some(snapshot) => store
                                .files(snapshot)
                                .map(|_| snapshot.clone())
                                .map_err(|error| store::Failure {
                                    content: Content::Invalid,
                                    message: format!("{error:#}"),
                                }),
                            None => store.snapshot(Path::new(&file.path)),
                        };
                    match snapshot {
                        Ok(snapshot) => {
                            match validate_file(context.runtime, key, &snapshot, &store) {
                                Ok(()) => {
                                    plan.active.content.insert(
                                        key.clone(),
                                        Content::Sha256(snapshot.hash.clone()),
                                    );
                                    plan.active.files.insert(key.clone(), snapshot);
                                    if !context.preview {
                                        item.status = status(ToolboxItemCode::Active, "Active");
                                    }
                                }
                                Err(error) => {
                                    plan.active.content.insert(key.clone(), Content::Invalid);
                                    item.status = status(
                                        ToolboxItemCode::FailedPreflight,
                                        "Failed preflight",
                                    );
                                    item.detail = Some(format!("{error:#}"));
                                }
                            }
                        }
                        Err(error) => {
                            plan.active.content.insert(key.clone(), error.content);
                            item.status =
                                status(ToolboxItemCode::FailedPreflight, "Failed preflight");
                            item.detail = Some(error.message);
                        }
                    }
                }
                Item::Hook(hook) => {
                    if !HOOK_EVENTS.contains(&hook.event.as_str()) {
                        item.status = status(
                            ToolboxItemCode::NotAvailable,
                            "Not available on Claude Code",
                        );
                        item.detail = Some(format!(
                            "hook event {} has no confirmed mapping",
                            hook.event
                        ));
                    } else if !context.preview {
                        item.status = status(ToolboxItemCode::Active, "Active");
                    }
                }
            }
        }
        if required
            && !matches!(
                item.status.code,
                ToolboxItemCode::Active | ToolboxItemCode::Planned
            )
        {
            plan.stops.push(format!(
                "{key}: {}{}",
                item.status.text,
                item.detail
                    .as_ref()
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            ));
        }
        plan.view.items.push(item);
    }
    plan.runtime.scrub_env.sort();
    plan.runtime.scrub_env.dedup();
    plan.active.scrub_env = plan.runtime.scrub_env.iter().cloned().collect();
    plan.view.sources = super::sources::prepare(context, &plan.active.resolved, &mut plan.runtime)?;
    if plan.active.resolved.strict {
        for source in &plan.view.sources {
            if source.status.code == crew_core::ToolboxSourceCode::CannotBeExcluded {
                plan.stops.push(format!(
                    "{}: {}",
                    source.label,
                    source.detail.as_deref().unwrap_or(&source.status.text)
                ));
            }
        }
    }
    plan.finish_fingerprint(context.runtime)?;
    if !context.preview && context.runtime != RuntimeKind::Shell {
        render(context, &store, &mut plan)?;
    }
    Ok(plan)
}

const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "SessionEnd",
];

pub fn status(code: ToolboxItemCode, text: &str) -> ToolboxItemStatus {
    ToolboxItemStatus {
        code,
        text: text.to_string(),
    }
}

pub fn detail(item: &Item) -> Option<String> {
    match item {
        Item::Mcp(server) => {
            let mut text = server
                .url
                .clone()
                .unwrap_or_else(|| server.command.join(" "));
            let names: BTreeSet<&str> = server
                .env
                .values()
                .chain(server.headers.values())
                .filter_map(|reference| match reference {
                    crew_core::toolbox::Reference::Env { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            if !names.is_empty() {
                text.push_str(&format!(
                    " · {}",
                    names
                        .into_iter()
                        .map(|name| format!("${name}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Some(text)
        }
        Item::File(file) => Some(file.path.clone()),
        Item::Hook(hook) => Some(format!("{}: {}", hook.event, hook.command.join(" "))),
    }
}

fn validate_file(
    runtime: RuntimeKind,
    key: &ItemRef,
    snapshot: &Snapshot,
    store: &Store,
) -> Result<()> {
    let files = store.files(snapshot)?;
    match key.kind {
        ItemKind::Skills => {
            if !snapshot.directory {
                bail!("a skill must be a directory containing SKILL.md");
            }
            let bytes = files
                .get(Path::new("SKILL.md"))
                .ok_or_else(|| anyhow!("skill has no SKILL.md"))?;
            frontmatter(&bytes.0)?;
        }
        ItemKind::Subagents => {
            if snapshot.directory {
                bail!("a subagent must be a Markdown file");
            }
            let bytes = &files
                .get(Path::new(""))
                .ok_or_else(|| anyhow!("subagent file is missing"))?
                .0;
            let (metadata, _) = frontmatter(bytes)?;
            if runtime == RuntimeKind::Codex
                && (metadata.contains_key("tools")
                    || metadata.contains_key("disallowedTools")
                    || metadata.contains_key("permissionMode")
                    || metadata.contains_key("mcpServers"))
            {
                bail!("the Markdown subagent's tool or permission restrictions cannot be translated to Codex");
            }
        }
        ItemKind::Plugins => {
            if !snapshot.directory {
                bail!("a plugin must be a directory");
            }
            let bytes = files
                .get(Path::new(".claude-plugin/plugin.json"))
                .ok_or_else(|| anyhow!("plugin has no .claude-plugin/plugin.json"))?;
            let manifest: Value = serde_json::from_slice(&bytes.0)
                .map_err(|_| anyhow!("plugin manifest is not valid JSON"))?;
            let name = manifest
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| anyhow!("plugin manifest must have a name"))?;
            if crew_core::toolbox::RESERVED_NAMES.contains(&name) {
                bail!("plugin name {name} is reserved for crew");
            }
        }
        _ => bail!("item is not a file format"),
    }
    Ok(())
}

fn frontmatter(bytes: &[u8]) -> Result<(BTreeMap<String, serde_yaml::Value>, String)> {
    let text = std::str::from_utf8(bytes).map_err(|_| anyhow!("definition is not UTF-8"))?;
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        bail!("definition needs YAML front matter");
    }
    let mut header = String::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        header.push_str(line);
        header.push('\n');
    }
    if !closed {
        bail!("definition front matter is not closed");
    }
    let fields: BTreeMap<String, serde_yaml::Value> = serde_yaml::from_str(&header)
        .map_err(|_| anyhow!("definition front matter must be a YAML mapping"))?;
    for key in ["name", "description"] {
        if fields
            .get(key)
            .and_then(serde_yaml::Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            bail!("definition front matter needs a nonempty {key}");
        }
    }
    Ok((fields, lines.collect::<Vec<_>>().join("\n")))
}

fn render(context: &Context, store: &Store, plan: &mut Plan) -> Result<()> {
    let parent = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&context.agent_dir)?;
    match context.runtime {
        RuntimeKind::Claude => {
            let mut generated = BTreeFiles::new();
            let mut hooks: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for item in &plan.view.items {
                if item.status.code != ToolboxItemCode::Active {
                    continue;
                }
                let key = ItemRef::new(
                    match item.kind {
                        crew_core::ToolboxItemKind::Skills => ItemKind::Skills,
                        crew_core::ToolboxItemKind::Subagents => ItemKind::Subagents,
                        crew_core::ToolboxItemKind::Hooks => ItemKind::Hooks,
                        _ => continue,
                    },
                    &item.name,
                );
                match &plan.active.resolved.items[&key].item {
                    Item::File(_) => {
                        let snapshot = &plan.active.files[&key];
                        let files = store.files(snapshot)?;
                        let base = if key.kind == ItemKind::Skills {
                            PathBuf::from("skills").join(&key.name)
                        } else {
                            PathBuf::from("agents").join(format!("{}.md", key.name))
                        };
                        for (relative, value) in files {
                            generated.insert(base.join(relative), value);
                        }
                    }
                    Item::Hook(hook) => {
                        let command = hook
                            .command
                            .iter()
                            .map(|arg| crate::agents::shell_quote(arg))
                            .collect::<Vec<_>>()
                            .join(" ");
                        let mut entry = json!({"hooks":[{"type":"command","command":command}]});
                        if let Some(matcher) = &hook.matcher {
                            entry["matcher"] = json!(matcher);
                        }
                        hooks.entry(hook.event.clone()).or_default().push(entry);
                    }
                    _ => {}
                }
            }
            if !hooks.is_empty() {
                generated.insert(
                    "hooks/hooks.json".into(),
                    (serde_json::to_vec_pretty(&json!({"hooks":hooks}))?, false),
                );
            }
            let has_generated = !generated.is_empty();
            if has_generated {
                generated.insert(
                    ".claude-plugin/plugin.json".into(),
                    (
                        serde_json::to_vec_pretty(
                            &json!({"name":"crew-toolbox","version":"1.0.0"}),
                        )?,
                        false,
                    ),
                );
            }
            store::replace_tree(&parent, Path::new("crew-toolbox"), &generated)?;
            if has_generated {
                plan.runtime
                    .claude_plugins
                    .push(context.agent_dir.join("crew-toolbox"));
            }
            for (key, snapshot) in &plan.active.files {
                if key.kind == ItemKind::Plugins {
                    plan.runtime.claude_plugins.push(snapshot.path.clone());
                }
            }
        }
        RuntimeKind::Codex => {
            if let Some(home) = &context.private_home {
                let home_fd = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(home)?;
                let codex = store::directory(&home_fd, Path::new("codex"))?;
                let mut agents = BTreeFiles::new();
                for (key, snapshot) in &plan.active.files {
                    if key.kind != ItemKind::Subagents {
                        continue;
                    }
                    let files = store.files(snapshot)?;
                    let (metadata, body) = frontmatter(&files[Path::new("")].0)?;
                    let mut record = toml::Table::new();
                    record.insert("name".into(), toml::Value::String(key.name.clone()));
                    record.insert(
                        "description".into(),
                        toml::Value::String(metadata["description"].as_str().unwrap().to_string()),
                    );
                    record.insert("developer_instructions".into(), toml::Value::String(body));
                    if let Some(model) = metadata.get("model").and_then(serde_yaml::Value::as_str) {
                        if model != "inherit" {
                            record.insert("model".into(), toml::Value::String(model.to_string()));
                        }
                    }
                    agents.insert(
                        PathBuf::from(format!("{}.toml", key.name)),
                        (toml::to_string(&record)?.into_bytes(), false),
                    );
                }
                store::replace_tree(&codex, Path::new("agents"), &agents)?;
                let agents_home = store::directory(&home_fd, Path::new(".agents"))?;
                store::replace_tree(&agents_home, Path::new("skills"), &BTreeFiles::new())?;
                if !context.socket_delivery {
                    let skills = store::directory(&agents_home, Path::new("skills"))?;
                    for (key, snapshot) in &plan.active.files {
                        if key.kind == ItemKind::Skills {
                            store.copy_snapshot(snapshot, &skills, Path::new(&key.name))?;
                        }
                    }
                }
            }
            if context.socket_delivery {
                plan.runtime.codex_skill_roots = plan
                    .active
                    .files
                    .iter()
                    .filter(|(key, _)| key.kind == ItemKind::Skills)
                    .map(|(_, snapshot)| snapshot.path.clone())
                    .collect();
            }
        }
        RuntimeKind::Pi => {
            plan.runtime.pi_skills.extend(
                plan.active
                    .files
                    .iter()
                    .filter(|(key, _)| key.kind == ItemKind::Skills)
                    .map(|(_, snapshot)| snapshot.path.clone()),
            );
            plan.runtime.pi_skills.sort();
            plan.runtime.pi_skills.dedup();
        }
        RuntimeKind::Shell => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::toolbox::{FileItem, Origin, ResolvedItem};

    fn context(root: &Path, runtime: RuntimeKind, member: &str) -> Context {
        let agent_dir = root.join("state/agents").join(member);
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::create_dir_all(root.join("work/.git")).unwrap();
        Context {
            runtime,
            state_dir: root.join("state"),
            agent_dir,
            workdir: root.join("work"),
            private_home: None,
            operator: Operator {
                claude: root.join("operator/.claude"),
                codex: root.join("operator/.codex"),
                pi: root.join("operator/.pi/agent"),
            },
            operator_home: root.join("operator"),
            socket_delivery: true,
            auto_trust: false,
            preview: false,
        }
    }

    fn skill(path: &Path, required: bool) -> Resolved {
        Resolved {
            items: BTreeMap::from([(
                ItemRef::new(ItemKind::Skills, "example"),
                ResolvedItem {
                    item: Item::File(FileItem {
                        path: path.to_string_lossy().into_owned(),
                        required,
                        runtimes: vec![],
                    }),
                    origin: Origin::Member,
                },
            )]),
            ..Resolved::default()
        }
    }

    #[test]
    fn snapshots_are_frozen_across_member_inheritance_and_refresh_on_a_normal_start() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: example\ndescription: original\n---\nOriginal body.",
        )
        .unwrap();
        let parent_context = context(temp.path(), RuntimeKind::Claude, "parent");
        let one = prepare(&parent_context, skill(&source, true), None, |_| {
            Ok(ToolboxReferences::default())
        })
        .unwrap();
        assert!(one.stops.is_empty());
        assert_eq!(one.view.items[0].status.code, ToolboxItemCode::Active);
        let first = std::fs::read(
            parent_context
                .agent_dir
                .join("crew-toolbox/skills/example/SKILL.md"),
        )
        .unwrap();
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: example\ndescription: changed\n---\nChanged body.",
        )
        .unwrap();
        let child_context = context(temp.path(), RuntimeKind::Claude, "child");
        let child = prepare(
            &child_context,
            one.active.resolved.clone(),
            Some(&one.active),
            |_| Ok(ToolboxReferences::default()),
        )
        .unwrap();
        assert_eq!(one.active.fingerprint, child.active.fingerprint);
        assert_eq!(
            std::fs::read(
                child_context
                    .agent_dir
                    .join("crew-toolbox/skills/example/SKILL.md")
            )
            .unwrap(),
            first
        );
        let changed = prepare(&parent_context, skill(&source, true), None, |_| {
            Ok(ToolboxReferences::default())
        })
        .unwrap();
        assert_ne!(one.active.fingerprint, changed.active.fingerprint);
        cleanup(temp.path());
    }

    #[test]
    fn missing_optional_items_are_visible_and_shell_never_reads_them_even_when_strict() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("absent");
        let optional = prepare(
            &context(temp.path(), RuntimeKind::Claude, "a"),
            skill(&path, false),
            None,
            |_| Ok(ToolboxReferences::default()),
        )
        .unwrap();
        assert!(optional.stops.is_empty());
        assert_eq!(
            optional.view.items[0].status.code,
            ToolboxItemCode::FailedPreflight
        );
        assert_eq!(
            optional.active.content.values().next(),
            Some(&Content::Missing)
        );
        let required = prepare(
            &context(temp.path(), RuntimeKind::Claude, "b"),
            skill(&path, true),
            None,
            |_| Ok(ToolboxReferences::default()),
        )
        .unwrap();
        assert_eq!(required.stops.len(), 1);
        let mut resolved = skill(&path, true);
        resolved.strict = true;
        let shell = prepare(
            &context(temp.path(), RuntimeKind::Shell, "shell"),
            resolved,
            None,
            |_| panic!("shell never resolves toolbox references"),
        )
        .unwrap();
        assert!(shell.stops.is_empty());
        assert_eq!(
            shell.view.items[0].status.code,
            ToolboxItemCode::NotApplicable
        );
        assert_eq!(
            shell.active.content.values().next(),
            Some(&Content::NotLoaded)
        );
        cleanup(temp.path());
    }

    #[test]
    fn preview_and_unsupported_runtime_never_claim_active() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: example\ndescription: fixture\n---\nBody.",
        )
        .unwrap();
        let mut ctx = context(temp.path(), RuntimeKind::Claude, "preview");
        ctx.preview = true;
        let preview = prepare(&ctx, skill(&source, true), None, |_| {
            Ok(ToolboxReferences::default())
        })
        .unwrap();
        assert_eq!(preview.view.items[0].status.code, ToolboxItemCode::Planned);
        assert!(!ctx.agent_dir.join("crew-toolbox").exists());
        ctx.runtime = RuntimeKind::Codex;
        ctx.socket_delivery = false;
        let unsupported = prepare(&ctx, skill(&source, true), None, |_| {
            Ok(ToolboxReferences::default())
        })
        .unwrap();
        assert_eq!(
            unsupported.view.items[0].status.code,
            ToolboxItemCode::NotAvailable
        );
        assert_eq!(
            unsupported.active.content.values().next(),
            Some(&Content::NotLoaded)
        );
        assert_eq!(unsupported.stops.len(), 1);
        cleanup(temp.path());
    }

    fn cleanup(root: &Path) {
        fn writable(path: &Path) {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::symlink_metadata(path) {
                if metadata.is_dir() {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
                    for entry in std::fs::read_dir(path).unwrap() {
                        writable(&entry.unwrap().path());
                    }
                }
            }
        }
        writable(root);
    }
}
