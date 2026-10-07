//! Reading and creating files under the repository's administrative
//! directories without ever following a symlink.
//!
//! The directories under `.git/worktrees/` are, in a team that shares its
//! checkout, writable by agents; a path walk from the top could be steered
//! outside by a symlink swapped in for a component. The common git directory
//! is opened once, by a path none of whose components is a symlink, and
//! everything below it is opened *relative to that descriptor*, beneath it
//! and without following a symlink (`crate::os::open_no_symlinks`).

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::os::{list_dir, open_no_symlinks, PATH_ONLY};

/// A directory held open by descriptor.
pub struct Root(OwnedFd);

impl Root {
    /// Open `path` as a directory, refusing a symlink in any component.
    pub fn open(path: &Path) -> io::Result<Self> {
        open_no_symlinks(
            libc::AT_FDCWD,
            path,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
            false,
        )
        .map(Self)
    }

    fn at(&self, rel: &str, flags: i32, mode: u32) -> io::Result<OwnedFd> {
        open_no_symlinks(self.0.as_raw_fd(), Path::new(rel), flags | libc::O_CLOEXEC, mode, true)
    }

    /// The text of the regular file at `rel` (at most 64 KiB).
    pub fn read_text(&self, rel: &str) -> Option<String> {
        let fd = self.at(rel, libc::O_RDONLY | libc::O_NONBLOCK, 0).ok()?;
        let file = File::from(fd);
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut text = String::new();
        file.take(64 * 1024).read_to_string(&mut text).ok()?;
        Some(text)
    }

    /// Whether `rel` exists (as anything, a symlink counting as absent).
    pub fn exists(&self, rel: &str) -> bool {
        self.at(rel, PATH_ONLY, 0).is_ok()
    }

    /// Create an empty regular file at `rel` unless there is one.
    pub fn create_empty(&self, rel: &str) -> io::Result<()> {
        match self.at(
            rel,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o644,
        ) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Create the regular file `rel` holding `bytes`; it must not exist yet
    /// (a symlink in its place counts as existing and is not followed).
    pub fn write_new(&self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let fd = self.at(
            rel,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o644,
        )?;
        File::from(fd).write_all(bytes)
    }

    /// Remove the file `rel` (a symlink is removed itself, never followed).
    pub fn remove(&self, rel: &str) {
        let (Some(dir), Some(name)) = (Path::new(rel).parent(), Path::new(rel).file_name()) else {
            return;
        };
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        let Ok(fd) = self.at(&dir.to_string_lossy(), PATH_ONLY | libc::O_DIRECTORY, 0) else {
            return;
        };
        let Ok(name) = CString::new(name.as_bytes()) else {
            return;
        };
        // SAFETY: `fd` is open and `name` is a valid C string.
        unsafe { libc::unlinkat(fd.as_raw_fd(), name.as_ptr(), 0) };
    }

    /// The names of the entries of the directory at `rel`.
    pub fn list(&self, rel: &str) -> Vec<String> {
        let Ok(fd) = self.at(rel, libc::O_RDONLY | libc::O_DIRECTORY, 0) else {
            return Vec::new();
        };
        // The descriptor is the directory: it lists exactly that directory,
        // whatever the path now names.
        list_dir(fd.as_fd()).unwrap_or_default()
    }
}
