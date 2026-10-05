//! The crew binary a daemon hands out (notes/binary-pinning.md).
//!
//! Everything a daemon starts or writes down for its agents names a crew
//! binary: their MCP server, hooks, credential helper and extension, the
//! `{crew}` of a shell agent, `crew sandbox-run` and what it runs inside.
//! Rebuilding crew replaces the file at the path the daemon was started
//! from, and a session that starts after that (after a compaction, a usage
//! fallback, a restart) would run another build against this daemon: every
//! crew tool would fail. So a daemon the crew binary started keeps a copy
//! of the binary it runs in `<state>/bin` and hands out that copy for as
//! long as it lives. Nothing outside crew changes it.
//!
//! The copy is read from the running inode (`/proc/self/exe`), whatever has
//! become of its path, and is named by build: `crew-<id>`, where `<id>` is
//! 16 hex digits of a hash of that inode's device, number, size and
//! modification time, so the same file run again finds its copy there. The
//! name only says which build a copy was made for: one found there is used
//! only if it holds the running binary's bytes, and is made again if not.
//! Copies outlive the daemon, as agents on herdr do, which hold the path
//! they were launched with. An agent launched with another copy, or with
//! one that had to be made again at start, is not adopted
//! (supervisor/recovery.rs), and once recovery has decided about every
//! agent, the other copies are deleted: at once, or, when an agent of
//! another build could not be stopped, once it has been.

use std::ffi::CString;
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::anyhow;
use sha2::{Digest, Sha256};

use super::Daemon;
use crate::persistence::NewEvent;
use crate::sandbox::pin::{Identity, Kind};

/// `<state>/bin`, where the copies are kept.
pub(crate) const BIN_DIR: &str = "bin";

/// What a daemon hands out as the crew binary.
pub(crate) struct Binary {
    path: PathBuf,
    mode: Mode,
    /// The copy could not be made again; said once, until it can.
    failing: AtomicBool,
    /// Recovery left an agent running another build's copy, which it could
    /// not stop: the other copies are deleted once none is left.
    sweep_waits: AtomicBool,
}

enum Mode {
    /// The path the daemon was given: an in-process daemon (a test) keeps it.
    Given,
    Pinned(Pin),
    /// No copy could be kept, for this reason: the path the binary was
    /// started from is handed out.
    Unpinned(String),
}

struct Pin {
    /// The running binary, copied again when the copy goes.
    source: PathBuf,
    dir: PathBuf,
    name: String,
    /// Only a copy found intact at start identifies what surviving agents
    /// ran. Making a copy again does not restore their hooks or tools.
    at_start: AtStart,
    /// The copy as it was made. Held while a copy is made or the directory
    /// swept.
    made: Mutex<Made>,
}

/// What was under the copy's name when the daemon started.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AtStart {
    Intact,
    Missing,
    Replaced,
}

/// What a file was when it was looked at: a file replaced, or written in
/// place, is another. Its change time counts as well, which anything done
/// to the file moves and nobody can set back: a file written in place and
/// given its old modification time again is still another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Made {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
}

impl Made {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
            ctime_ns: i128::from(meta.ctime()) * 1_000_000_000 + i128::from(meta.ctime_nsec()),
        }
    }
}

impl Binary {
    /// The path given, handed out as it is.
    pub(crate) fn given(path: PathBuf) -> Self {
        Self {
            path,
            mode: Mode::Given,
            failing: AtomicBool::new(false),
            sweep_waits: AtomicBool::new(false),
        }
    }

    /// A copy of `source`, the running binary (`/proc/self/exe`), in
    /// `<state_dir>/bin`: found there or made. When none can be kept, the
    /// daemon hands out `started_from`, the path it was started from.
    /// Blocking.
    pub(crate) fn pin(state_dir: &Path, source: &Path, started_from: PathBuf) -> Self {
        let dir = state_dir.join(BIN_DIR);
        let (path, mode) = match keep(&dir, source) {
            Ok((name, made, at_start)) => (
                dir.join(&name),
                Mode::Pinned(Pin {
                    source: source.to_path_buf(),
                    dir,
                    name,
                    at_start,
                    made: Mutex::new(made),
                }),
            ),
            Err(reason) => (started_from, Mode::Unpinned(reason)),
        };
        Self {
            path,
            mode,
            failing: AtomicBool::new(false),
            sweep_waits: AtomicBool::new(false),
        }
    }

    /// What everything the daemon starts is handed. The same for the
    /// daemon's whole life.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The copy's name (`crew-<id>`), recorded with each agent launched
    /// with it; `None` when the daemon hands out no copy.
    pub(crate) fn name(&self) -> Option<&str> {
        match &self.mode {
            Mode::Pinned(pin) => Some(&pin.name),
            Mode::Given | Mode::Unpinned(_) => None,
        }
    }

    /// Whether an agent launched with `launched_with` (the copy's name
    /// recorded for it) runs the build this daemon runs. A daemon that
    /// could not keep a copy cannot tell what anything runs: an agent
    /// recorded without a name was launched with whatever file was at some
    /// path then. Nor can one whose copy was missing or something else at
    /// start: an agent launched with that name ran whatever was there
    /// then, and its tools may have failed while the copy was gone.
    pub(crate) fn launched_this(&self, launched_with: Option<&str>) -> bool {
        match &self.mode {
            Mode::Pinned(pin) => {
                pin.at_start == AtStart::Intact && launched_with == Some(pin.name.as_str())
            }
            // An in-process daemon (a test) has only the path it was given.
            Mode::Given => launched_with.is_none(),
            Mode::Unpinned(_) => false,
        }
    }

    /// Recovery left an agent running another build's copy: the sweep
    /// waits until none is left (Daemon::binary_housekeeping).
    pub(crate) fn defer_sweep(&self) {
        self.sweep_waits.store(true, Ordering::SeqCst);
    }

    /// The copy as it was made, which a sandbox binds and nothing else.
    pub(crate) fn identity(&self) -> Option<Identity> {
        let Mode::Pinned(pin) = &self.mode else {
            return None;
        };
        let made = *pin.made.lock().unwrap_or_else(|e| e.into_inner());
        Some(Identity {
            dev: made.dev,
            ino: made.ino,
            kind: Kind::File,
        })
    }

    /// When the file found under the copy's name at start was not the
    /// running binary, and was replaced: what the operator is told.
    pub(crate) fn replaced(&self) -> Option<String> {
        match &self.mode {
            Mode::Pinned(pin) if pin.at_start == AtStart::Replaced => Some(format!(
                "the copy of the crew binary at {} was not this build: it was made again from the \
running binary, and agents launched with it are restarted, not adopted",
                self.path.display()
            )),
            _ => None,
        }
    }

    /// When no copy could be kept: what the operator is told.
    pub(crate) fn warning(&self) -> Option<String> {
        let Mode::Unpinned(reason) = &self.mode else {
            return None;
        };
        Some(format!(
            "could not keep a copy of the crew binary ({reason}): replacing {} while this team runs \
will break its agents' tools",
            self.path.display()
        ))
    }

    /// Whether the copy handed out is still the one made: there, a regular
    /// file of this user's, read-and-run for them alone, as it was made. If
    /// not, it is made again from the running binary under the same name,
    /// and what had become of it is returned. Blocking.
    pub(crate) fn check(&self) -> Result<Option<String>, String> {
        let Mode::Pinned(pin) = &self.mode else {
            return Ok(None);
        };
        let mut made = pin.made.lock().unwrap_or_else(|e| e.into_inner());
        let what = match fs::symlink_metadata(&self.path) {
            Ok(meta) if intact(&meta, &made) => return Ok(None),
            Ok(_) => "was replaced or changed",
            Err(e) if e.kind() == io::ErrorKind::NotFound => "was gone",
            Err(_) => "could not be looked at",
        };
        let remade = crew_core::paths::ensure_private_dir(&pin.dir)
            .map_err(|e| format!("{e:#}"))
            .and_then(|()| {
                let mut source = File::open(&pin.source)
                    .map_err(|e| format!("reading {}: {e}", pin.source.display()))?;
                copy(&mut source, &pin.dir, &pin.name).map_err(|e| e.to_string())
            });
        match remade {
            Ok(new) => {
                *made = new;
                Ok(Some(format!(
                    "the copy of the crew binary at {} {what}: it was made again from the running \
binary",
                    self.path.display()
                )))
            }
            Err(e) => Err(format!(
                "the copy of the crew binary at {} {what}, and it could not be made again: {e}",
                self.path.display()
            )),
        }
    }

    /// Delete everything in `<state>/bin` but the copy handed out: what
    /// earlier daemons kept, and what a copy cut short left. Called once
    /// recovery has decided about every agent, when none runs another copy.
    /// Returns the names deleted.
    pub(crate) fn sweep(&self) -> Vec<String> {
        let Mode::Pinned(pin) = &self.mode else {
            return Vec::new();
        };
        // Not beside a copy being made, whose temporary file would go.
        let _made = pin.made.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(entries) = fs::read_dir(&pin.dir) else {
            return Vec::new();
        };
        let mut removed = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.as_bytes() == pin.name.as_bytes() {
                continue;
            }
            let path = entry.path();
            let gone = match entry.file_type() {
                Ok(kind) if kind.is_dir() => fs::remove_dir_all(&path),
                _ => fs::remove_file(&path),
            };
            if gone.is_ok() {
                removed.push(name.to_string_lossy().into_owned());
            }
        }
        removed
    }
}

/// The copy of `source` in `dir`, found or made: its name, what it is, and
/// what was under its name at start.
fn keep(dir: &Path, source: &Path) -> Result<(String, Made, AtStart), String> {
    let mut file = File::open(source).map_err(|e| format!("reading {}: {e}", source.display()))?;
    let running = file
        .metadata()
        .map_err(|e| format!("reading {}: {e}", source.display()))?;
    let name = format!("crew-{}", build_id(&running));
    crew_core::paths::ensure_private_dir(dir).map_err(|e| format!("{e:#}"))?;
    runnable(dir)?;
    let path = dir.join(&name);
    if let Some(made) = kept(&path, &file, running.size()) {
        return Ok((name, made, AtStart::Intact));
    }
    let at_start = if fs::symlink_metadata(&path).is_ok() {
        AtStart::Replaced
    } else {
        AtStart::Missing
    };
    let made = copy(&mut file, dir, &name)
        .map_err(|e| format!("copying it into {}: {e}", dir.display()))?;
    Ok((name, made, at_start))
}

/// The copy an earlier daemon of this build kept at `path`, if that is
/// what is there: a file as `copy` makes them, holding the bytes of
/// `running`, `size` long. Its name says which build it was made for, not
/// what is in it, so it is compared with the running binary. That reads
/// both (from the page cache, as a rule), which costs less than hashing
/// them and much less than a copy synced to disk.
fn kept(path: &Path, running: &File, size: u64) -> Option<Made> {
    // Not through a link, and without waiting on a FIFO.
    let found = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = found.metadata().ok()?;
    let same = as_made(&meta, size) && same_bytes(&found, running).ok()?;
    same.then(|| Made::of(&meta))
}

/// Whether two files hold the same bytes, compared a chunk at a time.
fn same_bytes(a: &File, b: &File) -> io::Result<bool> {
    const CHUNK: usize = 1 << 20;
    let (mut x, mut y) = (vec![0; CHUNK], vec![0; CHUNK]);
    let mut offset = 0;
    loop {
        let n = fill_at(a, &mut x, offset)?;
        if fill_at(b, &mut y, offset)? != n || x[..n] != y[..n] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
        offset += n as u64;
    }
}

/// Read `file` from `offset` until `buf` is full or the file ends; how much
/// was read.
fn fill_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read_at(&mut buf[filled..], offset + filled as u64) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// 16 hex digits naming the build a running inode is.
fn build_id(running: &fs::Metadata) -> String {
    let made = Made::of(running);
    let mut hash = Sha256::new();
    hash.update(made.dev.to_le_bytes());
    hash.update(made.ino.to_le_bytes());
    hash.update(made.size.to_le_bytes());
    hash.update(made.mtime_ns.to_le_bytes());
    hash.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether a file found under a copy's name has the shape `copy` gives
/// them, for a binary `size` bytes long.
fn as_made(meta: &fs::Metadata, size: u64) -> bool {
    meta.file_type().is_file()
        && meta.uid() == crew_core::paths::uid()
        && meta.mode() & 0o7777 == 0o500
        && meta.nlink() == 1
        && meta.size() == size
}

/// Whether the copy is still the file made, and can still be run: a mode
/// changed (made unrunnable, or writable) leaves the rest as it was.
fn intact(meta: &fs::Metadata, made: &Made) -> bool {
    meta.file_type().is_file()
        && meta.uid() == crew_core::paths::uid()
        && meta.mode() & 0o7777 == 0o500
        && Made::of(meta) == *made
}

/// Copy `source` to `dir/name`: written under a temporary name, made
/// read-and-run for its owner only, synced, then renamed into place.
fn copy(source: &mut File, dir: &Path, name: &str) -> io::Result<Made> {
    let temp = dir.join(format!(".{name}.{}", std::process::id()));
    let _ = fs::remove_file(&temp);
    let copied = (|| {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        io::copy(source, &mut out)?;
        out.set_permissions(fs::Permissions::from_mode(0o500))?;
        out.sync_all()?;
        fs::rename(&temp, dir.join(name))?;
        // After the rename, which moves the change time.
        let made = Made::of(&out.metadata()?);
        File::open(dir)?.sync_all()?;
        Ok(made)
    })();
    if copied.is_err() {
        let _ = fs::remove_file(&temp);
    }
    copied
}

/// A copy in `dir` can be run: its file system is neither read-only nor
/// mounted `noexec`, which a file's mode does not show.
fn runnable(dir: &Path) -> Result<(), String> {
    let c_dir = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| format!("{} contains a NUL byte", dir.display()))?;
    // SAFETY: statvfs is plain integers; all-zero is a valid value.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_dir` is a valid C string and `st` a valid out-pointer.
    if unsafe { libc::statvfs(c_dir.as_ptr(), &mut st) } != 0 {
        return Err(format!("{}: {}", dir.display(), io::Error::last_os_error()));
    }
    match refusal(st.f_flag) {
        Some(why) => Err(format!("{} {why}", dir.display())),
        None => Ok(()),
    }
}

/// Why a file system mounted with `flags` (statvfs's) cannot hold a copy
/// that runs.
fn refusal(flags: libc::c_ulong) -> Option<&'static str> {
    if flags & libc::ST_RDONLY != 0 {
        Some("is on a read-only file system")
    } else if flags & libc::ST_NOEXEC != 0 {
        Some("is on a file system mounted noexec")
    } else {
        None
    }
}

impl Daemon {
    /// Binary::check, before every launch and on the housekeeping tick: a
    /// copy made again is a warning; one that cannot be is an error, said
    /// once until it can. Blocking.
    pub(crate) fn check_binary(&self) -> anyhow::Result<()> {
        match self.binary.check() {
            Ok(remade) => {
                self.binary.failing.store(false, Ordering::SeqCst);
                if let Some(what) = remade {
                    self.emit(NewEvent::new("daemon", "daemon.repinned", what).warn());
                }
                Ok(())
            }
            Err(e) => {
                if !self.binary.failing.swap(true, Ordering::SeqCst) {
                    self.emit(NewEvent::new("daemon", "daemon.repin_failed", &e).error());
                }
                Err(anyhow!(e))
            }
        }
    }

    pub(crate) async fn binary_housekeeping(&self) {
        let daemon = self.clone();
        let _ = tokio::task::spawn_blocking(move || daemon.check_binary()).await;
        // Agents recovery could not stop are stopped now if they can be;
        // the copies other builds kept go once none runs one.
        if self.binary.sweep_waits.load(Ordering::SeqCst) && self.stop_other_builds().await {
            self.binary.sweep_waits.store(false, Ordering::SeqCst);
            self.sweep_binaries();
        }
    }

    /// Once recovery has decided about every agent: the copies other builds
    /// kept go.
    pub(crate) fn sweep_binaries(&self) {
        let removed = self.binary.sweep();
        if !removed.is_empty() {
            tracing::info!(
                "deleted {} from {}",
                removed.join(", "),
                self.state_dir.join(BIN_DIR).display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the running binary.
    fn build(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn state() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().canonicalize().unwrap().join(".crew");
        crew_core::paths::ensure_private_dir(&state).unwrap();
        (temp, state)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_copy_is_private_named_by_build_and_found_again_by_the_same_build() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "build one");
        let pinned = Binary::pin(&state, &running, running.clone());
        let name = pinned.name().unwrap().to_string();
        assert!(
            name.starts_with("crew-") && name.len() == "crew-".len() + 16,
            "{name}"
        );
        assert_eq!(pinned.path(), state.join("bin").join(&name));
        assert_eq!(fs::read_to_string(pinned.path()).unwrap(), "build one");
        let meta = fs::symlink_metadata(pinned.path()).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o500);
        let dir = fs::metadata(state.join("bin")).unwrap();
        assert_eq!(dir.mode() & 0o7777, 0o700);
        assert_eq!(pinned.warning(), None);
        assert_eq!(
            pinned.identity(),
            Some(Identity {
                dev: meta.dev(),
                ino: meta.ino(),
                kind: Kind::File
            })
        );

        // The same file run again: the same name, and no copy.
        let again = Binary::pin(&state, &running, running.clone());
        assert_eq!(again.name(), Some(name.as_str()));
        let after = fs::symlink_metadata(again.path()).unwrap();
        assert_eq!(Made::of(&after), Made::of(&meta), "the copy was made again");
        assert_eq!(names(&state.join("bin")), vec![name]);
    }

    #[test]
    fn another_build_gets_its_own_copy_and_the_sweep_leaves_only_it() {
        let (temp, state) = state();
        let one = build(temp.path(), "one", "build one");
        let first = Binary::pin(&state, &one, one.clone());
        // Another build: another file (what `cargo build` makes).
        let two = build(temp.path(), "two", "build two");
        let second = Binary::pin(&state, &two, two.clone());
        assert_ne!(first.name(), second.name());
        assert_eq!(fs::read_to_string(second.path()).unwrap(), "build two");
        // What a copy cut short leaves.
        fs::write(state.join("bin/.crew-0000000000000000.77"), "half").unwrap();
        assert_eq!(names(&state.join("bin")).len(), 3);

        let mut removed = second.sweep();
        removed.sort();
        let mut expected = vec![
            first.name().unwrap().to_string(),
            ".crew-0000000000000000.77".to_string(),
        ];
        expected.sort();
        assert_eq!(removed, expected);
        assert_eq!(
            names(&state.join("bin")),
            vec![second.name().unwrap().to_string()]
        );
        // A daemon handed its binary keeps away from the directory.
        assert!(Binary::given(two.clone()).sweep().is_empty());
    }

    #[test]
    fn a_copy_gone_or_replaced_is_made_again_under_its_name() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "the build");
        let pinned = Binary::pin(&state, &running, running.clone());
        assert_eq!(pinned.check(), Ok(None));

        fs::remove_file(pinned.path()).unwrap();
        let said = pinned.check().unwrap().expect("a warning");
        assert!(said.contains("was gone"), "{said}");
        assert_eq!(fs::read_to_string(pinned.path()).unwrap(), "the build");
        let meta = fs::symlink_metadata(pinned.path()).unwrap();
        let now = pinned.identity().unwrap();
        assert_eq!((now.dev, now.ino), (meta.dev(), meta.ino()));
        assert_eq!(pinned.check(), Ok(None));

        // A file planted under its name, with another inode: replaced, and
        // what a sandbox binds follows the new copy.
        let planted = state.join("bin/planted");
        fs::write(&planted, "not crew").unwrap();
        fs::set_permissions(&planted, fs::Permissions::from_mode(0o500)).unwrap();
        let planted_ino = fs::metadata(&planted).unwrap().ino();
        fs::rename(&planted, pinned.path()).unwrap();
        let said = pinned.check().unwrap().expect("a warning");
        assert!(said.contains("was replaced"), "{said}");
        assert_eq!(fs::read_to_string(pinned.path()).unwrap(), "the build");
        let now = pinned.identity().unwrap();
        let meta = fs::symlink_metadata(pinned.path()).unwrap();
        assert_eq!((now.dev, now.ino), (meta.dev(), meta.ino()));
        assert_ne!(now.ino, planted_ino);

        // Made writable by others: the same inode, but no longer the copy.
        fs::set_permissions(pinned.path(), fs::Permissions::from_mode(0o722)).unwrap();
        assert!(pinned.check().unwrap().is_some());
        assert_eq!(
            fs::symlink_metadata(pinned.path()).unwrap().mode() & 0o7777,
            0o500
        );
    }

    #[test]
    fn a_planted_file_found_at_start_is_replaced_unless_it_is_as_made() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "the build");
        let name = Binary::pin(&state, &running, running.clone())
            .name()
            .unwrap()
            .to_string();
        let path = state.join("bin").join(&name);
        fs::remove_file(&path).unwrap();
        // Writable, and of another length: not one of crew's copies.
        fs::write(&path, "x").unwrap();
        let pinned = Binary::pin(&state, &running, running.clone());
        assert_eq!(fs::read_to_string(pinned.path()).unwrap(), "the build");
    }

    #[test]
    fn reviewer_a_same_size_replacement_is_not_accepted_as_the_running_build_at_restart() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "the build");
        let first = Binary::pin(&state, &running, running.clone());
        let path = first.path().to_path_buf();
        drop(first);
        // Between daemon starts, another file took the cached copy's name.
        // Its shape matches a copy, but its bytes are a different build.
        let replacement = state.join("bin/replacement");
        fs::write(&replacement, "not crew!").unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o500)).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let again = Binary::pin(&state, &running, running.clone());
        assert_eq!(again.path(), path);
        assert_eq!(fs::read(again.path()).unwrap(), fs::read(&running).unwrap());
    }

    #[test]
    fn reviewer_a_copy_without_execute_permission_is_repaired_before_launch() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "the build");
        let pinned = Binary::pin(&state, &running, running.clone());
        fs::set_permissions(pinned.path(), fs::Permissions::from_mode(0o400)).unwrap();
        assert!(
            pinned.check().unwrap().is_some(),
            "an unusable copy was accepted"
        );
        assert_eq!(fs::metadata(pinned.path()).unwrap().mode() & 0o7777, 0o500);
    }

    #[test]
    fn a_copy_whose_mode_changed_is_made_again() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "the build");
        let pinned = Binary::pin(&state, &running, running.clone());
        // Not runnable; writable by its owner; set-user-ID.
        for mode in [0o400, 0o700, 0o4500] {
            fs::set_permissions(pinned.path(), fs::Permissions::from_mode(mode)).unwrap();
            let said = pinned.check().unwrap();
            assert!(
                said.is_some_and(|s| s.contains("was replaced or changed")),
                "{mode:o}"
            );
            let meta = fs::symlink_metadata(pinned.path()).unwrap();
            assert_eq!(meta.mode() & 0o7777, 0o500, "{mode:o}");
            assert_eq!(pinned.check(), Ok(None), "{mode:o}");
        }
    }

    #[test]
    fn a_copy_written_in_place_is_made_again_whatever_its_mtime_says() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "the build");
        let pinned = Binary::pin(&state, &running, running.clone());
        let before = fs::symlink_metadata(pinned.path()).unwrap();
        // Past the file system's clock tick, as anything later is.
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Other bytes of the same length, then the mode and the
        // modification time it had: only the change time tells.
        fs::set_permissions(pinned.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(pinned.path(), "not crew!").unwrap();
        fs::set_permissions(pinned.path(), fs::Permissions::from_mode(0o500)).unwrap();
        File::open(pinned.path())
            .unwrap()
            .set_modified(before.modified().unwrap())
            .unwrap();
        let after = fs::symlink_metadata(pinned.path()).unwrap();
        assert_eq!(
            (after.ino(), after.size(), after.modified().unwrap()),
            (before.ino(), before.size(), before.modified().unwrap())
        );
        assert!(pinned.check().unwrap().is_some());
        assert_eq!(fs::read_to_string(pinned.path()).unwrap(), "the build");
    }

    #[test]
    fn a_file_found_at_start_is_used_only_if_it_holds_the_running_binary() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "the build");
        let path = Binary::pin(&state, &running, running.clone())
            .path()
            .to_path_buf();
        let made = |path: &Path| Made::of(&fs::symlink_metadata(path).unwrap());

        // As a copy is made, with other bytes, written where the copy was.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&path, "the bUild").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500)).unwrap();
        let planted = made(&path);
        let pinned = Binary::pin(&state, &running, running.clone());
        assert_eq!(fs::read_to_string(pinned.path()).unwrap(), "the build");
        assert_ne!(made(&path), planted);
        // What an agent launched with that name ran is not known.
        let name = pinned.name().unwrap().to_string();
        assert!(pinned.replaced().is_some());
        assert!(!pinned.launched_this(Some(&name)));

        // The running binary's bytes: kept as it is, no copy made.
        let kept = made(&path);
        let again = Binary::pin(&state, &running, running.clone());
        assert_eq!(made(again.path()), kept);
        assert_eq!(again.replaced(), None);
        assert!(again.launched_this(Some(&name)));

        // A link to the running binary is not followed; a FIFO is not
        // waited on.
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&running, &path).unwrap();
        let linked = Binary::pin(&state, &running, running.clone());
        assert!(fs::symlink_metadata(&path).unwrap().file_type().is_file());
        assert!(linked.replaced().is_some());
        fs::remove_file(&path).unwrap();
        let fifo = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid C string.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let piped = Binary::pin(&state, &running, running.clone());
        assert_eq!(fs::read_to_string(&path).unwrap(), "the build");
        assert!(piped.replaced().is_some());

        // Nothing there: made, and nothing replaced.
        fs::remove_file(&path).unwrap();
        let made_anew = Binary::pin(&state, &running, running.clone());
        assert_eq!(made_anew.replaced(), None);
        assert!(!made_anew.launched_this(Some(&name)));
    }

    #[test]
    fn what_an_agent_runs_is_known_from_an_intact_copy_or_a_given_path_only() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "the build");
        // A new copy says nothing about agents that survived an earlier
        // daemon. Only a copy found intact identifies what they ran.
        let fresh = Binary::pin(&state, &running, running.clone());
        assert!(!fresh.launched_this(fresh.name()));
        let pinned = Binary::pin(&state, &running, running.clone());
        let name = pinned.name().unwrap();
        assert!(pinned.launched_this(Some(name)));
        assert!(!pinned.launched_this(Some("crew-0000000000000000")));
        assert!(!pinned.launched_this(None));

        let given = Binary::given(running.clone());
        assert!(given.launched_this(None));
        assert!(!given.launched_this(Some(name)));

        fs::remove_dir_all(state.join("bin")).unwrap();
        fs::write(state.join("bin"), "").unwrap();
        let unpinned = Binary::pin(&state, &running, running.clone());
        assert!(unpinned.warning().is_some());
        assert!(!unpinned.launched_this(None));
        assert!(!unpinned.launched_this(Some(name)));
    }

    #[test]
    fn without_a_usable_directory_the_daemon_hands_out_its_own_path_and_says_why() {
        let (temp, state) = state();
        let running = build(temp.path(), "crew", "the build");
        // `<state>/bin` cannot be made: a file is in its way.
        fs::write(state.join("bin"), "").unwrap();
        let unpinned = Binary::pin(&state, &running, running.clone());
        assert_eq!(unpinned.path(), running);
        assert_eq!(unpinned.name(), None);
        assert_eq!(unpinned.identity(), None);
        assert_eq!(unpinned.check(), Ok(None));
        assert!(unpinned.sweep().is_empty());
        let warning = unpinned.warning().unwrap();
        assert!(
            warning.starts_with("could not keep a copy of the crew binary (")
                && warning.ends_with(&format!(
                    "): replacing {} while this team runs will break its agents' tools",
                    running.display()
                )),
            "{warning}"
        );

        // A running binary that cannot be read: the same.
        let gone = Binary::pin(&state, &temp.path().join("missing"), running.clone());
        assert!(gone.warning().unwrap().contains("reading"));
    }

    #[test]
    fn read_only_and_noexec_file_systems_are_refused() {
        assert_eq!(refusal(0), None);
        assert_eq!(
            refusal(libc::ST_NOEXEC | libc::ST_NOSUID),
            Some("is on a file system mounted noexec")
        );
        assert_eq!(
            refusal(libc::ST_RDONLY | libc::ST_NOEXEC),
            Some("is on a read-only file system")
        );
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(runnable(temp.path()), Ok(()));
    }

    #[test]
    fn a_daemon_given_its_binary_hands_it_out_as_it_is() {
        let given = Binary::given("/opt/crew".into());
        assert_eq!(given.path(), Path::new("/opt/crew"));
        assert_eq!(
            (given.name(), given.identity(), given.warning()),
            (None, None, None)
        );
        assert_eq!(given.check(), Ok(None));
    }
}
