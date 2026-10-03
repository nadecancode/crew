//! Reading and creating files under the repository's administrative
//! directories without ever following a symlink.
//!
//! The directories under `.git/worktrees/` are, in a team that shares its
//! checkout, writable by agents; a path walk from the top could be steered
//! outside by a symlink swapped in for a component. The common git directory
//! is opened once, by a path none of whose components is a symlink, and
//! everything below it is opened *relative to that descriptor* with
//! `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// A directory held open by descriptor.
pub struct Root(OwnedFd);

impl Root {
    /// Open `path` as a directory, refusing a symlink in any component.
    pub fn open(path: &Path) -> io::Result<Self> {
        open_at(
            libc::AT_FDCWD,
            path,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
            libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS,
        )
        .map(Self)
    }

    fn at(&self, rel: &str, flags: i32, mode: u32) -> io::Result<OwnedFd> {
        open_at(
            self.0.as_raw_fd(),
            Path::new(rel),
            flags | libc::O_CLOEXEC,
            mode,
            libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS,
        )
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
        self.at(rel, libc::O_PATH, 0).is_ok()
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

    /// The names of the entries of the directory at `rel`.
    pub fn list(&self, rel: &str) -> Vec<String> {
        let Ok(fd) = self.at(rel, libc::O_RDONLY | libc::O_DIRECTORY, 0) else {
            return Vec::new();
        };
        // The descriptor is the directory; reading it through /proc/self/fd
        // lists exactly that directory, whatever the path now names.
        let Ok(entries) = std::fs::read_dir(format!("/proc/self/fd/{}", fd.as_raw_fd())) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    }
}

fn open_at(dir: RawFd, path: &Path, flags: i32, mode: u32, resolve: u64) -> io::Result<OwnedFd> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: open_how is plain integers; all-zero is its default.
    let mut how: libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = flags as u64;
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: `c_path` and `how` are valid for the call; the result is checked.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dir,
            c_path.as_ptr(),
            &how as *const libc::open_how,
            std::mem::size_of::<libc::open_how>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat2 returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}
