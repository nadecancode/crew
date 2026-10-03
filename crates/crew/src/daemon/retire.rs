//! The retirement ledger (design-b3.md §3.4, D2): every sandbox the daemon
//! starts for an agent stays in it until the sandbox is known to be gone,
//! and nothing of the agent's next launch (its writable tree, identity,
//! briefing, CLI configuration, home) is written while one is left.
//!
//! An entry is made, and written to disk, before its sandbox is started. It
//! is kept under the daemon's state directory, which no sandbox mounts, so
//! a restarted daemon is bound by what an earlier one started. An entry
//! goes only once its sandbox is known to be gone:
//! - `crew sandbox-run` wrote the entry's done marker. It does that only
//!   after it has reaped bubblewrap and the init of the sandbox's PID
//!   namespace (it is their subreaper), and the kernel lets that init
//!   finish only after it has reaped every other process in the namespace,
//!   descendants in sessions or namespaces of their own included;
//! - or the namespace's init, named by its pid and start time while it ran,
//!   has finished;
//! - or the entry is from an earlier boot;
//! - or the operator deleted its file, having checked that nothing of the
//!   sandbox is left.
//!
//! Nothing else counts: not how a process the daemon started ended
//! (bubblewrap returns before its init is reaped), nor elapsed time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use crew_core::paths;
use serde::{Deserialize, Serialize};

/// A process, named by its pid and its start time (clock ticks after boot).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Proc {
    pub pid: u32,
    pub start: u64,
}

impl Proc {
    /// Process `pid` as it is now, if there is one.
    pub fn of(pid: u32) -> Option<Proc> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, start) = parse_stat(&stat)?;
        Some(Proc { pid, start })
    }

    /// Whether it has not finished. A zombie has, and so has a process
    /// whose pid now names a later one. What `/proc` does not say for
    /// certain counts as running.
    pub fn running(&self) -> bool {
        match std::fs::read_to_string(format!("/proc/{}/stat", self.pid)) {
            Ok(stat) => parse_stat(&stat)
                .is_none_or(|(state, start)| start == self.start && !matches!(state, 'Z' | 'X')),
            Err(e) => !matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)),
        }
    }
}

/// The state (field 3) and start time (field 22) in a `/proc/<pid>/stat`
/// line. The command name before them is in parentheses and may hold
/// anything, so the fields are counted from the last `)`.
fn parse_stat(stat: &str) -> Option<(char, u64)> {
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    Some((
        fields.first()?.chars().next()?,
        fields.get(19)?.parse().ok()?,
    ))
}

/// The init of the PID namespace a sandbox runs its programs in, `pid`
/// being the process the daemon started (`crew sandbox-run`): bubblewrap's
/// child, or the shim's own once bubblewrap has gone, pid 1 of a namespace
/// of its own. None while it is not there.
pub(crate) fn sandbox_init(pid: u32) -> Option<Proc> {
    let ns = namespace_of(pid)?;
    children(pid)
        .into_iter()
        .flat_map(|child| std::iter::once(child).chain(children(child)))
        .find_map(|candidate| {
            let init = Proc::of(candidate)?;
            let nested = namespace_of(candidate).is_some_and(|n| n != ns);
            // pid 1 where it is, and still the process just read.
            (nested && first_in_its_namespace(candidate) && init.running()).then_some(init)
        })
}

fn children(pid: u32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .map(|c| {
            c.split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether process `pid` is pid 1 of its innermost PID namespace.
fn first_in_its_namespace(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/status")).is_ok_and(|status| {
        status
            .lines()
            .find_map(|l| l.strip_prefix("NSpid:"))
            .and_then(|n| n.split_whitespace().last())
            == Some("1")
    })
}

/// The PID namespace process `pid` is in.
fn namespace_of(pid: u32) -> Option<u64> {
    let link = std::fs::read_link(format!("/proc/{pid}/ns/pid")).ok()?;
    link.to_str()?
        .strip_prefix("pid:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// This boot's identity, or "" when it cannot be read (then no entry is
/// taken for an earlier boot's).
fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|b| b.trim().to_string())
        .unwrap_or_default()
}

/// What the daemon knows about one sandbox it started for an agent, as kept
/// on disk.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Retiring {
    pub agent: String,
    /// `session`, `sidecar`, `git` or `queue`, for messages.
    pub what: String,
    /// The boot it was started in.
    pub boot: String,
    /// The process the daemon started (`crew sandbox-run`), once it has.
    pub pid: Option<u32>,
    /// The init of its PID namespace, once seen.
    pub init: Option<Proc>,
}

/// One entry of the ledger, shared by whoever learns about its sandbox.
#[derive(Clone, Debug)]
pub(crate) struct Record(Arc<Entry>);

#[derive(Debug)]
struct Entry {
    file: PathBuf,
    done: PathBuf,
    state: Mutex<Retiring>,
}

impl Record {
    /// Where `crew sandbox-run` reports that the sandbox is gone.
    pub fn done(&self) -> &Path {
        &self.0.done
    }

    pub fn get(&self) -> Retiring {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Change the entry, on disk too. A change that cannot be written
    /// leaves the earlier file, which proves less.
    pub fn update(&self, f: impl FnOnce(&mut Retiring)) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut state);
        let _ = save(&self.0.file, &state);
    }

    /// The sandbox is known to be gone (see the module's documentation).
    pub fn gone(&self, boot: &str) -> bool {
        let r = self.get();
        deleted(&self.0.file)
            || (!boot.is_empty() && r.boot != boot)
            || reported(&self.0.done)
            || r.init.is_some_and(|init| !init.running())
    }

    /// Why it is not known to be gone.
    fn left(&self) -> String {
        let r = self.get();
        match (r.pid, r.init) {
            (_, Some(_)) => format!("processes of its {} sandbox are still running", r.what),
            (pid, None) => format!(
                "its {} sandbox has not reported that it is gone and {}; once no process of it is \
left, delete {} to clear this",
                r.what,
                match pid {
                    Some(_) => "its namespace was not seen",
                    None => "was being started when its launch or the daemon stopped",
                },
                self.0.file.display()
            ),
        }
    }
}

/// Whether `file` is gone for certain: not a file that cannot be checked.
fn deleted(file: &Path) -> bool {
    matches!(std::fs::symlink_metadata(file), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Whether `done` is a done marker `crew sandbox-run` wrote: a regular file
/// holding exactly "exited <code>", "signal <n>" or "not started", and a
/// newline.
fn reported(done: &Path) -> bool {
    let regular = std::fs::symlink_metadata(done).is_ok_and(|m| m.file_type().is_file());
    let text = std::fs::read_to_string(done).unwrap_or_default();
    let number = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    regular
        && text.strip_suffix('\n').is_some_and(|line| {
            line == "not started"
                || line
                    .strip_prefix("exited ")
                    .or_else(|| line.strip_prefix("signal "))
                    .is_some_and(number)
        })
}

fn save(file: &Path, state: &Retiring) -> Result<()> {
    paths::write_private_file(file, &serde_json::to_vec_pretty(state)?)
}

fn forget(record: &Record) {
    let _ = std::fs::remove_file(&record.0.file);
    let _ = std::fs::remove_file(&record.0.done);
}

/// Every agent's entries, kept in a directory of their own.
pub(crate) struct Ledger {
    dir: PathBuf,
    boot: String,
    agents: Mutex<HashMap<String, Vec<Record>>>,
    /// Files found at start that could not be read: they may be any
    /// agent's.
    unreadable: Vec<PathBuf>,
}

impl Ledger {
    /// The ledger kept in `dir`, with the entries an earlier daemon left
    /// there. Those of an earlier boot, and stray files, are removed.
    pub fn open(dir: &Path) -> Result<Ledger> {
        paths::ensure_private_dir(dir)?;
        let boot = boot_id();
        let mut agents: HashMap<String, Vec<Record>> = HashMap::new();
        let mut unreadable = Vec::new();
        let listing =
            std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
        for item in listing {
            let path = item
                .with_context(|| format!("reading {}", dir.display()))?
                .path();
            if path.extension().is_none_or(|e| e != "json") {
                // A done marker whose entry went, or a write cut short.
                if !path.with_extension("json").exists() {
                    let _ = std::fs::remove_file(&path);
                }
                continue;
            }
            let read = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Retiring>(&b).ok());
            let Some(state) = read else {
                unreadable.push(path);
                continue;
            };
            if !boot.is_empty() && state.boot != boot {
                let _ = std::fs::remove_file(path.with_extension("done"));
                let _ = std::fs::remove_file(&path);
                continue;
            }
            agents
                .entry(state.agent.clone())
                .or_default()
                .push(Record(Arc::new(Entry {
                    done: path.with_extension("done"),
                    file: path,
                    state: Mutex::new(state),
                })));
        }
        Ok(Ledger {
            dir: dir.to_path_buf(),
            boot,
            agents: Mutex::new(agents),
            unreadable,
        })
    }

    /// A new entry for a `what` sandbox about to be started for `agent`,
    /// on disk before this returns: without it nothing may be started.
    pub fn start(&self, agent: &str, what: &str) -> Result<Record> {
        // An agent's git starts a sandbox per command: those gone go here,
        // not only at its next launch.
        self.forget_gone(agent);
        let name = uuid::Uuid::new_v4().simple().to_string();
        let file = self.dir.join(format!("{name}.json"));
        let state = Retiring {
            agent: agent.to_string(),
            what: what.to_string(),
            boot: self.boot.clone(),
            ..Retiring::default()
        };
        save(&file, &state)
            .with_context(|| format!("recording {agent}'s {what} sandbox before it starts"))?;
        let record = Record(Arc::new(Entry {
            done: file.with_extension("done"),
            file,
            state: Mutex::new(state),
        }));
        self.with(agent, |l| l.push(record.clone()));
        Ok(record)
    }

    /// `record`'s sandbox was never started: no process was made.
    pub fn never_started(&self, record: &Record) {
        let agent = record.get().agent;
        self.with(&agent, |l| l.retain(|r| !Arc::ptr_eq(&r.0, &record.0)));
        forget(record);
    }

    /// `agent`'s entries.
    pub fn of(&self, agent: &str) -> Vec<Record> {
        self.with(agent, |l| l.clone())
    }

    /// Drop those of `agent`'s entries that are gone, with their files.
    pub fn forget_gone(&self, agent: &str) {
        let gone: Vec<Record> = self.with(agent, |l| {
            let (gone, kept) = l.drain(..).partition(|r| r.gone(&self.boot));
            *l = kept;
            gone
        });
        gone.iter().for_each(forget);
    }

    /// Wait until every sandbox of `agent` is gone, then drop their
    /// entries. Entries leave only once gone, so a launch that gives up
    /// here leaves the next one the same wait.
    pub async fn retire(&self, agent: &str, within: Duration) -> Result<()> {
        if let Some(file) = self.unreadable.first() {
            bail!(
                "the retirement ledger entry {} cannot be read, so it may be any agent's; once no \
sandbox of this team is left, delete it",
                file.display()
            );
        }
        let records = self.of(agent);
        let deadline = tokio::time::Instant::now() + within;
        let waited = loop {
            let left: Vec<String> = records
                .iter()
                .filter(|r| !r.gone(&self.boot))
                .map(Record::left)
                .collect();
            if left.is_empty() {
                break Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                break Err(anyhow!("{}", left.join("; ")));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        self.forget_gone(agent);
        waited
    }

    fn with<T>(&self, agent: &str, f: impl FnOnce(&mut Vec<Record>) -> T) -> T {
        let mut agents = self.agents.lock().unwrap_or_else(|e| e.into_inner());
        f(agents.entry(agent.to_string()).or_default())
    }
}

/// How long a sandbox may take to show the init of its PID namespace.
pub(crate) const DISCOVER_WITHIN: Duration = Duration::from_secs(5);

/// Find the init of the sandbox the daemon has just started as `pid`, while
/// `running` says it runs. `Ok(None)`: it ended first.
pub(crate) async fn discover(
    pid: u32,
    running: impl Fn() -> bool,
    within: Duration,
) -> Result<Option<Proc>> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if let Some(init) = sandbox_init(pid) {
            return Ok(Some(init));
        }
        if !running() {
            return Ok(None);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("its sandbox's PID namespace was not found");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::Daemon;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn a_stat_line_is_read_past_any_command_name() {
        let tail = "S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 987654 20 21";
        assert_eq!(
            parse_stat(&format!("42 (sleep) {tail}")),
            Some(('S', 987654))
        );
        assert_eq!(
            parse_stat(&format!("42 (a) Z 1) (b) {tail}")),
            Some(('S', 987654))
        );
        assert_eq!(parse_stat("42 (sleep) S 1"), None);
    }

    #[test]
    fn a_process_is_running_until_it_is_a_zombie_or_its_pid_names_another() {
        let me = Proc::of(std::process::id()).unwrap();
        assert!(me.running());
        assert!(
            !Proc {
                start: me.start + 1,
                ..me
            }
            .running(),
            "a reused pid"
        );
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let proc = Proc::of(child.id()).unwrap();
        assert!(proc.running());
        // SAFETY: plain kill of the test's child, not reaped yet.
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGKILL) };
        eventually("the child to be a zombie", || !proc.running());
        assert!(
            Path::new(&format!("/proc/{}", child.id())).exists(),
            "not reaped yet"
        );
        child.wait().unwrap();
        assert!(!proc.running());
    }

    /// A process that runs until killed, and its name.
    fn stand_in() -> (std::process::Child, Proc) {
        let child = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .unwrap();
        let proc = Proc::of(child.id()).unwrap();
        (child, proc)
    }

    fn file_of(record: &Record) -> PathBuf {
        record.0.file.clone()
    }

    #[tokio::test]
    async fn an_entry_is_on_disk_before_its_sandbox_starts_and_binds_the_next_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let short = Duration::from_millis(100);
        let ledger = Ledger::open(dir.path()).unwrap();
        let record = ledger.start("a", "session").unwrap();
        let file = file_of(&record);
        assert_eq!(
            serde_json::from_slice::<Retiring>(&std::fs::read(&file).unwrap()).unwrap(),
            Retiring {
                agent: "a".into(),
                what: "session".into(),
                boot: boot_id(),
                pid: None,
                init: None,
            }
        );
        let (mut child, init) = stand_in();
        record.update(|r| {
            r.pid = Some(4242);
            r.init = Some(init);
        });
        drop((record, ledger));

        // The next daemon finds it, and waits for the same init.
        let ledger = Ledger::open(dir.path()).unwrap();
        let found = ledger.of("a");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].get().init, Some(init));
        let refused = ledger.retire("a", short).await.unwrap_err();
        assert!(
            format!("{refused:#}").contains("still running"),
            "{refused:#}"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        ledger.retire("a", short).await.unwrap();
        assert!(ledger.of("a").is_empty());
        assert!(!file.exists());
        assert!(Ledger::open(dir.path()).unwrap().of("a").is_empty());
    }

    #[tokio::test]
    async fn an_entry_nothing_proves_gone_holds_until_its_done_marker_or_the_operator() {
        let dir = tempfile::tempdir().unwrap();
        let short = Duration::from_millis(100);
        let ledger = Ledger::open(dir.path()).unwrap();
        // Started, but its init never seen (its shim was killed early, or
        // the daemon stopped): every try is refused, naming the file.
        let unseen = ledger.start("a", "session").unwrap();
        unseen.update(|r| r.pid = Some(4242));
        for _ in 0..2 {
            let refused = format!("{:#}", ledger.retire("a", short).await.unwrap_err());
            assert!(
                refused.contains(&format!("delete {}", file_of(&unseen).display())),
                "{refused}"
            );
        }
        // Not even started: the same.
        let unstarted = ledger.start("b", "git").unwrap();
        let refused = format!("{:#}", ledger.retire("b", short).await.unwrap_err());
        assert!(refused.contains("was being started"), "{refused}");
        // Only what `crew sandbox-run` writes reports it gone.
        for forged in ["", "exited\n", "exited 0", "gone\n", "exited 0\nmore\n"] {
            std::fs::write(unseen.done(), forged).unwrap();
            assert!(ledger.retire("a", short).await.is_err(), "{forged:?}");
        }
        std::fs::remove_file(unseen.done()).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), unseen.done()).unwrap();
        std::fs::write(dir.path().join("elsewhere"), "exited 0\n").unwrap();
        assert!(ledger.retire("a", short).await.is_err(), "a link");
        std::fs::remove_file(unseen.done()).unwrap();
        std::fs::write(unseen.done(), "exited 0\n").unwrap();
        ledger.retire("a", short).await.unwrap();
        assert!(!unseen.done().exists());
        // A file that cannot be checked is not taken for deleted.
        use std::os::unix::fs::PermissionsExt;
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(dir.path(), mode(0o000)).unwrap();
        let refused = ledger.retire("b", short).await;
        std::fs::set_permissions(dir.path(), mode(0o700)).unwrap();
        assert!(refused.is_err(), "an entry that could not be checked went");
        // The operator, having checked, deletes it.
        std::fs::remove_file(file_of(&unstarted)).unwrap();
        ledger.retire("b", short).await.unwrap();
        for report in ["signal 9\n", "not started\n"] {
            let record = ledger.start("d", "git").unwrap();
            std::fs::write(record.done(), report).unwrap();
            ledger.retire("d", short).await.unwrap();
        }
        // One that was never started goes at once.
        let never = ledger.start("c", "queue").unwrap();
        ledger.never_started(&never);
        assert!(!file_of(&never).exists());
        ledger.retire("c", short).await.unwrap();
    }

    #[tokio::test]
    async fn entries_of_an_earlier_boot_go_and_an_unreadable_one_holds_every_agent() {
        let dir = tempfile::tempdir().unwrap();
        let short = Duration::from_millis(100);
        let earlier = Retiring {
            agent: "a".into(),
            what: "session".into(),
            boot: "an earlier boot".into(),
            pid: Some(4242),
            init: None,
        };
        let old = dir.path().join("old.json");
        std::fs::write(&old, serde_json::to_vec(&earlier).unwrap()).unwrap();
        let stray = dir.path().join("stray.done");
        std::fs::write(&stray, "").unwrap();
        let ledger = Ledger::open(dir.path()).unwrap();
        assert!(ledger.of("a").is_empty());
        assert!(!old.exists() && !stray.exists());
        ledger.retire("a", short).await.unwrap();

        let torn = dir.path().join("torn.json");
        std::fs::write(&torn, "{\"agent\": ").unwrap();
        let ledger = Ledger::open(dir.path()).unwrap();
        for agent in ["a", "b"] {
            let refused = format!("{:#}", ledger.retire(agent, short).await.unwrap_err());
            assert!(refused.contains(&torn.display().to_string()), "{refused}");
        }
    }

    /// bubblewrap from PATH, if the host has it (a failure under
    /// CREW_TEST_SANDBOX=require otherwise).
    fn bwrap() -> Option<PathBuf> {
        let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join("bwrap"))
            .find(|p| p.is_file());
        crate::sandbox::test_support::host_has(found.is_some(), "bubblewrap")
            .then(|| found.unwrap())
    }

    /// A sandbox like an agent's (user and PID namespaces, /proc and /dev),
    /// running `script`.
    fn sandbox_running(bwrap: &Path, die_with_parent: bool, script: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new(bwrap);
        cmd.args(["--unshare-user", "--unshare-pid"]);
        if die_with_parent {
            cmd.arg("--die-with-parent");
        }
        cmd.args(["--ro-bind", "/", "/", "--proc", "/proc", "--dev", "/dev"])
            .args(["sh", "-c", script]);
        cmd
    }

    /// The command lines of the processes in PID namespace `ns`.
    fn in_namespace(ns: u64) -> Vec<String> {
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| namespace_of(*pid) == Some(ns))
            .filter_map(|pid| std::fs::read(format!("/proc/{pid}/cmdline")).ok())
            .map(|c| {
                String::from_utf8_lossy(&c)
                    .replace('\0', " ")
                    .trim()
                    .to_string()
            })
            .collect()
    }

    /// Wait (up to 2 s) for `found` to hold.
    fn eventually(what: &str, mut found: impl FnMut() -> bool) {
        for _ in 0..200 {
            if found() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{what} never happened");
    }

    /// The init of the sandbox started as `pid`, and its namespace.
    fn init_of(pid: u32) -> (Proc, u64) {
        let mut init = None;
        eventually("the sandbox's init", || {
            init = sandbox_init(pid);
            init.is_some()
        });
        let init = init.unwrap();
        let ns = namespace_of(init.pid).unwrap();
        assert_ne!(Some(ns), namespace_of(std::process::id()));
        (init, ns)
    }

    #[tokio::test]
    async fn a_sandboxs_init_finishes_only_after_its_setsid_descendant() {
        let Some(bwrap) = bwrap() else { return };
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path()).unwrap();
        let mut sandbox = sandbox_running(
            &bwrap,
            true,
            "setsid sleep 301 </dev/null >/dev/null 2>&1 & exec sleep 300",
        )
        .spawn()
        .unwrap();
        let (init, ns) = init_of(sandbox.id());
        // Both are running in it, the descendant in a session of its own.
        eventually("the setsid descendant", || {
            in_namespace(ns).iter().any(|c| c == "sleep 301")
        });
        assert!(in_namespace(ns).iter().any(|c| c == "sleep 300"));
        let entry = ledger.start("a", "session").unwrap();
        entry.update(|r| r.init = Some(init));
        let short = Duration::from_millis(100);
        assert!(ledger.retire("a", short).await.is_err(), "still running");
        // Killed, as the daemon kills a session: gone once its init has
        // finished, and by then nothing is left in its namespace.
        sandbox.kill().unwrap();
        sandbox.wait().unwrap();
        ledger.retire("a", Duration::from_secs(5)).await.unwrap();
        assert_eq!(in_namespace(ns), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_sandbox_whose_bubblewrap_has_exited_is_not_gone_while_its_init_runs() {
        let Some(bwrap) = bwrap() else { return };
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path()).unwrap();
        let short = Duration::from_millis(300);
        // Killed, or (bubblewrap returns once its program has) exited on
        // its own: neither says its init has finished.
        for (script, kill) in [
            ("exec sleep 302", true),
            (
                "setsid sleep 303 </dev/null >/dev/null 2>&1 & sleep 0.5; exit 0",
                false,
            ),
        ] {
            let mut sandbox = sandbox_running(&bwrap, false, script).spawn().unwrap();
            let (init, ns) = init_of(sandbox.id());
            ledger.start("a", "session").unwrap().update(|r| {
                r.pid = Some(sandbox.id());
                r.init = Some(init);
            });
            if kill {
                sandbox.kill().unwrap();
            }
            let status = sandbox.wait().unwrap();
            let refused = ledger.retire("a", short).await;
            let left = in_namespace(ns);
            // SAFETY: plain kill of the test's sandbox init.
            unsafe { libc::kill(init.pid as libc::pid_t, libc::SIGKILL) };
            assert!(!left.is_empty(), "{script}: nothing outlived bubblewrap");
            assert!(
                refused.is_err(),
                "{script}: gone while its init ran ({status})"
            );
            ledger.retire("a", Duration::from_secs(5)).await.unwrap();
            assert_eq!(in_namespace(ns), Vec::<String>::new());
        }
    }

    /// A stand-in sidecar: `sh` starting a sandbox that runs `sleep
    /// <marker>`; the sandbox dies with `sh` or (`outlives`) does not. Put in
    /// the daemon's sidecars for `id`, with its init.
    async fn sidecar_for(daemon: &Daemon, id: &str, bwrap: &Path, outlives: bool) -> Proc {
        let marker = 400_000 + std::process::id() % 1_000 + u32::from(outlives);
        let inner = sandbox_running(bwrap, !outlives, &format!("exec sleep {marker}"));
        let argv: Vec<String> = std::iter::once(inner.get_program())
            .chain(inner.get_args())
            .map(|a| format!("'{}'", a.to_string_lossy()))
            .collect();
        let entry = daemon.ledger.start(id, "sidecar").unwrap();
        let child = tokio::process::Command::new("sh")
            .args(["-c", &format!("{} & wait", argv.join(" "))])
            .spawn()
            .unwrap();
        // sh's child is bubblewrap, whose child is the init.
        let mut init = None;
        eventually("the stand-in sidecar's sandbox", || {
            init = sandbox_init(child.id().unwrap());
            init.is_some()
        });
        entry.update(|r| {
            r.pid = child.id();
            r.init = init;
        });
        daemon.sidecars.lock().unwrap().insert(
            id.to_string(),
            super::super::SidecarProc {
                child,
                socket: PathBuf::new(),
                env: Vec::new(),
            },
        );
        init.unwrap()
    }

    #[tokio::test]
    async fn a_sidecar_stopped_before_the_restart_is_still_waited_out() {
        let Some(bwrap) = bwrap() else { return };
        let temp = tempfile::tempdir().unwrap();
        let running = super::super::start(super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
        })
        .await
        .unwrap();
        let daemon = running.daemon.clone();
        let procs = crate::process::pty::PtyManager::new(daemon.proc_tx.clone());
        let short = Duration::from_millis(300);

        // Stopped when its session exited or was killed (on_exit, kill_agent),
        // before the restart: its sandbox still has a process, so the home
        // is not seeded.
        let init = sidecar_for(&daemon, "a", &bwrap, true).await;
        daemon.stop_sidecar("a");
        let refused = daemon.retire_sandboxes("a", &procs, short).await;
        // A second try (another restart) is refused too: the entry stayed.
        let again = daemon.retire_sandboxes("a", &procs, short).await;
        let outlived = init.running();
        // SAFETY: plain kill of the test's sandbox init.
        unsafe { libc::kill(init.pid as libc::pid_t, libc::SIGKILL) };
        assert!(outlived, "the stand-in sandbox did not outlive its parent");
        assert!(
            refused.is_err(),
            "seeded while the stopped sidecar's sandbox ran"
        );
        assert!(again.is_err(), "a retry skipped the wait");
        // Once it is gone, the next try goes through and the entry leaves.
        daemon
            .retire_sandboxes("a", &procs, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(daemon.ledger.of("a").is_empty());

        // One that goes with it is waited for, and then the home may be seeded.
        let init = sidecar_for(&daemon, "b", &bwrap, false).await;
        daemon.stop_sidecar("b");
        daemon
            .retire_sandboxes("b", &procs, Duration::from_secs(5))
            .await
            .unwrap();
        assert!(!init.running());
    }

    #[tokio::test]
    async fn a_launch_that_cannot_retire_writes_nothing_of_the_agents_next_generation() {
        let temp = tempfile::tempdir().unwrap();
        let options = || super::super::DaemonOptions {
            workspace: temp.path().to_path_buf(),
            crew_bin: std::env::current_exe().unwrap(),
        };
        // A daemon started a sandbox for `a`, which has not reported that it
        // is gone, and then stopped.
        let running = super::super::start(options()).await.unwrap();
        let agent_dir = running.daemon.agent_dir("a");
        running
            .daemon
            .ledger
            .start("a", "sidecar")
            .unwrap()
            .update(|r| r.pid = Some(4242));
        drop(running);

        // The next daemon, its team requiring sandboxes, is asked to start `a`.
        // (Other tests' forked children may hold the workspace lock a moment.)
        let mut next = super::super::start(options()).await;
        for _ in 0..100 {
            if next.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            next = super::super::start(options()).await;
        }
        let running = next.unwrap();
        let daemon = running.daemon.clone();
        let mut team = crew_core::config::TeamConfig::from_yaml(
            "project: retire\nobjective: retire\nagents:\n  - id: a\n    role: work\n",
        )
        .unwrap();
        team.workspace.sandbox.mode = crew_core::config::SandboxMode::Required;
        daemon.model().session.team = Some(team);
        let agent: crew_core::Agent = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "role": "work", "status": "stopped", "runtime": "shell",
            "workdir": temp.path().to_str().unwrap(), "backend": "pty", "last_activity_at": 0
        }))
        .unwrap();
        daemon.model().agents.push(agent);
        *daemon.procs.write().unwrap() = Some(Arc::new(crate::process::pty::PtyManager::new(
            daemon.proc_tx.clone(),
        )));
        // What the previous generation left: its identity and briefing.
        std::fs::create_dir_all(&agent_dir).unwrap();
        for file in ["identity.json", "briefing.md"] {
            std::fs::write(agent_dir.join(file), "previous generation").unwrap();
        }
        let before: Vec<_> = ["identity.json", "briefing.md"]
            .iter()
            .map(|f| std::fs::metadata(agent_dir.join(f)).unwrap())
            .map(|m| (m.ino(), m.modified().unwrap()))
            .collect();

        let refused = daemon
            .spawn_agent("a", false, None)
            .await
            .map(|_| ())
            .unwrap_err();
        assert!(
            format!("{refused:#}").contains("home is not written"),
            "{refused:#}"
        );
        let after: Vec<_> = ["identity.json", "briefing.md"]
            .iter()
            .map(|f| std::fs::metadata(agent_dir.join(f)).unwrap())
            .map(|m| (m.ino(), m.modified().unwrap()))
            .collect();
        assert_eq!(before, after, "the identity or briefing was rewritten");
        for file in ["identity.json", "briefing.md"] {
            assert_eq!(
                std::fs::read_to_string(agent_dir.join(file)).unwrap(),
                "previous generation"
            );
        }
        assert!(!agent_dir.join("rw").exists(), "its writable tree was made");
        assert_eq!(std::fs::read_dir(&agent_dir).unwrap().count(), 2);
    }
}
