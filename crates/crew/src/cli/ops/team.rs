//! Starting a team and opening its dashboard: `crew run`, and what plain `crew`
//! and `crew start` share with it.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use crew_core::client::Client;
use crew_core::config::{BackendChoice, TeamConfig, ROSTER_FILE};
use crew_core::paths::{self, DaemonInfo};
use crew_core::rpc::{Call, SessionInfo, Snapshot};

use super::super::daemonize;
use super::Target;
use crate::endpoints::Layers;
use crate::models::Catalog;

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

pub(super) fn set_backend(config: &mut TeamConfig, backend: Option<&str>) -> Result<()> {
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

pub(super) fn saved_roster(workspace: &Path) -> Option<TeamConfig> {
    TeamConfig::load(&paths::state_dir(workspace).join(ROSTER_FILE)).ok()
}

/// A team file without agents leaves the roster to the orchestrator. If it
/// already settled on one in an earlier run, start from that.
pub(super) fn reuse_roster(config: &mut TeamConfig, workspace: &Path) {
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
pub(super) fn launch(config: TeamConfig, workspace: &Path, no_attach: bool) -> Result<()> {
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
        "team '{}' started: {} agent(s), backend {}, isolation {}{}",
        config.project,
        config.agents.len() + usize::from(config.orchestrator.agent),
        session.backend,
        session.isolation,
        vcs_suffix(&session)
    );
    let note = (!session.vcs_note.is_empty()).then(|| session.vcs_note.clone());
    if let Some(note) = &note {
        println!("note: {note}");
    }
    println!("workspace  {}", session.workspace);
    println!(
        "daemon     pid {} (keeps running when you close the dashboard)",
        session.daemon_pid
    );
    open_dashboard(&info, no_attach, note)
}

/// `, vcs jj` when the agents have workspaces of their own.
pub(super) fn vcs_suffix(session: &SessionInfo) -> String {
    if session.vcs.is_empty() {
        String::new()
    } else {
        format!(", vcs {}", session.vcs)
    }
}

/// Open the dashboard on the team; `note` is what it says first.
pub(super) fn open_dashboard(
    info: &DaemonInfo,
    no_attach: bool,
    note: Option<String>,
) -> Result<()> {
    if no_attach || !std::io::stdout().is_terminal() {
        println!("dashboard  crew attach -C {}", info.workspace);
        return Ok(());
    }
    if dashboard(info, note)? == crew_tui::Exit::Stopped {
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

/// Show the dashboard until the user leaves it, opening with `note` (the
/// team's version control fallback, when it was just started). When they
/// stopped the team from it, wait for the daemon to go and say what is
/// left.
pub(super) fn dashboard(info: &DaemonInfo, note: Option<String>) -> Result<crew_tui::Exit> {
    // A team started by an older crew sends its agents without their
    // models' proper names; the catalog supplies them.
    let stale = stale_daemon_note(info);
    let notice = match (&note, &stale) {
        (Some(note), Some(stale)) => Some(format!("{note} — {stale}")),
        (Some(note), None) => Some(note.clone()),
        (None, stale) => stale.clone(),
    };
    let exit = crew_tui::run(
        Path::new(&info.socket),
        Catalog::load().display_names(),
        notice,
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
pub(super) fn stale_daemon_note(info: &DaemonInfo) -> Option<String> {
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
pub(super) fn wait_for_exit(pid: u32) {
    for _ in 0..300 {
        if !paths::pid_alive(pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Where the results are once every task is done. With a worktree per agent
/// nothing lands in the workspace's own checkout until it is merged there.
pub(super) fn landing_hint(snapshot: &Snapshot) -> Option<String> {
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
}
