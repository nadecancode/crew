//! What crew asks of the operating system that Linux and macOS answer
//! differently.
//!
//! crew's guarantees were built on Linux (`openat2`, `O_PATH`, `/proc`);
//! macOS gives the same answers through interfaces of its own
//! (`O_NOFOLLOW_ANY`, `fdopendir`, `proc_pidpath`, `KERN_PROCARGS2`). What
//! only Linux has, the agent sandbox, is refused on macOS by its probe.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Open flags for a descriptor that only names an object, for `*at` calls
/// and `fstat`: `O_PATH` on Linux. macOS has none, so it is a read-only open
/// that does not wait on a FIFO.
#[cfg(target_os = "linux")]
pub const PATH_ONLY: i32 = libc::O_PATH;
#[cfg(not(target_os = "linux"))]
pub const PATH_ONLY: i32 = libc::O_RDONLY | libc::O_NONBLOCK;

/// Open `path` (relative to `dir`, or absolute) without following a symlink
/// in any of its components. With `beneath`, the path must also stay below
/// `dir`: no absolute path, and no `..` that leaves it.
///
/// Linux: `openat2` with `RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS` (and
/// `RESOLVE_BENEATH`).
#[cfg(target_os = "linux")]
pub fn open_no_symlinks(dir: RawFd, path: &Path, flags: i32, mode: u32, beneath: bool) -> io::Result<OwnedFd> {
    let c_path = c_path(path)?;
    let mut resolve = libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_MAGICLINKS;
    if beneath {
        resolve |= libc::RESOLVE_BENEATH;
    }
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

/// macOS: `openat` with `O_NOFOLLOW_ANY`, which refuses a symlink in any
/// component. macOS has no `RESOLVE_BENEATH`; without symlinks, a relative
/// path with no `..` cannot leave `dir`, so that is what `beneath` requires
/// (more strictly than Linux, which allows a `..` that stays below).
#[cfg(target_os = "macos")]
pub fn open_no_symlinks(dir: RawFd, path: &Path, flags: i32, mode: u32, beneath: bool) -> io::Result<OwnedFd> {
    if beneath && !stays_beneath(path) {
        return Err(io::Error::from_raw_os_error(libc::EXDEV));
    }
    let c_path = c_path(path)?;
    // SAFETY: `c_path` is a valid C string; the result is checked.
    let fd = unsafe { libc::openat(dir, c_path.as_ptr(), flags | libc::O_NOFOLLOW_ANY, mode as libc::c_uint) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Other systems have no way to refuse a symlink in every component.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn open_no_symlinks(_dir: RawFd, _path: &Path, _flags: i32, _mode: u32, _beneath: bool) -> io::Result<OwnedFd> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "opening a path without following symlinks needs Linux or macOS",
    ))
}

/// Whether `path` names something below the directory it is relative to,
/// read lexically: not absolute, and no `..`.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn stays_beneath(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|c| !matches!(c, std::path::Component::ParentDir))
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

/// The names in the directory held open as `dir`, enumerated through that
/// descriptor whatever its path names now (`.` and `..` left out).
#[cfg(target_os = "linux")]
pub fn list_dir(dir: BorrowedFd<'_>) -> io::Result<Vec<String>> {
    Ok(std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect())
}

/// Without `/proc`: a duplicate of the descriptor read with `fdopendir`.
#[cfg(not(target_os = "linux"))]
pub fn list_dir(dir: BorrowedFd<'_>) -> io::Result<Vec<String>> {
    let copy = dir.try_clone_to_owned()?;
    // The duplicate shares the original's offset: read from the start.
    // SAFETY: fdopendir takes over the descriptor, which closedir closes.
    let stream = unsafe { libc::fdopendir(copy.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error());
    }
    std::mem::forget(copy);
    let mut names = Vec::new();
    // SAFETY: `stream` is a live directory stream until closedir; each entry
    // readdir returns is valid until the next call.
    unsafe {
        libc::rewinddir(stream);
        loop {
            let entry = libc::readdir(stream);
            if entry.is_null() {
                break;
            }
            let name = std::ffi::CStr::from_ptr((*entry).d_name.as_ptr());
            if let Ok(name) = name.to_str() {
                if name != "." && name != ".." {
                    names.push(name.to_string());
                }
            }
        }
        libc::closedir(stream);
    }
    Ok(names)
}

/// The executable process `pid` runs, as a path to compare after
/// canonicalizing it.
#[cfg(target_os = "linux")]
pub fn process_exe(pid: u32) -> Option<PathBuf> {
    Some(PathBuf::from(format!("/proc/{pid}/exe")))
}

#[cfg(target_os = "macos")]
pub fn process_exe(pid: u32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is writable for the length given.
    let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_exe(_pid: u32) -> Option<PathBuf> {
    None
}

/// The arguments process `pid` was started with.
#[cfg(target_os = "linux")]
pub fn process_args(pid: u32) -> Option<Vec<Vec<u8>>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        raw.split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(<[u8]>::to_vec)
            .collect(),
    )
}

/// `KERN_PROCARGS2`: the argument count, the executable's path, padding,
/// then the arguments, each ending in a NUL (the environment follows).
#[cfg(target_os = "macos")]
pub fn process_args(pid: u32) -> Option<Vec<Vec<u8>>> {
    let mut argmax: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: `argmax` is a c_int out-pointer of the size given.
    if unsafe {
        libc::sysctl(mib.as_mut_ptr(), 2, (&mut argmax as *mut libc::c_int).cast(), &mut size, std::ptr::null_mut(), 0)
    } != 0
    {
        return None;
    }
    let mut buf = vec![0u8; usize::try_from(argmax).ok()?];
    let mut size = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    // SAFETY: `buf` is writable for `size` bytes.
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    parse_procargs(&buf[..size.min(buf.len())])
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_args(_pid: u32) -> Option<Vec<Vec<u8>>> {
    None
}

/// The arguments of a `KERN_PROCARGS2` buffer.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_procargs(buf: &[u8]) -> Option<Vec<Vec<u8>>> {
    let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
    let mut rest = &buf[4..];
    // The executable's path, then the NULs that pad it.
    let end = rest.iter().position(|b| *b == 0)?;
    rest = &rest[end..];
    let start = rest.iter().position(|b| *b != 0)?;
    rest = &rest[start..];
    let args: Vec<Vec<u8>> = rest
        .split(|b| *b == 0)
        .take(argc)
        .map(<[u8]>::to_vec)
        .collect();
    (args.len() == argc).then_some(args)
}

/// The file the running crew binary was loaded from, to keep a copy of
/// (daemon::binary). Linux names the running inode itself, whatever has
/// become of its path; elsewhere it is the path it was started from, read
/// at once, so a rebuild in the moment between start and copy is the gap.
pub fn running_binary() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        Some(PathBuf::from("/proc/self/exe"))
    } else {
        std::env::current_exe().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::os::unix::fs::symlink;

    #[test]
    fn a_symlink_in_any_component_is_refused_and_beneath_stays_below() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("a")).unwrap();
        std::fs::write(root.join("a/f"), "x").unwrap();
        symlink(root.join("a"), root.join("link")).unwrap();
        let top = open_no_symlinks(libc::AT_FDCWD, &root, libc::O_RDONLY | libc::O_DIRECTORY, 0, false).unwrap();
        let at = |p: &str, beneath: bool| open_no_symlinks(top.as_raw_fd(), Path::new(p), libc::O_RDONLY, 0, beneath);
        assert!(at("a/f", true).is_ok());
        assert!(at("link/f", false).is_err(), "a symlink in the middle");
        assert!(at("link", false).is_err(), "a symlink at the end");
        assert!(at("../x", true).is_err(), "out of the directory");
        assert!(at("/etc/hosts", true).is_err(), "an absolute path");
        let mut names = list_dir(top.as_fd()).unwrap();
        names.sort();
        assert_eq!(names, ["a", "link"]);
    }

    #[test]
    fn this_process_is_found_by_its_executable_and_arguments() {
        let me = std::process::id();
        let exe = process_exe(me).and_then(|p| p.canonicalize().ok());
        assert_eq!(exe, std::env::current_exe().ok().and_then(|p| p.canonicalize().ok()));
        let args = process_args(me).unwrap();
        assert_eq!(args.len(), std::env::args_os().count());
    }

    #[test]
    fn procargs_are_read_past_the_executable_and_its_padding() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/sh\0\0\0\0sh\0-c\0HOME=/x\0");
        assert_eq!(parse_procargs(&buf), Some(vec![b"sh".to_vec(), b"-c".to_vec()]));
        assert_eq!(parse_procargs(&3i32.to_ne_bytes()), None);
    }
}
