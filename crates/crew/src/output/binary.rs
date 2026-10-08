//! Executable identity for an explicit, frozen output-view skill.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

fn digest(mut file: File) -> Result<String> {
    let before = file.metadata()?;
    if !before.is_file() || before.len() == 0 {
        bail!("output helper must be a nonempty regular executable");
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if (
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        bail!("output helper changed while its bytes were checked");
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn pinned_digest(path: &Path) -> Result<String> {
    if !path.is_absolute() {
        bail!("output helper pin must be absolute");
    }
    let file = File::from(crate::os::open_no_symlinks(
        libc::AT_FDCWD,
        path,
        libc::O_RDONLY | libc::O_CLOEXEC,
        0,
        false,
    )?);
    let meta = file.metadata()?;
    if meta.uid() != crew_core::paths::uid() || meta.mode() & 0o777 != 0o500 || meta.nlink() != 1 {
        bail!("output helper needs the daemon's private executable pin");
    }
    digest(file)
}

pub(crate) fn verify_running(expected: &str) -> Result<()> {
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("expected-helper must be a lowercase SHA-256 digest");
    }
    // Linux opens the running inode, even after its installed path is replaced.
    // On other platforms use the same running-binary source as daemon pinning.
    let path = crate::os::running_binary().context("output helper identity unavailable")?;
    if digest(File::open(path)?)? != expected {
        bail!("compact-cli helper changed; restart or reload the toolbox before using this skill");
    }
    Ok(())
}
