//! Descriptor-pinned documentation roots. A path in a watch event or a client
//! request is never used as an authority to open a different root.

use std::ffi::CStr;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crew_core::rpc::docs::DocFailureCode;

use super::config::{Policy, Relative};

pub const MAX_SCAN_ENTRIES: usize = 20_000;
pub const STEP_ENTRIES: usize = 256;
pub const STEP_READ_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub dev: u64,
    pub ino: u64,
    pub bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    links: u64,
}

impl From<&Metadata> for Stamp {
    fn from(value: &Metadata) -> Self {
        Self {
            dev: value.dev(),
            ino: value.ino(),
            bytes: value.len(),
            modified: (value.mtime(), value.mtime_nsec()),
            changed: (value.ctime(), value.ctime_nsec()),
            links: value.nlink(),
        }
    }
}

pub struct ReadBytes {
    pub bytes: Vec<u8>,
    pub stamp: Stamp,
}

pub struct Root {
    path: PathBuf,
    directory: File,
    identity: (u64, u64),
}

impl Root {
    /// Only daemon-registered paths may reach this constructor. The descriptor
    /// stays anchored even if an editor replaces a path component afterwards.
    pub fn open(path: &Path) -> Result<Self, DocFailureCode> {
        if !path.is_absolute()
            || path
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir))
        {
            return Err(DocFailureCode::ForbiddenPath);
        }
        let directory = File::from(
            crate::os::open_no_symlinks(
                libc::AT_FDCWD,
                path,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0,
                false,
            )
            .map_err(classify)?,
        );
        let meta = directory.metadata().map_err(classify)?;
        Ok(Self {
            path: path.to_owned(),
            directory,
            identity: (meta.dev(), meta.ino()),
        })
    }

    pub fn identity(&self) -> (u64, u64) {
        self.identity
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reject a replacement root, without reading through that replacement.
    pub fn check_identity(&self) -> Result<(), DocFailureCode> {
        let now = Self::open(&self.path).map_err(|_| DocFailureCode::SourceGone)?;
        if now.identity == self.identity {
            Ok(())
        } else {
            Err(DocFailureCode::SourceGone)
        }
    }

    fn open_at(&self, relative: &Path, flags: i32) -> Result<File, DocFailureCode> {
        let mut flags = flags | libc::O_CLOEXEC;
        // openat2 rejects NONBLOCK/NOCTTY with O_PATH. O_PATH itself performs
        // no device/FIFO open and therefore needs no nonblocking protection.
        #[cfg(target_os = "linux")]
        let path_only = flags & libc::O_PATH != 0;
        #[cfg(not(target_os = "linux"))]
        let path_only = false;
        if !path_only {
            flags |= libc::O_NONBLOCK | libc::O_NOCTTY;
        }
        Ok(File::from(
            crate::os::open_no_symlinks(self.directory.as_raw_fd(), relative, flags, 0, true)
                .map_err(classify)?,
        ))
    }

    pub fn metadata(&self, name: &Relative, policy: &Policy) -> Result<Metadata, DocFailureCode> {
        if !policy.allows(name) {
            return Err(DocFailureCode::ForbiddenPath);
        }
        let file = self.open_at(name.as_path(), crate::os::PATH_ONLY)?;
        file.metadata().map_err(classify)
    }

    /// Max plus one, with descriptor metadata before/after. No partial file is
    /// returned as a successful snapshot. Retry a concurrent save at most twice.
    pub fn read(
        &self,
        name: &Relative,
        policy: &Policy,
        max: usize,
    ) -> Result<ReadBytes, DocFailureCode> {
        if !policy.allows(name) {
            return Err(DocFailureCode::ForbiddenPath);
        }
        self.check_identity()?;
        let mut parent = name.as_path().parent();
        while let Some(path) = parent.filter(|p| !p.as_os_str().is_empty()) {
            if self.submodule(
                &Relative::new(path.to_str().ok_or(DocFailureCode::ForbiddenPath)?)
                    .map_err(|_| DocFailureCode::ForbiddenPath)?,
            ) {
                return Err(DocFailureCode::ForbiddenPath);
            }
            parent = path.parent();
        }
        for _ in 0..3 {
            let mut file = self.open_at(name.as_path(), libc::O_RDONLY)?;
            let before = file.metadata().map_err(classify)?;
            if !before.is_file() || before.nlink() != 1 {
                return Err(DocFailureCode::ForbiddenPath);
            }
            if before.len() > max as u64 {
                return Err(DocFailureCode::TooLarge);
            }
            let mut bytes = Vec::with_capacity((before.len() as usize).min(max));
            Read::by_ref(&mut file)
                .take(max as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(classify)?;
            if bytes.len() > max {
                return Err(DocFailureCode::TooLarge);
            }
            let after = file.metadata().map_err(classify)?;
            self.check_identity()?;
            let stamp = Stamp::from(&before);
            if stamp == Stamp::from(&after) && after.len() == bytes.len() as u64 {
                return Ok(ReadBytes { bytes, stamp });
            }
        }
        Err(DocFailureCode::UnstableFile)
    }

    /// Enumeration is bounded before allocating all directory entries. Invalid
    /// UTF-8 names still consume the budget, but never become document paths.
    fn list(&self, relative: Option<&Relative>) -> Result<Directory, DocFailureCode> {
        let dir = self.open_at(
            relative.map_or(Path::new("."), Relative::as_path),
            libc::O_RDONLY | libc::O_DIRECTORY,
        )?;
        Directory::new(dir).map_err(classify)
    }

    fn submodule(&self, relative: &Relative) -> bool {
        // Even malformed or symlinked administrative markers make this a
        // boundary, rather than a reason to traverse another repository.
        let Ok(dir) = self.open_at(relative.as_path(), libc::O_RDONLY | libc::O_DIRECTORY) else {
            return true;
        };
        for marker in [c".git", c".jj"] {
            // SAFETY: dir is held, marker is a static NUL-terminated name.
            let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
            if unsafe {
                libc::fstatat(
                    dir.as_raw_fd(),
                    marker.as_ptr(),
                    &mut stat,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } == 0
            {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn k01_k03_scan_yields_and_never_enters_private_or_nested_repositories() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "README.md",
            "child/a.md",
            "notes/deep/b.md",
            "src/deep/no.md",
            ".crew/no.md",
            "notes/repo/.git/HEAD",
            "notes/repo/no.md",
        ] {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "# Safe").unwrap();
        }
        for n in 0..600 {
            fs::write(dir.path().join(format!("ignored-{n}")), "").unwrap();
        }
        let root = Root::open(dir.path()).unwrap();
        let policy = Policy::default();
        let mut scan = Scan::new(&root).unwrap();
        assert!(!scan.step(&root, &policy).unwrap().done);
        let mut paths = Vec::new();
        loop {
            let step = scan.step(&root, &policy).unwrap();
            paths.extend(step.documents.into_iter().map(|p| p.as_str().to_owned()));
            assert!(!step.limited);
            if step.done {
                break;
            }
        }
        assert_eq!(paths, ["README.md", "child/a.md", "notes/deep/b.md"]);
        assert!(root
            .read(&Relative::new("notes/repo/no.md").unwrap(), &policy, 256)
            .is_err());
    }

    #[test]
    fn k04_descriptor_reads_refuse_symlinks_hardlinks_pipes_and_replaced_roots() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("root");
        fs::create_dir(&base).unwrap();
        fs::write(dir.path().join("outside"), "outside-secret").unwrap();
        fs::write(base.join("good.md"), "# Good").unwrap();
        symlink(dir.path().join("outside"), base.join("link.md")).unwrap();
        symlink(dir.path(), base.join("linked-dir")).unwrap();
        fs::hard_link(dir.path().join("outside"), base.join("hard.md")).unwrap();
        let fifo =
            std::ffi::CString::new(base.join("pipe.md").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let root = Root::open(&base).unwrap();
        let policy = Policy::default();
        for name in ["link.md", "linked-dir/outside", "hard.md", "pipe.md"] {
            assert!(
                root.read(&Relative::new(name).unwrap(), &policy, 256)
                    .is_err(),
                "{name}"
            );
        }
        assert_eq!(
            root.read(&Relative::new("good.md").unwrap(), &policy, 256)
                .unwrap()
                .bytes,
            b"# Good"
        );
        assert!(matches!(
            root.read(&Relative::new("good.md").unwrap(), &policy, 2),
            Err(DocFailureCode::TooLarge)
        ));
        fs::rename(&base, dir.path().join("old")).unwrap();
        fs::create_dir(&base).unwrap();
        fs::write(base.join("good.md"), "replacement").unwrap();
        assert!(matches!(
            root.read(&Relative::new("good.md").unwrap(), &policy, 256),
            Err(DocFailureCode::SourceGone)
        ));
    }
}

fn classify(error: io::Error) -> DocFailureCode {
    match error.kind() {
        io::ErrorKind::NotFound => DocFailureCode::ObjectMissing,
        io::ErrorKind::PermissionDenied => DocFailureCode::ForbiddenPath,
        io::ErrorKind::Unsupported => DocFailureCode::SourceGone,
        _ => DocFailureCode::ForbiddenPath,
    }
}

struct Directory(*mut libc::DIR);
// SAFETY: Directory uniquely owns its DIR. All iteration requires &mut self;
// it may move between serialized blocking jobs but is never read concurrently.
unsafe impl Send for Directory {}
impl Drop for Directory {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns DIR.
        unsafe { libc::closedir(self.0) };
    }
}
impl Directory {
    fn new(directory: File) -> io::Result<Self> {
        let fd = directory.into_raw_fd();
        // SAFETY: fd is owned; fdopendir takes it on success only.
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: failed fdopendir did not take ownership.
            unsafe { libc::close(fd) };
            return Err(error);
        }
        Ok(Self(raw))
    }
    // None is EOF; Some(None) is a counted but inadmissible filename.
    fn next(&mut self) -> io::Result<Option<Option<String>>> {
        loop {
            #[cfg(target_os = "linux")]
            let errno = unsafe { libc::__errno_location() };
            #[cfg(target_os = "macos")]
            let errno = unsafe { libc::__error() };
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            // SAFETY: errno points to the calling thread's errno cell.
            unsafe {
                *errno = 0
            };
            // SAFETY: the guard owns this stream until the loop exits.
            let entry = unsafe { libc::readdir(self.0) };
            if entry.is_null() {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if unsafe { *errno } != 0 {
                    return Err(io::Error::last_os_error());
                }
                return Ok(None);
            }
            // SAFETY: readdir supplies a terminated name, valid until its next call.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            return Ok(Some(
                name.to_str()
                    .ok()
                    .filter(|name| {
                        name.len() <= super::config::MAX_PATH_BYTES
                            && !name.chars().any(char::is_control)
                    })
                    .map(str::to_owned),
            ));
        }
    }
}

pub struct ScanStep {
    pub documents: Vec<Relative>,
    pub directories: Vec<Relative>,
    pub done: bool,
    pub limited: bool,
}

/// Incremental deterministic walk. Content parsing/read budgets are enforced by
/// the caller; this walk yields after at most 256 entries or 25 ms.
pub struct Scan {
    pending: std::collections::VecDeque<Relative>,
    reading: Option<Reading>,
    visited: usize,
    limited: bool,
    path_bytes: usize,
}

struct Reading {
    directory: Directory,
    parent: Option<Relative>,
    names: Vec<String>,
}

impl Scan {
    pub fn new(root: &Root) -> Result<Self, DocFailureCode> {
        root.check_identity()?;
        Ok(Self {
            pending: Default::default(),
            reading: Some(Reading {
                directory: root.list(None)?,
                parent: None,
                names: Vec::new(),
            }),
            visited: 0,
            limited: false,
            path_bytes: 0,
        })
    }

    pub fn step(&mut self, root: &Root, policy: &Policy) -> Result<ScanStep, DocFailureCode> {
        root.check_identity()?;
        let start = Instant::now();
        let mut documents = Vec::new();
        let mut directories = Vec::new();
        for _ in 0..STEP_ENTRIES {
            if let Some(reading) = &mut self.reading {
                let next = if self.visited == MAX_SCAN_ENTRIES {
                    self.limited = true;
                    None
                } else {
                    reading.directory.next().map_err(classify)?
                };
                if let Some(name) = next {
                    self.visited += 1;
                    if let Some(name) = name {
                        let charge = name.len()
                            + reading.parent.as_ref().map_or(0, |p| p.as_str().len() + 1)
                            + 64;
                        if self.path_bytes + charge > 4 * 1024 * 1024 {
                            self.limited = true;
                            self.visited = MAX_SCAN_ENTRIES;
                        } else {
                            self.path_bytes += charge;
                            reading.names.push(name);
                        }
                    }
                } else {
                    let mut reading = self.reading.take().expect("present directory");
                    reading.names.sort();
                    for name in reading.names.into_iter().rev() {
                        let path = reading
                            .parent
                            .as_ref()
                            .map_or_else(|| Relative::new(&name), |parent| parent.join(&name));
                        if let Ok(path) = path {
                            self.pending.push_front(path);
                        }
                    }
                }
                if start.elapsed() >= std::time::Duration::from_millis(25) {
                    break;
                }
                continue;
            }
            let Some(path) = self.pending.pop_front() else {
                break;
            };
            self.path_bytes = self.path_bytes.saturating_sub(path.as_str().len() + 64);
            if let Ok(meta) = root.metadata(&path, policy) {
                if meta.is_dir() && policy.descends(&path) && !root.submodule(&path) {
                    directories.push(path.clone());
                    if self.visited < MAX_SCAN_ENTRIES {
                        self.reading = Some(Reading {
                            directory: root.list(Some(&path))?,
                            parent: Some(path),
                            names: Vec::new(),
                        });
                    } else {
                        self.limited = true;
                    }
                } else if meta.is_file() && meta.nlink() == 1 && policy.selects(&path) {
                    documents.push(path);
                }
            }
            if start.elapsed() >= std::time::Duration::from_millis(25) {
                break;
            }
        }
        Ok(ScanStep {
            documents,
            directories,
            done: self.pending.is_empty() && self.reading.is_none(),
            limited: self.limited,
        })
    }
}
