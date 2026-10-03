//! The Nix store's socket scan on kernels without Landlock's socket rule
//! (ABI 6 to 8). From ABI 9 the shim refuses every socket made outside the
//! sandbox and the store is not walked at all.
//!
//! Walking a live store takes minutes, so each top-level store object
//! found free of sockets is remembered, but only while it is a finalized,
//! immutable object: owned by root, no write bit, the modification time of
//! 1 that Nix gives registered paths. Such an object is remembered by name,
//! device, inode and change time; anything else (a path still being built,
//! a symlink, a changed or replaced object, an unreadable cache entry) is
//! walked again on every launch.
//!
//! Why a remembered object cannot come to hide a socket:
//! - poisoning: an entry only skips an object that is finalized now and has
//!   the same name, device, inode and change time, so a forged entry cannot
//!   hide a path being built or any object it does not describe exactly;
//!   and no sandbox can write the cache (below);
//! - staleness: replacing, recreating or repairing an object, or changing
//!   its own permissions or owner, changes its inode or change time, so it is walked again; a
//!   socket mounted into one is found through the mount table, read on
//!   every launch. What remains is root writing inside a registered,
//!   read-only store object, which Nix never does.
//!
//! The cache lives under `$XDG_CACHE_HOME/crew`, which is in the protected
//! set: no sandbox mounts it or an ancestor of it.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use super::scan::{mount_points_under, scan, Findings};

const HEADER: &str = "crew nix-store-scan v1";

type Key = (String, u64, u64, i64, i64);

/// Top-level store objects known to be free of sockets.
#[derive(Debug, Default)]
pub struct StoreCache {
    clean: HashSet<Key>,
}

impl StoreCache {
    /// The cache at `path`; missing or unreadable is empty, and an entry
    /// that does not parse is a miss.
    pub fn load(path: &Path) -> Self {
        let mut cache = Self::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return cache;
        };
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return cache;
        }
        for line in lines {
            let fields: Vec<&str> = line.split(' ').collect();
            let [dev, ino, sec, nsec, name] = fields[..] else {
                continue;
            };
            let parsed = (dev.parse(), ino.parse(), sec.parse(), nsec.parse());
            if let (Ok(dev), Ok(ino), Ok(sec), Ok(nsec)) = parsed {
                cache.clean.insert((name.to_string(), dev, ino, sec, nsec));
            }
        }
        cache
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut text = format!("{HEADER}\n");
        let mut keys: Vec<&Key> = self.clean.iter().collect();
        keys.sort();
        for (name, dev, ino, sec, nsec) in keys {
            text.push_str(&format!("{dev} {ino} {sec} {nsec} {name}\n"));
        }
        if let Some(dir) = path.parent() {
            crew_core::paths::ensure_private_dir(dir)?;
        }
        crew_core::paths::write_private_file(path, text.as_bytes())
    }
}

/// What a store walk did, for tests and logs.
#[derive(Debug, Default, PartialEq)]
pub struct StoreScan {
    pub findings: Findings,
    pub walked: usize,
    pub remembered: usize,
}

/// Walk the store, skipping objects `cache` remembers, and remember the
/// finalized ones found clean. `owner` is the uid finalized objects belong
/// to (root; a parameter so tests can build a store).
pub fn scan_store(store: &Path, cache: &mut StoreCache, owner: u32) -> io::Result<StoreScan> {
    let mut result = StoreScan::default();
    for entry in std::fs::read_dir(store)? {
        let entry = entry?;
        let path = entry.path();
        let Ok(before) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let key = key(&entry.file_name(), &before, owner);
        if key.as_ref().is_some_and(|k| cache.clean.contains(k)) {
            result.remembered += 1;
            continue;
        }
        let found = match scan(&path) {
            Ok(found) => found,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        result.walked += 1;
        // Remember it only if it is still the same finalized object.
        let after = std::fs::symlink_metadata(&path).ok();
        let unchanged =
            after.is_some_and(|after| key == self::key(&entry.file_name(), &after, owner));
        if let (Some(key), true, true) = (key, unchanged, found == Findings::default()) {
            cache.clean.insert(key);
        }
        result.findings.sockets.extend(found.sockets);
        result.findings.unlisted.extend(found.unlisted);
    }
    // Only root can change a finalized object, but anyone with mount rights
    // could mount a socket into one: mount points are checked every time.
    for point in mount_points_under(store, &[]) {
        let socket = std::fs::symlink_metadata(&point).is_ok_and(|m| m.file_type().is_socket());
        if socket && !result.findings.sockets.contains(&point) {
            result.findings.sockets.push(point);
        }
    }
    result.findings.sockets.sort();
    result.findings.unlisted.sort();
    Ok(result)
}

/// The cache key of a finalized, immutable store object, or none.
fn key(name: &std::ffi::OsStr, meta: &std::fs::Metadata, owner: u32) -> Option<Key> {
    let name = name.to_str().filter(|n| is_store_name(n))?;
    let finalized = meta.uid() == owner
        && meta.mode() & 0o222 == 0
        && meta.mtime() == 1
        && !meta.file_type().is_symlink();
    finalized.then(|| {
        (
            name.to_string(),
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec(),
        )
    })
}

/// Store object names: hash, dash, name, from a small alphabet.
fn is_store_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-._?=".contains(&b))
}

/// Where the cache lives for this user.
pub fn default_cache_path(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let cache = env("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(cache.join("crew").join("nix-store-scan"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    fn uid() -> u32 {
        // SAFETY: getuid has no failure modes.
        unsafe { libc::getuid() }
    }

    /// Make `path` look like a registered store object: no write bits,
    /// modification time 1.
    fn finalize(path: &Path) {
        let mode = if path.is_dir() { 0o555 } else { 0o444 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        set_time_1(path);
    }

    fn set_time_1(path: &Path) {
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let times = [
            libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: 1,
                tv_nsec: 0,
            },
        ];
        // SAFETY: valid C string and two timespecs.
        let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(rc, 0);
    }

    fn unlock(path: &Path) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    struct Store {
        dir: tempfile::TempDir,
        _listener: UnixListener,
    }

    impl Store {
        fn path(&self) -> PathBuf {
            self.dir.path().join("store")
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            for entry in std::fs::read_dir(self.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                unlock(&entry.path());
            }
        }
    }

    /// A store with a finalized clean object, a finalized object holding a
    /// socket, an object still being built, one with write bits gone but
    /// the wrong time, and a symlink.
    fn store() -> Store {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        for sub in [
            "aaa-clean/bin",
            "bbb-sock",
            "ccc-building",
            "eee-not-registered",
        ] {
            std::fs::create_dir_all(store.join(sub)).unwrap();
        }
        std::fs::write(store.join("aaa-clean/bin/tool"), "x").unwrap();
        let listener = UnixListener::bind(store.join("bbb-sock/s")).unwrap();
        std::os::unix::fs::symlink("aaa-clean", store.join("ddd-link")).unwrap();
        finalize(&store.join("aaa-clean"));
        finalize(&store.join("bbb-sock"));
        // Still being built: canonical time, but writable.
        set_time_1(&store.join("ccc-building"));
        let pending = store.join("eee-not-registered");
        std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o555)).unwrap();
        Store {
            dir,
            _listener: listener,
        }
    }

    #[test]
    fn only_finalized_clean_objects_are_remembered_and_then_skipped() {
        let s = store();
        let mut cache = StoreCache::default();
        let first = scan_store(&s.path(), &mut cache, uid()).unwrap();
        assert_eq!(first.walked, 5);
        assert_eq!(first.remembered, 0);
        assert_eq!(first.findings.sockets, vec![s.path().join("bbb-sock/s")]);
        assert_eq!(cache.clean.len(), 1);
        let second = scan_store(&s.path(), &mut cache, uid()).unwrap();
        assert_eq!(second.remembered, 1);
        assert_eq!(second.walked, 4);
        assert_eq!(second.findings, first.findings);
        // An object owned by someone else is never remembered.
        let mut other = StoreCache::default();
        scan_store(&s.path(), &mut other, uid() + 1).unwrap();
        assert!(other.clean.is_empty());
    }

    #[test]
    fn a_changed_object_is_walked_again() {
        let s = store();
        let mut cache = StoreCache::default();
        scan_store(&s.path(), &mut cache, uid()).unwrap();
        let clean = s.path().join("aaa-clean");
        unlock(&clean);
        let _late = UnixListener::bind(clean.join("late.sock")).unwrap();
        finalize(&clean);
        let again = scan_store(&s.path(), &mut cache, uid()).unwrap();
        assert_eq!(again.remembered, 0);
        assert!(again.findings.sockets.contains(&clean.join("late.sock")));
    }

    #[test]
    fn the_cache_round_trips_and_bad_entries_are_misses() {
        let s = store();
        let mut cache = StoreCache::default();
        scan_store(&s.path(), &mut cache, uid()).unwrap();
        let file = s.dir.path().join("cache/crew/nix-store-scan");
        cache.save(&file).unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(StoreCache::load(&file).clean, cache.clean);
        let text = std::fs::read_to_string(&file).unwrap();
        let corrupt = text.replace("aaa-clean", "aaa clean") + "garbage\n1 2 3\n";
        std::fs::write(&file, corrupt).unwrap();
        let mut loaded = StoreCache::load(&file);
        assert!(loaded.clean.is_empty());
        assert_eq!(
            scan_store(&s.path(), &mut loaded, uid())
                .unwrap()
                .remembered,
            0
        );
        std::fs::write(&file, text.replace(HEADER, "other v2")).unwrap();
        assert!(StoreCache::load(&file).clean.is_empty());
        assert!(StoreCache::load(&s.dir.path().join("missing"))
            .clean
            .is_empty());
    }

    #[test]
    fn forged_entries_cannot_hide_an_object_that_is_not_finalized() {
        let s = store();
        let building = s.path().join("ccc-building");
        let _late = UnixListener::bind(building.join("s")).unwrap();
        // A cache naming every object that is not finalized, exactly.
        let mut forged = format!("{HEADER}\n");
        for name in ["ccc-building", "eee-not-registered", "ddd-link"] {
            let m = std::fs::symlink_metadata(s.path().join(name)).unwrap();
            forged.push_str(&format!(
                "{} {} {} {} {name}\n",
                m.dev(),
                m.ino(),
                m.ctime(),
                m.ctime_nsec()
            ));
        }
        let file = s.dir.path().join("forged");
        std::fs::write(&file, forged).unwrap();
        let mut cache = StoreCache::load(&file);
        assert_eq!(cache.clean.len(), 3);
        let walk = scan_store(&s.path(), &mut cache, uid()).unwrap();
        assert_eq!(walk.remembered, 0);
        assert!(walk.findings.sockets.contains(&building.join("s")));
    }

    #[test]
    fn a_recreated_object_is_walked_again() {
        let s = store();
        let mut cache = StoreCache::default();
        scan_store(&s.path(), &mut cache, uid()).unwrap();
        let clean = s.path().join("aaa-clean");
        unlock(&clean);
        unlock(&clean.join("bin"));
        std::fs::remove_dir_all(&clean).unwrap();
        std::fs::create_dir_all(clean.join("bin")).unwrap();
        let _s = UnixListener::bind(clean.join("bin/s")).unwrap();
        finalize(&clean);
        let walk = scan_store(&s.path(), &mut cache, uid()).unwrap();
        assert_eq!(walk.remembered, 0);
        assert!(walk.findings.sockets.contains(&clean.join("bin/s")));
    }

    #[test]
    fn a_socket_mounted_into_a_remembered_object_is_found() {
        use crate::sandbox::test_support::{host_has, in_child};
        use std::ffi::CString;
        let s = store();
        let mut cache = StoreCache::default();
        scan_store(&s.path(), &mut cache, uid()).unwrap();
        let target = s.path().join("aaa-clean/bin/tool");
        let outside = s.dir.path().join("outside.sock");
        let _l = UnixListener::bind(&outside).unwrap();
        let (src, dst) = (
            CString::new(outside.as_os_str().as_bytes()).unwrap(),
            CString::new(target.as_os_str().as_bytes()).unwrap(),
        );
        let real_uid = uid();
        let code = in_child(|| {
            // SAFETY: the forked child is single-threaded, as unshare needs.
            if unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) } != 0 {
                return 90;
            }
            if std::fs::write("/proc/self/uid_map", format!("0 {real_uid} 1")).is_err() {
                return 91;
            }
            // SAFETY: valid C strings; a bind mount in the child's namespace.
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
            // The real uid shows as 0 in the child's namespace.
            match scan_store(&s.path(), &mut cache, 0) {
                Ok(walk) if walk.remembered == 1 && walk.findings.sockets.contains(&target) => 0,
                Ok(_) => 1,
                Err(_) => 2,
            }
        });
        if !host_has(code < 90, "unprivileged user and mount namespaces") {
            return;
        }
        assert_eq!(code, 0, "check {code} failed");
    }

    #[test]
    fn the_cache_lives_under_crews_cache_directory() {
        let env = |name: &str| match name {
            "HOME" => Some(OsString::from("/home/u")),
            _ => None,
        };
        assert_eq!(
            default_cache_path(&env),
            Some(PathBuf::from("/home/u/.cache/crew/nix-store-scan"))
        );
        let xdg = |name: &str| match name {
            "XDG_CACHE_HOME" => Some(OsString::from("/c")),
            _ => None,
        };
        assert_eq!(
            default_cache_path(&xdg),
            Some(PathBuf::from("/c/crew/nix-store-scan"))
        );
    }
}
