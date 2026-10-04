//! The model catalog: what each model is good for, so a team can put the
//! right model on each job across Claude Code and Codex.
//!
//! The catalog belongs to crew, not to a project. It is compiled in
//! (`models.toml`) and can be extended or corrected in two files, a later
//! one replacing entries of the same name:
//!
//! | origin   | file                                        |
//! |----------|---------------------------------------------|
//! | built-in | compiled in                                 |
//! | global   | `/etc/crew/models.toml`                     |
//! | user     | `$XDG_CONFIG_HOME/crew/models.toml`         |
//!
//! It is used in two places: the orchestrator's briefing lists the models
//! available on the machine with what each is for, and naming a catalog
//! model for a member (`agent_add`, a team file, a style) is enough to put
//! that member on the right CLI.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use crew_core::rpc::ModelChoice;
use crew_core::RuntimeKind;
use serde::Deserialize;

use crate::endpoints::Scope;

const BUILT_IN: &str = include_str!("../models.toml");
const FILE_NAME: &str = "models.toml";

/// What `crew models init` writes.
pub const TEMPLATE: &str = r#"# Your additions and corrections to crew's model catalog.
# An entry replaces the built-in entry of the same name; `crew models` shows
# the result and `crew models show <name>` one entry in full.

# [models.my-model]
# runtime = "codex"                 # claude | codex | pi: the CLI that runs it (a pi model
#                                   # is named as pi names it, e.g. anthropic/claude-opus-4-6)
# name = "My Model"
# tier = "balanced"                 # frontier | strong | balanced | fast
# price = "$1 / $4 per MTok"
# summary = "One line on what it is."
# good_for = ["what to give it"]
# avoid_for = ["what not to give it"]
# long_context = true               # it has a 1M-token context window; start it in that

# [models.gpt-6-luna]               # take a built-in model out of the catalog
# disabled = true

# [[picks]]                         # replace (or add) the pick for a kind of work
# work = "plan and design review"
# models = ["gpt-6-astra", "fable"]
# why = "Your reason."
"#;

/// One model's profile.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    /// The name members are given and the CLI is started with.
    #[serde(skip)]
    pub key: String,
    pub runtime: Option<RuntimeKind>,
    #[serde(default)]
    pub name: String,
    /// The vendor's full model id; accepted in place of the name.
    #[serde(default)]
    pub id: String,
    /// The model has a 1M-token context window, which crew asks for
    /// (Claude Code: the model's `[1m]` variant; Codex: a
    /// `model_context_window` override).
    #[serde(default)]
    pub long_context: bool,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// `frontier`, `strong`, `balanced` or `fast`.
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub speed: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub good_for: Vec<String>,
    #[serde(default)]
    pub avoid_for: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub disabled: bool,
}

/// The models to prefer for a kind of work, best first.
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Pick {
    pub work: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub why: String,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    as_of: Option<String>,
    #[serde(default)]
    models: toml::Table,
    #[serde(default)]
    picks: Vec<Pick>,
}

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    /// When the built-in profiles were last checked against their sources.
    pub as_of: String,
    /// `(profile, where it comes from)`, in display order.
    pub models: Vec<(ModelProfile, &'static str)>,
    pub picks: Vec<Pick>,
    /// Override files that could not be used.
    pub problems: Vec<String>,
}

/// The runtime and model a member ends up with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub runtime: RuntimeKind,
    pub model: Option<String>,
    /// Set when the model decided the runtime against what was asked for.
    pub note: Option<String>,
}

fn tier_rank(tier: &str) -> usize {
    ["frontier", "strong", "balanced", "fast"]
        .iter()
        .position(|t| *t == tier)
        .unwrap_or(4)
}

fn runtime_rank(runtime: Option<RuntimeKind>) -> usize {
    match runtime {
        Some(RuntimeKind::Claude) => 0,
        Some(RuntimeKind::Codex) => 1,
        Some(RuntimeKind::Pi) => 2,
        _ => 3,
    }
}

pub fn runtime_label(runtime: RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Claude => "Claude Code",
        RuntimeKind::Codex => "Codex",
        RuntimeKind::Pi => "pi",
        RuntimeKind::Shell => "shell",
    }
}

/// The override file of a scope (`None` for the project scope: the catalog
/// is not per project).
pub fn path(scope: Scope) -> Option<PathBuf> {
    if scope == Scope::Project {
        return None;
    }
    Some(scope.path(Path::new("/"))?.parent()?.join(FILE_NAME))
}

impl Catalog {
    /// The built-in catalog alone.
    #[cfg(test)]
    pub(crate) fn built_in() -> Self {
        let mut catalog = Catalog::default();
        catalog.apply(BUILT_IN, "built-in").expect("the built-in model catalog is valid");
        catalog
    }

    /// The built-in catalog with the global and user files applied.
    pub fn load() -> Self {
        let mut catalog = Catalog::default();
        catalog
            .apply(BUILT_IN, "built-in")
            .expect("the built-in model catalog is valid");
        for scope in [Scope::Global, Scope::User] {
            let Some(path) = path(scope) else { continue };
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if let Err(e) = catalog.apply(&text, scope.as_str()) {
                catalog.problems.push(format!("{}: {e:#}", path.display()));
            }
        }
        catalog
    }

    /// Merge one catalog file. Nothing is changed if the file is not valid.
    fn apply(&mut self, text: &str, origin: &'static str) -> Result<()> {
        let file: CatalogFile = toml::from_str(text).map_err(|e| anyhow!("{}", e.message()))?;
        let mut models = self.models.clone();
        for (key, value) in file.models {
            let mut profile: ModelProfile = value
                .try_into()
                .map_err(|e: toml::de::Error| anyhow!("[models.{key}]: {}", e.message()))?;
            if key.trim().is_empty() || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
                bail!("[models.{key}]: not a model name");
            }
            profile.key = key.clone();
            models.retain(|(m, _)| m.key != key);
            if profile.disabled {
                continue;
            }
            match profile.runtime {
                Some(RuntimeKind::Claude | RuntimeKind::Codex | RuntimeKind::Pi) => {}
                _ => bail!("[models.{key}]: runtime must be \"claude\", \"codex\" or \"pi\""),
            }
            if profile.name.is_empty() {
                profile.name = key.clone();
            }
            models.push((profile, origin));
        }
        let mut picks = self.picks.clone();
        for pick in file.picks {
            if pick.work.trim().is_empty() || pick.models.is_empty() {
                bail!("a pick needs `work` and at least one model");
            }
            match picks.iter_mut().find(|p| p.work == pick.work) {
                Some(existing) => *existing = pick,
                None => picks.push(pick),
            }
        }
        // Claude before Codex, strongest first: the order of every listing.
        models.sort_by_key(|(m, _)| (runtime_rank(m.runtime), tier_rank(&m.tier)));
        self.models = models;
        self.picks = picks;
        if let Some(as_of) = file.as_of.filter(|_| origin == "built-in") {
            self.as_of = as_of;
        }
        Ok(())
    }

    /// Look a model up by its name, full id or an alias.
    pub fn find(&self, name: &str) -> Option<&ModelProfile> {
        let name = name.trim();
        self.models.iter().map(|(m, _)| m).find(|m| {
            m.key.eq_ignore_ascii_case(name)
                || (!m.id.is_empty() && m.id.eq_ignore_ascii_case(name))
                || m.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
        })
    }

    /// The proper name of every model, by each name it goes by (its key,
    /// its id, its aliases; lowercase): `opus` and `claude-opus-5-5` are
    /// both `Opus 5.5`.
    pub fn display_names(&self) -> std::collections::HashMap<String, String> {
        let mut names = std::collections::HashMap::new();
        for (model, _) in &self.models {
            let id = if model.id.is_empty() { &model.key } else { &model.id };
            let name = crew_core::types::model_display_name(id);
            for known in std::iter::once(&model.key).chain(std::iter::once(&model.id)).chain(&model.aliases) {
                if !known.is_empty() {
                    names.insert(known.to_lowercase(), name.clone());
                }
            }
        }
        names
    }

    /// Decide a member's runtime and model.
    ///
    /// A model the catalog knows decides the runtime: a model only runs on
    /// its own CLI, whatever runtime was written next to it. Without a
    /// model, the team's default model is used only if it belongs to the
    /// member's runtime.
    pub fn assign(
        &self,
        runtime: Option<RuntimeKind>,
        model: Option<&str>,
        default_runtime: RuntimeKind,
        default_model: Option<&str>,
    ) -> Assignment {
        if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
            return match self.find(model) {
                Some(profile) => {
                    let own = profile.runtime.unwrap_or(default_runtime);
                    let note = runtime.filter(|asked| *asked != own).map(|asked| {
                        format!(
                            "{} runs on {}, not {}",
                            profile.key,
                            runtime_label(own),
                            runtime_label(asked)
                        )
                    });
                    Assignment {
                        runtime: own,
                        model: Some(profile.key.clone()),
                        note,
                    }
                }
                None => Assignment {
                    runtime: runtime.unwrap_or(default_runtime),
                    model: Some(model.to_string()),
                    note: None,
                },
            };
        }
        let runtime = runtime.unwrap_or(default_runtime);
        let model = default_model.filter(|m| match self.find(m) {
            Some(profile) => profile.runtime == Some(runtime),
            // An unknown name is taken to belong to the default runtime.
            None => runtime == default_runtime,
        });
        Assignment {
            runtime,
            model: model.map(|m| self.find(m).map_or_else(|| m.to_string(), |p| p.key.clone())),
            note: None,
        }
    }

    /// The catalog as the orchestrator's briefing presents it: only what can
    /// be launched on this machine. Empty when nothing is.
    pub fn briefing(&self, available: &Availability) -> String {
        let models: Vec<&ModelProfile> = self
            .models
            .iter()
            .map(|(m, _)| m)
            .filter(|m| available.check(m).is_ok())
            .collect();
        if models.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "\n## Models\n\
Each member runs on one model. Choose it per member to fit the member's role and pass it as \
`model` to agent_add; crew starts the member on the CLI the model belongs to (Claude Code or \
Codex), so you need not set `runtime`. Available here:\n",
        );
        for m in &models {
            let mut facts = vec![runtime_label(m.runtime.unwrap_or(RuntimeKind::Claude)).to_string()];
            facts.extend([&m.tier, &m.price, &m.speed].into_iter().filter(|f| !f.is_empty()).cloned());
            let _ = write!(out, "- {} ({}): {}", m.key, facts.join(", "), m.summary);
            if !m.good_for.is_empty() {
                let _ = write!(out, " Good for: {}.", m.good_for.join("; "));
            }
            if !m.avoid_for.is_empty() {
                let _ = write!(out, " Not for: {}.", m.avoid_for.join("; "));
            }
            out.push('\n');
        }
        let known: HashSet<&str> = models.iter().map(|m| m.key.as_str()).collect();
        let picks: Vec<String> = self
            .picks
            .iter()
            .filter_map(|pick| {
                let usable: Vec<&str> = pick
                    .models
                    .iter()
                    .filter_map(|name| self.find(name))
                    .map(|m| m.key.as_str())
                    .filter(|key| known.contains(key))
                    .collect();
                (!usable.is_empty()).then(|| {
                    // The reason is about the full list; with models missing
                    // it could point at one that is not there.
                    let complete = usable.len() == pick.models.len();
                    let why = if pick.why.is_empty() || !complete {
                        String::new()
                    } else {
                        format!(" ({})", pick.why)
                    };
                    format!("- {}: {}{why}\n", pick.work, usable.join(", then "))
                })
            })
            .collect();
        if !picks.is_empty() {
            out.push_str("\nWhich to pick, best first:\n");
            out.extend(picks);
        }
        if available.pi {
            out.push_str(
                "\nA member may also run on pi (`runtime: \"pi\"`), which runs any model its own \
providers offer, named as pi names it (`pi --list-models`: e.g. `anthropic/claude-opus-4-6`); \
only the models above are described here.\n",
            );
        }
        let families: HashSet<_> = models.iter().filter_map(|m| m.runtime).collect();
        out.push_str("\nWhen you staff the team:\n");
        if families.len() > 1 {
            out.push_str(
                "- Have work checked by the other model family: a reviewer, skeptic or analyst \
should not run on the same vendor's models as the author of what it checks.\n",
            );
        }
        out.push_str(
            "- Keep frontier models for the few members whose judgment decides the outcome; \
routine and bulk work goes to cheaper models.\n\
- A member you give no model gets the team default.\n",
        );
        // What to take for a model whose window is used up, with an example
        // from what is on offer here.
        let twin = models.iter().find_map(|a| {
            let b = models.iter().find(|b| !a.tier.is_empty() && b.tier == a.tier && b.runtime != a.runtime)?;
            Some(format!(" (for {}, {})", a.key, b.key))
        });
        let closest = match twin {
            Some(example) => format!("the same tier of the other family first{example}, then the next tier down"),
            None => "the same tier if there is one, else the next tier down".to_string(),
        };
        let _ = writeln!(
            out,
            "- A model whose usage window is used up cannot be started until the window resets. \
Your kickoff says what is out, and usage_status says it at any time. For a role that needs a \
model that is out, take the closest one that is not: {closest}."
        );
        out
    }

    /// What an agent can be put on from this machine, for the dashboard's
    /// picker: Claude Code's models, then Codex's, then pi's — its own
    /// default and the models it reaches (`pi_models`: its listing, or why
    /// pi cannot be used) — each marked with why it cannot be launched here
    /// when it cannot.
    pub fn choices(&self, available: &Availability, pi_models: Result<Vec<String>, String>) -> Vec<ModelChoice> {
        let mut profiles: Vec<&ModelProfile> = self
            .models
            .iter()
            .map(|(m, _)| m)
            .filter(|m| !m.disabled && matches!(m.runtime, Some(r) if r != RuntimeKind::Shell))
            .collect();
        profiles.sort_by_key(|m| runtime_rank(m.runtime));
        let choice = |m: &ModelProfile| ModelChoice {
            runtime: m.runtime.unwrap_or(RuntimeKind::Claude),
            model: m.key.clone(),
            name: crew_core::types::model_display_name(if m.id.is_empty() { &m.key } else { &m.id }),
            tier: m.tier.clone(),
            price: m.price.clone(),
            summary: m.summary.clone(),
            unavailable: available.check(m).err(),
        };
        let mut out: Vec<ModelChoice> = profiles
            .iter()
            .filter(|m| m.runtime != Some(RuntimeKind::Pi))
            .map(|m| choice(m))
            .collect();
        // pi: whatever its own settings choose, the catalog's pi models,
        // then the rest of what it lists.
        let (unavailable, listed) = match pi_models {
            Ok(listed) => (None, listed),
            Err(why) => (Some(why), Vec::new()),
        };
        out.push(ModelChoice {
            runtime: RuntimeKind::Pi,
            model: String::new(),
            name: "pi's default".to_string(),
            tier: String::new(),
            price: String::new(),
            summary: "The model pi's own settings choose (`defaultModel` in ~/.pi/agent/settings.json).".to_string(),
            unavailable: unavailable.clone(),
        });
        for m in profiles.iter().filter(|m| m.runtime == Some(RuntimeKind::Pi)) {
            out.push(ModelChoice {
                unavailable: unavailable.clone().or_else(|| available.check(m).err()),
                ..choice(m)
            });
        }
        for model in listed {
            if out.iter().any(|c| c.runtime == RuntimeKind::Pi && c.model == model) {
                continue;
            }
            out.push(ModelChoice {
                runtime: RuntimeKind::Pi,
                name: model.clone(),
                model,
                tier: String::new(),
                price: String::new(),
                summary: String::new(),
                unavailable: None,
            });
        }
        out
    }
}

/// What can be launched from this machine.
#[derive(Clone, Debug, Default)]
pub struct Availability {
    pub claude: bool,
    pub codex: bool,
    pub pi: bool,
    /// The models this Codex installation offers, when it says so.
    pub codex_models: Option<HashSet<String>>,
}

fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(program))
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
    })
}

/// The model list Codex keeps of what the account may use. It is Codex's own
/// cache, not an interface: anything unexpected means "unknown".
fn codex_models() -> Option<HashSet<String>> {
    let home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))?;
    let text = std::fs::read_to_string(home.join("models_cache.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let slugs: HashSet<String> = value
        .get("models")?
        .as_array()?
        .iter()
        .filter_map(|m| m.get("slug")?.as_str().map(str::to_string))
        .collect();
    (!slugs.is_empty()).then_some(slugs)
}

impl Availability {
    pub fn detect() -> Self {
        let codex = on_path("codex");
        Self {
            claude: on_path("claude"),
            codex,
            pi: crate::agents::pi::check_installed().is_ok(),
            codex_models: if codex { codex_models() } else { None },
        }
    }

    /// `Ok` if `runtime`'s CLI can be started here, or why not.
    pub fn runtime(runtime: RuntimeKind) -> Result<(), String> {
        match runtime {
            RuntimeKind::Claude if !on_path("claude") => Err("`claude` is not on PATH".to_string()),
            RuntimeKind::Codex if !on_path("codex") => Err("`codex` is not on PATH".to_string()),
            RuntimeKind::Pi => crate::agents::pi::check_installed(),
            _ => Ok(()),
        }
    }

    /// Everything is launchable (tests, documentation).
    pub fn all() -> Self {
        Self {
            claude: true,
            codex: true,
            pi: true,
            codex_models: None,
        }
    }

    /// `Ok` if a member can be started on this model, or why not.
    pub fn check(&self, model: &ModelProfile) -> Result<(), String> {
        match model.runtime {
            Some(RuntimeKind::Claude) if !self.claude => Err("`claude` is not on PATH".to_string()),
            Some(RuntimeKind::Codex) if !self.codex => Err("`codex` is not on PATH".to_string()),
            Some(RuntimeKind::Pi) if !self.pi => Err("pi is not installed".to_string()),
            Some(RuntimeKind::Codex) => match &self.codex_models {
                Some(offered) if !offered.contains(&model.key) && !offered.contains(&model.id) => {
                    Err("not in the model list of this Codex account".to_string())
                }
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    }
}

/// Write the override template for `scope`, unless a file is already there.
pub fn init(scope: Scope) -> Result<PathBuf> {
    let path = path(scope).ok_or_else(|| {
        anyhow!("the model catalog is not per project; use --scope user or --scope global")
    })?;
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, TEMPLATE)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built_in() -> Catalog {
        let mut catalog = Catalog::default();
        catalog.apply(BUILT_IN, "built-in").unwrap();
        catalog
    }

    #[test]
    fn the_built_in_catalog_is_complete_and_consistent() {
        let catalog = built_in();
        assert!(!catalog.as_of.is_empty());
        let keys: Vec<&str> = catalog.models.iter().map(|(m, _)| m.key.as_str()).collect();
        // Claude before Codex, strongest first.
        assert_eq!(
            keys,
            ["fable", "opus", "sonnet", "haiku", "gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"]
        );
        for (m, origin) in &catalog.models {
            assert_eq!(*origin, "built-in");
            assert!(!m.summary.is_empty() && !m.price.is_empty() && !m.tier.is_empty(), "{}", m.key);
            assert!(!m.good_for.is_empty() && !m.avoid_for.is_empty(), "{}", m.key);
            assert!(m.sources.iter().all(|s| s.starts_with("https://")) && !m.sources.is_empty(), "{}", m.key);
            assert!(tier_rank(&m.tier) < 4, "{}: unknown tier {}", m.key, m.tier);
        }
        // Every pick names models that exist, and both families are on offer
        // wherever a review is involved.
        assert!(catalog.picks.len() >= 8);
        for pick in &catalog.picks {
            for name in &pick.models {
                assert!(catalog.find(name).is_some(), "pick '{}' names unknown model {name}", pick.work);
            }
            assert!(!pick.why.is_empty(), "{}", pick.work);
        }
        let review = catalog.picks.iter().find(|p| p.work == "plan and design review").unwrap();
        assert_eq!(review.models[0], "gpt-6-astra");
        // Found by name, full id or alias, whatever the case.
        assert_eq!(catalog.find("claude-fable-5-1").unwrap().key, "fable");
        assert_eq!(catalog.find("Claude-Haiku-4-5-20251001").unwrap().key, "haiku");
        assert_eq!(catalog.find("GPT-6-Astra").unwrap().runtime, Some(RuntimeKind::Codex));
        assert!(catalog.find("gpt-4").is_none());
        // The template for overrides is a valid, empty catalog file.
        let mut copy = built_in();
        copy.apply(TEMPLATE, "user").unwrap();
        assert_eq!(copy.models.len(), catalog.models.len());
    }

    #[test]
    fn a_model_decides_the_runtime() {
        let catalog = built_in();
        let claude = RuntimeKind::Claude;
        let codex = RuntimeKind::Codex;
        // Naming a catalog model is enough…
        let a = catalog.assign(None, Some("gpt-6-astra"), claude, Some("sonnet"));
        assert_eq!((a.runtime, a.model.as_deref(), a.note.is_none()), (codex, Some("gpt-6-astra"), true));
        // …and wins over a runtime that cannot run it, saying so.
        let a = catalog.assign(Some(claude), Some("gpt-6.1-sol"), claude, None);
        assert_eq!(a.runtime, codex);
        assert_eq!(a.note.as_deref(), Some("gpt-6.1-sol runs on Codex, not Claude Code"));
        // A full id is turned into the name the CLI is started with.
        let a = catalog.assign(None, Some("claude-opus-5-5"), codex, None);
        assert_eq!((a.runtime, a.model.as_deref()), (claude, Some("opus")));
        // A model crew does not know is passed through on the stated runtime.
        let a = catalog.assign(Some(codex), Some("my-local-model"), claude, None);
        assert_eq!((a.runtime, a.model.as_deref()), (codex, Some("my-local-model")));

        // Without a model, the team default applies only where it can run:
        // a Codex member of a Claude team does not get `--model sonnet`.
        let a = catalog.assign(None, None, claude, Some("sonnet"));
        assert_eq!((a.runtime, a.model.as_deref()), (claude, Some("sonnet")));
        let a = catalog.assign(Some(codex), None, claude, Some("sonnet"));
        assert_eq!((a.runtime, a.model), (codex, None));
        let a = catalog.assign(Some(codex), None, claude, Some("my-proxy-model"));
        assert_eq!((a.runtime, a.model), (codex, None));
        let a = catalog.assign(None, None, claude, Some("my-proxy-model"));
        assert_eq!(a.model.as_deref(), Some("my-proxy-model"));
        let a = catalog.assign(Some(RuntimeKind::Shell), None, RuntimeKind::Shell, None);
        assert_eq!((a.runtime, a.model), (RuntimeKind::Shell, None));
    }

    #[test]
    fn user_files_replace_remove_and_add_entries() {
        let mut catalog = built_in();
        catalog
            .apply(
                r#"
                [models.gpt-6-luna]
                disabled = true
                [models.sonnet]
                runtime = "claude"
                tier = "balanced"
                summary = "Ours."
                [models.local-qwen]
                runtime = "codex"
                tier = "fast"
                good_for = ["drafts"]
                [[picks]]
                work = "plan and design review"
                models = ["fable"]
                [[picks]]
                work = "drafts"
                models = ["local-qwen"]
                "#,
                "user",
            )
            .unwrap();
        assert!(catalog.find("gpt-6-luna").is_none());
        let sonnet = catalog.models.iter().find(|(m, _)| m.key == "sonnet").unwrap();
        assert_eq!((sonnet.0.summary.as_str(), sonnet.1), ("Ours.", "user"));
        assert_eq!(catalog.find("local-qwen").unwrap().name, "local-qwen");
        let review = catalog.picks.iter().find(|p| p.work == "plan and design review").unwrap();
        assert_eq!(review.models, ["fable"]);
        assert!(catalog.picks.iter().any(|p| p.work == "drafts"));
        assert_eq!(catalog.as_of, built_in().as_of, "only the built-in file dates the catalog");

        // A file with a mistake changes nothing.
        let before = catalog.models.len();
        for bad in [
            "[models.x]\nruntime = \"shell\"",
            "[models.x]\ntier = \"fast\"",
            "[models.x]\nruntime = \"codex\"\nprice_per_token = 1",
            "[[picks]]\nwork = \"x\"\nmodels = []",
            "[models.\"a b\"]\nruntime = \"codex\"",
            "not toml",
        ] {
            assert!(catalog.apply(bad, "user").is_err(), "{bad}");
            assert_eq!(catalog.models.len(), before, "{bad}");
        }
    }

    #[test]
    fn the_briefing_lists_only_what_can_be_launched_here() {
        let catalog = built_in();
        let both = catalog.briefing(&Availability::all());
        assert!(both.starts_with("\n## Models\n"), "{both}");
        assert!(both.contains("- fable (Claude Code, frontier, $10 / $50 per MTok, slower): "), "{both}");
        assert!(both.contains("- gpt-6-astra (Codex, frontier, $10 / $50 per MTok): "), "{both}");
        assert!(both.contains("Good for: reviewing a plan or design before work starts;"), "{both}");
        assert!(both.contains("- plan and design review: gpt-6-astra, then fable ("), "{both}");
        assert!(both.contains("checked by the other model family"), "{both}");
        assert!(both.contains("pass it as `model` to agent_add"), "{both}");
        // Compact enough to sit in a system prompt.
        assert!(both.len() < 9000, "{} bytes", both.len());

        // No Codex on the machine: its models are not offered, picks fall
        // back to what there is, and cross-family advice is dropped.
        let claude_only = catalog.briefing(&Availability {
            claude: true,
            ..Availability::default()
        });
        assert!(!claude_only.contains("gpt-6"), "{claude_only}");
        assert!(claude_only.contains("- plan and design review: fable\n"), "{claude_only}");
        assert!(!claude_only.contains("bulk extraction and summaries: gpt"), "{claude_only}");
        assert!(!claude_only.contains("other model family"), "{claude_only}");

        // A Codex account that does not list a model does not get it offered.
        let partial = Availability {
            claude: true,
            codex: true,
            pi: false,
            codex_models: Some(["gpt-6.1-sol".to_string()].into_iter().collect()),
        };
        let text = catalog.briefing(&partial);
        assert!(text.contains("- gpt-6.1-sol (") && !text.contains("- gpt-6-astra ("), "{text}");
        let astra = catalog.find("gpt-6-astra").unwrap();
        assert!(partial.check(astra).unwrap_err().contains("model list of this Codex account"));
        assert_eq!(catalog.briefing(&Availability::default()), "");
    }

    #[test]
    fn the_picker_lists_every_cli_with_what_cannot_start_here() {
        let mut catalog = built_in();
        catalog
            .apply("[models.pi-opus]\nruntime = \"pi\"\nname = \"anthropic/claude-opus-4-6\"\ntier = \"strong\"\n", "user")
            .unwrap();
        let listed = vec!["anthropic/claude-opus-4-6".to_string(), "pi-opus".to_string(), "openai/gpt-6-astra".to_string()];
        let no_codex = Availability { claude: true, pi: true, ..Availability::default() };
        let choices = catalog.choices(&no_codex, Ok(listed));
        let runtimes: Vec<RuntimeKind> = choices.iter().map(|c| c.runtime).collect();
        let first_codex = runtimes.iter().position(|r| *r == RuntimeKind::Codex).unwrap();
        let first_pi = runtimes.iter().position(|r| *r == RuntimeKind::Pi).unwrap();
        assert!(runtimes[..first_codex].iter().all(|r| *r == RuntimeKind::Claude), "{runtimes:?}");
        assert!(runtimes[first_pi..].iter().all(|r| *r == RuntimeKind::Pi), "{runtimes:?}");
        let fable = choices.iter().find(|c| c.model == "fable").unwrap();
        assert_eq!((fable.name.as_str(), fable.tier.as_str(), fable.unavailable.as_deref()), ("Fable 5.1", "frontier", None));
        assert!(!fable.summary.is_empty() && !fable.price.is_empty());
        let astra = choices.iter().find(|c| c.model == "gpt-6-astra").unwrap();
        assert_eq!(astra.unavailable.as_deref(), Some("`codex` is not on PATH"));
        // pi: its own default first, the catalog's pi models, then the rest
        // of its listing, each once.
        let pi: Vec<&str> = choices[first_pi..].iter().map(|c| c.model.as_str()).collect();
        assert_eq!(pi, ["", "pi-opus", "anthropic/claude-opus-4-6", "openai/gpt-6-astra"]);
        assert!(choices[first_pi..].iter().all(|c| c.unavailable.is_none()), "pi listed them: it runs");
        // Without pi, the one pi row says why.
        let without = catalog.choices(&Availability::all(), Err("`pi` is not on PATH".to_string()));
        let pi: Vec<_> = without.iter().filter(|c| c.runtime == RuntimeKind::Pi).collect();
        assert_eq!(pi[0].model, "");
        assert_eq!(pi[0].unavailable.as_deref(), Some("`pi` is not on PATH"));
        assert!(pi.iter().all(|c| c.unavailable.is_some()));
    }
}
