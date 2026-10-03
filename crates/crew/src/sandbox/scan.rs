//! Sockets in bound trees (design-b3 §3.3, rule 5). Sockets are denied by
//! default: a socket in a tree a sandbox binds gets an empty file mounted
//! over it, and a directory the user may enter but not list (so a socket
//! could hide in it) gets an empty tmpfs over it. A directory the user
//! cannot enter at all needs nothing: nothing in it is reachable inside
//! either.
//!
//! The walk trusts each directory entry's type, which at a mount point is
//! the type of what lies underneath, so mount points are checked
//! separately: a socket bind-mounted over a file shows up only there.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq)]
pub struct Findings {
    /// Sockets to mask.
    pub sockets: Vec<PathBuf>,
    /// Directories that can be entered but not listed, to hide.
    pub unlisted: Vec<PathBuf>,
}

/// Walk `root` without following symlinks (crossing mount points, as a
/// recursive bind does).
pub fn scan(root: &Path) -> io::Result<Findings> {
    scan_excluding(root, &[])
}

/// Like [`scan`], skipping the subtrees in `excluded` (paths another mount
/// will cover).
pub fn scan_excluding(root: &Path, excluded: &[&Path]) -> io::Result<Findings> {
    let mut findings = Findings::default();
    let meta = std::fs::symlink_metadata(root)?;
    if meta.file_type().is_socket() {
        findings.sockets.push(root.to_path_buf());
    }
    if !meta.is_dir() {
        return Ok(findings);
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                if can_enter(&dir) {
                    findings.unlisted.push(dir);
                }
                continue;
            }
            // Gone since it was listed: nothing to mount over.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", dir.display()))),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_socket() {
                findings.sockets.push(entry.path());
            } else if kind.is_dir() && !excluded.contains(&entry.path().as_path()) {
                pending.push(entry.path());
            }
        }
    }
    for point in mount_points_under(root, excluded) {
        let socket = std::fs::symlink_metadata(&point).is_ok_and(|m| m.file_type().is_socket());
        if socket && !findings.sockets.contains(&point) {
            findings.sockets.push(point);
        }
    }
    findings.sockets.sort();
    findings.unlisted.sort();
    Ok(findings)
}

/// Mount points strictly inside `root` and outside `excluded`, from
/// `/proc/self/mountinfo`.
fn mount_points_under(root: &Path, excluded: &[&Path]) -> Vec<PathBuf> {
    let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    info.lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(|field| PathBuf::from(unescape_mount_field(field)))
        .filter(|point| point != root && point.starts_with(root))
        .filter(|point| !excluded.iter().any(|e| point.starts_with(e)))
        .collect()
}

/// mountinfo writes space, tab, newline and backslash as `\ooo`.
fn unescape_mount_field(field: &str) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let digits = bytes
            .get(i + 1..i + 4)
            .filter(|d| bytes[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)));
        match digits {
            Some(d) => {
                out.push(d.iter().fold(0u8, |n, c| n.wrapping_mul(8) + (c - b'0')));
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    std::ffi::OsString::from_vec(out)
}

fn can_enter(dir: &Path) -> bool {
    let Ok(c_dir) = CString::new(dir.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `c_dir` is a valid C string.
    unsafe { libc::access(c_dir.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    #[test]
    fn sockets_anywhere_in_the_tree_are_found_and_symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("a/file"), "x").unwrap();
        let _top = UnixListener::bind(root.join("top.sock")).unwrap();
        let _deep = UnixListener::bind(root.join("a/b/deep.sock")).unwrap();
        let _out = UnixListener::bind(outside.join("out.sock")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("a/link")).unwrap();
        let findings = scan(&root).unwrap();
        assert_eq!(
            findings.sockets,
            vec![root.join("a/b/deep.sock"), root.join("top.sock")]
        );
        assert!(findings.unlisted.is_empty());
        // A socket bound as a single file is found too.
        assert_eq!(scan(&root.join("top.sock")).unwrap().sockets.len(), 1);
    }

    #[test]
    fn a_socket_bind_mounted_over_a_file_is_found() {
        use crate::sandbox::test_support::{host_has, in_child};
        use std::ffi::CString;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("plain"), "").unwrap();
        let socket = dir.path().join("s");
        let _listener = UnixListener::bind(&socket).unwrap();
        // Positive control: the walk alone sees a regular file there.
        assert!(scan(&root).unwrap().sockets.is_empty());
        let uid = unsafe { libc::getuid() };
        let (src, dst) = (
            CString::new(socket.as_os_str().as_bytes()).unwrap(),
            CString::new(root.join("plain").as_os_str().as_bytes()).unwrap(),
        );
        let code = in_child(|| {
            // SAFETY: the forked child is single-threaded, as unshare needs.
            if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) } != 0 {
                return 90;
            }
            if std::fs::write("/proc/self/uid_map", format!("0 {uid} 1")).is_err() {
                return 91;
            }
            // SAFETY: valid C strings; a private bind mount in our namespace.
            let rc = unsafe {
                libc::mount(
                    src.as_ptr(),
                    dst.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            };
            if rc != 0 {
                return 92;
            }
            match scan(&root) {
                Ok(found) if found.sockets == [root.join("plain")] => 0,
                _ => 1,
            }
        });
        if !host_has(code < 90, "unprivileged user and mount namespaces") {
            return;
        }
        assert_eq!(code, 0, "the mounted socket was not found");
    }

    #[test]
    fn mount_fields_are_unescaped() {
        assert_eq!(
            unescape_mount_field("/a\\040b\\011c\\134d"),
            std::ffi::OsString::from("/a b\tc\\d")
        );
        assert_eq!(
            unescape_mount_field("/plain\\x"),
            std::ffi::OsString::from("/plain\\x")
        );
    }

    #[test]
    fn a_directory_that_can_be_entered_but_not_listed_is_reported() {
        // SAFETY: geteuid has no failure modes.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: permissions do not bind root");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        let hidden = root.join("enter-only");
        let closed = root.join("closed");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::create_dir_all(&closed).unwrap();
        let _s = UnixListener::bind(hidden.join("s")).unwrap();
        std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o311)).unwrap();
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
        let findings = scan(&root);
        for d in [&hidden, &closed] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let findings = findings.unwrap();
        assert_eq!(findings.unlisted, vec![hidden]);
        assert!(findings.sockets.is_empty());
    }
}
