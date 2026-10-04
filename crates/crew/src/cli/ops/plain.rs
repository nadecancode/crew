//! Plain `crew`: choose a work style, say what to do, and let the orchestrator
//! staff the team.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::client::Client;
use crew_core::config::{AgentSpec, Isolation, Style, TeamConfig};
use crew_core::paths::{self, DaemonInfo};
use crew_core::rpc::{Call, SendParams, SendResult, Snapshot};
use crew_core::util::{one_line, slash_command, truncate_chars};
use crew_core::RuntimeKind;

use super::super::QuickArgs;
use super::team::{launch, open_dashboard, reuse_roster, saved_roster, set_backend};
use super::Target;
use crate::styles::{self, Styles};
use crew_tui::{Launcher, StyleChoice};

/// Refuse an objective that is really a slash command (`/exit`, `/stop`):
/// typed out of habit, it must not start a team or reach the orchestrator.
pub fn refuse_command(objective: &str) -> Result<()> {
    let Some((name, _)) = slash_command(objective) else {
        return Ok(());
    };
    bail!(
        "'/{name}' looks like a command, not an objective: no team was started and nothing was sent.\n\
In the dashboard, /exit leaves it and /stop stops the team; from the shell that is `crew stop`.\n\
If it really is the objective, put a space in front of it."
    )
}

/// File names plain `crew` picks a team definition up from.
const TEAM_FILES: [&str; 3] = ["crew.yaml", "crew.yml", "crew.toml"];

/// Plain `crew`: the one command to start with.
///
/// In a directory where a team is running it opens the dashboard (passing a
/// new objective on to the orchestrator). Otherwise it starts a team there:
/// from `crew.yaml` if the directory has one, from the roster of the last
/// session if there was one, and from nothing but an objective on a blank
/// workspace, where the orchestrator decides who is needed.
pub fn quick(target: &Target, objective: Option<String>, opts: QuickArgs) -> Result<()> {
    if std::env::var_os(paths::IDENTITY_ENV).is_some() {
        bail!(
            "this shell belongs to an agent of a running team; plain `crew` starts a new \
team and is meant for the operator (agents use their crew tools, or `crew ctl`)"
        );
    }
    let dir = match &target.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    let team_file = TEAM_FILES
        .iter()
        .map(|name| dir.join(name))
        .find(|f| f.is_file());
    let from_file = team_file.as_deref().map(TeamConfig::load).transpose()?;
    let mut workspace = match &from_file {
        Some(config) => config.workspace_dir(&dir),
        None => dir,
    };
    let mut objective = objective
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty());

    if from_file.is_some() && opts.style.is_some() {
        bail!("this directory has a team file, which defines the team; --style is for directories without one");
    }
    if let Some(info) = running_team(&workspace) {
        if let Some(style) = &opts.style {
            bail!(
                "team '{}' is running here; `crew stop` it before launching the {style} style",
                info.project.as_deref().unwrap_or("?")
            );
        }
        let (mut client, _) = Client::operator(Path::new(&info.socket), "cli")?;
        if let Some(objective) = &objective {
            let snapshot: Snapshot = client.call(Call::Snapshot {
                messages: Some(0),
                events: Some(0),
            })?;
            let orchestrator =
                snapshot
                    .agents
                    .iter()
                    .find(|a| a.orchestrator)
                    .ok_or_else(|| {
                        anyhow!(
                            "a team is already running here and has no orchestrator to hand a new \
objective to; use `crew task add`, or `crew stop` first"
                        )
                    })?;
            let _: SendResult = client.call(Call::Send(SendParams {
                to: orchestrator.id.clone(),
                body: objective.clone(),
                ..SendParams::default()
            }))?;
            println!(
                "a team is already running here; sent the objective to {}",
                orchestrator.id
            );
        } else {
            println!(
                "team '{}' is running here",
                info.project.as_deref().unwrap_or("?")
            );
        }
        return open_dashboard(&info, opts.no_attach);
    }

    // What the last session here was about, offered again when no new
    // objective is given. A team file speaks for itself.
    let mut previous = None;
    let (mut config, origin) = match from_file {
        Some(mut config) => {
            if !opts.replan {
                reuse_roster(&mut config, &workspace);
            }
            let name = team_file
                .as_deref()
                .and_then(Path::file_name)
                .unwrap_or_default();
            (config, format!("team file {}", name.to_string_lossy()))
        }
        None => {
            if !workspace.is_dir() {
                std::fs::create_dir_all(&workspace)
                    .with_context(|| format!("creating workspace {}", workspace.display()))?;
            }
            workspace = workspace
                .canonicalize()
                .with_context(|| format!("resolving {}", workspace.display()))?;
            refuse_broad_workspace(&workspace)?;
            let styles = Styles::load(&workspace);
            for problem in &styles.problems {
                eprintln!("warning: work style {problem}");
            }
            let last = saved_roster(&workspace);
            let Some(picked) = pick_style(
                &workspace,
                &styles,
                last.as_ref(),
                &opts,
                objective.as_deref(),
            )?
            else {
                println!("cancelled; nothing was started");
                return Ok(());
            };
            objective = picked.objective.or(objective);
            match picked.style {
                Some(name) => {
                    let entry = styles.require(&name)?;
                    let mut config = style_team(&entry.style, &workspace);
                    let mut origin = format!("a new team ({name} style)");
                    if let Some(saved) = styles::saved_team(&workspace, &name) {
                        previous = Some(saved.objective).filter(|o| !o.trim().is_empty());
                        if !opts.replan && !saved.agents.is_empty() {
                            origin = format!(
                                "the {name} team of the last session ({} member(s))",
                                saved.agents.len()
                            );
                            config.agents = saved.agents;
                        }
                    }
                    (config, origin)
                }
                // The team of the last session, launched from a team file
                // rather than a style.
                None => {
                    let mut roster =
                        last.ok_or_else(|| anyhow!("there is no earlier team here"))?;
                    let members = if opts.replan { 0 } else { roster.agents.len() };
                    roster.agents.truncate(members);
                    roster.workspace.path = workspace.to_string_lossy().into_owned();
                    previous = Some(std::mem::take(&mut roster.objective))
                        .filter(|o| !o.trim().is_empty());
                    (
                        roster,
                        format!("the team of the last session ({members} member(s))"),
                    )
                }
            }
        }
    };
    set_backend(&mut config, opts.backend.as_deref())?;
    if let Some(runtime) = &opts.runtime {
        let runtime: RuntimeKind = runtime.parse().map_err(|e: String| anyhow!(e))?;
        if runtime == RuntimeKind::Shell {
            bail!("--runtime takes claude or codex");
        }
        config.defaults.runtime = runtime;
        config.orchestrator.runtime = None;
    }
    if let Some(model) = &opts.model {
        config.defaults.model = Some(model.clone());
        config.orchestrator.model = None;
    }
    match objective {
        Some(objective) => config.objective = objective,
        // A team file that brings its own plan needs no objective.
        None if !config.objective.trim().is_empty() || !config.all_tasks().is_empty() => {}
        None => config.objective = ask_objective(previous.as_deref())?,
    }
    config.validate()?;
    if !workspace_is_isolated(&workspace, &config) {
        println!(
            "note: {} has files but is not a git repository with a commit, so the agents \
will share it without a worktree each (commit it first to isolate them)",
            workspace.display()
        );
    }
    println!("starting {origin} in {}", workspace.display());
    if !config.objective.trim().is_empty() {
        println!(
            "objective  {}",
            truncate_chars(&one_line(&config.objective), 200)
        );
    }
    launch(config, &workspace, opts.no_attach)
}

/// The daemon of `workspace`, if it has a team with agents still around.
fn running_team(workspace: &Path) -> Option<DaemonInfo> {
    let info = paths::discover(Some(workspace), None).ok()?;
    let (mut client, _) = Client::operator(Path::new(&info.socket), "cli").ok()?;
    let snapshot: Snapshot = client
        .call(Call::Snapshot {
            messages: Some(0),
            events: Some(0),
        })
        .ok()?;
    let running = snapshot.session.project.is_some()
        && snapshot.agents.iter().any(|a| !a.status.is_terminal());
    running.then_some(info)
}

/// What the chooser (or its stand-ins) decided.
struct Picked {
    /// A work style, or `None` for the style-less team of the last session.
    style: Option<String>,
    /// The objective typed into the chooser.
    objective: Option<String>,
}

/// Which work style to launch: the one named on the command line, the one
/// picked in the chooser, or — with nobody to ask — the one used last.
/// `None` when the user backed out of the chooser.
fn pick_style(
    workspace: &Path,
    styles: &Styles,
    last: Option<&TeamConfig>,
    opts: &QuickArgs,
    objective: Option<&str>,
) -> Result<Option<Picked>> {
    if let Some(name) = &opts.style {
        styles.require(name)?;
        return Ok(Some(Picked {
            style: Some(name.clone()),
            objective: None,
        }));
    }
    // A last team that was not launched from a style stays on offer.
    let legacy = last.filter(|team| team.style.is_none());
    let last_style = last
        .and_then(|team| team.style.as_deref())
        .filter(|name| styles.get(name).is_some());
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        let style = match (legacy, last_style) {
            (Some(_), _) => None,
            (None, Some(name)) => Some(name.to_string()),
            (None, None) => Some(styles::DEFAULT_STYLE.to_string()),
        };
        return Ok(Some(Picked {
            style,
            objective: None,
        }));
    }

    let mut choices: Vec<StyleChoice> = Vec::new();
    if let Some(team) = legacy {
        choices.push(last_team_choice(team, opts.replan));
    }
    let offset = choices.len();
    choices.extend(
        styles
            .entries
            .iter()
            .map(|entry| style_choice(workspace, entry, opts.replan)),
    );
    let selected = match (legacy, last_style) {
        (Some(_), _) => 0,
        (None, name) => {
            offset
                + name
                    .and_then(|n| styles.position(n))
                    .or_else(|| styles.position(styles::DEFAULT_STYLE))
                    .unwrap_or(0)
        }
    };
    let launcher = Launcher::new(
        &workspace.to_string_lossy(),
        choices,
        selected,
        objective.unwrap_or_default(),
    );
    let Some(launch) = crew_tui::run_launcher(launcher)? else {
        return Ok(None);
    };
    let style = match launch.choice.checked_sub(offset) {
        Some(index) => Some(
            styles
                .entries
                .get(index)
                .ok_or_else(|| anyhow!("the chooser returned an unknown style"))?
                .style
                .name
                .clone(),
        ),
        None => None,
    };
    Ok(Some(Picked {
        style,
        objective: Some(launch.objective),
    }))
}

pub(super) fn members_line(agents: &[AgentSpec]) -> String {
    let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
    format!("{} member(s): {}", ids.len(), ids.join(", "))
}

/// A work style as the chooser shows it.
fn style_choice(workspace: &Path, entry: &styles::Entry, replan: bool) -> StyleChoice {
    let mut details = vec![
        ("team".to_string(), entry.staffing()),
        ("runtime".to_string(), entry.runtime()),
    ];
    let fallbacks = &entry.style.defaults.model_fallbacks;
    if !fallbacks.is_empty() {
        let rules: Vec<String> = fallbacks
            .iter()
            .map(|(model, instead)| format!("{model} → {}", instead.join(" → ")))
            .collect();
        details.push((
            "fallbacks".to_string(),
            format!("{} while its window is used up", rules.join(", ")),
        ));
    }
    let refused = &entry.style.defaults.refusal_fallbacks;
    if !refused.is_empty() {
        details.push((
            "refused".to_string(),
            format!("{} takes over what a model refuses", refused.join(" → ")),
        ));
    }
    let saved = styles::saved_team(workspace, &entry.style.name);
    if let Some(saved) = saved.as_ref().filter(|s| !replan && !s.agents.is_empty()) {
        details.push(("last time".to_string(), members_line(&saved.agents)));
    }
    if let Some(path) = &entry.path {
        details.push(("file".to_string(), path.display().to_string()));
    }
    StyleChoice {
        name: entry.style.name.clone(),
        origin: entry.origin.as_str().to_string(),
        description: entry.style.description.clone(),
        details,
        instructions: entry.style.instructions.clone(),
        previous_objective: saved.map(|s| s.objective).filter(|o| !o.trim().is_empty()),
    }
}

fn last_team_choice(team: &TeamConfig, replan: bool) -> StyleChoice {
    let staffing = if replan || team.agents.is_empty() {
        "staffed by the orchestrator".to_string()
    } else {
        members_line(&team.agents)
    };
    StyleChoice {
        name: "last team".to_string(),
        origin: "here".to_string(),
        description: format!(
            "Continue with the team of the last session ('{}').",
            team.project
        ),
        details: vec![("team".to_string(), staffing)],
        instructions: team.instructions.clone(),
        previous_objective: Some(team.objective.clone()).filter(|o| !o.trim().is_empty()),
    }
}

fn project_name(workspace: &Path) -> String {
    let name: String = workspace
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if name.trim_matches('-').is_empty() {
        "crew".to_string()
    } else {
        name
    }
}

/// The team plain `crew` launches from a work style on `workspace`.
fn style_team(style: &Style, workspace: &Path) -> TeamConfig {
    let mut team = style.team(&project_name(workspace), &workspace.to_string_lossy(), "");
    // Only a directory with nothing in it is turned into a repository: crew
    // does not commit files it did not create.
    team.workspace.init = is_blank(workspace);
    // Running `crew` in a directory is the decision to let agents work
    // there; do not ask again once per agent worktree.
    team.defaults.auto_trust = true;
    team
}

/// Nothing in the directory but, at most, crew's and git's own state.
fn is_blank(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .all(|e| matches!(e.file_name().to_str(), Some(".crew" | ".git"))),
        Err(_) => false,
    }
}

/// Whether the daemon will give each agent a worktree (or was told not to).
fn workspace_is_isolated(workspace: &Path, config: &TeamConfig) -> bool {
    config.workspace.isolation == Isolation::Shared
        || config.workspace.init
        || (crate::git::is_root(workspace) && crate::git::has_commits(workspace))
}

/// Agents run without permission prompts by default; a workspace that is the
/// whole home directory (or the filesystem root) is never what was meant.
fn refuse_broad_workspace(workspace: &Path) -> Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|h| h.canonicalize().ok());
    if workspace.parent().is_none() || home.as_deref() == Some(workspace) {
        bail!(
            "refusing to start a team on {}: run `crew` inside a project directory (or pass \
-C <dir>)",
            workspace.display()
        );
    }
    Ok(())
}

/// Ask the operator what to build. `previous` is the objective of the last
/// session in this workspace, which an empty answer continues.
fn ask_objective(previous: Option<&str>) -> Result<String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return previous.map(str::to_string).ok_or_else(|| {
            anyhow!(
                "no objective: say what the team should build, e.g. \
crew \"build a todo CLI with tests\""
            )
        });
    }
    match previous {
        Some(previous) => println!(
            "What should the team build? (one line; empty to continue with: {})",
            truncate_chars(&one_line(previous), 120)
        ),
        None => println!("What should the team build? (one line; empty to cancel)"),
    }
    print!("> ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    // Checked as typed: a leading space is how a line that looks like a
    // command is given as the objective after all.
    let typed = line.trim_end();
    if let Some((name, _)) = slash_command(typed) {
        let leave = crew_tui::slash::find(crew_tui::slash::LAUNCHER, name)
            .is_some_and(|c| c.name == "exit");
        if leave {
            bail!("cancelled; nothing was started");
        }
        refuse_command(typed)?;
    }
    let objective = line.trim();
    if !objective.is_empty() {
        return Ok(objective.to_string());
    }
    previous
        .map(str::to_string)
        .ok_or_else(|| anyhow!("cancelled: no objective given"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_workspace_gets_an_orchestrator_only_team() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("My Project!");
        std::fs::create_dir(&dir).unwrap();
        let styles = Styles::load(&dir);
        let default = &styles.require(styles::DEFAULT_STYLE).unwrap().style;
        let mut team = style_team(default, &dir);
        assert_eq!(team.project, "My-Project-");
        assert_eq!(team.style.as_deref(), Some("default"));
        assert!(team.agents.is_empty() && team.tasks.is_empty());
        assert!(team.orchestrator.agent);
        // Empty: crew may turn it into a repository, one worktree per agent.
        assert!(team.workspace.init);
        assert!(team.defaults.auto_trust);
        assert_eq!(team.workspace.path, dir.to_string_lossy());
        team.objective = "build a thing".to_string();
        team.validate().unwrap();
        assert!(workspace_is_isolated(&dir, &team));

        // A directory with files in it is never committed on the user's behalf.
        std::fs::create_dir(dir.join(".crew")).unwrap();
        assert!(is_blank(&dir));
        std::fs::write(dir.join("notes.txt"), "mine").unwrap();
        assert!(!is_blank(&dir));
        let team = style_team(default, &dir);
        assert!(!team.workspace.init);
        assert!(!workspace_is_isolated(&dir, &team));

        // A work style brings its instructions along.
        let research = style_team(&styles.require("research").unwrap().style, &dir);
        assert_eq!(research.style.as_deref(), Some("research"));
        assert!(research.instructions.contains("research team"));
    }

    #[test]
    fn the_chooser_lists_each_style_with_its_last_team() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let styles = Styles::load(ws);
        let entry = styles.require("research").unwrap();
        let fresh = style_choice(ws, entry, false);
        assert_eq!(
            (fresh.name.as_str(), fresh.origin.as_str()),
            ("research", "built-in")
        );
        assert_eq!(
            fresh.details[0],
            (
                "team".to_string(),
                "staffed by the orchestrator".to_string()
            )
        );
        assert!(fresh.previous_objective.is_none());
        assert!(fresh.instructions.contains("research team"));

        // After a session, the style remembers its team and its objective.
        let mut saved = style_team(&entry.style, ws);
        saved.objective = "How does X work?".to_string();
        saved.agents = vec![
            AgentSpec {
                id: "scout".into(),
                ..AgentSpec::default()
            },
            AgentSpec {
                id: "skeptic".into(),
                ..AgentSpec::default()
            },
        ];
        let path = styles::roster_path(ws, "research");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, saved.to_yaml().unwrap()).unwrap();
        let again = style_choice(ws, entry, false);
        assert!(again.details.contains(&(
            "last time".to_string(),
            "2 member(s): scout, skeptic".to_string()
        )));
        assert_eq!(
            again.previous_objective.as_deref(),
            Some("How does X work?")
        );
        // --replan: the members are not offered, the objective still is.
        let replanned = style_choice(ws, entry, true);
        assert!(!replanned
            .details
            .iter()
            .any(|(label, _)| label == "last time"));
        assert_eq!(
            replanned.previous_objective.as_deref(),
            Some("How does X work?")
        );
        // Another style in the same workspace has its own memory.
        assert!(
            style_choice(ws, styles.require("experiment").unwrap(), false)
                .previous_objective
                .is_none()
        );

        let legacy = last_team_choice(&saved, false);
        assert_eq!(legacy.name, "last team");
        assert_eq!(legacy.details[0].1, "2 member(s): scout, skeptic");
    }

    #[test]
    fn the_home_directory_is_not_a_workspace() {
        assert!(refuse_broad_workspace(Path::new("/")).is_err());
        if let Some(home) =
            std::env::var_os("HOME").and_then(|h| PathBuf::from(h).canonicalize().ok())
        {
            assert!(refuse_broad_workspace(&home).is_err());
            assert!(refuse_broad_workspace(&home.join("project")).is_ok());
        }
    }
}
