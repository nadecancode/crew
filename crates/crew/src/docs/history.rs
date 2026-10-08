//! Published Git history only (including crew's Git-backed jj exports). Commands
//! see a fixed private bare view and a read-only object directory, never the
//! repository's config, worktree, helpers, credentials or network. No host fallback.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crew_core::rpc::docs::{DocFailureCode, MAX_MARKDOWN_BYTES};
use crew_core::rpc::features::{Id, Label, Timestamp};

use super::config::Relative;
use super::markdown::{hash, label};
use crate::git::launch::{Launcher, SandboxLauncher};
use crate::sandbox::pin::{identify, Identity};
use crate::sandbox::spec::{Mount, Spec};

const STDOUT_LIMIT: usize = 2 * 1024 * 1024;
const STDERR_LIMIT: usize = 16 * 1024;
pub const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Tools {
    /// All three paths are daemon-resolved executables, never request input.
    pub git: PathBuf,
    pub crew: PathBuf,
    pub bwrap: PathBuf,
    pub landlock_abi: u32,
}

pub struct Reader {
    objects: PathBuf,
    objects_identity: Identity,
    directory: File,
    git: PathBuf,
    view: tempfile::TempDir,
    launcher: SandboxLauncher,
}

#[derive(Clone)]
pub struct Record {
    pub commit: Id,
    pub recorded_at: Timestamp,
    pub author: Label,
    pub subject: Label,
    pub hash: Id,
    pub bytes: u32,
}

pub struct Records {
    pub rows: Vec<Record>,
    pub examined: usize,
}

impl Reader {
    pub fn from_store(store: &crate::vcs::Store, tools: Tools) -> Result<Self, DocFailureCode> {
        let git_dir = match store {
            crate::vcs::Store::Git { git_dir } | crate::vcs::Store::Jj { git_dir, .. } => git_dir,
        };
        Self::open(git_dir, tools)
    }

    fn open(git_dir: &Path, tools: Tools) -> Result<Self, DocFailureCode> {
        if !cfg!(target_os = "linux") {
            return Err(DocFailureCode::HistoryUnavailable);
        }
        let objects = git_dir.join("objects");
        let directory = File::from(
            crate::os::open_no_symlinks(
                libc::AT_FDCWD,
                &objects,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
                false,
            )
            .map_err(|_| unavailable())?,
        );
        let objects_identity = identify(&objects).map_err(|_| unavailable())?;
        no_alternates(&directory)?;
        // No document bodies are written here. Git's private administrative view
        // has no copied configuration or refs and is mounted read-only.
        let view = tempfile::Builder::new()
            .prefix("crew-docs-git-")
            .tempdir()
            .map_err(|_| unavailable())?;
        for name in ["refs", "objects", "home"] {
            std::fs::create_dir(view.path().join(name)).map_err(|_| unavailable())?;
        }
        std::fs::write(view.path().join("HEAD"), "ref: refs/heads/unused\n")
            .map_err(|_| unavailable())?;
        std::fs::write(
            view.path().join("config"),
            "[core]\nrepositoryformatversion=0\nbare=true\n[gc]\nauto=0\n",
        )
        .map_err(|_| unavailable())?;
        let mut mounts = vec![Mount::Tmpfs {
            path: "/tmp".into(),
        }];
        let mut bound = BTreeSet::new();
        // Public executable/library trees only. In particular /etc, user homes,
        // /run, the source repository, and daemon runtime are not broad mounts.
        for path in [
            "/usr",
            "/bin",
            "/sbin",
            "/lib",
            "/lib64",
            "/nix/store",
            "/etc/ld.so.cache",
        ] {
            let path = Path::new(path);
            let Ok(real) = path.canonicalize() else {
                continue;
            };
            if bound.insert(real.clone()) {
                bind(&mut mounts, &real)?;
            }
            if real != path {
                mounts.push(Mount::Symlink {
                    path: path.to_owned(),
                    target: real,
                });
            }
        }
        for path in [&tools.git, &tools.crew] {
            if !mounts
                .iter()
                .any(|m| matches!(m,Mount::Bind {path:p,..} if path.starts_with(p)))
            {
                bind(&mut mounts, path)?;
            }
        }
        bind(&mut mounts, view.path())?;
        mounts.push(Mount::Bind {
            path: objects.clone(),
            id: objects_identity,
            writable: false,
        });
        // info/alternates cannot be introduced between admission and Git's open.
        // Commit graphs and graft-like hints are unnecessary for this reader.
        mounts.push(Mount::Tmpfs {
            path: objects.join("info"),
        });
        let env = [
            ("PATH", "/usr/bin:/bin".to_owned()),
            (
                "HOME",
                view.path().join("home").to_string_lossy().into_owned(),
            ),
            ("LANG", "C".into()),
            ("LC_ALL", "C".into()),
            ("GIT_CONFIG_SYSTEM", "/dev/null".into()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("GIT_CONFIG_COUNT", "0".into()),
            ("GIT_NO_REPLACE_OBJECTS", "1".into()),
            ("GIT_NO_LAZY_FETCH", "1".into()),
            ("GIT_TERMINAL_PROMPT", "0".into()),
            ("GIT_OPTIONAL_LOCKS", "0".into()),
            ("GIT_ATTR_NOSYSTEM", "1".into()),
            ("GIT_ALTERNATE_OBJECT_DIRECTORIES", "".into()),
            (
                "GIT_OBJECT_DIRECTORY",
                objects.to_string_lossy().into_owned(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        let spec = Spec {
            bwrap: tools.bwrap,
            crew_bin: tools.crew,
            mounts,
            env,
            pass_env: Vec::new(),
            cwd: view.path().to_owned(),
            network: false,
            new_session: true,
            min_landlock_abi: tools.landlock_abi,
            allow_unix: Vec::new(),
            done: None,
        };
        spec.check().map_err(|_| unavailable())?;
        Ok(Self {
            objects,
            objects_identity,
            directory,
            git: tools.git,
            view,
            launcher: SandboxLauncher::new(spec, Vec::new()),
        })
    }

    pub fn records(
        &self,
        head: &Id,
        path: &Relative,
        skip: usize,
        limit: usize,
        deadline: Instant,
    ) -> Result<Records, DocFailureCode> {
        oid(head)?;
        if limit == 0 || limit > 51 || skip > 4000 {
            return Err(DocFailureCode::TooLarge);
        }
        let max = format!("--max-count={limit}");
        let skip = format!("--skip={skip}");
        let raw = self.command(
            &[
                "log",
                "--full-history",
                "--no-renames",
                "--no-show-signature",
                "--format=%H%x00%at%x00%an%x00%s",
                "-z",
                &max,
                &skip,
                head.as_str(),
                "--",
                path.as_str(),
            ],
            deadline,
            STDOUT_LIMIT,
            false,
        )?;
        let fields: Vec<_> = raw.split(|b| *b == 0).collect();
        let fields = fields.strip_suffix(&[&[][..]]).unwrap_or(&fields);
        if fields.len() % 4 != 0 {
            return Err(unavailable());
        }
        let mut result = Vec::new();
        for row in fields.chunks_exact(4) {
            let text = |b| std::str::from_utf8(b).map_err(|_| unavailable());
            let commit = Id::new(text(row[0])?).map_err(|_| unavailable())?;
            oid(&commit)?;
            let seconds: i64 = text(row[1])?.parse().map_err(|_| unavailable())?;
            let recorded_at = Timestamp::new(seconds.checked_mul(1000).ok_or_else(unavailable)?)
                .map_err(|_| unavailable())?;
            let bytes = match self.blob(&commit, path, MAX_MARKDOWN_BYTES, deadline) {
                Ok(bytes) => bytes,
                Err(DocFailureCode::ObjectMissing | DocFailureCode::ForbiddenPath) => continue,
                Err(e) => return Err(e),
            };
            result.push(Record {
                commit,
                recorded_at,
                author: label(text(row[2])?),
                subject: label(text(row[3])?),
                hash: hash(&bytes),
                bytes: bytes.len() as u32,
            });
        }
        Ok(Records {
            rows: result,
            examined: fields.len() / 4,
        })
    }

    pub fn ancestor(
        &self,
        commit: &Id,
        head: &Id,
        deadline: Instant,
    ) -> Result<bool, DocFailureCode> {
        oid(commit)?;
        oid(head)?;
        Ok(self
            .command(
                &[
                    "merge-base",
                    "--is-ancestor",
                    commit.as_str(),
                    head.as_str(),
                ],
                deadline,
                1024,
                true,
            )?
            .is_empty())
    }

    /// Raw blob bytes only: no worktree filters, textconv or symlink resolution.
    pub fn blob(
        &self,
        commit: &Id,
        path: &Relative,
        max: usize,
        deadline: Instant,
    ) -> Result<Vec<u8>, DocFailureCode> {
        oid(commit)?;
        if path.forbidden() {
            return Err(DocFailureCode::ForbiddenPath);
        }
        let raw = self.command(
            &["ls-tree", "-z", commit.as_str(), "--", path.as_str()],
            deadline,
            4096,
            false,
        )?;
        let line = raw
            .strip_suffix(&[0])
            .ok_or(DocFailureCode::ObjectMissing)?;
        let split = line
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(unavailable)?;
        if &line[split + 1..] != path.as_str().as_bytes() {
            return Err(DocFailureCode::ObjectMissing);
        }
        let header = std::str::from_utf8(&line[..split]).map_err(|_| unavailable())?;
        let parts: Vec<_> = header.split(' ').collect();
        if parts.len() != 3 || !matches!(parts[0], "100644" | "100755") || parts[1] != "blob" {
            return Err(DocFailureCode::ForbiddenPath);
        }
        let blob = Id::new(parts[2]).map_err(|_| unavailable())?;
        oid(&blob)?;
        let size = self.command(&["cat-file", "-s", blob.as_str()], deadline, 64, false)?;
        let size: usize = std::str::from_utf8(&size)
            .map_err(|_| unavailable())?
            .trim()
            .parse()
            .map_err(|_| unavailable())?;
        if size > max {
            return Err(DocFailureCode::TooLarge);
        }
        let bytes = self.command(&["cat-file", "blob", blob.as_str()], deadline, max, false)?;
        if bytes.len() != size {
            return Err(DocFailureCode::UnstableFile);
        }
        Ok(bytes)
    }

    fn command(
        &self,
        args: &[&str],
        deadline: Instant,
        max: usize,
        allow_one: bool,
    ) -> Result<Vec<u8>, DocFailureCode> {
        if Instant::now() >= deadline || identify(&self.objects).ok() != Some(self.objects_identity)
        {
            return Err(unavailable());
        }
        no_alternates(&self.directory)?;
        let mut cmd = Command::new(&self.git);
        cmd.arg("--no-pager")
            .arg("--no-replace-objects")
            .arg("--literal-pathspecs")
            .arg(format!("--git-dir={}", self.view.path().display()));
        for setting in [
            "core.hooksPath=/dev/null",
            "core.attributesFile=/dev/null",
            "core.fsmonitor=false",
            "gc.auto=0",
            "maintenance.auto=false",
            "credential.helper=",
            "protocol.allow=never",
            "log.showSignature=false",
        ] {
            cmd.arg("-c").arg(setting);
        }
        cmd.args(args);
        let command = self.launcher.launch(&cmd).map_err(|_| unavailable())?;
        bounded_command(command, deadline, max, allow_one)
    }
}

fn bind(mounts: &mut Vec<Mount>, path: &Path) -> Result<(), DocFailureCode> {
    mounts.push(Mount::Bind {
        path: path.to_owned(),
        id: identify(path).map_err(|_| unavailable())?,
        writable: false,
    });
    Ok(())
}
fn oid(value: &Id) -> Result<(), DocFailureCode> {
    if value.as_str().len() == 40 && value.as_str().bytes().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(unavailable())
    }
}
fn unavailable() -> DocFailureCode {
    DocFailureCode::HistoryUnavailable
}

fn no_alternates(directory: &File) -> Result<(), DocFailureCode> {
    for name in ["info/alternates", "info/http-alternates"] {
        let fd = match crate::os::open_no_symlinks(
            directory.as_raw_fd(),
            Path::new(name),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
            true,
        ) {
            Ok(fd) => fd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(unavailable()),
        };
        let mut file = File::from(fd);
        if !file.metadata().map_err(|_| unavailable())?.is_file() {
            return Err(unavailable());
        }
        let mut byte = [0];
        if file.read(&mut byte).map_err(|_| unavailable())? != 0 {
            return Err(unavailable());
        }
    }
    Ok(())
}

fn bounded_command(
    mut command: Command,
    deadline: Instant,
    max: usize,
    allow_one: bool,
) -> Result<Vec<u8>, DocFailureCode> {
    use std::os::fd::AsRawFd;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().map_err(|_| unavailable())?;
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
    let mut stdout = child.stdout.take().ok_or_else(unavailable)?;
    let mut stderr = child.stderr.take().ok_or_else(unavailable)?;
    let mut child = Kill(child);
    for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
        if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
            return Err(unavailable());
        }
    }
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_done = false;
    let mut err_done = false;
    loop {
        if Instant::now() >= deadline {
            return Err(unavailable());
        }
        drain(&mut stdout, &mut out, max, &mut out_done)?;
        drain(&mut stderr, &mut err, STDERR_LIMIT, &mut err_done)?;
        if let Some(status) = child.0.try_wait().map_err(|_| unavailable())? {
            if out_done && err_done {
                return if status.success() {
                    Ok(out)
                } else if allow_one && status.code() == Some(1) {
                    Ok(vec![1])
                } else {
                    Err(unavailable())
                };
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn drain(
    reader: &mut impl Read,
    output: &mut Vec<u8>,
    max: usize,
    done: &mut bool,
) -> Result<(), DocFailureCode> {
    if *done {
        return Ok(());
    }
    let mut bytes = [0; 8192];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => {
                *done = true;
                return Ok(());
            }
            Ok(n) => {
                if n > max.saturating_sub(output.len()) {
                    return Err(DocFailureCode::TooLarge);
                }
                output.extend_from_slice(&bytes[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(unavailable()),
        }
    }
}
