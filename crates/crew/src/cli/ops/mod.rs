//! Operator commands: everything the human runs.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::client::Client;
use crew_core::config::{AgentSpec, BackendChoice, Isolation, Style, TeamConfig, ROSTER_FILE};
use crew_core::paths::{self, DaemonInfo};
use crew_core::rpc::{Call, Push, SendParams, SendResult, SessionInfo, Snapshot};
use crew_core::util::{fmt_clock, fmt_elapsed, now_ms, one_line, slash_command, truncate_chars};
use crew_core::{Agent, Event, Message, RuntimeKind, TaskStatus};
use serde_json::Value;

use super::{daemonize, ConfigCommand, ModelsCommand, QuickArgs, StyleCommand};
use crate::endpoints::{Layers, Scope};
use crate::limits::fmt_when;
use crate::mcp::tools::{render_agents, render_tasks};
use crate::models::{self, Availability, Catalog};
use crate::styles::{self, Styles};
use crew_tui::{Launcher, StyleChoice};

mod agents;
#[cfg(test)]
pub(super) use agents::restart_call;
pub use agents::{agent, task};

/// Which session a command addresses.
pub struct Target {
    pub dir: Option<PathBuf>,
    pub project: Option<String>,
}

impl Target {
    fn discover(&self) -> Result<DaemonInfo> {
        paths::discover(self.dir.as_deref(), self.project.as_deref())
    }

    fn connect(&self, client: &str) -> Result<(Client, DaemonInfo)> {
        let info = self.discover()?;
        let (c, _) = Client::operator(Path::new(&info.socket), client)?;
        Ok((c, info))
    }
}

/// The herdr socket of the terminal crew was started from, if any.
fn herdr_socket(explicit_backend: BackendChoice) -> Option<String> {
    let from_env = std::env::var("HERDR_SOCKET_PATH")
        .ok()
        .filter(|s| !s.is_empty());
    if std::env::var("HERDR_ENV").as_deref() == Ok("1") {
        return from_env;
    }
    // Outside a herdr pane, only use herdr when it was asked for by name.
    if explicit_backend == BackendChoice::Herdr {
        return from_env.or_else(|| {
            let home = std::env::var_os("HOME")?;
            let default = PathBuf::from(home).join(".config/herdr/herdr.sock");
            default
                .exists()
                .then(|| default.to_string_lossy().into_owned())
        });
    }
    None
}

pub fn run_team(
    target: &Target,
    team_file: &Path,
    no_attach: bool,
    backend: Option<&str>,
    replan: bool,
) -> Result<()> {
    let mut config = TeamConfig::load(team_file)?;
    set_backend(&mut config, backend)?;
    let cwd = std::env::current_dir()?;
    let workspace = match &target.dir {
        Some(dir) => dir.clone(),
        None => config.workspace_dir(&cwd),
    };
    if !replan {
        reuse_roster(&mut config, &workspace);
    }
    launch(config, &workspace, no_attach)
}

fn set_backend(config: &mut TeamConfig, backend: Option<&str>) -> Result<()> {
    if let Some(backend) = backend {
        config.backend = match backend {
            "auto" => BackendChoice::Auto,
            "pty" => BackendChoice::Pty,
            "herdr" => BackendChoice::Herdr,
            other => bail!("unknown backend '{other}' (expected auto, pty or herdr)"),
        };
    }
    Ok(())
}

fn saved_roster(workspace: &Path) -> Option<TeamConfig> {
    TeamConfig::load(&paths::state_dir(workspace).join(ROSTER_FILE)).ok()
}

/// A team file without agents leaves the roster to the orchestrator. If it
/// already settled on one in an earlier run, start from that.
fn reuse_roster(config: &mut TeamConfig, workspace: &Path) {
    if !config.agents.is_empty() {
        return;
    }
    let Some(roster) = saved_roster(workspace) else {
        return;
    };
    if roster.project == config.project && !roster.agents.is_empty() {
        println!(
            "using the saved roster ({} member(s), {}); --replan lets the orchestrator \
design the team again",
            roster.agents.len(),
            paths::state_dir(workspace).join(ROSTER_FILE).display()
        );
        config.agents = roster.agents;
    }
}

/// Start the daemon for `workspace`, load the team into it, and open the
/// dashboard.
fn launch(config: TeamConfig, workspace: &Path, no_attach: bool) -> Result<()> {
    // Before the workspace is created or the daemon started.
    crew_core::client::refuse_in_agent_context()?;
    // A config file that cannot be used is reported here, before anything
    // starts, rather than as one failed agent after another.
    let layers = Layers::load(workspace);
    layers.check()?;
    for warning in layers.warnings() {
        eprintln!("warning: crew config {warning}");
    }
    if !workspace.is_dir() {
        if config.workspace.init {
            std::fs::create_dir_all(workspace)
                .with_context(|| format!("creating workspace {}", workspace.display()))?;
        } else {
            bail!(
                "workspace {} does not exist (set workspace.init: true to create it)",
                workspace.display()
            );
        }
    }
    let info = daemonize::ensure_daemon(workspace)?;
    let (mut client, _) = Client::operator(Path::new(&info.socket), "cli")?;
    let herdr = herdr_socket(config.backend);
    let session: SessionInfo = client.call(Call::LoadTeam {
        config: Box::new(config.clone()),
        herdr_socket: herdr,
    })?;
    println!(
        "team '{}' started: {} agent(s), backend {}, isolation {}",
        config.project,
        config.agents.len() + usize::from(config.orchestrator.agent),
        session.backend,
        session.isolation
    );
    println!("workspace  {}", session.workspace);
    println!(
        "daemon     pid {} (keeps running when you close the dashboard)",
        session.daemon_pid
    );
    open_dashboard(&info, no_attach)
}

fn open_dashboard(info: &DaemonInfo, no_attach: bool) -> Result<()> {
    if no_attach || !std::io::stdout().is_terminal() {
        println!("dashboard  crew attach -C {}", info.workspace);
        return Ok(());
    }
    if dashboard(info)? == crew_tui::Exit::Stopped {
        return Ok(());
    }
    // Back at the shell: if the work is finished, say where it is.
    let snapshot = Client::operator(Path::new(&info.socket), "cli")
        .ok()
        .and_then(|(mut client, _)| {
            client
                .call::<Snapshot>(Call::Snapshot {
                    messages: Some(0),
                    events: Some(0),
                })
                .ok()
        });
    if let Some(hint) = snapshot.as_ref().and_then(landing_hint) {
        println!("{hint}");
    }
    Ok(())
}

/// Show the dashboard until the user leaves it. When they stopped the team
/// from it, wait for the daemon to go and say what is left.
fn dashboard(info: &DaemonInfo) -> Result<crew_tui::Exit> {
    // A team started by an older crew sends its agents without their
    // models' proper names; the catalog supplies them.
    let stale = stale_daemon_note(info);
    let exit = crew_tui::run(
        Path::new(&info.socket),
        Catalog::load().display_names(),
        stale.clone(),
    )?;
    if let (Some(note), crew_tui::Exit::Detached) = (&stale, exit) {
        // Said again where it can be read at leisure, and copied.
        println!("note: {note}");
    }
    if exit == crew_tui::Exit::Stopped {
        wait_for_exit(info.pid);
        println!(
            "stopped the team in {}: its agents and the crew daemon are gone",
            info.workspace
        );
        println!("committed work stays on the agents' branches; `crew` starts a team here again");
    }
    Ok(exit)
}

/// A daemon keeps running the build of crew it was started from: closing
/// and reopening the dashboard does not change it. When the `crew` being run
/// is newer than the daemon, say so, and how to bring the daemon up to date.
fn stale_daemon_note(info: &DaemonInfo) -> Option<String> {
    let built = std::env::current_exe()
        .ok()?
        .metadata()
        .ok()?
        .modified()
        .ok()?;
    let built_ms = built
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    stale_since(info.started_at, built_ms)
}

fn stale_since(daemon_started_ms: i64, built_ms: i64) -> Option<String> {
    (daemon_started_ms < built_ms).then(|| {
        "this team's daemon is older than this build of crew, so newer features are missing — \
update it with: crew stop --keep-agents && crew start"
            .to_string()
    })
}

/// Give a daemon that was told to stop a few seconds to go away (it stops
/// its agents first, each of which gets a moment to exit).
fn wait_for_exit(pid: u32) {
    for _ in 0..300 {
        if !paths::pid_alive(pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

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

/// Where the results are once every task is done. With a worktree per agent
/// nothing lands in the workspace's own checkout until it is merged there.
fn landing_hint(snapshot: &Snapshot) -> Option<String> {
    let session = &snapshot.session;
    if session.completed_at.is_none() || session.isolation != "worktree" {
        return None;
    }
    Some(format!(
        "all tasks done — the work is on the agents' branches (agents/<id>), not yet in {}.\n\
to land it:  crew integrate && git merge agents/integration",
        session.workspace
    ))
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

fn members_line(agents: &[AgentSpec]) -> String {
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

/// `crew config`: which files set the team's endpoints and keys, and what
/// they add up to.
pub fn config(target: &Target, command: ConfigCommand) -> Result<()> {
    let workspace = match &target.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    match command {
        ConfigCommand::Show => {
            let layers = Layers::load(&workspace);
            print!("{}", layers.describe());
            // Exit non-zero for a rejected file, after showing which.
            layers.check()
        }
        ConfigCommand::Init { scope } => {
            let path = crate::endpoints::init(Scope::parse(&scope)?, &workspace)?;
            println!("wrote {}", path.display());
            println!("everything in it is commented out; edit it, then check with `crew config`");
            Ok(())
        }
    }
}

/// `crew models`: the model catalog and what can be launched from here.
pub fn models(command: ModelsCommand) -> Result<()> {
    let catalog = Catalog::load();
    let available = Availability::detect();
    match command {
        ModelsCommand::List => {
            let width = catalog
                .models
                .iter()
                .map(|(m, _)| m.key.len())
                .max()
                .unwrap_or(0);
            for (m, origin) in &catalog.models {
                let runtime = m.runtime.map(models::runtime_label).unwrap_or_default();
                let state = match available.check(m) {
                    Ok(()) => "available".to_string(),
                    Err(why) => format!("unavailable: {why}"),
                };
                println!(
                    "{:<width$}  {:<11} {:<8}  {:<22}  {state}{}",
                    m.key,
                    runtime,
                    m.tier,
                    m.price,
                    if *origin == "built-in" {
                        String::new()
                    } else {
                        format!("  [{origin}]")
                    },
                );
                println!("{:<width$}  {}", "", m.summary);
            }
            println!("\nwhich to pick, best first:");
            let work = catalog
                .picks
                .iter()
                .map(|p| p.work.len())
                .max()
                .unwrap_or(0);
            for pick in &catalog.picks {
                println!("  {:<work$}  {}", pick.work, pick.models.join(", "));
            }
            println!(
                "\nThe orchestrator gets this catalog in its briefing and picks a model per member. \
Profiles checked {}.",
                catalog.as_of
            );
            println!(
                "`crew models show <name>` has the detail and sources. Your own entries go in:"
            );
            for scope in [Scope::User, Scope::Global] {
                if let Some(path) = models::path(scope) {
                    println!("  {:<7} {}", scope.as_str(), path.display());
                }
            }
            for problem in &catalog.problems {
                eprintln!("warning: {problem}");
            }
            Ok(())
        }
        ModelsCommand::Show { name } => {
            let m = catalog.find(&name).ok_or_else(|| {
                let names: Vec<&str> = catalog.models.iter().map(|(m, _)| m.key.as_str()).collect();
                anyhow!(
                    "no model '{name}' in the catalog (there are: {})",
                    names.join(", ")
                )
            })?;
            println!("{}  ({})", m.key, m.name);
            println!(
                "runs on    {}",
                m.runtime.map(models::runtime_label).unwrap_or_default()
            );
            if !m.id.is_empty() {
                println!("model id   {}", m.id);
            }
            for (label, value) in [("tier", &m.tier), ("price", &m.price), ("speed", &m.speed)] {
                if !value.is_empty() {
                    println!("{label:<10} {value}");
                }
            }
            match available.check(m) {
                Ok(()) => println!("here       available"),
                Err(why) => println!("here       unavailable: {why}"),
            }
            println!("\n{}", m.summary);
            for (title, items) in [("good for", &m.good_for), ("not for", &m.avoid_for)] {
                if !items.is_empty() {
                    println!("\n{title}");
                    for item in items {
                        println!("  - {item}");
                    }
                }
            }
            let picked: Vec<&str> = catalog
                .picks
                .iter()
                .filter(|p| {
                    p.models
                        .iter()
                        .any(|n| catalog.find(n).is_some_and(|f| f.key == m.key))
                })
                .map(|p| p.work.as_str())
                .collect();
            if !picked.is_empty() {
                println!("\npicked for\n  {}", picked.join("; "));
            }
            if !m.notes.is_empty() {
                println!("\nnotes\n  {}", m.notes);
            }
            if !m.sources.is_empty() {
                println!("\nsources (checked {})", catalog.as_of);
                for source in &m.sources {
                    println!("  {source}");
                }
            }
            Ok(())
        }
        ModelsCommand::Init { scope } => {
            let path = models::init(Scope::parse(&scope)?)?;
            println!("wrote {}", path.display());
            println!("everything in it is commented out; edit it, then check with `crew models`");
            Ok(())
        }
    }
}

/// `crew style`: the work styles available in this workspace.
pub fn style(target: &Target, command: StyleCommand) -> Result<()> {
    let workspace = match &target.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    match command {
        StyleCommand::List => {
            let all = Styles::load(&workspace);
            let name_width = all
                .entries
                .iter()
                .map(|e| e.style.name.len())
                .max()
                .unwrap_or(0);
            for entry in &all.entries {
                println!(
                    "{:<name_width$}  {:<8}  {}",
                    entry.style.name,
                    entry.origin.as_str(),
                    one_line(&entry.style.description)
                );
            }
            println!("\nstyle folders (a later one replaces a style of the same name):");
            for scope in Scope::ALL {
                if let Some(dir) = styles::dir(scope, &workspace) {
                    println!("  {:<8} {}", scope.as_str(), dir.display());
                }
            }
            println!("launch one with `crew --style <name>`, or pick it in the chooser (`crew`)");
            for problem in &all.problems {
                eprintln!("warning: {problem}");
            }
            Ok(())
        }
        StyleCommand::Show { name } => {
            let all = Styles::load(&workspace);
            let entry = all.require(&name)?;
            println!("{}  ({})", entry.style.name, entry.origin.as_str());
            if let Some(path) = &entry.path {
                println!("file       {}", path.display());
            }
            println!("team       {}", entry.staffing());
            println!("runtime    {}", entry.runtime());
            if let Some(saved) =
                styles::saved_team(&workspace, &name).filter(|s| !s.agents.is_empty())
            {
                println!("last time  {}", members_line(&saved.agents));
            }
            println!("\n{}", entry.source()?.trim_end());
            Ok(())
        }
        StyleCommand::New { name, scope, from } => {
            let path = styles::create(&workspace, &name, Scope::parse(&scope)?, from.as_deref())?;
            println!("wrote {}", path.display());
            println!(
                "edit it, then launch it with `crew --style {name}` or pick it in the chooser"
            );
            Ok(())
        }
    }
}

pub fn start(target: &Target) -> Result<()> {
    let workspace = match &target.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    let info = daemonize::ensure_daemon(&workspace)?;
    println!("daemon running (pid {}) for {}", info.pid, info.workspace);
    Ok(())
}

pub fn attach(target: &Target) -> Result<()> {
    let info = target.discover()?;
    if !std::io::stdout().is_terminal() {
        bail!("the dashboard needs a terminal; use `crew status` for plain output");
    }
    dashboard(&info).map(|_| ())
}

pub fn sessions() -> Result<()> {
    let sessions = paths::list_sessions();
    if sessions.is_empty() {
        println!("no crew sessions are running");
        return Ok(());
    }
    for s in sessions {
        println!(
            "{:<24} pid {:<8} up {:<8} {}",
            s.project.as_deref().unwrap_or("<no team>"),
            s.pid,
            fmt_elapsed(now_ms() - s.started_at),
            s.workspace
        );
    }
    Ok(())
}

pub fn status(target: &Target, json: bool) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let snapshot: Snapshot = client.call(Call::Snapshot {
        messages: Some(0),
        events: Some(0),
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    let s = &snapshot.session;
    match &s.project {
        Some(project) => println!(
            "project    {project}  (backend {}, isolation {})",
            s.backend, s.isolation
        ),
        None => println!("project    <no team loaded>"),
    }
    if let Some(style) = &s.style {
        println!("style      {style}");
    }
    println!("workspace  {}", s.workspace);
    println!(
        "daemon     pid {}, up {}",
        s.daemon_pid,
        fmt_elapsed(snapshot.server_time - s.daemon_started_at)
    );
    let done = snapshot
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    println!("tasks      {done}/{} done", snapshot.tasks.len());
    if s.completed_at.is_some() {
        match snapshot.questions.len() {
            0 => println!("state      all tasks done"),
            n => println!("state      all tasks done · waiting for your answer to {n} question(s)"),
        }
    }
    if landing_hint(&snapshot).is_some() {
        println!("next       crew integrate && git merge agents/integration   (lands the agents' branches)");
    }
    if let Some(summary) = &s.summary {
        println!("summary    {}", one_line(summary));
    }
    for line in usage_lines(&snapshot) {
        println!("{line}");
    }
    if let Some(note) = target.discover().ok().as_ref().and_then(stale_daemon_note) {
        println!("note       {note}");
    }
    match snapshot.questions.len() {
        0 => {}
        n => println!("questions  {n} waiting for your answer — see `crew questions`"),
    }
    println!("\nAGENTS\n{}", render_agents(&snapshot.agents).trim_end());
    println!("\nTASKS\n{}", render_tasks(&snapshot.tasks).trim_end());
    Ok(())
}

/// `crew resume`: continue agents paused at a usage limit.
pub fn resume(target: &Target, agents: Vec<String>, restart: bool) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let reply: Value = client.call(Call::Resume { agents, restart })?;
    let resumed: Vec<&str> = reply
        .get("resumed")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if resumed.is_empty() {
        println!("no agent is paused at a usage limit");
    } else if restart {
        println!("restarted, resuming their sessions: {}", resumed.join(", "));
    } else {
        println!("told to continue: {}", resumed.join(", "));
        println!("(after switching account or key, use `crew resume --restart` instead)");
    }
    Ok(())
}

/// A usage figure older than this is said to be old.
const STALE_QUOTA_MS: i64 = 20 * 60_000;

fn usage_lines(snapshot: &Snapshot) -> Vec<String> {
    let now = snapshot.server_time;
    let mut lines = Vec::new();
    let mut label = "usage";
    // The window names make a column: as wide as the widest.
    let name_w = snapshot
        .session
        .quotas
        .iter()
        .map(|q| crate::limits::window_label(&q.window).chars().count())
        .max()
        .unwrap_or(0)
        .max(8);
    for runtime in [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Pi] {
        let mut name = runtime.to_string();
        for q in snapshot
            .session
            .quotas
            .iter()
            .filter(|q| q.runtime == runtime)
        {
            const CELLS: usize = 24;
            let percent = q.percent_used.clamp(0.0, 100.0);
            let mut filled = ((percent / 100.0) * CELLS as f32).round() as usize;
            if percent > 0.0 {
                filled = filled.max(1);
            }
            let window = crate::limits::window_label(&q.window);
            let resets = q
                .resets_at
                .filter(|at| *at > now)
                .map(|at| format!(" · resets {}", fmt_when(at, now)))
                .unwrap_or_default();
            // A figure that is not from a session just now says how old it is.
            let age = if now - q.updated_at > STALE_QUOTA_MS {
                format!(" · as of {} ago", fmt_elapsed(now - q.updated_at))
            } else {
                String::new()
            };
            lines.push(format!(
                "{label:<10} {name:<7} {window:<name_w$} {}{}  {:>3.0}% used{resets}{age}",
                "█".repeat(filled),
                "░".repeat(CELLS - filled),
                q.percent_used.max(0.0),
            ));
            // The label and the runtime are written once.
            label = "";
            name.clear();
        }
    }
    // Sessions moved to another model than their member was given.
    for a in snapshot.agents.iter().filter(|a| a.fallback.is_some()) {
        let fallback = a.fallback.as_ref().unwrap();
        let running = a
            .model_label()
            .unwrap_or_else(|| "another model".to_string());
        let why = if fallback.note.is_empty() {
            String::new()
        } else {
            format!(" — {}", fallback.note)
        };
        lines.push(if fallback.policy {
            format!(
                "fallback   {} runs on {running} instead of {}{why}; it goes back by itself",
                a.id, fallback.from
            )
        } else {
            format!(
                "fallback   {} runs on {running} instead of {}{why} · crew agent model {} <model> to choose",
                a.id, fallback.from, a.id
            )
        });
    }
    let parked: Vec<&Agent> = snapshot
        .agents
        .iter()
        .filter(|a| a.limit.is_some())
        .collect();
    if !parked.is_empty() {
        let ids: Vec<&str> = parked.iter().map(|a| a.id.as_str()).collect();
        let next = parked
            .iter()
            .filter_map(|a| a.limit.as_ref()?.retry_at)
            .min();
        let when = match next {
            Some(at) => format!("next automatic resume {}", fmt_when(at, now)),
            None => "waiting for you".to_string(),
        };
        lines.push(format!(
            "limits     {} paused at a usage limit ({}) — {when}",
            ids.len(),
            ids.join(", ")
        ));
        lines.push(
            "           continue now: crew resume · after switching account or key: crew resume --restart"
                .to_string(),
        );
    }
    lines
}

/// Render the dashboard once, as text: the same view `crew attach` shows,
/// for terminals that cannot host it (logs, CI, documentation).
pub fn frame(target: &Target, size: &str) -> Result<()> {
    let (width, height) = size
        .split_once(['x', 'X'])
        .and_then(|(w, h)| Some((w.parse::<u16>().ok()?, h.parse::<u16>().ok()?)))
        .filter(|(w, h)| *w >= 20 && *h >= 8)
        .ok_or_else(|| anyhow!("--frame expects WIDTHxHEIGHT, e.g. 120x40"))?;
    let (mut client, _) = target.connect("cli")?;
    let snapshot: Snapshot = client.call(Call::Snapshot {
        messages: Some(300),
        events: Some(400),
    })?;
    let now = snapshot.server_time;
    let mut app = crew_tui::App::new();
    app.model_names = Catalog::load().display_names();
    app.apply_snapshot(snapshot, now);
    println!("{}", crew_tui::render_to_string(&app, width, height, now));
    Ok(())
}

pub fn stop(target: &Target, keep_agents: bool) -> Result<()> {
    let (mut client, info) = target.connect("cli")?;
    match client.call_value(Call::Shutdown {
        stop_agents: !keep_agents,
    }) {
        Ok(_) => {}
        // The daemon can be gone before its answer got through; it was
        // told to stop, and whether it did is checked below.
        Err(
            crew_core::client::ClientError::Closed
            | crew_core::client::ClientError::Io(_)
            | crew_core::client::ClientError::Timeout,
        ) => {}
        Err(e) => return Err(e.into()),
    }
    // Wait for the daemon to go away so a following `crew run` starts clean.
    wait_for_exit(info.pid);
    if paths::pid_alive(info.pid) {
        bail!(
            "the crew daemon (pid {}) was told to stop but is still running",
            info.pid
        );
    }
    println!(
        "stopped the crew daemon for {}{}",
        info.workspace,
        if keep_agents {
            " (agents left running)"
        } else {
            ""
        }
    );
    Ok(())
}

fn print_event(event: &Event, json: bool) {
    if json {
        if let Ok(line) = serde_json::to_string(event) {
            println!("{line}");
        }
        return;
    }
    println!(
        "{} {:<5} {:<10} {:<22} {:<14} {}",
        fmt_clock(event.timestamp),
        event.level.as_str(),
        event.source,
        event.event_type,
        event.agent_id.as_deref().unwrap_or("-"),
        event.message
    );
}

pub fn logs(
    target: &Target,
    agent: Option<String>,
    lines: usize,
    follow: bool,
    json: bool,
) -> Result<()> {
    let (mut client, info) = target.connect("cli")?;
    let events: Vec<Event> = client.call(Call::Events {
        agent: agent.clone(),
        limit: Some(lines),
    })?;
    let mut last_seq = 0;
    for event in &events {
        print_event(event, json);
        last_seq = event.seq;
    }
    if !follow {
        return Ok(());
    }
    let (subscriber, _) = Client::operator(Path::new(&info.socket), "cli-logs")?;
    let mut stream = subscriber.subscribe()?;
    loop {
        match stream.next() {
            Ok(Push::Event(event)) => {
                if event.seq <= last_seq {
                    continue;
                }
                if agent.is_some() && event.agent_id != agent {
                    continue;
                }
                print_event(&event, json);
            }
            Ok(Push::Shutdown) | Err(_) => return Ok(()),
            Ok(_) => {}
        }
    }
}

fn print_message(m: &Message) {
    let arrow = if m.is_broadcast() {
        "⇶"
    } else if m.is_p2p() {
        "→"
    } else {
        "⇉"
    };
    println!(
        "{} {:>13} {arrow} {:<13} [{}] {}",
        fmt_clock(m.timestamp),
        m.from,
        m.to,
        m.kind,
        truncate_chars(&one_line(&m.body), 140)
    );
}

pub fn messages(target: &Target, agent: Option<String>, lines: usize, json: bool) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let messages: Vec<Message> = client.call(Call::History {
        agent,
        limit: Some(lines),
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
    } else {
        for m in &messages {
            print_message(m);
        }
    }
    Ok(())
}

pub fn send(target: &Target, to: &str, body: &str, kind: &str) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let sent: SendResult = client.call(Call::Send(SendParams {
        to: to.to_string(),
        kind: kind.parse().map_err(|e: String| anyhow!(e))?,
        body: body.to_string(),
        ..SendParams::default()
    }))?;
    println!("sent {} to {}", sent.message.id, sent.recipients.join(", "));
    Ok(())
}

fn open_questions(client: &mut Client) -> Result<(Vec<Message>, i64)> {
    let snapshot: Snapshot = client.call(Call::Snapshot {
        messages: Some(0),
        events: Some(0),
    })?;
    Ok((snapshot.questions, snapshot.server_time))
}

/// `crew questions`: what the team wants the operator to decide.
pub fn questions(target: &Target, json: bool) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let (open, now) = open_questions(&mut client)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&open)?);
        return Ok(());
    }
    if open.is_empty() {
        println!("no question is waiting for you");
        return Ok(());
    }
    for question in &open {
        println!(
            "{}  from {} · {} ago",
            question.id,
            question.asker(),
            fmt_elapsed((now - question.timestamp).max(0))
        );
        for line in question.body.trim_end().lines() {
            println!("  {line}");
        }
        for (i, option) in question.options().iter().enumerate() {
            println!("    {}. {option}", i + 1);
        }
        println!();
    }
    println!("answer with: crew answer <id> \"your answer\"   (a number picks a suggested answer)");
    Ok(())
}

/// `crew answer`: answer an open question, in words or by picking one of
/// its suggested answers.
pub fn answer(target: &Target, id: Option<&str>, answer: &str) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let (open, _) = open_questions(&mut client)?;
    let question = match id {
        Some(id) => {
            // The id as shown, or any unambiguous part of it.
            let mut matches = open.iter().filter(|q| q.id == id || q.id.contains(id));
            match (matches.next(), matches.next()) {
                (Some(question), None) => question,
                (Some(_), Some(_)) => bail!("'{id}' matches more than one open question; see `crew questions`"),
                _ => bail!("no open question '{id}'; see `crew questions`"),
            }
        }
        None => match open.as_slice() {
            [] => bail!("no question is waiting for you"),
            [only] => only,
            _ => bail!(
                "{} questions are open; name the one you answer: crew answer <id> \"...\" (ids: {})",
                open.len(),
                open.iter().map(|q| q.id.as_str()).collect::<Vec<_>>().join(", ")
            ),
        },
    };
    let options = question.options();
    let body = match answer.trim().parse::<usize>() {
        Ok(number) if (1..=options.len()).contains(&number) => options[number - 1].clone(),
        _ => answer.trim().to_string(),
    };
    refuse_command_answer(&body)?;
    let sent: SendResult = client.call(Call::Answer {
        id: question.id.clone(),
        body: body.clone(),
    })?;
    println!(
        "answered {}: {}",
        sent.recipients.join(", "),
        truncate_chars(&one_line(&body), 120)
    );
    Ok(())
}

/// An answer that is really a slash command was typed in the wrong place.
fn refuse_command_answer(answer: &str) -> Result<()> {
    match slash_command(answer) {
        Some((name, _)) => bail!("'/{name}' looks like a command, not an answer; nothing was sent"),
        None => Ok(()),
    }
}

pub fn integrate(target: &Target) -> Result<()> {
    let (mut client, _) = target.connect("cli")?;
    let result: Value = client.call(Call::Integrate)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_older_than_the_crew_being_run_is_pointed_out() {
        // Started before this build: it runs the older code until it is restarted.
        let note = stale_since(1_000, 2_000).expect("a note");
        assert!(
            note.contains("crew stop --keep-agents && crew start"),
            "{note}"
        );
        // Started by this build (or a later one): nothing to say.
        assert_eq!(stale_since(2_000, 2_000), None);
        assert_eq!(stale_since(3_000, 2_000), None);
    }

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
    fn finished_work_on_agent_branches_comes_with_a_landing_hint() {
        let mut snapshot = Snapshot::default();
        snapshot.session.workspace = "/work/app".to_string();
        snapshot.session.isolation = "worktree".to_string();
        assert!(landing_hint(&snapshot).is_none(), "still running");
        snapshot.session.completed_at = Some(1);
        let hint = landing_hint(&snapshot).unwrap();
        assert!(
            hint.contains("crew integrate") && hint.contains("/work/app"),
            "{hint}"
        );
        // Shared workspace: the files are already where the user looks.
        snapshot.session.isolation = "shared".to_string();
        assert!(landing_hint(&snapshot).is_none());
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
