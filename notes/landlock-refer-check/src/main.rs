//! What Landlock does to rename and link across directories, with and
//! without REFER. Usage: refer <scratch dir> <mode>
//! mode: none | unix-only | refer-root
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

const REFER: u64 = 1 << 13;
const RESOLVE_UNIX: u64 = 1 << 16;
#[repr(C)]
struct RulesetAttr { handled_access_fs: u64, handled_access_net: u64, scoped: u64 }
#[repr(C, packed)]
struct PathBeneathAttr { allowed_access: u64, parent_fd: i32 }

fn open_dir(p: &Path) -> OwnedFd {
    let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    assert!(fd >= 0, "open {}", p.display());
    unsafe { OwnedFd::from_raw_fd(fd) }
}
fn restrict(handled: u64, rules: &[(&Path, u64)]) {
    let attr = RulesetAttr { handled_access_fs: handled, handled_access_net: 0, scoped: 0 };
    let rc = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, &attr as *const _, std::mem::size_of::<RulesetAttr>(), 0u32) };
    assert!(rc >= 0, "create_ruleset: {}", std::io::Error::last_os_error());
    let ruleset = unsafe { OwnedFd::from_raw_fd(rc as i32) };
    for (dir, access) in rules {
        let d = open_dir(dir);
        let rule = PathBeneathAttr { allowed_access: *access, parent_fd: d.as_raw_fd() };
        let rc = unsafe { libc::syscall(libc::SYS_landlock_add_rule, ruleset.as_raw_fd(), 1, &rule as *const _, 0u32) };
        assert!(rc >= 0, "add_rule {}: {}", dir.display(), std::io::Error::last_os_error());
    }
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) }, 0);
    let rc = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0u32) };
    assert!(rc >= 0, "restrict_self: {}", std::io::Error::last_os_error());
}
fn show(what: &str, r: std::io::Result<()>) {
    match r {
        Ok(()) => println!("{what}: ok"),
        Err(e) => println!("{what}: {}", match e.raw_os_error() { Some(libc::EXDEV) => "EXDEV".into(), Some(libc::EACCES) => "EACCES".into(), _ => e.to_string() }),
    }
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = Path::new(&args[1]);
    let mode = args[2].as_str();
    let _ = fs::remove_dir_all(root);
    for d in ["a", "b", "objects/19", "sock"] { fs::create_dir_all(root.join(d)).unwrap(); }
    for f in ["a/f", "a/g", "a/h", "objects/.tmp1"] { fs::write(root.join(f), "x").unwrap(); }
    let sock = root.join("sock");
    match mode {
        "none" => {}
        // crew's ruleset today: sockets only.
        "unix-only" => restrict(RESOLVE_UNIX, &[(&sock, RESOLVE_UNIX)]),
        // The amendment: REFER handled, allowed beneath the root.
        "refer-root" => restrict(RESOLVE_UNIX | REFER, &[(&sock, RESOLVE_UNIX), (Path::new("/"), REFER)]),
        _ => panic!("mode"),
    }
    println!("-- {mode}");
    show("rename within one directory   a/f -> a/f2", fs::rename(root.join("a/f"), root.join("a/f2")));
    show("rename across directories     a/g -> b/g", fs::rename(root.join("a/g"), root.join("b/g")));
    show("link across directories       a/h -> b/h", fs::hard_link(root.join("a/h"), root.join("b/h")));
    show("gix: objects/.tmp1 -> objects/19/abc", fs::rename(root.join("objects/.tmp1"), root.join("objects/19/abc")));
    show("into the socket directory     a/h -> sock/h (would gain RESOLVE_UNIX)", fs::rename(root.join("a/h"), root.join("sock/h")));
}
