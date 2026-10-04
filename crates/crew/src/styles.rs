//! Work styles: presets for how a team is staffed and how it works
//! (research, experiment, …), chosen when a team is launched.
//!
//! A style is a YAML (or TOML) file named after the style. A few are built
//! in; more are picked up from a `styles` folder next to each crew config
//! file, a later folder replacing a style of the same name:
//!
//! | origin   | folder                                              |
//! |----------|-----------------------------------------------------|
//! | built-in | compiled in (`default`, `research`, `experiment`, `cleanup`) |
//! | global   | `/etc/crew/styles/`                                 |
//! | user     | `$XDG_CONFIG_HOME/crew/styles/` (`~/.config/crew/…`) |
//! | project  | `<workspace>/.crew/styles/`                         |

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::config::{Style, TeamConfig, ROSTERS_DIR, STYLES_DIR};
use crew_core::paths;
use crew_core::util::is_valid_id;

use crate::endpoints::Scope;

/// The style used when none is chosen.
pub const DEFAULT_STYLE: &str = "default";

const BUILT_IN: [(&str, &str); 4] = [
    (DEFAULT_STYLE, include_str!("../styles/default.yaml")),
    ("research", include_str!("../styles/research.yaml")),
    ("experiment", include_str!("../styles/experiment.yaml")),
    ("cleanup", include_str!("../styles/cleanup.yaml")),
];

/// What `crew style new` writes when it is not copying an existing style.
const TEMPLATE: &str = r#"# A crew work style: how a team is staffed and how it works.
# Pick it in the chooser (`crew`) or with `crew --style <name>`.

# One line for the chooser.
description: Describe this way of working in one line.

# Standing instructions. Every member, the orchestrator included, gets these
# in its briefing, so say how the work is done here and what a finished
# piece of work looks like. Address the orchestrator separately if it should
# staff or plan the team in a particular way.
instructions: |
  - …

# Leave `agents` out and the orchestrator staffs the team from the
# instructions. To fix the roster instead:
# agents:
#   - id: scout
#     role: finds and reads the sources
#   - id: skeptic
#     role: tries to refute every finding

# defaults:
#   runtime: claude          # claude | codex
#   model: sonnet
# isolation: auto            # auto | worktree | shared
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    BuiltIn,
    File(Scope),
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::BuiltIn => "built-in",
            Origin::File(scope) => scope.as_str(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub style: Style,
    pub origin: Origin,
    /// The file it was read from; `None` for a built-in.
    pub path: Option<PathBuf>,
}

/// Every style available for a workspace.
#[derive(Debug, Default)]
pub struct Styles {
    pub entries: Vec<Entry>,
    /// Style files that could not be used, with the reason.
    pub problems: Vec<String>,
}

/// The folder a scope's styles live in.
pub fn dir(scope: Scope, workspace: &Path) -> Option<PathBuf> {
    Some(scope.path(workspace)?.parent()?.join(STYLES_DIR))
}

fn extension(path: &Path) -> Option<bool> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "yaml" | "yml" => Some(false),
        "toml" => Some(true),
        _ => None,
    }
}

impl Styles {
    pub fn load(workspace: &Path) -> Self {
        let mut styles = Styles::default();
        for (name, text) in BUILT_IN {
            let style = Style::parse(name, text, false).expect("built-in styles are valid");
            styles.put(Entry {
                style,
                origin: Origin::BuiltIn,
                path: None,
            });
        }
        for scope in Scope::ALL {
            let Some(dir) = dir(scope, workspace) else { continue };
            let Ok(read) = std::fs::read_dir(&dir) else { continue };
            let mut files: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
            files.sort();
            for path in files {
                let Some(toml) = extension(&path) else { continue };
                let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                let parsed = std::fs::read_to_string(&path)
                    .map_err(anyhow::Error::from)
                    .and_then(|text| Style::parse(&name, &text, toml));
                match parsed {
                    Ok(style) => styles.put(Entry {
                        style,
                        origin: Origin::File(scope),
                        path: Some(path),
                    }),
                    Err(e) => styles.problems.push(format!("{}: {e:#}", path.display())),
                }
            }
        }
        styles
    }

    /// Add a style, replacing one of the same name where it stands.
    fn put(&mut self, entry: Entry) {
        match self.entries.iter_mut().find(|e| e.style.name == entry.style.name) {
            Some(existing) => *existing = entry,
            None => self.entries.push(entry),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.style.name == name)
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.style.name == name)
    }

    /// `get`, with an error that lists what there is.
    pub fn require(&self, name: &str) -> Result<&Entry> {
        self.get(name).ok_or_else(|| {
            let names: Vec<&str> = self.entries.iter().map(|e| e.style.name.as_str()).collect();
            anyhow!("no work style '{name}' (available: {})", names.join(", "))
        })
    }
}

impl Entry {
    /// Who is on a team launched from this style, in a few words.
    pub fn staffing(&self) -> String {
        let style = &self.style;
        if style.agents.is_empty() {
            return "staffed by the orchestrator".to_string();
        }
        let ids: Vec<&str> = style.agents.iter().map(|a| a.id.as_str()).collect();
        let orchestrator = if style.orchestrator.agent { " + orchestrator" } else { "" };
        format!("{}{orchestrator}", ids.join(", "))
    }

    pub fn runtime(&self) -> String {
        let defaults = &self.style.defaults;
        match &defaults.model {
            Some(model) => format!("{} · {model}", defaults.runtime),
            None => defaults.runtime.to_string(),
        }
    }

    /// The file's text (the embedded text for a built-in).
    pub fn source(&self) -> Result<String> {
        match &self.path {
            Some(path) => std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display())),
            None => Ok(BUILT_IN
                .iter()
                .find(|(name, _)| *name == self.style.name)
                .map(|(_, text)| (*text).to_string())
                .unwrap_or_default()),
        }
    }
}

/// Where the last team launched from `style` in this workspace is kept.
pub fn roster_path(workspace: &Path, style: &str) -> PathBuf {
    paths::state_dir(workspace).join(ROSTERS_DIR).join(format!("{style}.yaml"))
}

/// The team the orchestrator settled on the last time `style` ran here.
pub fn saved_team(workspace: &Path, style: &str) -> Option<TeamConfig> {
    TeamConfig::load(&roster_path(workspace, style)).ok()
}

/// Create a style file in `scope`'s folder, from `from` or a template.
pub fn create(workspace: &Path, name: &str, scope: Scope, from: Option<&str>) -> Result<PathBuf> {
    if !is_valid_id(name) {
        bail!("'{name}' is not a style name (expected [a-z][a-z0-9_-]{{0,31}})");
    }
    let dir = dir(scope, workspace).ok_or_else(|| anyhow!("no home directory to put the style in"))?;
    let path = dir.join(format!("{name}.yaml"));
    for existing in ["yaml", "yml", "toml"] {
        let other = dir.join(format!("{name}.{existing}"));
        if other.exists() {
            bail!("{} already exists", other.display());
        }
    }
    let text = match from {
        Some(source) => {
            let entry = Styles::load(workspace);
            let entry = entry.require(source)?;
            if entry.path.as_deref().and_then(extension) == Some(true) {
                bail!("'{source}' is a TOML style; copy {} by hand", entry.path.as_ref().unwrap().display());
            }
            entry.source()?
        }
        None => TEMPLATE.to_string(),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_styles_are_valid_and_staffed_by_the_orchestrator() {
        let tmp = tempfile::tempdir().unwrap();
        let mut styles = Styles::default();
        for (name, text) in BUILT_IN {
            let style = Style::parse(name, text, false).unwrap();
            assert!(!style.description.is_empty(), "{name}");
            assert!(style.agents.is_empty() && style.orchestrator.agent, "{name}");
            styles.put(Entry {
                style,
                origin: Origin::BuiltIn,
                path: None,
            });
        }
        let names: Vec<&str> = styles.entries.iter().map(|e| e.style.name.as_str()).collect();
        assert_eq!(names, ["default", "research", "experiment", "cleanup"]);
        let research = styles.get("research").unwrap();
        assert_eq!(research.staffing(), "staffed by the orchestrator");
        assert_eq!(research.runtime(), "claude");
        assert!(research.style.instructions.contains("research team"));
        assert!(styles.get("experiment").unwrap().style.instructions.contains("hypothesis"));
        let cleanup = &styles.get("cleanup").unwrap().style;
        assert!(cleanup.instructions.contains("Change structure, not behaviour"));
        assert!(cleanup.instructions.contains("Set `reviewer` on every cleanup task"));
        assert_eq!(cleanup.defaults.model_fallbacks["fable"], ["opus"]);
        assert!(research.source().unwrap().contains("description:"));
        let error = styles.require("reserch").unwrap_err().to_string();
        assert!(error.contains("available: default, research, experiment, cleanup"), "{error}");
        // The template `crew style new` writes is itself a valid style.
        Style::parse("mine", TEMPLATE, false).unwrap();
        assert_eq!(
            roster_path(tmp.path(), "research"),
            tmp.path().join(".crew/rosters/research.yaml")
        );
    }

    #[test]
    fn project_styles_are_found_and_shadow_built_ins() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let folder = dir(Scope::Project, ws).unwrap();
        assert_eq!(folder, ws.join(".crew/styles"));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("research.yaml"),
            "description: Our research\nagents:\n  - id: scout\n  - id: skeptic\n",
        )
        .unwrap();
        std::fs::write(folder.join("field-notes.toml"), "description = \"Notes\"\n").unwrap();
        std::fs::write(folder.join("broken.yaml"), "agents: 3\n").unwrap();
        std::fs::write(folder.join("README.md"), "not a style").unwrap();

        let styles = Styles::load(ws);
        // Shadowed in place: the order of the chooser does not jump around.
        let research = styles.get("research").unwrap();
        assert_eq!(research.origin, Origin::File(Scope::Project));
        assert_eq!(research.style.description, "Our research");
        assert_eq!(research.staffing(), "scout, skeptic + orchestrator");
        assert_eq!(styles.position("research"), Some(1));
        assert_eq!(styles.get("field-notes").unwrap().origin.as_str(), "project");
        assert!(styles.get("broken").is_none() && styles.get("README").is_none());
        assert!(styles.problems.iter().any(|p| p.contains("broken.yaml")), "{:?}", styles.problems);

        // `crew style new`: from the template, or as a copy to edit.
        let path = create(ws, "mine", Scope::Project, None).unwrap();
        assert_eq!(path, folder.join("mine.yaml"));
        assert!(create(ws, "mine", Scope::Project, None).is_err(), "never overwrites");
        assert!(create(ws, "Bad Name", Scope::Project, None).is_err());
        assert!(create(ws, "copy", Scope::Project, Some("nope")).is_err());
        let copy = create(ws, "my-experiment", Scope::Project, Some("experiment")).unwrap();
        assert!(std::fs::read_to_string(copy).unwrap().contains("hypothesis"));
        let styles = Styles::load(ws);
        assert!(styles.get("mine").is_some() && styles.get("my-experiment").is_some());
    }
}
