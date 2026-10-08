//! Private, bounded raw artifacts. All lookup, creation and removal below the
//! member root use held directory descriptors, never reconstructed paths.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use crew_core::output_view::{self, CapturedStatus, MAX_CAPTURE_BYTES};
use crew_core::paths;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAGIC: &[u8] = b"crew-output\0\x01";
const HEADER_LIMIT: usize = 1024;
const FILE_LIMIT: usize = MAX_CAPTURE_BYTES + HEADER_LIMIT + MAGIC.len() + 4;
const MAX_FILES: usize = 128;
const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub(super) struct Capture {
    pub status: CapturedStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    version: u32,
    status: CapturedStatus,
    stdout_bytes: usize,
    stderr_bytes: usize,
}

/// Scope is derived only from an authenticated member identity. The digest is
/// private routing metadata; it is not a substitute for authenticating again
/// on the next invocation. A credential rotation makes old artifacts unavailable.
pub(super) fn namespace(identity: &paths::Identity) -> String {
    let mut hash = Sha256::new();
    hash.update(b"crew-output-scope-v1\0");
    for part in [&identity.socket, &identity.agent_id, &identity.token] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

pub(super) struct Store {
    root: File,
    _lock: File,
    scope: String,
}

impl Drop for Store {
    fn drop(&mut self) {
        // A concurrent fork elsewhere in the process can inherit this open
        // file description until exec closes CLOEXEC descriptors. Closing only
        // our fd would then leave the completed operation's flock held briefly.
        // SAFETY: _lock is still open and belongs to this successful Store.
        unsafe { libc::flock(self._lock.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl Store {
    pub fn open(member: &File, scope: String) -> Result<Self> {
        if !hex(&scope) {
            bail!("invalid output scope");
        }
        let rw = private_dir(member, "rw")?;
        let root = private_dir(&rw, "output-v1")?;
        let lock = open_at(&root, ".lock", libc::O_RDWR | libc::O_CREAT, 0o600)?;
        private_file(&lock)?;
        // A busy store is unavailable, never an unbounded wait on tool output.
        // SAFETY: lock is an open regular file; flock borrows the descriptor.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("output store is busy");
        }
        Ok(Self {
            root,
            _lock: lock,
            scope,
        })
    }

    fn name(&self, reference: &str) -> Result<String> {
        let digest = reference.strip_prefix("sha256:").unwrap_or("");
        if !hex(digest) {
            bail!("expected a sha256 content reference");
        }
        Ok(format!("{}-{digest}.raw", self.scope))
    }

    pub fn put(&self, capture: &Capture) -> Result<String> {
        if !capture.status.valid()
            || capture.stdout.len().saturating_add(capture.stderr.len()) > MAX_CAPTURE_BYTES
        {
            bail!("invalid or oversized output capture");
        }
        let reference = output_view::reference(&capture.stdout, &capture.stderr, capture.status);
        let name = self.name(&reference)?;
        let header = serde_json::to_vec(&Header {
            version: output_view::FORMAT_VERSION,
            status: capture.status,
            stdout_bytes: capture.stdout.len(),
            stderr_bytes: capture.stderr.len(),
        })?;
        let size =
            (MAGIC.len() + 4 + header.len() + capture.stdout.len() + capture.stderr.len()) as u64;
        let entries = self.prune(SystemTime::now())?;
        if entries.iter().any(|entry| entry.name == name) {
            self.read(&reference)?; // Verify an existing immutable artifact.
            return Ok(reference);
        }
        self.make_room(entries, size)?;
        let pending = format!(".pending-{}", uuid::Uuid::new_v4().simple());
        let result: Result<()> = (|| {
            let mut file = open_at(
                &self.root,
                &pending,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )?;
            private_file(&file)?;
            file.write_all(MAGIC)?;
            file.write_all(&(header.len() as u32).to_le_bytes())?;
            file.write_all(&header)?;
            file.write_all(&capture.stdout)?;
            file.write_all(&capture.stderr)?;
            file.sync_all()?;
            // Publish without replacing a name another process inserted. A
            // hard link is transient within the locked private directory.
            let source = CString::new(pending.as_str())?;
            let destination = CString::new(name.as_str())?;
            // SAFETY: both names are single validated components; root is held.
            if unsafe {
                libc::linkat(
                    self.root.as_raw_fd(),
                    source.as_ptr(),
                    self.root.as_raw_fd(),
                    destination.as_ptr(),
                    0,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            remove_at(&self.root, &pending)?;
            self.root.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = remove_at(&self.root, &pending);
        }
        result.context("could not retain raw output")?;
        Ok(reference)
    }

    pub fn get(&self, reference: &str) -> Result<Capture> {
        self.name(reference)?;
        self.make_room(self.prune(SystemTime::now())?, 0)?;
        self.read(reference)
    }

    fn read(&self, reference: &str) -> Result<Capture> {
        let name = self.name(reference)?;
        let file = open_at(&self.root, &name, libc::O_RDONLY, 0)
            .context("raw output unavailable (expired, evicted, or outside this member scope)")?;
        private_file(&file)?;
        if file.metadata()?.len() > FILE_LIMIT as u64 {
            bail!("raw output artifact is oversized");
        }
        let mut bytes = Vec::new();
        file.take(FILE_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
        let capture = decode(&bytes)?;
        if output_view::reference(&capture.stdout, &capture.stderr, capture.status) != reference {
            bail!("raw output integrity check failed");
        }
        Ok(capture)
    }

    fn prune(&self, now: SystemTime) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let names = names(&self.root)?;
        // A crash between publication and pending-name removal leaves two
        // links to the same file. Remove pending names first, then validate
        // the immutable artifacts, independent of directory enumeration order.
        for name in &names {
            if pending_name(name) {
                let file = open_at(&self.root, name, libc::O_RDONLY, 0)?;
                let meta = file.metadata()?;
                if !meta.is_file()
                    || meta.uid() != paths::uid()
                    || meta.permissions().mode() & 0o777 != 0o600
                {
                    bail!("invalid pending output artifact");
                }
                remove_at(&self.root, name)?;
            }
        }
        for name in names {
            if name == ".lock" {
                continue;
            }
            if pending_name(&name) {
                continue;
            }
            if !artifact_name(&name) {
                bail!("output store contains an unexpected entry");
            }
            let file = open_at(&self.root, &name, libc::O_RDONLY, 0)?;
            private_file(&file)?;
            let meta = file.metadata()?;
            let modified = meta.modified()?;
            if now
                .duration_since(modified)
                .is_ok_and(|age| age >= RETENTION)
            {
                remove_at(&self.root, &name)?;
                continue;
            }
            if meta.len() > FILE_LIMIT as u64 {
                bail!("output store contains an oversized artifact");
            }
            entries.push(Entry {
                name,
                bytes: meta.len(),
                modified,
            });
        }
        entries.sort_by(|a, b| {
            a.modified
                .cmp(&b.modified)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }

    fn make_room(&self, entries: Vec<Entry>, incoming: u64) -> Result<()> {
        let mut count = entries.len();
        let mut total: u64 = entries.iter().map(|e| e.bytes).sum();
        for entry in entries {
            if count + usize::from(incoming > 0) <= MAX_FILES && total + incoming <= MAX_BYTES {
                break;
            }
            remove_at(&self.root, &entry.name)?;
            total -= entry.bytes;
            count -= 1;
        }
        Ok(())
    }
}

struct Entry {
    name: String,
    bytes: u64,
    modified: SystemTime,
}

fn decode(bytes: &[u8]) -> Result<Capture> {
    if bytes.len() > FILE_LIMIT || !bytes.starts_with(MAGIC) || bytes.len() < MAGIC.len() + 4 {
        bail!("invalid raw output artifact");
    }
    let at = MAGIC.len();
    let header_len = u32::from_le_bytes(bytes[at..at + 4].try_into()?) as usize;
    let start = at + 4;
    let end = start + header_len;
    if header_len > HEADER_LIMIT || end > bytes.len() {
        bail!("invalid raw output header length");
    }
    let header: Header = serde_json::from_slice(&bytes[start..end])
        .map_err(|_| anyhow::anyhow!("invalid raw output metadata"))?;
    if header.version != output_view::FORMAT_VERSION
        || !header.status.valid()
        || header.stdout_bytes.saturating_add(header.stderr_bytes) > MAX_CAPTURE_BYTES
        || end + header.stdout_bytes + header.stderr_bytes != bytes.len()
    {
        bail!("invalid raw output metadata or length");
    }
    Ok(Capture {
        status: header.status,
        stdout: bytes[end..end + header.stdout_bytes].to_vec(),
        stderr: bytes[end + header.stdout_bytes..].to_vec(),
    })
}

fn hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn pending_name(name: &str) -> bool {
    name.strip_prefix(".pending-")
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn artifact_name(name: &str) -> bool {
    let Some((scope, digest)) = name.strip_suffix(".raw").and_then(|s| s.split_once('-')) else {
        return false;
    };
    hex(scope) && hex(digest)
}

pub(super) fn open_at(root: &File, name: &str, flags: i32, mode: u32) -> Result<File> {
    Ok(File::from(crate::os::open_no_symlinks(
        root.as_raw_fd(),
        Path::new(name),
        flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        mode,
        true,
    )?))
}

pub(super) fn private_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != paths::uid()
        || meta.permissions().mode() & 0o777 != 0o600
        || meta.nlink() != 1
    {
        bail!("output file must be a private, owner-held regular file without hard links");
    }
    Ok(())
}

fn private_dir(parent: &File, name: &str) -> Result<File> {
    let dir = match open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
        Ok(dir) => dir,
        Err(error) => {
            if error
                .downcast_ref::<std::io::Error>()
                .is_none_or(|e| e.kind() != std::io::ErrorKind::NotFound)
            {
                return Err(error);
            }
            let component = CString::new(name)?;
            // SAFETY: parent is held; component is one constant directory name.
            if unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
            }
            open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?
        }
    };
    let meta = dir.metadata()?;
    if !meta.is_dir() || meta.uid() != paths::uid() || meta.permissions().mode() & 0o777 != 0o700 {
        bail!("output directory must be owned by the current user with mode 0700");
    }
    Ok(dir)
}

fn remove_at(root: &File, name: &str) -> Result<()> {
    let name = CString::new(name)?;
    // SAFETY: root is held; a directory entry is unlinked, never followed.
    if unsafe { libc::unlinkat(root.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn names(root: &File) -> Result<Vec<String>> {
    // fdopendir owns its descriptor. Opening '.' independently also avoids
    // sharing a directory offset with earlier enumeration or another reader.
    let copy = open_at(root, ".", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    use std::os::fd::IntoRawFd;
    let fd = copy.into_raw_fd();
    // SAFETY: fd is owned here, transferred to DIR on success.
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        // SAFETY: fdopendir failed without taking ownership.
        unsafe { libc::close(fd) };
        return Err(std::io::Error::last_os_error().into());
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            // SAFETY: this guard uniquely owns the live DIR.
            unsafe { libc::closedir(self.0) };
        }
    }
    let guard = Directory(dir);
    let mut names = Vec::new();
    loop {
        // SAFETY: guard retains the DIR; this entry lives until the next call.
        let entry = unsafe { libc::readdir(guard.0) };
        if entry.is_null() {
            break;
        }
        // SAFETY: readdir provides a NUL-terminated d_name.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_str()?;
        if matches!(name, "." | "..") {
            continue;
        }
        if names.len() >= MAX_ENTRIES {
            bail!("output store entry limit exceeded");
        }
        names.push(name.to_owned());
    }
    Ok(names)
}

#[cfg(test)]
mod tests;
