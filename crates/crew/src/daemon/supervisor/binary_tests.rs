//! The copy of crew a daemon keeps (daemon::binary), as its launches and
//! its recovery use it: every launch is handed the copy, and an agent that
//! outlived the previous daemon is adopted only if it was launched with it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use crew_core::config::{SandboxMode, TeamConfig};
use crew_core::{Agent, AgentStatus};

use super::Start;
use crate::agents::{shell_quote, LaunchSpec};
use crate::daemon::{start, AgentLocal, Daemon, DaemonOptions, Running};
use crate::process::{ProcessManager, Spawned};

/// Processes that are only recorded: what was started, what was stopped,
/// and which agents still run (an adopted one included).
#[derive(Default)]
struct Recorded {
    spawned: Mutex<Vec<LaunchSpec>>,
    killed: Mutex<Vec<String>>,
    alive: Mutex<HashSet<String>>,
    fail_kill: AtomicBool,
}

#[async_trait::async_trait]
impl ProcessManager for Recorded {
    fn name(&self) -> &'static str {
        "pty"
    }
    async fn spawn(&self, spec: &LaunchSpec) -> Result<Spawned> {
        self.spawned.lock().unwrap().push(spec.clone());
        self.alive.lock().unwrap().insert(spec.agent_id.clone());
        Ok(Spawned {
            ready: true,
            ..Spawned::default()
        })
    }
    async fn submit_prompt(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    async fn write_raw(&self, _: &str, _: &[u8]) -> Result<()> {
        Ok(())
    }
    async fn screen(&self, _: &str, _: usize) -> Result<String> {
        Ok(String::new())
    }
    async fn pause(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn resume(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn kill(&self, id: &str) -> Result<()> {
        self.killed.lock().unwrap().push(id.to_string());
        if self.fail_kill.load(Ordering::SeqCst) {
            anyhow::bail!("the backend could not stop the old process");
        }
        self.alive.lock().unwrap().remove(id);
        Ok(())
    }
    fn is_alive(&self, id: &str) -> bool {
        self.alive.lock().unwrap().contains(id)
    }
    /// Every agent outlived the previous daemon.
    async fn adopt(&self, agent: &Agent) -> Result<bool> {
        self.alive.lock().unwrap().insert(agent.id.clone());
        Ok(true)
    }
}

struct Pinned {
    _temp: tempfile::TempDir,
    running: Running,
    procs: Arc<Recorded>,
    /// The path the daemon was started from, never handed out.
    on_disk: PathBuf,
}

impl Pinned {
    fn daemon(&self) -> &Daemon {
        &self.running.daemon
    }

    fn pin(&self) -> PathBuf {
        self.daemon().crew_bin.clone()
    }

    fn launched(&self, id: &str) -> LaunchSpec {
        let spawned = self.procs.spawned.lock().unwrap();
        spawned
            .iter()
            .rev()
            .find(|s| s.agent_id == id)
            .unwrap_or_else(|| panic!("{id} was not started"))
            .clone()
    }
}

/// A daemon keeping a copy of a stand-in for the running binary, with a
/// team of `agents` (`id`, runtime), each working on a task of its own.
async fn pinned(agents: &[(&str, &str)]) -> Pinned {
    binary_fixture(agents, false).await
}

async fn binary_fixture(agents: &[(&str, &str)], cannot_pin: bool) -> Pinned {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let running_binary = root.join("running-crew");
    std::fs::write(&running_binary, "the running build").unwrap();
    let workspace = root.join("ws");
    std::fs::create_dir(&workspace).unwrap();
    if cannot_pin {
        std::fs::create_dir(workspace.join(".crew")).unwrap();
        std::fs::write(workspace.join(".crew/bin"), "not a directory").unwrap();
    }
    let on_disk = root.join("target/release/crew");
    let running = start(DaemonOptions {
        workspace: workspace.clone(),
        crew_bin: on_disk.clone(),
        pin: Some(running_binary),
    })
    .await
    .unwrap();
    let daemon = running.daemon.clone();
    let mut team = TeamConfig::from_yaml(
        "project: pinned\nobjective: pinned\nagents:\n  - id: a\n    role: w\n",
    )
    .unwrap();
    team.workspace.sandbox.mode = SandboxMode::Off;
    daemon.model().session.team = Some(team);
    for (id, runtime) in agents {
        let agent: Agent = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "role": "work", "status": "working", "runtime": runtime,
            "workdir": workspace.to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        let task: crew_core::Task = serde_json::from_value(serde_json::json!({
            "id": format!("t-{id}"), "title": "t", "description": "", "status": "working",
            "assigned_agent": id, "created_at": 0
        }))
        .unwrap();
        let mut local = AgentLocal::default();
        local.settings.command = vec!["{crew}".into(), "scripted-agent".into()];
        let mut model = daemon.model();
        model.agents.push(agent);
        model.tasks.push(task);
        model.local.insert(id.to_string(), local);
    }
    let procs = Arc::new(Recorded::default());
    *daemon.procs.write().unwrap() = Some(procs.clone());
    Pinned {
        _temp: temp,
        running,
        procs,
        on_disk,
    }
}

async fn launch(p: &Pinned, id: &str) {
    let daemon = p.daemon();
    daemon
        .spawn_agent(id, Start::New, None, daemon.stops(id))
        .await
        .unwrap();
}

fn launched_with(daemon: &Daemon, id: &str) -> Option<String> {
    daemon.model().local[id].binary.clone()
}

#[tokio::test]
async fn every_launch_hands_out_the_daemons_copy_for_its_tools_and_hooks() {
    let p = pinned(&[("c", "claude"), ("x", "codex"), ("s", "shell")]).await;
    let pin = p.pin();
    let name = p.daemon().binary.name().unwrap().to_string();
    assert_eq!(pin, p.daemon().state_dir.join("bin").join(&name));
    assert_eq!(std::fs::read_to_string(&pin).unwrap(), "the running build");
    let on_disk = p.on_disk.to_string_lossy().into_owned();
    for id in ["c", "x", "s"] {
        launch(&p, id).await;
        assert_eq!(
            launched_with(p.daemon(), id).as_deref(),
            Some(name.as_str())
        );
    }

    // Claude Code: its MCP server, every hook, and the bridge mod.
    let dir = p.daemon().agent_dir("c");
    let read = |file: &str| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap()
    };
    assert_eq!(
        read("mcp.json")["mcpServers"]["crew"]["command"],
        pin.to_str().unwrap()
    );
    let settings = read("settings.json");
    let hooks = settings["hooks"].as_object().unwrap();
    assert!(!hooks.is_empty());
    let quoted = shell_quote(pin.to_str().unwrap());
    for (event, entries) in hooks {
        let command = entries[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            command.starts_with(&format!("{quoted} hook ")),
            "{event}: {command}"
        );
    }
    let bridge = std::fs::read_to_string(dir.join("mod/crew-bridge/hooks/register.ts")).unwrap();
    assert!(bridge.contains(&serde_json::json!(pin).to_string()));
    assert!(!settings.to_string().contains(&on_disk) && !bridge.contains(&on_disk));

    // Codex: its MCP server and its notify hook.
    let codex = p.launched("x").args.join("\n");
    let pin_toml = serde_json::json!(pin).to_string();
    assert!(
        codex.contains(&format!("mcp_servers.crew.command={pin_toml}")),
        "{codex}"
    );
    assert!(
        codex.contains(&format!("notify=[{pin_toml}, \"hook\", \"codex-notify\""))
            || codex.contains(&format!("notify=[{pin_toml},\"hook\",\"codex-notify\"")),
        "{codex}"
    );
    assert!(!codex.contains(&on_disk), "{codex}");

    // A shell agent: `{crew}` and CREW_BIN.
    let shell = p.launched("s");
    assert_eq!(Path::new(&shell.program), pin);
    assert!(shell
        .env
        .contains(&("CREW_BIN".to_string(), pin.to_string_lossy().into_owned())));
}

#[tokio::test]
async fn a_launch_makes_a_copy_that_went_missing_again_first() {
    let p = pinned(&[("s", "shell")]).await;
    std::fs::remove_file(p.pin()).unwrap();
    launch(&p, "s").await;
    assert_eq!(
        std::fs::read_to_string(p.pin()).unwrap(),
        "the running build"
    );
    let events = p.daemon().store.recent_events(None, 50).unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.event_type == "daemon.repinned" && e.level == crew_core::EventLevel::Warn),
        "{events:?}"
    );
}

#[tokio::test]
async fn an_agent_that_outlived_the_daemon_is_adopted_only_if_it_runs_this_build() {
    let p = pinned(&[("same", "shell"), ("other", "shell"), ("unknown", "shell")]).await;
    let daemon = p.daemon();
    let name = daemon.binary.name().unwrap().to_string();
    {
        let mut model = daemon.model();
        model.local.get_mut("same").unwrap().binary = Some(name.clone());
        // Another build's copy; and one launched before copies were kept.
        model.local.get_mut("other").unwrap().binary = Some("crew-0000000000000000".into());
    }
    daemon.recover_agents(p.procs.clone()).await;

    let mut killed = p.procs.killed.lock().unwrap().clone();
    killed.sort();
    assert_eq!(killed, ["other", "unknown"]);
    for _ in 0..500 {
        if p.procs.spawned.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let mut restarted: Vec<String> = p
        .procs
        .spawned
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.agent_id.clone())
        .collect();
    restarted.sort();
    assert_eq!(restarted, ["other", "unknown"], "only these are restarted");
    for id in ["other", "unknown"] {
        let spec = p.launched(id);
        assert_eq!(Path::new(&spec.program), p.pin());
        assert_eq!(launched_with(daemon, id).as_deref(), Some(name.as_str()));
    }
    assert_ne!(daemon.agent("same").unwrap().status, AgentStatus::Spawning);
    let events = daemon.store.recent_events(None, 100).unwrap();
    let other_builds: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "agent.other_build")
        .map(|e| e.message.clone())
        .collect();
    assert_eq!(other_builds.len(), 2, "{other_builds:?}");
    assert!(other_builds
        .iter()
        .any(|m| m.contains("other") && m.contains("crew-0000000000000000")));
    assert!(other_builds
        .iter()
        .any(|m| m.contains("unknown") && m.contains("none recorded")));
}

#[tokio::test]
async fn reviewer_recovery_does_not_forget_an_old_process_whose_kill_failed() {
    let p = pinned(&[("other", "shell")]).await;
    let daemon = p.daemon();
    {
        let mut model = daemon.model();
        model.local.get_mut("other").unwrap().binary = Some("crew-0000000000000000".into());
        let agent = model.agents.iter_mut().find(|a| a.id == "other").unwrap();
        agent.pid = Some(12345);
        agent.backend_ref = Some("old-pane".into());
    }
    p.procs.fail_kill.store(true, Ordering::SeqCst);
    daemon.recover_agents(p.procs.clone()).await;
    assert!(
        p.procs.is_alive("other"),
        "the old process is still running"
    );
    let agent = daemon.agent("other").unwrap();
    assert_eq!(
        agent.pid,
        Some(12345),
        "recovery discarded the surviving process"
    );
    assert_eq!(agent.backend_ref.as_deref(), Some("old-pane"));
    assert_ne!(
        agent.status,
        AgentStatus::Spawning,
        "a replacement was scheduled"
    );
}

#[tokio::test]
async fn reviewer_unpinned_recovery_does_not_treat_two_missing_build_ids_as_a_match() {
    let p = binary_fixture(&[("old", "shell")], true).await;
    let daemon = p.daemon();
    assert!(
        daemon.binary.warning().is_some(),
        "a real pin attempt failed"
    );
    assert!(daemon.binary.name().is_none());
    assert!(daemon.model().local["old"].binary.is_none());
    // An unpinned or legacy process supplies no evidence that its tools
    // belong to this build. Recovery must restart it to use today's path.
    daemon.recover_agents(p.procs.clone()).await;
    assert_eq!(*p.procs.killed.lock().unwrap(), vec!["old".to_string()]);
}

#[tokio::test]
async fn an_in_process_daemon_keeps_the_binary_it_was_given_and_makes_no_copy() {
    let temp = tempfile::tempdir().unwrap();
    let given = std::env::current_exe().unwrap();
    let running = start(DaemonOptions {
        workspace: temp.path().to_path_buf(),
        crew_bin: given.clone(),
        pin: None,
    })
    .await
    .unwrap();
    let daemon = &running.daemon;
    assert_eq!(daemon.crew_bin, given);
    assert!(!daemon.state_dir.join("bin").exists());
    assert_eq!(daemon.session_info().binary_warning, "");
    daemon.sweep_binaries();
    daemon.check_binary().unwrap();
}

#[tokio::test]
async fn a_daemon_that_cannot_keep_a_copy_runs_and_says_so() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().canonicalize().unwrap();
    let running_binary = workspace.join("running-crew");
    std::fs::write(&running_binary, "the running build").unwrap();
    crew_core::paths::ensure_private_dir(&workspace.join(".crew")).unwrap();
    // `<state>/bin` cannot be made: a file is in its way.
    std::fs::write(workspace.join(".crew/bin"), "").unwrap();
    let on_disk = workspace.join("target/release/crew");
    let running = start(DaemonOptions {
        workspace: workspace.clone(),
        crew_bin: on_disk.clone(),
        pin: Some(running_binary),
    })
    .await
    .unwrap();
    let daemon = &running.daemon;
    assert_eq!(daemon.crew_bin, on_disk);
    let warning = daemon.session_info().binary_warning;
    assert!(
        warning.starts_with("could not keep a copy of the crew binary (")
            && warning.contains(&format!("replacing {}", on_disk.display())),
        "{warning}"
    );
    let events = daemon.store.recent_events(None, 50).unwrap();
    let unpinned = events
        .iter()
        .find(|e| e.event_type == "daemon.unpinned")
        .expect("a daemon.unpinned event");
    assert_eq!(unpinned.level, crew_core::EventLevel::Warn);
    assert_eq!(unpinned.message, warning);
}
