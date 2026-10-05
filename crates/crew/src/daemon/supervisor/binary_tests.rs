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
use crate::process::{ProcEvent, ProcessManager, Spawned};

/// Processes that are only recorded: what was started, what was stopped,
/// and which agents still run (an adopted one included).
#[derive(Default)]
struct Recorded {
    spawned: Mutex<Vec<LaunchSpec>>,
    killed: Mutex<Vec<String>>,
    alive: Mutex<HashSet<String>>,
    fail_kill: AtomicBool,
    wait_kill: AtomicBool,
    kill_entered: tokio::sync::Notify,
    kill_release: tokio::sync::Notify,
    spawn_with_identity: AtomicBool,
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
            pid: self
                .spawn_with_identity
                .load(Ordering::SeqCst)
                .then_some(23456),
            backend_ref: self
                .spawn_with_identity
                .load(Ordering::SeqCst)
                .then(|| "new-pane".into()),
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
        let launches = || {
            self.spawned
                .lock()
                .unwrap()
                .iter()
                .filter(|spec| spec.agent_id == id)
                .count()
        };
        let stopped_instance = launches();
        let fail = self.fail_kill.load(Ordering::SeqCst);
        if self.wait_kill.swap(false, Ordering::SeqCst) {
            self.kill_entered.notify_one();
            self.kill_release.notified().await;
        }
        if fail {
            anyhow::bail!("the backend could not stop the old process");
        }
        // A delayed reply belongs to the process being stopped, not to a
        // replacement that another operation launched before it arrived.
        if launches() == stopped_instance {
            self.alive.lock().unwrap().remove(id);
        }
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
    binary_fixture_with_cache(agents, cannot_pin, None, false).await
}

async fn binary_fixture_with_cache(
    agents: &[(&str, &str)],
    cannot_pin: bool,
    cached_bytes: Option<&str>,
    remove_cache: bool,
) -> Pinned {
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
    if let Some(bytes) = cached_bytes {
        use std::os::unix::fs::PermissionsExt;

        let cached = crate::daemon::binary::Binary::pin(
            &workspace.join(".crew"),
            &running_binary,
            running_binary.clone(),
        );
        assert!(cached.name().is_some());
        std::fs::set_permissions(cached.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(cached.path(), bytes).unwrap();
        std::fs::set_permissions(cached.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        if remove_cache {
            std::fs::remove_file(cached.path()).unwrap();
        }
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
    let p = binary_fixture_with_cache(
        &[("same", "shell"), ("other", "shell"), ("unknown", "shell")],
        false,
        Some("the running build"),
        false,
    )
    .await;
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

/// Agent `id` recorded as launched with another build's copy, which is in
/// `<state>/bin`, and its process (pid 12345, pane "old-pane") as having
/// outlived the previous daemon. Returns that copy's path.
fn of_another_build(p: &Pinned, id: &str) -> PathBuf {
    let daemon = p.daemon();
    let old = daemon.state_dir.join("bin/crew-0000000000000000");
    std::fs::write(&old, "another build").unwrap();
    let mut model = daemon.model();
    model.local.get_mut(id).unwrap().binary = Some("crew-0000000000000000".into());
    let agent = model.agents.iter_mut().find(|a| a.id == id).unwrap();
    agent.pid = Some(12345);
    agent.backend_ref = Some("old-pane".into());
    old
}

/// Its record is the process that outlived the previous daemon, nothing
/// replaced it, and the copy it runs is still there.
fn still_held(p: &Pinned, id: &str, old: &Path) {
    let agent = p.daemon().agent(id).unwrap();
    assert_eq!(
        (agent.pid, agent.backend_ref.as_deref()),
        (Some(12345), Some("old-pane"))
    );
    assert_ne!(agent.status, AgentStatus::Spawning);
    assert!(p.procs.spawned.lock().unwrap().is_empty(), "restarted");
    assert!(old.exists(), "the copy it runs was deleted");
}

#[tokio::test]
async fn an_agent_of_another_build_that_could_not_be_stopped_is_held_until_a_stop_works() {
    let p = pinned(&[("other", "shell")]).await;
    let daemon = p.daemon();
    let old = of_another_build(&p, "other");
    p.procs.fail_kill.store(true, Ordering::SeqCst);
    assert!(
        !daemon.recover_agents(p.procs.clone()).await,
        "recovery took an agent that still runs another build for decided"
    );
    still_held(&p, "other", &old);
    let events = daemon.store.recent_events(None, 50).unwrap();
    assert!(
        events.iter().any(|e| e.event_type == "agent.stop_failed"
            && e.level == crew_core::EventLevel::Warn
            && e.message.starts_with("other could not be stopped")),
        "{events:?}"
    );

    // What its backend reports of it is not its end; nor is a tick on
    // which it still cannot be stopped.
    daemon
        .on_proc_event(ProcEvent::Exited {
            agent_id: "other".into(),
            instance: "12345".into(),
            code: None,
            detail: "lost the connection".into(),
        })
        .await;
    daemon.binary_housekeeping().await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    still_held(&p, "other", &old);

    // A stop that works: it is restarted with this daemon's copy, and the
    // other copy goes.
    p.procs.fail_kill.store(false, Ordering::SeqCst);
    daemon.binary_housekeeping().await;
    for _ in 0..500 {
        if !p.procs.spawned.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(Path::new(&p.launched("other").program), p.pin());
    assert!(!old.exists());
    let name = daemon.binary.name().unwrap().to_string();
    assert_eq!(
        launched_with(daemon, "other").as_deref(),
        Some(name.as_str())
    );
    assert_eq!(
        std::fs::read_dir(daemon.state_dir.join("bin"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [name]
    );
}

#[tokio::test]
async fn an_operators_stop_that_works_ends_the_hold_on_an_agent_of_another_build() {
    let p = pinned(&[("other", "shell")]).await;
    let daemon = p.daemon();
    let old = of_another_build(&p, "other");
    p.procs.fail_kill.store(true, Ordering::SeqCst);
    assert!(!daemon.recover_agents(p.procs.clone()).await);
    // A stop that fails holds it still.
    assert!(daemon.kill_agent("other").await.is_err());
    daemon.binary_housekeeping().await;
    still_held(&p, "other", &old);

    p.procs.fail_kill.store(false, Ordering::SeqCst);
    daemon.kill_agent("other").await.unwrap();
    daemon.binary_housekeeping().await;
    assert!(!old.exists());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(daemon.agent("other").unwrap().status, AgentStatus::Stopped);
    assert!(
        p.procs.spawned.lock().unwrap().is_empty(),
        "a stopped agent was restarted"
    );
}

#[tokio::test]
async fn reviewer_an_inflight_operator_stop_keeps_the_old_binary_pinned() {
    let p = pinned(&[("other", "shell")]).await;
    let daemon = p.daemon();
    let old = of_another_build(&p, "other");
    p.procs.fail_kill.store(true, Ordering::SeqCst);
    assert!(!daemon.recover_agents(p.procs.clone()).await);

    p.procs.wait_kill.store(true, Ordering::SeqCst);
    let stop = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.kill_agent("other").await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        p.procs.kill_entered.notified(),
    )
    .await
    .unwrap();
    // A fix may serialize this tick behind the pending stop; either way
    // the old copy must remain until that stop has actually succeeded.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        daemon.binary_housekeeping(),
    )
    .await;
    let kept = old.exists();
    p.procs.kill_release.notify_one();
    assert!(stop.await.unwrap().is_err());

    assert!(p.procs.is_alive("other"));
    assert!(
        kept,
        "the old copy was swept while its stop was still pending"
    );
    still_held(&p, "other", &old);
}

#[tokio::test]
async fn reviewer_a_failed_stop_retry_keeps_its_record_during_backend_events() {
    let p = pinned(&[("other", "shell")]).await;
    let daemon = p.daemon();
    let old = of_another_build(&p, "other");
    p.procs.fail_kill.store(true, Ordering::SeqCst);
    assert!(!daemon.recover_agents(p.procs.clone()).await);

    p.procs.wait_kill.store(true, Ordering::SeqCst);
    let retry = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.stop_other_builds().await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        p.procs.kill_entered.notified(),
    )
    .await
    .unwrap();
    daemon
        .on_proc_event(ProcEvent::Exited {
            agent_id: "other".into(),
            instance: "12345".into(),
            code: None,
            detail: "lost the connection".into(),
        })
        .await;
    p.procs.kill_release.notify_one();
    assert!(!retry.await.unwrap());

    assert!(p.procs.is_alive("other"));
    still_held(&p, "other", &old);
}

#[tokio::test]
async fn reviewer_initial_recovery_keeps_its_record_during_a_failed_stop() {
    let p = pinned(&[("other", "shell")]).await;
    let daemon = p.daemon();
    let old = of_another_build(&p, "other");
    p.procs.fail_kill.store(true, Ordering::SeqCst);
    p.procs.wait_kill.store(true, Ordering::SeqCst);
    let recovery = tokio::spawn({
        let daemon = daemon.clone();
        let procs = p.procs.clone();
        async move { daemon.recover_agents(procs).await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        p.procs.kill_entered.notified(),
    )
    .await
    .unwrap();
    daemon
        .on_proc_event(ProcEvent::Exited {
            agent_id: "other".into(),
            instance: "12345".into(),
            code: None,
            detail: "lost the connection".into(),
        })
        .await;
    p.procs.kill_release.notify_one();
    assert!(!recovery.await.unwrap());

    assert!(p.procs.is_alive("other"));
    still_held(&p, "other", &old);
}

#[tokio::test]
async fn reviewer_a_late_stop_retry_does_not_clear_the_replacements_record() {
    let p = pinned(&[("other", "shell")]).await;
    let daemon = p.daemon();
    of_another_build(&p, "other");
    p.procs.fail_kill.store(true, Ordering::SeqCst);
    assert!(!daemon.recover_agents(p.procs.clone()).await);

    p.procs.fail_kill.store(false, Ordering::SeqCst);
    p.procs.wait_kill.store(true, Ordering::SeqCst);
    p.procs.spawn_with_identity.store(true, Ordering::SeqCst);
    let retry = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.stop_other_builds().await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        p.procs.kill_entered.notified(),
    )
    .await
    .unwrap();

    // A later operator restart completes its stop and launch while the
    // housekeeping retry still waits for the earlier stop's response.
    let restart = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.restart_agent("other").await }
    });
    // Allow either a later restart overtaking this retry or serialization
    // of the two stops. In both cases the final process record must belong
    // to the replacement returned by Recorded::spawn.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while daemon.agent("other").unwrap().pid != Some(23456) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    p.procs.kill_release.notify_one();
    retry.await.unwrap();
    restart.await.unwrap().unwrap();
    for _ in 0..500 {
        if !p.procs.spawned.lock().unwrap().is_empty()
            && daemon.agent("other").unwrap().status != AgentStatus::Spawning
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(p.procs.spawned.lock().unwrap().len(), 1);
    assert!(p.procs.is_alive("other"));

    let agent = daemon.agent("other").unwrap();
    assert_eq!(
        (agent.pid, agent.backend_ref.as_deref()),
        (Some(23456), Some("new-pane")),
        "the earlier retry erased the later restart's process"
    );
}

#[tokio::test]
async fn reviewer_replacing_a_cached_pin_restarts_its_old_agents_and_reports_the_repair() {
    let p = binary_fixture_with_cache(
        &[("same", "shell")],
        false,
        Some("a different build"),
        false,
    )
    .await;
    let daemon = p.daemon();
    assert_eq!(
        std::fs::read_to_string(p.pin()).unwrap(),
        "the running build"
    );
    let said = daemon
        .store
        .recent_events(None, 50)
        .unwrap()
        .iter()
        .any(|e| e.level == crew_core::EventLevel::Warn && e.message.contains("build"));
    daemon.model().local.get_mut("same").unwrap().binary =
        Some(daemon.binary.name().unwrap().to_string());

    daemon.recover_agents(p.procs.clone()).await;

    assert_eq!(
        *p.procs.killed.lock().unwrap(),
        vec!["same".to_string()],
        "an agent using the replaced copy was adopted by name alone"
    );
    assert!(said, "startup repaired a different build without a warning");
}

#[tokio::test]
async fn a_stop_given_up_halfway_leaves_an_agent_of_another_build_held() {
    let p = pinned(&[("other", "shell")]).await;
    let daemon = p.daemon();
    let old = of_another_build(&p, "other");
    p.procs.fail_kill.store(true, Ordering::SeqCst);
    assert!(!daemon.recover_agents(p.procs.clone()).await);

    // An operator's stop, dropped while the backend has not answered.
    p.procs.fail_kill.store(false, Ordering::SeqCst);
    p.procs.wait_kill.store(true, Ordering::SeqCst);
    let stop = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.kill_agent("other").await }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        p.procs.kill_entered.notified(),
    )
    .await
    .unwrap();
    stop.abort();
    assert!(stop.await.unwrap_err().is_cancelled());
    still_held(&p, "other", &old);

    // It is still the tick's to stop, and once that works it is restarted
    // with this daemon's copy and the other copy goes.
    daemon.binary_housekeeping().await;
    for _ in 0..500 {
        if !p.procs.spawned.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(Path::new(&p.launched("other").program), p.pin());
    assert!(!old.exists());
}

#[tokio::test]
async fn an_agent_recorded_with_a_copy_replaced_at_start_is_restarted_and_the_event_says_why() {
    // Between two starts of this build, its copy was written over with
    // other bytes of the same length, and its mode put back.
    let p = binary_fixture_with_cache(
        &[("same", "shell")],
        false,
        Some("the bUnning build"),
        false,
    )
    .await;
    let daemon = p.daemon();
    let name = daemon.binary.name().unwrap().to_string();
    let events = daemon.store.recent_events(None, 50).unwrap();
    let repinned: Vec<_> = events
        .iter()
        .filter(|e| e.event_type == "daemon.repinned")
        .collect();
    assert_eq!(repinned.len(), 1, "{events:?}");
    assert_eq!(repinned[0].level, crew_core::EventLevel::Warn);
    assert_eq!(
        repinned[0].message,
        format!(
            "the copy of the crew binary at {} was not this build: it was made again from the \
running binary, and agents launched with it are restarted, not adopted",
            p.pin().display()
        )
    );

    daemon.model().local.get_mut("same").unwrap().binary = Some(name.clone());
    daemon.recover_agents(p.procs.clone()).await;
    for _ in 0..500 {
        if !p.procs.spawned.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(Path::new(&p.launched("same").program), p.pin());
    let events = daemon.store.recent_events(None, 50).unwrap();
    assert!(
        events.iter().any(|e| e.event_type == "agent.other_build"
            && e.message
                == format!(
                    "same was launched with {name}, which was not found intact when this daemon \
started; it is stopped, not adopted"
                )),
        "{events:?}"
    );
}

#[tokio::test]
async fn reviewer_only_a_pin_found_intact_at_start_can_adopt_a_surviving_agent() {
    for missing in [false, true] {
        let p = binary_fixture_with_cache(
            &[("same", "shell")],
            false,
            Some("the running build"),
            missing,
        )
        .await;
        let daemon = p.daemon();
        let name = daemon.binary.name().unwrap().to_string();
        daemon.model().local.get_mut("same").unwrap().binary = Some(name);
        assert_eq!(
            std::fs::read_to_string(p.pin()).unwrap(),
            "the running build"
        );

        daemon.recover_agents(p.procs.clone()).await;

        let expected: Vec<String> = if missing { vec!["same".into()] } else { vec![] };
        assert_eq!(
            *p.procs.killed.lock().unwrap(),
            expected,
            "missing at startup: {missing}; a recreated path does not restore a survivor's tools"
        );
        if missing {
            for _ in 0..500 {
                if !p.procs.spawned.lock().unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert_eq!(Path::new(&p.launched("same").program), p.pin());
            let events = daemon.store.recent_events(None, 50).unwrap();
            assert!(
                events.iter().any(|e| e.event_type == "agent.other_build"
                    && e.message.contains("was not found intact")),
                "{events:?}"
            );
        } else {
            assert!(p.procs.spawned.lock().unwrap().is_empty());
            assert!(daemon.model().rt("same").ready);
        }
    }
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
