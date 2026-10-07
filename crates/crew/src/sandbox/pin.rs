//! Pinning: what bubblewrap mounts must be the object that was validated.
//!
//! A source is opened `O_PATH` without following a symlink
//! (`crate::os::open_no_symlinks`), so no component of its (canonical) path
//! may be a symlink at the moment of opening, and its identity (device, inode, type) is compared with the
//! identity recorded when it was validated. The descriptor, not the path,
//! is then handed to bubblewrap.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The kinds of object a sandbox mounts.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Dir,
    File,
    Socket,
}

/// Which object a path named when it was validated.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub dev: u64,
    pub ino: u64,
    pub kind: Kind,
}

/// Open `path` as an `O_PATH` descriptor without following any symlink in
/// any component. `inheritable` leaves `O_CLOEXEC` off, for descriptors
/// handed to bubblewrap.
pub fn open_path(path: &Path, inheritable: bool) -> io::Result<OwnedFd> {
    // No O_NOFOLLOW: with O_PATH it would open a trailing symlink itself,
    // where refusing symlinks alone refuses it with ELOOP.
    let mut flags = crate::os::PATH_ONLY;
    if !inheritable {
        flags |= libc::O_CLOEXEC;
    }
    crate::os::open_no_symlinks(libc::AT_FDCWD, path, flags, 0, false)
}

/// The identity of an open descriptor. Anything but a directory, regular
/// file or socket is refused.
pub fn identity(fd: &OwnedFd) -> io::Result<Identity> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a valid out-pointer and `fd` is open.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = match st.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File,
        libc::S_IFSOCK => Kind::Socket,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a directory, regular file or socket",
            ))
        }
    };
    // A signed number on macOS.
    #[allow(clippy::unnecessary_cast)]
    let dev = st.st_dev as u64;
    Ok(Identity {
        dev,
        ino: st.st_ino,
        kind,
    })
}

/// The identity `path` has now, opened the way it will be pinned.
pub fn identify(path: &Path) -> io::Result<Identity> {
    identity(&open_path(path, false)?)
}

/// Open `path` for bubblewrap and check that it is still `expected`.
pub fn pin(path: &Path, expected: Identity) -> Result<OwnedFd, String> {
    let fd = open_path(path, true).map_err(|e| format!("pinning {}: {e}", path.display()))?;
    let found = identity(&fd).map_err(|e| format!("pinning {}: {e}", path.display()))?;
    if found != expected {
        return Err(format!(
            "{} changed after it was checked (expected {expected:?}, found {found:?})",
            path.display()
        ));
    }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn identities_name_the_kind_and_refuse_other_objects() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, "x").unwrap();
        let socket = dir.path().join("s");
        let _listener = UnixListener::bind(&socket).unwrap();
        assert_eq!(identify(dir.path()).unwrap().kind, Kind::Dir);
        assert_eq!(identify(&file).unwrap().kind, Kind::File);
        assert_eq!(identify(&socket).unwrap().kind, Kind::Socket);
        assert!(identify(Path::new("/dev/null")).is_err());
    }

    #[test]
    fn a_symlink_in_any_component_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("f"), "x").unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        // Positive control: the same object through real components.
        assert!(open_path(&real.join("f"), false).is_ok());
        for path in [dir.path().join("link"), dir.path().join("link/f")] {
            let error = open_path(&path, false).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{path:?}");
        }
    }

    #[test]
    fn a_pinned_source_survives_a_symlink_swap() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("toolchain");
        let elsewhere = dir.path().join("elsewhere");
        let moved = dir.path().join("moved");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(source.join("marker"), "validated").unwrap();
        let expected = identify(&source).unwrap();
        let fd = pin(&source, expected).unwrap();

        // Swap the validated path for a symlink to somewhere else.
        std::fs::rename(&source, &moved).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &source).unwrap();

        // The descriptor still names the validated directory...
        assert_eq!(identity(&fd).unwrap(), expected);
        let through_fd = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())).unwrap();
        assert_eq!(through_fd, moved);
        // ...and pinning the path again refuses the symlink.
        assert!(pin(&source, expected).is_err());
    }

    #[test]
    fn a_replaced_source_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src");
        std::fs::create_dir(&source).unwrap();
        let expected = identify(&source).unwrap();
        std::fs::rename(&source, dir.path().join("old")).unwrap();
        std::fs::create_dir(&source).unwrap();
        let error = pin(&source, expected).unwrap_err();
        assert!(error.contains("changed after it was checked"), "{error}");
        // A file where a directory was is refused the same way.
        std::fs::remove_dir(&source).unwrap();
        std::fs::write(&source, "x").unwrap();
        assert!(pin(&source, expected).is_err());
    }

    #[test]
    fn descriptors_for_bubblewrap_are_inheritable_and_others_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let flags = |fd: &OwnedFd| {
            // SAFETY: F_GETFD on an open descriptor.
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) }
        };
        let pinned = pin(dir.path(), identify(dir.path()).unwrap()).unwrap();
        assert_eq!(flags(&pinned) & libc::FD_CLOEXEC, 0);
        let private = open_path(dir.path(), false).unwrap();
        assert_eq!(flags(&private) & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
    }
}
