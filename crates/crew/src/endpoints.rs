//! Endpoints and keys for the agents crew launches, kept apart from the
//! user's own Claude Code and Codex setup.
//!
//! Three files are read, each later one overriding the earlier ones field by
//! field:
//!
//! | scope   | file                                                         |
//! |---------|--------------------------------------------------------------|
//! | global  | `/etc/crew/config.toml` (or `$CREW_GLOBAL_CONFIG`)           |
//! | user    | `$XDG_CONFIG_HOME/crew/config.toml` (`~/.config/crew/…`)     |
//! | project | `<workspace>/.crew/config.toml`                              |
//!
//! A file has a `[claude]` and a `[codex]` section, applying to every member
//! on that runtime, and `[agents.<id>]` sections for single members. Member
//! sections win over runtime sections whatever file they are in.
//!
//! The files are read again at every agent launch and resolved secrets live
//! only in memory: they are never written to the session database, the
//! roster, the event log, or a command line.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::util::is_valid_id;
use crew_core::RuntimeKind;
use serde::Deserialize;

/// Overrides the location of the global file (packaging, tests).
pub const GLOBAL_CONFIG_ENV: &str = "CREW_GLOBAL_CONFIG";
const GLOBAL_CONFIG: &str = "/etc/crew/config.toml";
const FILE_NAME: &str = "config.toml";

/// Environment variable a Codex member's key is handed over in.
pub const CODEX_KEY_ENV: &str = "CREW_CODEX_API_KEY";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Global,
    User,
    Project,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Global, Scope::User, Scope::Project];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::User => "user",
            Scope::Project => "project",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|scope| scope.as_str() == s)
            .ok_or_else(|| anyhow!("unknown scope '{s}' (expected global, user or project)"))
    }

    /// Where this scope's file lives.
    pub fn path(self, workspace: &Path) -> Option<PathBuf> {
        match self {
            Scope::Global => Some(
                std::env::var_os(GLOBAL_CONFIG_ENV)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(GLOBAL_CONFIG)),
            ),
            Scope::User => std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .map(|dir| dir.join("crew").join(FILE_NAME)),
            Scope::Project => Some(crew_core::paths::state_dir(workspace).join(FILE_NAME)),
        }
    }
}

/// A secret value. Its `Debug` output never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// Enough of a secret to recognise it, never enough to use it.
fn mask(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 16 {
        return "…".to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// One `[claude]`, `[codex]` or `[agents.<id>]` section as written.
#[derive(Deserialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
struct Section {
    base_url: Option<String>,
    api_key: Option<String>,
    api_key_env: Option<String>,
    api_key_file: Option<String>,
    auth_token: Option<String>,
    auth_token_env: Option<String>,
    auth_token_file: Option<String>,
    model: Option<String>,
    wire_api: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    env_files: BTreeMap<String, String>,
}

#[derive(Deserialize, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    claude: Option<Section>,
    codex: Option<Section>,
    #[serde(default)]
    agents: BTreeMap<String, Section>,
    /// The dashboard server's (`crate::web::process::Settings`), read there.
    #[serde(default)]
    #[allow(dead_code)]
    web: Option<toml::Value>,
}

/// How a credential is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    /// An API key (`x-api-key` for Claude Code, bearer for Codex).
    ApiKey,
    /// A bearer token (`Authorization: Bearer`), Claude Code only.
    AuthToken,
}

impl CredentialKind {
    fn field(self) -> &'static str {
        match self {
            CredentialKind::ApiKey => "api_key",
            CredentialKind::AuthToken => "auth_token",
        }
    }
}

/// Where a credential's value comes from.
#[derive(Clone, PartialEq)]
enum Source {
    Literal(Secret),
    /// An environment variable of the crew daemon.
    Env(String),
    File(PathBuf),
}

impl Section {
    /// The credential this section sets: `None` if it sets none,
    /// `Some(None)` if it clears the one inherited from earlier layers.
    fn credential(&self, dir: &Path) -> Result<Option<Option<(CredentialKind, Source)>>> {
        let fields = [
            (CredentialKind::ApiKey, 0, &self.api_key),
            (CredentialKind::ApiKey, 1, &self.api_key_env),
            (CredentialKind::ApiKey, 2, &self.api_key_file),
            (CredentialKind::AuthToken, 0, &self.auth_token),
            (CredentialKind::AuthToken, 1, &self.auth_token_env),
            (CredentialKind::AuthToken, 2, &self.auth_token_file),
        ];
        let mut set = fields
            .iter()
            .filter_map(|(kind, how, value)| value.as_ref().map(|v| (*kind, *how, v.trim())));
        let Some((kind, how, value)) = set.next() else {
            return Ok(None);
        };
        if set.next().is_some() {
            bail!(
                "a section takes one credential: one of api_key, api_key_env, api_key_file, \
auth_token, auth_token_env, auth_token_file"
            );
        }
        if value.is_empty() {
            return Ok(Some(None));
        }
        let source = match how {
            0 => Source::Literal(Secret(value.to_string())),
            1 => {
                if !is_env_name(value) {
                    bail!(
                        "{}_env: '{value}' is not an environment variable name",
                        kind.field()
                    );
                }
                Source::Env(value.to_string())
            }
            _ => Source::File(expand_path(value, dir)),
        };
        Ok(Some(Some((kind, source))))
    }

    fn has_literal_secret(&self) -> bool {
        [&self.api_key, &self.auth_token]
            .iter()
            .any(|v| v.as_deref().is_some_and(|s| !s.trim().is_empty()))
    }

    fn validate(&self, dir: &Path) -> Result<()> {
        if let Some(url) = self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            let rest = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
                .ok_or_else(|| anyhow!("base_url must start with http:// or https://"))?;
            if rest.is_empty() || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
                bail!("base_url '{url}' is not a URL");
            }
        }
        if let Some(wire) = self
            .wire_api
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
        {
            if !matches!(wire, "responses" | "chat") {
                bail!("wire_api must be \"responses\" or \"chat\"");
            }
        }
        for name in self.env.keys() {
            if !is_env_name(name) {
                bail!("env: '{name}' is not an environment variable name");
            }
            if name.starts_with("CREW_") {
                bail!("env: '{name}' is reserved for crew");
            }
        }
        for name in self.env_files.keys() {
            if !is_env_name(name) {
                bail!("env_files: '{name}' is not an environment variable name");
            }
            if name.starts_with("CREW_") {
                bail!("env_files: '{name}' is reserved for crew");
            }
            // Names that steer the loader or libc never reach an agent.
            crate::sandbox::spec::check_name(name).map_err(|e| anyhow!("env_files: {e}"))?;
            if self.env.contains_key(name) {
                bail!("'{name}' is in both env and env_files");
            }
            // The homes are the daemon's to set (a sandbox moves them).
            if matches!(
                name.as_str(),
                "CLAUDE_CONFIG_DIR" | "CODEX_HOME" | "PI_CODING_AGENT_DIR"
            ) {
                bail!("env_files: '{name}' is set by crew, not by a token file");
            }
        }
        self.credential(dir)?;
        Ok(())
    }
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `~/…` is the home directory; other relative paths are relative to the
/// config file.
fn expand_path(path: &str, dir: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    }
}

impl FileConfig {
    fn validate(&self, dir: &Path) -> Result<()> {
        if let Some(claude) = &self.claude {
            claude.validate(dir).context("[claude]")?;
            if claude.wire_api.is_some() {
                bail!("[claude]: wire_api is a Codex setting");
            }
        }
        if let Some(codex) = &self.codex {
            codex.validate(dir).context("[codex]")?;
            if matches!(
                codex.credential(dir)?,
                Some(Some((CredentialKind::AuthToken, _)))
            ) {
                bail!("[codex]: Codex sends its key as a bearer token already; use api_key");
            }
        }
        for (id, section) in &self.agents {
            if !is_valid_id(id) {
                bail!("[agents.{id}]: not an agent id");
            }
            section
                .validate(dir)
                .with_context(|| format!("[agents.{id}]"))?;
        }
        Ok(())
    }

    fn has_literal_secret(&self) -> bool {
        self.claude
            .iter()
            .chain(self.codex.iter())
            .chain(self.agents.values())
            .any(Section::has_literal_secret)
    }
}

enum LayerState {
    Missing,
    Loaded(Box<FileConfig>),
    /// The file exists and crew will not use it; nothing is launched until
    /// that is fixed, so a broken override never silently falls back to the
    /// user's own login.
    Rejected(String),
}

struct Layer {
    scope: Scope,
    path: PathBuf,
    state: LayerState,
    warning: Option<String>,
}

fn load_layer(scope: Scope, path: PathBuf, workspace: &Path) -> Layer {
    let mut layer = Layer {
        scope,
        path,
        state: LayerState::Missing,
        warning: None,
    };
    let meta = match std::fs::metadata(&layer.path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return layer,
        Err(e) => {
            layer.state = LayerState::Rejected(format!("cannot be read: {e}"));
            return layer;
        }
    };
    let state = (|| -> Result<FileConfig> {
        if !meta.is_file() {
            bail!("is not a regular file");
        }
        // SAFETY: geteuid has no preconditions.
        let me = unsafe { libc::geteuid() };
        if meta.uid() != me && meta.uid() != 0 {
            bail!("is owned by another user");
        }
        if meta.mode() & 0o022 != 0 {
            bail!("is writable by other users (chmod go-w)");
        }
        if scope == Scope::Project && tracked_by_git(workspace, &layer.path) {
            bail!(
                "is tracked by git; a project's endpoints and keys stay on this machine \
(git rm --cached it)"
            );
        }
        let text = std::fs::read_to_string(&layer.path).context("cannot be read")?;
        // Only the message and the line: toml's full error quotes the
        // offending line, which may hold a key.
        let config: FileConfig = toml::from_str(&text).map_err(|e| match e.span() {
            Some(span) => {
                let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
                anyhow!("line {line}: {}", e.message())
            }
            None => anyhow!("{}", e.message()),
        })?;
        config.validate(layer.path.parent().unwrap_or(Path::new("/")))?;
        Ok(config)
    })();
    match state {
        Ok(config) => {
            if config.has_literal_secret() && meta.mode() & 0o077 != 0 {
                layer.warning =
                    Some("holds a key and is readable by other users (chmod 600)".into());
            }
            layer.state = LayerState::Loaded(Box::new(config));
        }
        Err(e) => layer.state = LayerState::Rejected(format!("{e:#}")),
    }
    layer
}

/// A project file that came with the repository is somebody else's choice of
/// endpoint for this user's keys.
fn tracked_by_git(workspace: &Path, file: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The merged, still unresolved settings for one agent, with the scope each
/// came from.
#[derive(Default)]
struct Plan {
    base_url: Option<(String, Scope)>,
    credential: Option<(CredentialKind, Source, Scope)>,
    model: Option<(String, Scope)>,
    wire_api: Option<(String, Scope)>,
    env: BTreeMap<String, (String, Scope)>,
    /// Environment variable name to the token file that fills it.
    env_files: BTreeMap<String, (PathBuf, Scope)>,
}

impl Plan {
    fn apply(&mut self, section: &Section, scope: Scope, dir: &Path) {
        let set = |slot: &mut Option<(String, Scope)>, value: &Option<String>| {
            if let Some(value) = value {
                let value = value.trim();
                // An empty value takes back what an earlier layer set.
                *slot = (!value.is_empty()).then(|| (value.to_string(), scope));
            }
        };
        set(&mut self.base_url, &section.base_url);
        set(&mut self.model, &section.model);
        set(&mut self.wire_api, &section.wire_api);
        // Validated when the file was loaded.
        if let Ok(Some(credential)) = section.credential(dir) {
            self.credential = credential.map(|(kind, source)| (kind, source, scope));
        }
        for (name, value) in &section.env {
            // A more specific plain value takes the name back from a token file.
            self.env_files.remove(name);
            self.env.insert(name.clone(), (value.clone(), scope));
        }
        for (name, path) in &section.env_files {
            // An empty path takes back what an earlier layer set.
            if path.trim().is_empty() {
                self.env_files.remove(name);
            } else {
                // And a more specific token file takes it from a plain value.
                self.env.remove(name);
                self.env_files
                    .insert(name.clone(), (expand_path(path.trim(), dir), scope));
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.base_url.is_none()
            && self.credential.is_none()
            && self.model.is_none()
            && self.wire_api.is_none()
            && self.env.is_empty()
            && self.env_files.is_empty()
    }
}

/// What an agent is launched with: resolved, secrets included.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Endpoint {
    pub base_url: Option<String>,
    pub api_key: Option<Secret>,
    pub auth_token: Option<Secret>,
    /// The model every member on this endpoint runs.
    pub model: Option<String>,
    /// Codex: `responses` (default) or `chat`.
    pub wire_api: Option<String>,
    pub env: BTreeMap<String, String>,
    /// Environment secrets read from token files: name and value. Kept apart
    /// from `env` because they go only into the process environment, never
    /// into a settings file or the event log.
    pub secret_env: Vec<(String, Secret)>,
    /// Where the settings come from, for the event log. No secrets.
    pub origin: Option<String>,
}

impl Endpoint {
    /// Nothing is overridden: the runtime uses the user's own setup.
    pub fn is_default(&self) -> bool {
        self.base_url.is_none()
            && self.api_key.is_none()
            && self.auth_token.is_none()
            && self.model.is_none()
            && self.env.is_empty()
    }
}

/// The three configuration files as found on disk.
pub struct Layers {
    layers: Vec<Layer>,
}

impl Layers {
    pub fn load(workspace: &Path) -> Self {
        let layers = Scope::ALL
            .into_iter()
            .filter_map(|scope| Some(load_layer(scope, scope.path(workspace)?, workspace)))
            .collect();
        Self { layers }
    }

    /// Refuse to go on with a file that exists but cannot be used.
    pub fn check(&self) -> Result<()> {
        for layer in &self.layers {
            if let LayerState::Rejected(reason) = &layer.state {
                bail!(
                    "crew config ({}) {} {reason}",
                    layer.scope.as_str(),
                    layer.path.display()
                );
            }
        }
        Ok(())
    }

    pub fn warnings(&self) -> Vec<String> {
        self.layers
            .iter()
            .filter_map(|l| Some(format!("{} {}", l.path.display(), l.warning.as_ref()?)))
            .collect()
    }

    fn loaded(&self) -> impl Iterator<Item = (&Layer, &FileConfig)> {
        self.layers.iter().filter_map(|layer| match &layer.state {
            LayerState::Loaded(config) => Some((layer, config.as_ref())),
            _ => None,
        })
    }

    /// Merge the layers for one agent: the runtime's section from each file,
    /// then the member sections (the parent's for a subagent, then its own).
    fn plan(&self, runtime: RuntimeKind, agent_id: &str, parent: Option<&str>) -> Plan {
        let mut plan = Plan::default();
        let dir = |layer: &Layer| layer.path.parent().unwrap_or(Path::new("/")).to_path_buf();
        for (layer, config) in self.loaded() {
            let section = match runtime {
                RuntimeKind::Claude => config.claude.as_ref(),
                RuntimeKind::Codex => config.codex.as_ref(),
                // pi keeps its own providers and logins (`pi --list-models`).
                RuntimeKind::Pi | RuntimeKind::Shell => None,
            };
            if let Some(section) = section {
                plan.apply(section, layer.scope, &dir(layer));
            }
        }
        for id in parent.into_iter().chain([agent_id]) {
            for (layer, config) in self.loaded() {
                if let Some(section) = config.agents.get(id) {
                    plan.apply(section, layer.scope, &dir(layer));
                }
            }
        }
        plan
    }

    /// The endpoint and key `agent_id` is launched with.
    pub fn resolve(
        &self,
        runtime: RuntimeKind,
        agent_id: &str,
        parent: Option<&str>,
    ) -> Result<Endpoint> {
        self.check()?;
        if runtime == RuntimeKind::Shell {
            return Ok(Endpoint::default());
        }
        let plan = self.plan(runtime, agent_id, parent);
        if plan.is_empty() {
            return Ok(Endpoint::default());
        }
        let mut endpoint = Endpoint {
            base_url: plan.base_url.as_ref().map(|(url, _)| url.clone()),
            model: plan.model.as_ref().map(|(model, _)| model.clone()),
            wire_api: plan.wire_api.as_ref().map(|(wire, _)| wire.clone()),
            env: plan
                .env
                .iter()
                .map(|(k, (v, _))| (k.clone(), v.clone()))
                .collect(),
            ..Endpoint::default()
        };
        let mut origin: Vec<String> = Vec::new();
        if let Some((url, scope)) = &plan.base_url {
            origin.push(format!("endpoint {url} ({} config)", scope.as_str()));
        }
        if let Some((kind, source, scope)) = &plan.credential {
            if runtime == RuntimeKind::Codex && *kind == CredentialKind::AuthToken {
                bail!(
                    "crew config: {agent_id} runs Codex, which takes an api_key, but the {} \
config gives it an auth_token",
                    scope.as_str()
                );
            }
            let secret = resolve_secret(source)
                .with_context(|| format!("crew config ({}): {}", scope.as_str(), kind.field()))?;
            let what = match kind {
                CredentialKind::ApiKey => {
                    endpoint.api_key = Some(secret);
                    "api key"
                }
                CredentialKind::AuthToken => {
                    endpoint.auth_token = Some(secret);
                    "auth token"
                }
            };
            origin.push(format!(
                "{what} from {} ({} config)",
                source.describe(),
                scope.as_str()
            ));
        }
        if let Some((model, scope)) = &plan.model {
            origin.push(format!("model {model} ({} config)", scope.as_str()));
        }
        if !plan.env.is_empty() {
            origin.push(format!("{} environment variable(s)", plan.env.len()));
        }
        for (name, (path, scope)) in &plan.env_files {
            let secret = read_token_file(path)
                .with_context(|| format!("crew config ({}): env_files {name}", scope.as_str()))?;
            endpoint.secret_env.push((name.clone(), secret));
        }
        if !plan.env_files.is_empty() {
            let names: Vec<&str> = plan.env_files.keys().map(String::as_str).collect();
            origin.push(format!("token file(s) for {}", names.join(", ")));
        }
        endpoint.origin = Some(origin.join(", "));
        Ok(endpoint)
    }

    /// The report `crew config` prints. Keys are masked.
    pub fn describe(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "Endpoints and keys for the agents crew launches. Your own claude and codex are not\n\
affected. Later files override earlier ones, field by field.\n"
        );
        for scope in Scope::ALL {
            let line = match self.layers.iter().find(|l| l.scope == scope) {
                Some(layer) => {
                    let state = match &layer.state {
                        LayerState::Missing => "not present".to_string(),
                        LayerState::Loaded(_) => match &layer.warning {
                            Some(warning) => format!("loaded — warning: {warning}"),
                            None => "loaded".to_string(),
                        },
                        LayerState::Rejected(reason) => format!("REJECTED: {reason}"),
                    };
                    format!("{}  ({state})", layer.path.display())
                }
                None => "(no home directory)".to_string(),
            };
            let _ = writeln!(out, "  {:<8} {line}", scope.as_str());
        }

        let members: BTreeSet<&String> = self.loaded().flat_map(|(_, c)| c.agents.keys()).collect();
        let sections = [
            ("claude", RuntimeKind::Claude),
            ("codex", RuntimeKind::Codex),
        ];
        for (name, runtime) in sections {
            let plan = self.plan(runtime, "", None);
            let _ = writeln!(out, "\n[{name}]");
            if plan.is_empty() {
                let _ = writeln!(
                    out,
                    "  nothing set: members use your own {name} login and endpoint"
                );
            } else {
                describe_plan(&mut out, &plan, runtime);
            }
        }
        for id in members {
            let mut plan = Plan::default();
            for (layer, config) in self.loaded() {
                if let Some(section) = config.agents.get(id) {
                    plan.apply(
                        section,
                        layer.scope,
                        layer.path.parent().unwrap_or(Path::new("/")),
                    );
                }
            }
            let _ = writeln!(out, "\n[agents.{id}]  (on top of its runtime's section)");
            describe_plan(&mut out, &plan, RuntimeKind::Shell);
        }
        out
    }
}

fn describe_plan(out: &mut String, plan: &Plan, runtime: RuntimeKind) {
    use std::fmt::Write;
    fn row(out: &mut String, name: &str, value: &str, scope: Scope) {
        let _ = writeln!(out, "  {name:<10} {value}  ({})", scope.as_str());
    }
    if let Some((url, scope)) = &plan.base_url {
        row(out, "base_url", url, *scope);
    }
    match (&plan.credential, runtime) {
        (Some((kind, source, scope)), _) => {
            row(out, kind.field(), &source.describe_for_operator(), *scope);
        }
        (None, RuntimeKind::Claude) if plan.base_url.is_some() => {
            let _ = writeln!(
                out,
                "  api_key    not set: members send your own Claude Code credentials to this endpoint"
            );
        }
        (None, RuntimeKind::Codex) if plan.base_url.is_some() => {
            let _ = writeln!(
                out,
                "  api_key    not set: the endpoint is called without credentials"
            );
        }
        _ => {}
    }
    if let Some((model, scope)) = &plan.model {
        row(out, "model", model, *scope);
    }
    if let Some((wire, scope)) = &plan.wire_api {
        row(out, "wire_api", wire, *scope);
    }
    for (name, (path, scope)) in &plan.env_files {
        row(
            out,
            "token",
            &format!("{name} from {}", path.display()),
            *scope,
        );
    }
    for (name, (_, scope)) in &plan.env {
        row(out, "env", &format!("{name} (value not shown)"), *scope);
    }
}

impl Source {
    /// For the event log: where the value comes from, nothing of the value.
    fn describe(&self) -> String {
        match self {
            Source::Literal(_) => "the config file".to_string(),
            Source::Env(name) => format!("${name}"),
            Source::File(path) => path.display().to_string(),
        }
    }

    /// For `crew config`: also whether it can be resolved from here.
    fn describe_for_operator(&self) -> String {
        match self {
            Source::Literal(secret) => mask(secret.expose()),
            Source::Env(name) => match std::env::var(name) {
                Ok(value) if !value.trim().is_empty() => {
                    format!(
                        "${name} = {} (read from the crew daemon's environment)",
                        mask(value.trim())
                    )
                }
                _ => format!("${name} — not set in this shell"),
            },
            Source::File(path) => match std::fs::read_to_string(path) {
                Ok(text) if !text.trim().is_empty() => {
                    format!("{} = {}", path.display(), mask(text.trim()))
                }
                Ok(_) => format!("{} — empty", path.display()),
                Err(e) => format!("{} — {e}", path.display()),
            },
        }
    }
}

/// The most a token file may hold.
const MAX_TOKEN_FILE: u64 = 64 * 1024;

/// A token the operator keeps in a file for one agent's environment (a
/// `claude setup-token` token, an API key). The file is opened without
/// following a link anywhere in its path, and must be a regular file of this
/// user with mode 0600. The error never carries
/// the contents.
fn read_token_file(path: &Path) -> Result<Secret> {
    use std::io::Read;
    let flags = libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;
    let fd = match crate::os::open_no_symlinks(libc::AT_FDCWD, path, flags, 0, false) {
        Ok(fd) => fd,
        Err(e) => bail!("{}: {e} (no link may be followed)", path.display()),
    };
    let file = std::fs::File::from(fd);
    let meta = file.metadata()?;
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me {
        bail!("{} is not owned by you", path.display());
    }
    if meta.mode() & 0o777 != 0o600 {
        bail!(
            "{} can be read or written by other users (chmod 600)",
            path.display()
        );
    }
    if meta.len() > MAX_TOKEN_FILE {
        bail!("{} is larger than {MAX_TOKEN_FILE} bytes", path.display());
    }
    let mut text = String::new();
    file.take(MAX_TOKEN_FILE)
        .read_to_string(&mut text)
        .map_err(|_| anyhow!("{} is not text", path.display()))?;
    let text = text.trim();
    if text.is_empty() {
        bail!("{} is empty", path.display());
    }
    Ok(Secret(text.to_string()))
}

fn resolve_secret(source: &Source) -> Result<Secret> {
    let value = match source {
        Source::Literal(secret) => return Ok(secret.clone()),
        Source::Env(name) => std::env::var(name).map_err(|_| {
            anyhow!(
                "${name} is not set in the crew daemon's environment (export it, then \
`crew stop` and start the team again)"
            )
        })?,
        Source::File(path) => {
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
        }
    };
    let value = value.trim();
    if value.is_empty() {
        bail!("{} is empty", source.describe());
    }
    Ok(Secret(value.to_string()))
}

/// A commented starting point for `crew config init`.
pub const TEMPLATE: &str = r#"# crew: endpoints and keys for the agents crew launches.
# Your own `claude` and `codex` are not affected by this file.
#
# Three files are read; later ones override earlier ones field by field:
#   global   /etc/crew/config.toml
#   user     ~/.config/crew/config.toml
#   project  <workspace>/.crew/config.toml   (never committed)
# `crew config` shows what is in effect and where each value comes from.
# Setting a field to "" takes back what an earlier file set.

# [claude]                               # every Claude Code member
# base_url = "https://gateway.example.com"
# api_key_env = "TEAM_ANTHROPIC_KEY"     # or api_key = "sk-…", or api_key_file = "~/.secrets/anthropic"
# # auth_token_env = "GATEWAY_TOKEN"     # instead of an api key: sent as "Authorization: Bearer"
# model = "claude-sonnet-4-5"            # every member on this endpoint runs this model
#
# [claude.env]                           # extra environment for those sessions
# ANTHROPIC_DEFAULT_HAIKU_MODEL = "my-small-model"

# [codex]                                # every Codex member
# base_url = "https://gateway.example.com/v1"
# api_key_env = "TEAM_OPENAI_KEY"
# wire_api = "responses"                 # or "chat"
# model = "gpt-5.1"

# [agents.claude-3]                      # one member and its subagents; wins over [claude] / [codex]
# base_url = "http://localhost:4000"
# api_key = ""                           # no key for this one
"#;

/// Write the template for `scope`, unless a file is already there.
pub fn init(scope: Scope, workspace: &Path) -> Result<PathBuf> {
    let path = scope
        .path(workspace)
        .ok_or_else(|| anyhow!("no home directory to put the user config in"))?;
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    crew_core::paths::write_private_file(&path, TEMPLATE.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn layer(scope: Scope, toml_text: &str) -> Layer {
        let config: FileConfig = toml::from_str(toml_text).unwrap();
        config.validate(Path::new("/cfg")).unwrap();
        Layer {
            scope,
            path: PathBuf::from(format!("/cfg/{}/config.toml", scope.as_str())),
            state: LayerState::Loaded(Box::new(config)),
            warning: None,
        }
    }

    fn layers(global: &str, user: &str, project: &str) -> Layers {
        Layers {
            layers: vec![
                layer(Scope::Global, global),
                layer(Scope::User, user),
                layer(Scope::Project, project),
            ],
        }
    }

    #[test]
    fn later_layers_override_earlier_ones_field_by_field() {
        let l = layers(
            r#"
            [claude]
            base_url = "https://global.example"
            api_key = "sk-global-000000000000"
            model = "global-model"
            [claude.env]
            A = "global"
            B = "global"
            "#,
            r#"
            [claude]
            base_url = "https://user.example"
            [claude.env]
            B = "user"
            "#,
            r#"
            [claude]
            api_key = "sk-project-11111111111"
            "#,
        );
        let e = l.resolve(RuntimeKind::Claude, "claude-1", None).unwrap();
        assert_eq!(e.base_url.as_deref(), Some("https://user.example"));
        assert_eq!(
            e.api_key.as_ref().unwrap().expose(),
            "sk-project-11111111111"
        );
        assert_eq!(e.model.as_deref(), Some("global-model"));
        assert_eq!(e.env["A"], "global");
        assert_eq!(e.env["B"], "user");
        let origin = e.origin.unwrap();
        assert!(
            origin.contains("endpoint https://user.example (user config)"),
            "{origin}"
        );
        assert!(
            origin.contains("api key from the config file (project config)"),
            "{origin}"
        );
        assert!(!origin.contains("sk-"), "{origin}");
        // The other runtime and scripted agents are untouched.
        assert!(l
            .resolve(RuntimeKind::Codex, "codex-1", None)
            .unwrap()
            .is_default());
        assert!(l
            .resolve(RuntimeKind::Shell, "claude-1", None)
            .unwrap()
            .is_default());
    }

    #[test]
    fn a_later_layer_can_replace_or_take_back_a_credential() {
        // A token replaces a key (they are the same thing: the credential)…
        let l = layers(
            "[claude]\napi_key = \"sk-global-000000000000\"\nbase_url = \"https://g.example\"",
            "[claude]\nauth_token = \"bearer-user\"",
            "",
        );
        let e = l.resolve(RuntimeKind::Claude, "a", None).unwrap();
        assert!(e.api_key.is_none());
        assert_eq!(e.auth_token.unwrap().expose(), "bearer-user");
        // …and an empty value takes it, or the endpoint, back.
        let l = layers(
            "[claude]\napi_key = \"sk-global-000000000000\"\nbase_url = \"https://g.example\"",
            "[claude]\nauth_token = \"bearer-user\"",
            "[claude]\napi_key = \"\"\nbase_url = \"\"",
        );
        assert!(l
            .resolve(RuntimeKind::Claude, "a", None)
            .unwrap()
            .is_default());
    }

    #[test]
    fn member_sections_win_over_runtime_sections_and_cover_subagents() {
        let l = layers(
            "",
            r#"
            [agents.claude-3]
            base_url = "http://localhost:4000"
            api_key = ""
            "#,
            r#"
            [claude]
            base_url = "https://project.example"
            api_key = "sk-project-11111111111"
            [codex]
            base_url = "https://codex.example/v1"
            api_key = "sk-codex-2222222222222"
            wire_api = "chat"
            "#,
        );
        let other = l.resolve(RuntimeKind::Claude, "claude-1", None).unwrap();
        assert_eq!(other.base_url.as_deref(), Some("https://project.example"));
        assert!(other.api_key.is_some());
        // A member section in the user file beats the runtime section in the
        // project file: the more specific setting wins.
        let three = l.resolve(RuntimeKind::Claude, "claude-3", None).unwrap();
        assert_eq!(three.base_url.as_deref(), Some("http://localhost:4000"));
        assert!(three.api_key.is_none());
        let helper = l
            .resolve(RuntimeKind::Claude, "claude-3-s1", Some("claude-3"))
            .unwrap();
        assert_eq!(helper.base_url, three.base_url);
        let codex = l.resolve(RuntimeKind::Codex, "codex-1", None).unwrap();
        assert_eq!(codex.wire_api.as_deref(), Some("chat"));
        assert_eq!(codex.api_key.unwrap().expose(), "sk-codex-2222222222222");
    }

    #[test]
    fn keys_can_come_from_the_environment_or_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("key"), "  sk-from-file-3333333333\n").unwrap();
        let config: FileConfig = toml::from_str(
            "[claude]\napi_key_file = \"key\"\n[codex]\napi_key_env = \"CREW_TEST_ENDPOINT_KEY_UNSET\"",
        )
        .unwrap();
        let l = Layers {
            layers: vec![Layer {
                scope: Scope::User,
                path: tmp.path().join("config.toml"),
                state: LayerState::Loaded(Box::new(config)),
                warning: None,
            }],
        };
        // Relative to the config file, whitespace trimmed.
        let e = l.resolve(RuntimeKind::Claude, "a", None).unwrap();
        assert_eq!(e.api_key.unwrap().expose(), "sk-from-file-3333333333");
        // A key that cannot be resolved stops the launch: it must not fall
        // back to the user's own login.
        let error = format!(
            "{:#}",
            l.resolve(RuntimeKind::Codex, "b", None).unwrap_err()
        );
        assert!(
            error.contains("$CREW_TEST_ENDPOINT_KEY_UNSET is not set"),
            "{error}"
        );
    }

    #[test]
    fn malformed_sections_are_refused() {
        let bad = [
            ("[claude]\nbase_url = \"gateway.example\"", "http"),
            (
                "[claude]\napi_key = \"a\"\nauth_token = \"b\"",
                "one credential",
            ),
            (
                "[claude]\napi_key = \"a\"\napi_key_env = \"B\"",
                "one credential",
            ),
            ("[claude]\nwire_api = \"responses\"", "Codex setting"),
            ("[codex]\nauth_token = \"x\"", "use api_key"),
            ("[codex]\nwire_api = \"grpc\"", "wire_api"),
            ("[claude.env]\n\"A B\" = \"x\"", "environment variable name"),
            ("[claude.env]\nCREW_IDENTITY = \"x\"", "reserved"),
            ("[agents.\"Not An Id\"]\nmodel = \"x\"", "not an agent id"),
            (
                "[claude]\napi_key_env = \"not a name\"",
                "environment variable name",
            ),
        ];
        for (text, expected) in bad {
            let config: FileConfig = toml::from_str(text).unwrap();
            let error = format!("{:#}", config.validate(Path::new("/")).unwrap_err());
            assert!(error.contains(expected), "{text}: {error}");
        }
        // Unknown keys are typos, not extensions.
        assert!(toml::from_str::<FileConfig>("[claude]\nbase_ulr = \"https://x\"").is_err());
        assert!(toml::from_str::<FileConfig>("[gemini]\nmodel = \"x\"").is_err());
    }

    #[test]
    fn files_others_can_write_and_files_from_the_repository_are_rejected() {
        if crate::git_test_support::isolate_git_fixture(
            "endpoints::tests::files_others_can_write_and_files_from_the_repository_are_rejected",
        ) {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let path = Scope::Project.path(ws).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[claude]\nbase_url = \"https://p.example\"\napi_key = \"sk-literal-4444444444444\"",
        )
        .unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let layer = load_layer(Scope::Project, path.clone(), ws);
        assert!(
            matches!(&layer.state, LayerState::Rejected(r) if r.contains("writable by other users"))
        );
        let all = Layers {
            layers: vec![layer],
        };
        let error = all
            .resolve(RuntimeKind::Claude, "a", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("crew config (project)"), "{error}");

        // Readable by others: used, with a warning about the key in it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let layer = load_layer(Scope::Project, path.clone(), ws);
        assert!(matches!(layer.state, LayerState::Loaded(_)));
        assert!(layer.warning.as_deref().unwrap().contains("chmod 600"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_layer(Scope::Project, path.clone(), ws)
            .warning
            .is_none());

        // Committed to the repository: not this user's decision.
        let git = |args: &[&str]| {
            assert!(crate::git_test_support::git(ws)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        };
        git(&["init", "-q"]);
        assert!(matches!(
            load_layer(Scope::Project, path.clone(), ws).state,
            LayerState::Loaded(_)
        ));
        git(&["add", "-f", ".crew/config.toml"]);
        let layer = load_layer(Scope::Project, path.clone(), ws);
        assert!(matches!(&layer.state, LayerState::Rejected(r) if r.contains("tracked by git")));

        // A missing file is simply not there.
        let missing = load_layer(Scope::User, ws.join("nope.toml"), ws);
        assert!(matches!(missing.state, LayerState::Missing));
    }

    #[test]
    fn the_report_lists_env_names_without_their_values() {
        let l = layers(
            "",
            "[claude.env]\nANTHROPIC_AUTH_HELPER = \"super-secret-value-123\"",
            "",
        );
        let report = l.describe();
        assert!(report.contains("ANTHROPIC_AUTH_HELPER"), "{report}");
        assert!(!report.contains("super-secret-value-123"), "{report}");
    }

    fn token_file(dir: &Path, name: &str, text: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn token_files_become_environment_secrets_for_their_own_agents_only() {
        let dir = tempfile::tempdir().unwrap();
        let claude = token_file(dir.path(), "claude", "oauth-token-claude\n", 0o600);
        let codex = token_file(dir.path(), "codex", "sk-codex-token", 0o600);
        let pi = token_file(dir.path(), "pi", "pi-key", 0o600);
        let l = layers(
            "",
            &format!(
                "[claude.env_files]\nCLAUDE_CODE_OAUTH_TOKEN = \"{}\"\n\
[codex.env_files]\nOPENAI_API_KEY = \"{}\"\n\
[agents.pi-1.env_files]\nANTHROPIC_API_KEY = \"{}\"",
                claude.display(),
                codex.display(),
                pi.display()
            ),
            "",
        );
        let secrets = |e: Endpoint| -> Vec<(String, String)> {
            e.secret_env
                .iter()
                .map(|(n, s)| (n.clone(), s.expose().to_string()))
                .collect()
        };
        let c = l.resolve(RuntimeKind::Claude, "claude-1", None).unwrap();
        // Never in the plain environment or the origin that reaches the event log.
        assert!(c.env.is_empty());
        assert!(!c
            .origin
            .as_deref()
            .unwrap_or("")
            .contains("oauth-token-claude"));
        assert_eq!(
            secrets(c),
            [(
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "oauth-token-claude".to_string()
            )]
        );
        let x = l.resolve(RuntimeKind::Codex, "codex-1", None).unwrap();
        assert_eq!(
            secrets(x),
            [("OPENAI_API_KEY".to_string(), "sk-codex-token".to_string())]
        );
        // pi has no section of its own: one agent's table, nobody else's.
        let p = l.resolve(RuntimeKind::Pi, "pi-1", None).unwrap();
        assert_eq!(
            secrets(p),
            [("ANTHROPIC_API_KEY".to_string(), "pi-key".to_string())]
        );
        assert!(l
            .resolve(RuntimeKind::Pi, "pi-2", None)
            .unwrap()
            .secret_env
            .is_empty());
        // The report names the variable and the file, never a value.
        assert!(l.describe().contains("CLAUDE_CODE_OAUTH_TOKEN from "));
        assert!(!l.describe().contains("oauth-token-claude") && !l.describe().contains("pi-key"));
    }

    #[test]
    fn a_token_file_is_read_only_when_it_is_private_plain_and_not_a_link() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let resolve = |path: &Path| {
            layers(
                "",
                &format!(
                    "[claude.env_files]\nCLAUDE_CODE_OAUTH_TOKEN = \"{}\"",
                    path.display()
                ),
                "",
            )
            .resolve(RuntimeKind::Claude, "claude-1", None)
        };
        let good = token_file(dir.path(), "good", "tok", 0o600);
        assert!(resolve(&good).is_ok());
        for (name, mode) in [("group", 0o640), ("world", 0o604), ("writable", 0o620)] {
            let path = token_file(dir.path(), name, "tok-secret-value", mode);
            let err = format!("{:#}", resolve(&path).unwrap_err());
            assert!(err.contains("chmod 600"), "{name}: {err}");
            assert!(!err.contains("tok-secret-value"), "{err}");
        }
        let link = dir.path().join("link");
        symlink(&good, &link).unwrap();
        assert!(resolve(&link).is_err(), "followed a link");
        // A link in a parent directory is not followed either.
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        token_file(&real, "t", "tok", 0o600);
        symlink(&real, dir.path().join("alias")).unwrap();
        assert!(resolve(&dir.path().join("alias/t")).is_err());
        assert!(resolve(dir.path()).is_err(), "a directory");
        assert!(resolve(&token_file(dir.path(), "empty", "  \n", 0o600)).is_err());
        assert!(resolve(&token_file(dir.path(), "big", &"x".repeat(70_000), 0o600)).is_err());
        assert!(resolve(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn a_name_is_never_in_both_the_plain_and_the_secret_environment() {
        let dir = tempfile::tempdir().unwrap();
        let secret = token_file(dir.path(), "t", "tok-secret", 0o600);
        // A runtime-wide plain value and a more specific agent token file, and
        // the reverse: the more specific one wins and the other is gone.
        let l = layers(
            "",
            &format!(
                "[claude.env]\nCLAUDE_CODE_OAUTH_TOKEN = \"plain-value\"\nKEEP = \"1\"\n\
[agents.c1.env_files]\nCLAUDE_CODE_OAUTH_TOKEN = \"{p}\"\n\
[claude.env_files]\nOTHER = \"{p}\"\n[agents.c2.env]\nOTHER = \"plain-wins\"",
                p = secret.display()
            ),
            "",
        );
        let c1 = l.resolve(RuntimeKind::Claude, "c1", None).unwrap();
        assert!(
            !c1.env.contains_key("CLAUDE_CODE_OAUTH_TOKEN"),
            "{:?}",
            c1.env
        );
        assert_eq!(
            c1.secret_env
                .iter()
                .filter(|(n, _)| n == "CLAUDE_CODE_OAUTH_TOKEN")
                .count(),
            1
        );
        assert_eq!(c1.env.get("KEEP").map(String::as_str), Some("1"));
        let c2 = l.resolve(RuntimeKind::Claude, "c2", None).unwrap();
        assert_eq!(c2.env.get("OTHER").map(String::as_str), Some("plain-wins"));
        assert!(!c2.secret_env.iter().any(|(n, _)| n == "OTHER"));
        // Every runtime-chosen home is the daemon's to set.
        for name in ["CLAUDE_CONFIG_DIR", "CODEX_HOME", "PI_CODING_AGENT_DIR"] {
            let config: FileConfig =
                toml::from_str(&format!("[claude.env_files]\n{name} = \"/x\"")).unwrap();
            assert!(config.validate(Path::new("/cfg")).is_err(), "{name}");
        }
    }

    #[test]
    fn a_token_file_must_be_mode_0600_and_the_users_own() {
        let dir = tempfile::tempdir().unwrap();
        let resolve = |path: &Path| {
            layers(
                "",
                &format!("[claude.env_files]\nT = \"{}\"", path.display()),
                "",
            )
            .resolve(RuntimeKind::Claude, "claude-1", None)
        };
        assert!(resolve(&token_file(dir.path(), "ok", "tok", 0o600)).is_ok());
        for (name, mode) in [("ro", 0o400), ("exec", 0o700), ("rwx", 0o500)] {
            let err = format!(
                "{:#}",
                resolve(&token_file(dir.path(), name, "tok", mode)).unwrap_err()
            );
            assert!(err.contains("chmod 600"), "{name}: {err}");
        }
        // A file that cannot even be opened is refused too.
        assert!(resolve(&token_file(dir.path(), "unreadable", "tok", 0o100)).is_err());
        // Not root's either: a file owned by root is not the user's.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            let err = format!("{:#}", resolve(Path::new("/etc/shadow")).unwrap_err());
            assert!(!err.contains("tok"), "{err}");
        }
    }

    #[test]
    fn token_file_names_must_be_plain_variables_that_crew_does_not_own() {
        for bad in ["CREW_IDENTITY", "1BAD", "A-B", "LD_PRELOAD"] {
            let config: FileConfig =
                toml::from_str(&format!("[claude.env_files]\n\"{bad}\" = \"/x\"")).unwrap();
            assert!(config.validate(Path::new("/cfg")).is_err(), "{bad}");
        }
        let both: FileConfig =
            toml::from_str("[claude.env]\nT = \"1\"\n[claude.env_files]\nT = \"/x\"").unwrap();
        assert!(both.validate(Path::new("/cfg")).is_err());
    }

    #[test]
    fn the_readmes_sandbox_credentials_example_resolves_to_keys_for_both_runtimes() {
        // The example in README.md, parsed and resolved as an agent launch would.
        let readme = include_str!("../../../README.md");
        let marker = "<!-- sandbox-credentials-example -->\n```toml\n";
        let start = readme.find(marker).expect("the example") + marker.len();
        let example = &readme[start..start + readme[start..].find("```").unwrap()];
        std::env::set_var("TEAM_ANTHROPIC_KEY", "sk-ant-from-the-environment-0000");
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("openai");
        std::fs::write(&secret, "sk-openai-from-a-file-0000\n").unwrap();
        std::fs::set_permissions(&secret, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        let example = example.replace("~/.secrets/openai", &secret.to_string_lossy());
        let l = layers("", &example, "");
        let claude = l.resolve(RuntimeKind::Claude, "claude-1", None).unwrap();
        assert_eq!(
            claude.api_key.as_ref().map(|k| k.expose()),
            Some("sk-ant-from-the-environment-0000")
        );
        let codex = l.resolve(RuntimeKind::Codex, "codex-1", None).unwrap();
        assert_eq!(
            codex.api_key.as_ref().map(|k| k.expose()),
            Some("sk-openai-from-a-file-0000")
        );
    }

    #[test]
    fn the_readmes_token_file_example_resolves_to_environment_secrets() {
        let readme = include_str!("../../../README.md");
        let marker = "<!-- sandbox-token-files-example -->\n```toml\n";
        let start = readme.find(marker).expect("the example") + marker.len();
        let example = &readme[start..start + readme[start..].find("```").unwrap()];
        let dir = tempfile::tempdir().unwrap();
        let claude = token_file(dir.path(), "claude-oauth", "oauth-readme", 0o600);
        let openai = token_file(dir.path(), "openai", "sk-readme", 0o600);
        let example = example
            .replace("~/.secrets/claude-oauth", &claude.to_string_lossy())
            .replace("~/.secrets/openai", &openai.to_string_lossy());
        let l = layers("", &example, "");
        let c = l.resolve(RuntimeKind::Claude, "claude-1", None).unwrap();
        assert_eq!(c.secret_env[0].0, "CLAUDE_CODE_OAUTH_TOKEN");
        assert_eq!(c.secret_env[0].1.expose(), "oauth-readme");
        assert!(l
            .resolve(RuntimeKind::Claude, "claude-2", None)
            .unwrap()
            .secret_env
            .is_empty());
        let x = l.resolve(RuntimeKind::Codex, "codex-1", None).unwrap();
        assert_eq!(x.secret_env[0].0, "OPENAI_API_KEY");
    }

    #[test]
    fn the_report_shows_sources_and_never_a_whole_key() {
        let l = layers(
            "[claude]\nbase_url = \"https://global.example\"",
            "[claude]\napi_key = \"sk-ant-user-key-abcdefgh1234\"\n[codex]\nbase_url = \"http://localhost:11434/v1\"",
            "[agents.claude-3]\nmodel = \"small\"\napi_key = \"short\"",
        );
        let report = l.describe();
        assert!(
            report.contains("base_url   https://global.example  (global)"),
            "{report}"
        );
        assert!(report.contains("api_key    sk-a…1234  (user)"), "{report}");
        assert!(
            report.contains("the endpoint is called without credentials"),
            "{report}"
        );
        assert!(report.contains("[agents.claude-3]"), "{report}");
        assert!(report.contains("model      small  (project)"), "{report}");
        assert!(
            !report.contains("user-key") && !report.contains("short"),
            "{report}"
        );
        assert_eq!(mask("0123456789abcdef"), "0123…cdef");
        assert_eq!(mask("0123456789"), "…");
        assert_eq!(format!("{:?}", Secret("sk-secret".into())), "Secret(…)");

        // The template is a valid (empty) config.
        let config: FileConfig = toml::from_str(TEMPLATE).unwrap();
        assert!(config == FileConfig::default());
    }
}
