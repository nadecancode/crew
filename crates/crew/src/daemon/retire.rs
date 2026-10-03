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
        let start = crate::process::stat(pid)?.start;
        Some(Proc { pid, start })
    }

    /// Whether it has not finished. A zombie has, and so has a process
    /// whose pid now names a later one. What `/proc` does not say for
    /// certain counts as running.
    pub fn running(&self) -> bool {
        match std::fs::read_to_string(format!("/proc/{}/stat", self.pid)) {
            Ok(stat) => crate::process::parse_stat(&stat)
                .is_none_or(|s| s.start == self.start && !matches!(s.state, 'Z' | 'X')),
            Err(e) => !matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)),
        }
    }

    /// Whether it is, for certain, this process and alive: `/proc` shows
    /// its pid with this start time, not a zombie. Anything `/proc` does not
    /// say for certain counts as no (the opposite of `running`).
    pub fn alive(&self) -> bool {
        self.is(crate::process::stat(self.pid))
    }

    fn is(&self, stat: Option<crate::process::Stat>) -> bool {
        stat.is_some_and(|s| s.start == self.start && !matches!(s.state, 'Z' | 'X'))
    }
}

/// The init of the PID namespace a sandbox runs its programs in, `wrapper`
/// being the process the daemon started (`crew sandbox-run`), named while
/// nothing could have reaped it: pid 1 of a namespace of its own, the child
/// of bubblewrap (the wrapper's only child) or, once bubblewrap has gone,
/// of the wrapper. None while it is not there, and once the wrapper has
/// ended or cannot be told apart from a later process with its number.
///
/// Every number is read again after use: the wrapper, still alive with its
/// start time, was never reaped, so its pid and its children's were its
/// own throughout; a parent still among its children is bubblewrap, alive
/// since before the candidate was read; and the candidate, alive with the
/// start time first read, is the process whose namespace was read.
pub(crate) fn sandbox_init(wrapper: Proc) -> Option<Proc> {
    if !wrapper.alive() {
        return None;
    }
    let ns = namespace_of(wrapper.pid)?;
    let kids = children(wrapper.pid);
    let candidates: Vec<u32> = kids
        .iter()
        .copied()
        .chain(kids.iter().flat_map(|&kid| children(kid)))
        .collect();
    candidates.into_iter().find_map(|candidate| {
        let stat = crate::process::stat(candidate)?;
        let init = Proc {
            pid: candidate,
            start: stat.start,
        };
        let nested = namespace_of(candidate).is_some_and(|n| n != ns);
        let ours = wrapper.alive()
            && (stat.ppid == wrapper.pid || children(wrapper.pid).contains(&stat.ppid));
        (nested && first_in_its_namespace(candidate) && ours && init.alive()).then_some(init)
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

/// This boot's identity, or "" when it cannot be read.
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
    /// Its files were removed, the sandbox being gone: it is never written
    /// again. Set and read under `state`'s lock.
    forgotten: std::sync::atomic::AtomicBool,
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

    /// Change the entry, on disk too, unless it went (its sandbox known to
    /// be gone, its files removed). A change that cannot be written leaves
    /// the earlier file, which proves less.
    pub fn update(&self, f: impl FnOnce(&mut Retiring)) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut state);
        if !self.0.forgotten.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = save(&self.0.file, &state);
        }
    }

    /// The sandbox is known to be gone (see the module's documentation).
    pub fn gone(&self, boot: &str) -> bool {
        let r = self.get();
        deleted(&self.0.file)
            || earlier_boot(&r.boot, boot)
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

/// Whether an entry started in boot `entry` is from another boot than
/// `now`: only when both are known (an entry saved, or a daemon started,
/// without the boot id proves nothing).
fn earlier_boot(entry: &str, now: &str) -> bool {
    !entry.is_empty() && !now.is_empty() && entry != now
}

fn save(file: &Path, state: &Retiring) -> Result<()> {
    paths::write_private_file(file, &serde_json::to_vec_pretty(state)?)
}

/// Remove `record`'s files, for good: under its lock, so no update writes
/// the entry again afterwards.
fn forget(record: &Record) {
    let _state = record.0.state.lock().unwrap_or_else(|e| e.into_inner());
    record
        .0
        .forgotten
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = std::fs::remove_file(&record.0.file);
    let _ = std::fs::remove_file(&record.0.done);
}

/// Every agent's entries, kept in a directory of their own.
pub(crate) struct Ledger {
    dir: PathBuf,
    boot: String,
    agents: Mutex<HashMap<String, Vec<Record>>>,
    /// Files found at start that could not be read: they may be any
    /// agent's, until the operator deletes them.
    unreadable: Mutex<Vec<PathBuf>>,
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
            if earlier_boot(&state.boot, &boot) {
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
                    forgotten: Default::default(),
                })));
        }
        Ok(Ledger {
            dir: dir.to_path_buf(),
            boot,
            agents: Mutex::new(agents),
            unreadable: Mutex::new(unreadable),
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
            forgotten: Default::default(),
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

    /// End what is left of `agent`'s sandboxes: the init of each entry not
    /// gone that was identified while it ran, in this boot. Called only by
    /// a launch holding the agent's launch lease alone, its session not
    /// alive and its sidecar stopped, so every such entry is of a launch
    /// that has ended; a sandbox still running is one its parent did not
    /// take along (killed before bubblewrap tied it to it). Nothing else
    /// is signalled, and a signal proves nothing: `retire` still waits for
    /// the init to finish or the marker to say so. Returns how many were
    /// signalled.
    pub fn end_orphans(&self, agent: &str) -> usize {
        self.of(agent)
            .iter()
            .filter(|record| !record.gone(&self.boot))
            .filter_map(|record| {
                let r = record.get();
                let this_boot = !r.boot.is_empty() && r.boot == self.boot;
                r.init.filter(|_| this_boot)
            })
            .filter(|init| signal_init(*init))
            .count()
    }

    /// Wait until every sandbox of `agent` is gone, then drop their
    /// entries. Entries leave only once gone, so a launch that gives up
    /// here leaves the next one the same wait.
    pub async fn retire(&self, agent: &str, within: Duration) -> Result<()> {
        let unreadable = {
            let mut files = self.unreadable.lock().unwrap_or_else(|e| e.into_inner());
            files.retain(|file| !deleted(file));
            files.first().cloned()
        };
        if let Some(file) = unreadable {
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

/// SIGKILL to `init`, a namespace's init, only if it is that process for
/// certain: a pidfd is opened on its pid, then `/proc` must show that pid
/// alive with the init's start time and as pid 1 of its namespace. The
/// pidfd names the process that had the pid when it was opened; the init,
/// alive after that with its start time, had it then. So the signal,
/// through the pidfd, reaches the init or (if it has just ended) no one,
/// never a later process with its number. Anything uncertain sends
/// nothing. Whether it was sent.
fn signal_init(init: Proc) -> bool {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // SAFETY: plain syscall; no memory is passed.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, init.pid as libc::pid_t, 0) };
    let Ok(fd) = i32::try_from(fd) else {
        return false;
    };
    if fd < 0 {
        return false;
    }
    // SAFETY: a descriptor pidfd_open just returned, owned here only.
    let pidfd = unsafe { OwnedFd::from_raw_fd(fd) };
    if !(init.alive() && first_in_its_namespace(init.pid)) {
        return false;
    }
    // SAFETY: plain syscall on a live pidfd; a null siginfo is allowed.
    let sent = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    sent == 0
}

/// How long a sandbox may take to show the init of its PID namespace.
pub(crate) const DISCOVER_WITHIN: Duration = Duration::from_secs(5);

/// Find the init of the sandbox the daemon has just started as `wrapper`
/// (sandbox_init). `Ok(None)`: the wrapper ended first, or cannot be told
/// apart from a later process; its entry then stays unknown unless its
/// done marker proves it gone.
pub(crate) async fn discover(wrapper: Proc, within: Duration) -> Result<Option<Proc>> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if let Some(init) = sandbox_init(wrapper) {
            return Ok(Some(init));
        }
        if !wrapper.alive() {
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

    #[tokio::test]
    async fn an_entry_saved_without_a_boot_id_is_never_taken_for_an_earlier_boots() {
        let dir = tempfile::tempdir().unwrap();
        let short = Duration::from_millis(100);
        // Saved by a daemon that could not read the boot id.
        let unknown = Retiring {
            agent: "a".into(),
            what: "session".into(),
            boot: String::new(),
            pid: Some(4242),
            init: None,
        };
        let file = dir.path().join("unknown.json");
        std::fs::write(&file, serde_json::to_vec(&unknown).unwrap()).unwrap();
        // The next daemon can read it: the entry is kept and holds `a`.
        let ledger = Ledger::open(dir.path()).unwrap();
        assert!(file.exists(), "removed as an earlier boot's");
        assert_eq!(ledger.of("a").len(), 1);
        assert!(ledger.retire("a", short).await.is_err());
        // The same for an entry this daemon made while it could not.
        let record = ledger.start("b", "git").unwrap();
        record.update(|r| r.boot = String::new());
        assert!(ledger.retire("b", short).await.is_err());
    }

    #[tokio::test]
    async fn an_entry_that_went_is_not_written_again_by_whoever_still_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let short = Duration::from_millis(100);
        let ledger = Ledger::open(dir.path()).unwrap();
        // Its sandbox reported (it did not start) before the launch that
        // holds the entry recorded the pid; another launch of the agent
        // dropped the entries that are gone in between.
        let reported = ledger.start("a", "queue").unwrap();
        std::fs::write(reported.done(), "not started\n").unwrap();
        ledger.forget_gone("a");
        reported.update(|r| r.pid = Some(4242));
        // The operator deleted one, and a holder updates it afterwards.
        let deleted = ledger.start("b", "session").unwrap();
        std::fs::remove_file(file_of(&deleted)).unwrap();
        ledger.retire("b", short).await.unwrap();
        deleted.update(|r| r.pid = Some(4343));
        for (record, what) in [(&reported, "reported"), (&deleted, "deleted")] {
            assert!(
                !file_of(record).exists(),
                "the {what} entry was written again"
            );
        }
        let next = Ledger::open(dir.path()).unwrap();
        assert!(next.of("a").is_empty() && next.of("b").is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_entry_the_operator_deleted_no_longer_holds_anyone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let short = Duration::from_millis(100);
        let torn = dir.path().join("torn.json");
        std::fs::write(&torn, "{\"agent\": ").unwrap();
        let ledger = Ledger::open(dir.path()).unwrap();
        assert!(ledger.retire("a", short).await.is_err());
        // While it cannot be checked, it still holds.
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::set_permissions(dir.path(), mode(0o000)).unwrap();
        let refused = ledger.retire("a", short).await;
        std::fs::set_permissions(dir.path(), mode(0o700)).unwrap();
        assert!(refused.is_err(), "an entry that could not be checked went");
        // Deleted, as the refusal says: the next launch goes through, with
        // no daemon restart.
        std::fs::remove_file(&torn).unwrap();
        ledger.retire("a", short).await.unwrap();
        ledger.retire("b", short).await.unwrap();
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

    /// The processes in PID namespace `ns`, by identity. Read only while the
    /// namespace is known to be alive (its init runs): once it is gone, its
    /// number may name a later namespace, another test's sandbox.
    fn procs_in(ns: u64) -> Vec<Proc> {
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| namespace_of(*pid) == Some(ns))
            .filter_map(Proc::of)
            .collect()
    }

    /// Those of `seen` still running.
    fn still_running(seen: &[Proc]) -> Vec<Proc> {
        seen.iter().copied().filter(Proc::running).collect()
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
            init = Proc::of(pid).and_then(sandbox_init);
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
        let seen = procs_in(ns);
        let entry = ledger.start("a", "session").unwrap();
        entry.update(|r| r.init = Some(init));
        let short = Duration::from_millis(100);
        assert!(ledger.retire("a", short).await.is_err(), "still running");
        // Killed, as the daemon kills a session: gone once its init has
        // finished, and by then nothing is left in its namespace.
        sandbox.kill().unwrap();
        sandbox.wait().unwrap();
        ledger.retire("a", Duration::from_secs(5)).await.unwrap();
        assert_eq!(still_running(&seen), Vec::<Proc>::new());
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
            // Its init runs (it is killed only below), so the number is its
            // namespace's.
            let left = procs_in(ns);
            // SAFETY: plain kill of the test's sandbox init.
            unsafe { libc::kill(init.pid as libc::pid_t, libc::SIGKILL) };
            assert!(
                left.iter().any(|p| *p != init),
                "{script}: nothing outlived bubblewrap"
            );
            assert!(
                refused.is_err(),
                "{script}: gone while its init ran ({status})"
            );
            ledger.retire("a", Duration::from_secs(5)).await.unwrap();
            assert_eq!(still_running(&left), Vec::<Proc>::new());
        }
    }

    #[tokio::test]
    async fn a_launch_finds_only_its_own_sandboxs_init_and_none_once_it_has_ended() {
        let Some(bwrap) = bwrap() else { return };
        let mut sandbox = sandbox_running(&bwrap, true, "exec sleep 304")
            .spawn()
            .unwrap();
        let wrapper = Proc::of(sandbox.id()).unwrap();
        let within = Duration::from_millis(300);
        // Its own launch finds it.
        let found = discover(wrapper, within).await.unwrap();
        let (init, _) = init_of(sandbox.id());
        assert_eq!(found, Some(init));
        // A launch that has ended, whose number now names this process (its
        // start time differs), finds nothing, though a nested namespace's
        // init runs under that number.
        let ended = Proc {
            start: wrapper.start - 1,
            ..wrapper
        };
        let adopted = discover(ended, within).await.unwrap();
        sandbox.kill().unwrap();
        sandbox.wait().unwrap();
        assert_eq!(
            adopted, None,
            "an ended launch adopted another sandbox's init"
        );
        // Nor once its own process is gone.
        assert_eq!(discover(wrapper, within).await.unwrap(), None);
        // What /proc does not say for certain is no proof of life.
        assert!(!wrapper.is(None));
        assert!(!wrapper.alive());
    }

    /// Wait, up to 20 s, until `ready`; else fail with what `seen` says.
    fn within_20s(what: &str, mut ready: impl FnMut() -> bool, seen: impl Fn() -> String) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !ready() {
            if std::time::Instant::now() >= deadline {
                panic!("{what} did not happen within 20 s: {}", seen());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether the sandbox whose init is `init` is fully up: its `program`
    /// runs, so bubblewrap has set its own PR_SET_PDEATHSIG (it does so
    /// before it lets the init go on), and the init waits for its
    /// children, which it does right after setting its PR_SET_PDEATHSIG
    /// (bubblewrap.c, do_init). bubblewrap checks its parent neither time:
    /// a sandbox whose parent is killed before then does not die with it.
    fn fully_up(init: Proc, program: &str) -> bool {
        let runs =
            namespace_of(init.pid).is_some_and(|ns| in_namespace(ns).iter().any(|c| c == program));
        // Still the init whose namespace was read.
        runs && wchan(init.pid) == "do_wait" && init.alive()
    }

    fn wchan(pid: u32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default()
    }

    /// A stand-in sidecar: `sh` starting a sandbox that runs `sleep
    /// <marker>`; the sandbox dies with `sh` or (`outlives`) does not. Put in
    /// the daemon's sidecars for `id`, with its init, once the sandbox is
    /// fully up.
    async fn sidecar_for(daemon: &Daemon, id: &str, bwrap: &Path, outlives: bool) -> Proc {
        sidecar_running(daemon, id, bwrap, outlives, "").await
    }

    /// sidecar_for, its sandbox's program first running `before` (a shell
    /// fragment).
    async fn sidecar_running(
        daemon: &Daemon,
        id: &str,
        bwrap: &Path,
        outlives: bool,
        before: &str,
    ) -> Proc {
        let marker = 400_000 + std::process::id() % 1_000 + u32::from(outlives);
        let inner = sandbox_running(bwrap, !outlives, &format!("{before}exec sleep {marker}"));
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
        let wrapper = Proc::of(child.id().unwrap()).unwrap();
        let mut init = None;
        within_20s(
            "the stand-in sidecar's sandbox",
            || {
                init = sandbox_init(wrapper);
                init.is_some()
            },
            || format!("no init under sh {wrapper:?}"),
        );
        // Stopped before it is fully up, a sandbox told to die with its
        // parent may outlive it.
        let program = format!("sleep {marker}");
        let up = init.unwrap();
        within_20s(
            "the stand-in sidecar's sandbox to be fully up",
            || fully_up(up, &program),
            || {
                let ns = namespace_of(up.pid);
                format!(
                    "{:?} in its namespace, init {up:?} in {:?}",
                    ns.map(in_namespace),
                    wchan(up.pid)
                )
            },
        );
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

        // Stopped when its session exited or was killed (on_exit, kill_agent),
        // before the restart, its sandbox not tied to it (as one is whose
        // parent was killed before bubblewrap tied it): the restart ends it,
        // and lets the home be seeded only once nothing of it is left.
        let descendant = 500_000 + std::process::id() % 1_000;
        let init = sidecar_running(
            &daemon,
            "a",
            &bwrap,
            true,
            &format!("setsid sleep {descendant} </dev/null >/dev/null 2>&1 & "),
        )
        .await;
        let ns = namespace_of(init.pid).unwrap();
        eventually("its setsid descendant", || {
            in_namespace(ns).contains(&format!("sleep {descendant}"))
        });
        // Its init runs, so the number is its namespace's.
        let seen = procs_in(ns);
        daemon.stop_sidecar("a");
        let retired = daemon
            .retire_sandboxes("a", &procs, Duration::from_secs(5))
            .await;
        let left = still_running(&seen);
        let outlived = init.running();
        if outlived {
            // SAFETY: plain kill of the test's sandbox init.
            unsafe { libc::kill(init.pid as libc::pid_t, libc::SIGKILL) };
        }
        assert!(
            retired.is_ok(),
            "the orphaned sandbox of a stopped sidecar holds the restart: {retired:?}"
        );
        assert!(!outlived, "let through while its init ran");
        assert_eq!(
            left,
            Vec::<Proc>::new(),
            "let through while its sandbox ran"
        );
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
    async fn only_an_orphan_identified_for_certain_in_this_boot_is_signalled() {
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

        // An orphan whose entry has no boot id: not signalled, and it holds
        // every try, the entry kept.
        let orphan = sidecar_for(&daemon, "a", &bwrap, true).await;
        daemon.stop_sidecar("a");
        for record in daemon.ledger.of("a") {
            record.update(|r| r.boot = String::new());
        }
        let first = daemon.retire_sandboxes("a", &procs, short).await;
        let second = daemon.retire_sandboxes("a", &procs, short).await;
        let alive = orphan.running();
        // SAFETY: plain kill of the test's sandbox init.
        unsafe { libc::kill(orphan.pid as libc::pid_t, libc::SIGKILL) };
        assert!(alive, "an orphan of unknown boot was signalled");
        assert!(
            first.is_err() && second.is_err(),
            "let through while it ran"
        );
        assert_eq!(daemon.ledger.of("a").len(), 1, "the entry went");

        // A live process named by an entry: its own identity, but not the
        // init of a namespace; and its pid with another start time (a
        // recycled number). Neither is signalled.
        let mut control = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .unwrap();
        let me = Proc::of(control.id()).unwrap();
        let named = daemon.ledger.start("b", "sidecar").unwrap();
        named.update(|r| r.init = Some(me));
        let refused = daemon.retire_sandboxes("b", &procs, short).await;
        let recycled = daemon.ledger.start("c", "sidecar").unwrap();
        recycled.update(|r| {
            r.init = Some(Proc {
                start: me.start - 1,
                ..me
            })
        });
        let gone = daemon.retire_sandboxes("c", &procs, short).await;
        // An entry with no init: nothing to signal, and it holds.
        daemon
            .ledger
            .start("d", "sidecar")
            .unwrap()
            .update(|r| r.pid = Some(me.pid));
        let unknown = daemon.retire_sandboxes("d", &procs, short).await;
        let untouched = me.alive();
        control.kill().unwrap();
        control.wait().unwrap();
        assert!(
            untouched,
            "a process that is no namespace's init was signalled"
        );
        assert!(refused.is_err(), "let through while the named process ran");
        assert!(
            gone.is_ok(),
            "an init whose pid was recycled is gone: {gone:?}"
        );
        assert!(unknown.is_err(), "an entry with no init was let through");
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
