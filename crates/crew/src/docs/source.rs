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
        Ok(File::from(
            crate::os::open_no_symlinks(
                self.directory.as_raw_fd(),
                relative,
                flags | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY,
                0,
                true,
            )
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
    fn list(&self, relative: Option<&Relative>, max: usize) -> Result<Listing, DocFailureCode> {
        let dir = self.open_at(
            relative.map_or(Path::new("."), Relative::as_path),
            libc::O_RDONLY | libc::O_DIRECTORY,
        )?;
        list_bounded(dir, max).map_err(classify)
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

fn classify(error: io::Error) -> DocFailureCode {
    match error.kind() {
        io::ErrorKind::NotFound => DocFailureCode::ObjectMissing,
        io::ErrorKind::PermissionDenied => DocFailureCode::ForbiddenPath,
        io::ErrorKind::Unsupported => DocFailureCode::SourceGone,
        _ => DocFailureCode::ForbiddenPath,
    }
}

struct Listing {
    names: Vec<String>,
    visited: usize,
    limited: bool,
}

fn list_bounded(directory: File, limit: usize) -> io::Result<Listing> {
    let fd = directory.into_raw_fd();
    // SAFETY: fd is owned; fdopendir takes it on success only.
    let raw = unsafe { libc::fdopendir(fd) };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: failed fdopendir did not take ownership.
        unsafe { libc::close(fd) };
        return Err(error);
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            /* SAFETY: this guard uniquely owns DIR. */
            unsafe { libc::closedir(self.0) };
        }
    }
    let dir = Directory(raw);
    let mut result = Listing {
        names: Vec::new(),
        visited: 0,
        limited: false,
    };
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
        let entry = unsafe { libc::readdir(dir.0) };
        if entry.is_null() {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if unsafe { *errno } != 0 {
                return Err(io::Error::last_os_error());
            }
            break;
        }
        // SAFETY: readdir supplies a terminated name, valid until its next call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        if result.visited == limit {
            result.limited = true;
            break;
        }
        result.visited += 1;
        if let Ok(name) = name.to_str() {
            if name.len() <= super::config::MAX_PATH_BYTES && !name.chars().any(char::is_control) {
                result.names.push(name.to_owned());
            }
        }
    }
    result.names.sort();
    Ok(result)
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
    visited: usize,
    limited: bool,
}

impl Scan {
    pub fn new(root: &Root) -> Result<Self, DocFailureCode> {
        root.check_identity()?;
        let listing = root.list(None, MAX_SCAN_ENTRIES)?;
        Ok(Self {
            pending: listing
                .names
                .iter()
                .filter_map(|n| Relative::new(n).ok())
                .collect(),
            visited: listing.visited,
            limited: listing.limited,
        })
    }

    pub fn step(&mut self, root: &Root, policy: &Policy) -> Result<ScanStep, DocFailureCode> {
        root.check_identity()?;
        let start = Instant::now();
        let mut documents = Vec::new();
        let mut directories = Vec::new();
        for _ in 0..STEP_ENTRIES {
            let Some(path) = self.pending.pop_front() else {
                break;
            };
            if let Ok(meta) = root.metadata(&path, policy) {
                if meta.is_dir() && policy.descends(&path) && !root.submodule(&path) {
                    directories.push(path.clone());
                    let listing =
                        root.list(Some(&path), MAX_SCAN_ENTRIES.saturating_sub(self.visited))?;
                    self.visited += listing.visited;
                    self.limited |= listing.limited;
                    for child in listing.names.into_iter().rev() {
                        if let Ok(child) = path.join(&child) {
                            self.pending.push_front(child);
                        }
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
            done: self.pending.is_empty(),
            limited: self.limited,
        })
    }
}
