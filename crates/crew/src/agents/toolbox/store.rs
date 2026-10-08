//! Content-addressed file snapshots. Source and destination walks use pinned
//! directory descriptors and never follow links, including during cleanup.

use std::ffi::{CStr, CString, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use crew_core::paths;
use crew_core::toolbox::Content;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub hash: String,
    /// The file or directory under `store/<hash>/payload`.
    pub path: PathBuf,
    pub directory: bool,
    pub bytes: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Failure {
    pub content: Content,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("file item is {0} bytes; the limit is 16777216 bytes (16 MiB)")]
struct TooLarge(u64);

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    executable: bool,
    /// None denotes a directory, including an empty directory.
    bytes: Option<Vec<u8>>,
}

pub struct Store {
    directory: File,
    path: PathBuf,
}

impl Store {
    pub fn open(state_dir: &Path) -> Result<Self> {
        paths::ensure_private_dir(state_dir)?;
        let state = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(state_dir)?;
        let toolbox = directory(&state, Path::new("toolbox"))?;
        let directory = directory(&toolbox, Path::new("store"))?;
        Ok(Self {
            directory,
            path: state_dir.join("toolbox/store"),
        })
    }

    pub fn snapshot(&self, source: &Path) -> std::result::Result<Snapshot, Failure> {
        self.capture(source).map_err(|error| Failure {
            content: if error.downcast_ref::<TooLarge>().is_some() {
                Content::TooLarge
            } else if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                Content::Missing
            } else {
                Content::Invalid
            },
            message: format!("{error:#}"),
        })
    }

    /// Read a snapshot through the store descriptor. Persisted paths are
    /// checked against the content address before they are used again.
    pub fn files(&self, snapshot: &Snapshot) -> Result<BTreeFiles> {
        let entries = self.snapshot_entries(snapshot)?;
        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                entry
                    .bytes
                    .map(|bytes| (entry.path, (bytes, entry.executable)))
            })
            .collect())
    }

    pub fn copy_snapshot(&self, snapshot: &Snapshot, target: &File, path: &Path) -> Result<()> {
        write_entries(target, path, &self.snapshot_entries(snapshot)?)
    }

    fn snapshot_entries(&self, snapshot: &Snapshot) -> Result<Vec<Entry>> {
        if snapshot.hash.len() != 64
            || !snapshot
                .hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || snapshot.path != self.path.join(&snapshot.hash).join("payload")
        {
            bail!("toolbox snapshot has an invalid content address");
        }
        let directory = paths::list_dir_beneath(&self.directory, Path::new(&snapshot.hash))?;
        let payload = open(&directory, Path::new("payload"))?;
        let mut entries = Vec::new();
        let mut bytes = 0;
        capture(&payload, Path::new(""), &mut entries, &mut bytes, 0)?;
        if digest(&entries) != snapshot.hash
            || entries[0].bytes.is_none() != snapshot.directory
            || bytes != snapshot.bytes
        {
            bail!("toolbox snapshot content no longer matches its record");
        }
        Ok(entries)
    }

    fn capture(&self, source: &Path) -> Result<Snapshot> {
        if !source.is_absolute() {
            bail!("file item path must be absolute");
        }
        let relative = normalized(source)?;
        if relative.as_os_str().is_empty() {
            bail!("the filesystem root cannot be a file item");
        }
        let root = File::open("/")?;
        let source = open(&root, &relative)
            .with_context(|| format!("opening file item {}", source.display()))?;
        let mut entries = Vec::new();
        let mut bytes = 0;
        capture(&source, Path::new(""), &mut entries, &mut bytes, 0)?;
        let hash = digest(&entries);
        let want = PathBuf::from(&hash);
        match paths::list_dir_beneath(&self.directory, &want) {
            Ok(existing) => verify(&existing, &hash)?,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                self.publish(&want, &entries, &hash)?;
            }
            Err(error) => return Err(error).context("opening existing toolbox snapshot"),
        }
        Ok(Snapshot {
            hash,
            path: self.path.join(want).join("payload"),
            directory: entries[0].bytes.is_none(),
            bytes,
        })
    }

    fn publish(&self, name: &Path, entries: &[Entry], hash: &str) -> Result<()> {
        let temporary = PathBuf::from(format!(".snapshot-{}", uuid::Uuid::new_v4()));
        let target = directory(&self.directory, &temporary)?;
        let result = (|| {
            write_entries(&target, Path::new("payload"), entries)?;
            target.set_permissions(fs::Permissions::from_mode(0o555))?;
            let from = c_path(&temporary)?;
            let to = c_path(name)?;
            // SAFETY: both names are relative to the same live store descriptor.
            if unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    from.as_ptr(),
                    self.directory.as_raw_fd(),
                    to.as_ptr(),
                )
            } == 0
            {
                self.directory.sync_all()?;
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(libc::EEXIST | libc::ENOTEMPTY)) {
                // Another member published the same content concurrently.
                let existing = paths::list_dir_beneath(&self.directory, name)?;
                verify(&existing, hash)?;
                return Ok(());
            }
            Err(error).context("publishing toolbox snapshot")
        })();
        // On a successful rename there is nothing at this name. On failure,
        // remove only this invocation's scratch directory, without following links.
        let cleanup = remove(&self.directory, &temporary);
        result.and(cleanup)
    }
}

/// Lexical normalization is safe here because the subsequent absolute walk
/// refuses symlinks at every remaining component.
fn normalized(path: &Path) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    bail!("file item path escapes the filesystem root");
                }
            }
            Component::Normal(part) => result.push(part),
            Component::Prefix(_) => bail!("unsupported file item path"),
        }
    }
    Ok(result)
}

fn capture(
    source: &File,
    relative: &Path,
    entries: &mut Vec<Entry>,
    total: &mut u64,
    depth: usize,
) -> Result<()> {
    if entries.len() >= MAX_ENTRIES || depth > MAX_DEPTH {
        bail!(
            "file item exceeds the limit of {MAX_ENTRIES} entries or {MAX_DEPTH} directory levels"
        );
    }
    let before = source.metadata()?;
    if before.is_dir() {
        entries.push(Entry {
            path: relative.to_path_buf(),
            executable: false,
            bytes: None,
        });
        for name in names(source)? {
            let child = open(source, Path::new(&name))
                .with_context(|| format!("reading {}", relative.join(&name).display()))?;
            capture(&child, &relative.join(name), entries, total, depth + 1)?;
        }
    } else if before.is_file() {
        if total.saturating_add(before.len()) > MAX_BYTES {
            return Err(TooLarge(total.saturating_add(before.len())).into());
        }
        let mut bytes = Vec::new();
        source
            .take(MAX_BYTES - *total + 1)
            .read_to_end(&mut bytes)?;
        *total += bytes.len() as u64;
        if *total > MAX_BYTES {
            return Err(TooLarge(*total).into());
        }
        let after = source.metadata()?;
        if before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
        {
            bail!("file item changed while it was being snapshotted");
        }
        entries.push(Entry {
            path: relative.to_path_buf(),
            executable: before.permissions().mode() & 0o111 != 0,
            bytes: Some(bytes),
        });
    } else {
        bail!("file items may contain only regular files and directories");
    }
    Ok(())
}

fn digest(entries: &[Entry]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"crew-toolbox-files-v1\0");
    for entry in entries {
        let name = entry.path.as_os_str().as_bytes();
        hash.update([u8::from(entry.bytes.is_some()), u8::from(entry.executable)]);
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name);
        if let Some(bytes) = &entry.bytes {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
    }
    format!("{:x}", hash.finalize())
}

fn verify(directory: &File, expected: &str) -> Result<()> {
    if names(directory)? != [OsString::from("payload")] {
        bail!("toolbox snapshot has unexpected entries");
    }
    let payload = open(directory, Path::new("payload"))?;
    let mut entries = Vec::new();
    capture(&payload, Path::new(""), &mut entries, &mut 0, 0)?;
    if digest(&entries) != expected {
        bail!("toolbox snapshot content no longer matches its hash");
    }
    Ok(())
}

/// Replace a daemon-owned generated tree, without following a directory or
/// file link an earlier session put in its place. The caller holds the launch
/// lease and has stopped the previous session before using this function.
pub fn replace_tree(parent: &File, name: &Path, files: &BTreeFiles) -> Result<()> {
    remove(parent, name)?;
    let root = directory(parent, name)?;
    for (path, (bytes, executable)) in files {
        let (dir, leaf) = parent_of(&root, path)?;
        write_file(&dir, &leaf, bytes, *executable)?;
    }
    Ok(())
}

pub type BTreeFiles = std::collections::BTreeMap<PathBuf, (Vec<u8>, bool)>;

fn write_entries(target: &File, base: &Path, entries: &[Entry]) -> Result<()> {
    let mut directories = Vec::new();
    for entry in entries {
        let path = base.join(&entry.path);
        let (parent, name) = parent_of(target, &path)?;
        match &entry.bytes {
            None => directories.push(directory(&parent, &name)?),
            Some(bytes) => write_file(&parent, &name, bytes, entry.executable)?,
        }
    }
    for directory in directories.into_iter().rev() {
        directory.set_permissions(fs::Permissions::from_mode(0o555))?;
        directory.sync_all()?;
    }
    Ok(())
}

fn parent_of(root: &File, path: &Path) -> Result<(File, PathBuf)> {
    let mut components = path.components().peekable();
    let mut parent = root.try_clone()?;
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            bail!("generated toolbox path must stay beneath its root");
        };
        if components.peek().is_none() {
            return Ok((parent, PathBuf::from(name)));
        }
        parent = directory(&parent, Path::new(name))?;
    }
    bail!("generated toolbox path is empty")
}

fn write_file(parent: &File, name: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    let name = c_path(name)?;
    // SAFETY: parent is pinned; exclusive creation never opens an existing link.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    let mut file = owned(fd)?;
    file.write_all(bytes)?;
    file.set_permissions(fs::Permissions::from_mode(if executable {
        0o555
    } else {
        0o444
    }))?;
    file.sync_all()?;
    Ok(())
}

pub fn directory(parent: &File, name: &Path) -> Result<File> {
    let c_name = c_path(name)?;
    // SAFETY: creation and the subsequent open are relative to a pinned directory.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), c_name.as_ptr(), 0o700) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
    }
    paths::list_dir_beneath(parent, name)
}

fn open(parent: &File, name: &Path) -> Result<File> {
    match paths::list_dir_beneath(parent, name) {
        Ok(directory) => Ok(directory),
        Err(_) => paths::open_beneath(parent, name),
    }
}

fn owned(fd: libc::c_int) -> Result<File> {
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: a successful open returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn c_path(path: &Path) -> Result<CString> {
    if path.as_os_str().is_empty()
        || path.components().count() != 1
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("expected one relative toolbox path component");
    }
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

pub fn remove(parent: &File, name: &Path) -> Result<()> {
    let name_c = c_path(name)?;
    match paths::list_dir_beneath(parent, name) {
        Ok(child) => {
            child.set_permissions(fs::Permissions::from_mode(0o700))?;
            for entry in names(&child)? {
                remove(&child, Path::new(&entry))?;
            }
            // SAFETY: unlinking a directory entry never traverses its target.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name_c.as_ptr(), libc::AT_REMOVEDIR) }
                == 0
            {
                return Ok(());
            }
        }
        Err(_) => {
            // Files and links are unlinked, never followed.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name_c.as_ptr(), 0) } == 0 {
                return Ok(());
            }
        }
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        Ok(())
    } else {
        Err(error.into())
    }
}

fn names(directory: &File) -> Result<Vec<OsString>> {
    // A new open description avoids sharing a directory iteration offset with
    // a previous fdopendir on a duplicated descriptor.
    let dot = CString::new(".")?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            dot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: fdopendir owns fd after success.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error.into());
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            // SAFETY: this wrapper owns the directory stream.
            unsafe { libc::closedir(self.0) };
        }
    }
    let stream = Directory(stream);
    let mut names = Vec::new();
    loop {
        // SAFETY: stream is live; errno is thread-local and the name is copied
        // before readdir can invalidate its buffer.
        let entry = unsafe {
            *errno() = 0;
            libc::readdir(stream.0)
        };
        if entry.is_null() {
            if unsafe { *errno() } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            break;
        }
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes != b"." && bytes != b".." {
            if names.len() >= MAX_ENTRIES {
                return Err(anyhow!("file item exceeds {MAX_ENTRIES} directory entries"));
            }
            names.push(OsString::from_vec(bytes.to_vec()));
        }
    }
    names.sort();
    Ok(names)
}

unsafe fn errno() -> *mut libc::c_int {
    #[cfg(target_os = "linux")]
    {
        libc::__errno_location()
    }
    #[cfg(not(target_os = "linux"))]
    {
        libc::__error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Scratch(tempfile::TempDir);
    impl Scratch {
        fn new() -> Self {
            Self(tempfile::tempdir().unwrap())
        }
        fn path(&self) -> &Path {
            self.0.path()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            fn writable(path: &Path) {
                if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
                    if let Ok(entries) = fs::read_dir(path) {
                        for entry in entries.flatten() {
                            writable(&entry.path());
                        }
                    }
                }
            }
            writable(self.path());
        }
    }

    #[test]
    fn equal_trees_share_read_only_snapshots_and_source_changes_leave_the_old_one() {
        let temp = Scratch::new();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for source in [&a, &b] {
            fs::create_dir_all(source.join("scripts")).unwrap();
            fs::write(source.join("SKILL.md"), "same skill").unwrap();
            fs::write(source.join("scripts/run"), "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(
                source.join("scripts/run"),
                fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let store = Store::open(&temp.path().join("state")).unwrap();
        let one = store.snapshot(&a).unwrap();
        let two = store.snapshot(&b).unwrap();
        assert_eq!(one, two);
        assert_eq!(
            fs::metadata(one.path.join("SKILL.md")).unwrap().mode() & 0o777,
            0o444
        );
        assert_eq!(
            fs::metadata(one.path.join("scripts/run")).unwrap().mode() & 0o777,
            0o555
        );
        fs::write(a.join("SKILL.md"), "changed skill").unwrap();
        let changed = store.snapshot(&a).unwrap();
        assert_ne!(one.hash, changed.hash);
        assert_eq!(
            fs::read_to_string(one.path.join("SKILL.md")).unwrap(),
            "same skill"
        );
    }

    #[test]
    fn links_nonregular_files_size_limits_and_existing_snapshot_tampering_fail() {
        let temp = Scratch::new();
        let store = Store::open(&temp.path().join("state")).unwrap();
        let source = temp.path().join("skill");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("SKILL.md"), "skill").unwrap();
        let original = store.snapshot(&source).unwrap();
        symlink(temp.path(), source.join("outside")).unwrap();
        assert!(store.snapshot(&source).is_err());
        fs::remove_file(source.join("outside")).unwrap();
        symlink(&source, temp.path().join("root-link")).unwrap();
        assert!(store.snapshot(&temp.path().join("root-link")).is_err());
        let large = source.join("large");
        File::create(&large)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert_eq!(
            store.snapshot(&source).unwrap_err().content,
            Content::TooLarge
        );
        fs::remove_file(large).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(source.join("socket")).unwrap();
        assert!(store.snapshot(&source).is_err());
        fs::remove_file(source.join("socket")).unwrap();
        fs::set_permissions(&original.path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_file(original.path.join("SKILL.md")).unwrap();
        symlink(source.join("SKILL.md"), original.path.join("SKILL.md")).unwrap();
        assert!(store.snapshot(&source).is_err());
    }

    #[test]
    fn generated_tree_replacement_unlinks_prior_links_without_touching_the_target() {
        let temp = Scratch::new();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), "keep").unwrap();
        let parent = File::open(temp.path()).unwrap();
        symlink(&outside, temp.path().join("generated")).unwrap();
        let files =
            BTreeFiles::from([(PathBuf::from("skills/a/SKILL.md"), (b"new".to_vec(), false))]);
        replace_tree(&parent, Path::new("generated"), &files).unwrap();
        assert_eq!(
            fs::read_to_string(outside.join("sentinel")).unwrap(),
            "keep"
        );
        fs::write(temp.path().join("generated/stale"), "old").unwrap();
        replace_tree(&parent, Path::new("generated"), &files).unwrap();
        assert!(!temp.path().join("generated/stale").exists());
        assert_eq!(
            fs::read(temp.path().join("generated/skills/a/SKILL.md")).unwrap(),
            b"new"
        );
    }
}
