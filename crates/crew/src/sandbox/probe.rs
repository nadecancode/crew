//! Whether this host can run sandboxes (design-b3 §3.1 "Requirements" and
//! §3.6), and whether the Nix daemon may be offered to them.
//!
//! A successful probe is necessary, not sufficient: every launch must still
//! succeed through bubblewrap and the shim, and a launch that fails is
//! never retried unsandboxed.

use std::ffi::OsString;
use std::fmt;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::landlock;
use super::pin::{identify, pin};

/// bubblewrap 0.10.0 added `--bind-fd` and `--ro-bind-fd`.
pub const MIN_BWRAP: (u32, u32, u32) = (0, 10, 0);
/// Where the Nix daemon listens.
pub const NIX_DAEMON_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq)]
pub struct Available {
    pub bwrap: PathBuf,
    pub bwrap_version: (u32, u32, u32),
    pub landlock_abi: u32,
}

/// Why sandboxes cannot run here, and what would fix it.
#[derive(Clone, Debug, PartialEq)]
pub struct Unavailable {
    pub cause: String,
    pub fix: String,
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}; {}", self.cause, self.fix)
    }
}

impl std::error::Error for Unavailable {}

fn unavailable(cause: impl Into<String>, fix: impl Into<String>) -> Unavailable {
    Unavailable {
        cause: cause.into(),
        fix: fix.into(),
    }
}

const FIX_LANDLOCK: &str =
    "sandboxes need Linux 6.12 or newer with landlock in the kernel's lsm= list";
const FIX_BWRAP: &str =
    "install bubblewrap 0.10 or newer (starting crew from its nix-shell provides it)";
const FIX_USERNS: &str = "enable unprivileged user namespaces (sysctl \
kernel.unprivileged_userns_clone=1; on Ubuntu 24.04 also \
kernel.apparmor_restrict_unprivileged_userns=0 or an AppArmor profile for bwrap)";

/// Check this host: Landlock ABI >= 6, bubblewrap >= 0.10 on `path`, and
/// one real run of `crew --version` through bubblewrap (with a
/// descriptor bind, user and PID namespaces) and the shim.
pub fn probe(crew_bin: &Path, path: Option<&OsString>) -> Result<Available, Unavailable> {
    let landlock_abi = landlock::abi()
        .map_err(|e| unavailable(format!("Landlock is not available ({e})"), FIX_LANDLOCK))?;
    if landlock_abi < landlock::MIN_ABI {
        return Err(unavailable(
            format!("the kernel offers Landlock ABI {landlock_abi}, older than 6"),
            FIX_LANDLOCK,
        ));
    }
    let bwrap = path
        .and_then(|p| {
            std::env::split_paths(p)
                .map(|dir| dir.join("bwrap"))
                .find(|candidate| candidate.is_file())
        })
        .and_then(|found| found.canonicalize().ok())
        .ok_or_else(|| unavailable("bubblewrap (bwrap) is not on PATH", FIX_BWRAP))?;
    let out = run_with_deadline(Command::new(&bwrap).arg("--version"))
        .map_err(|e| unavailable(format!("running {}: {e}", bwrap.display()), FIX_BWRAP))?;
    let bwrap_version = parse_version(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| {
        unavailable(
            format!("{} --version printed no version", bwrap.display()),
            FIX_BWRAP,
        )
    })?;
    if bwrap_version < MIN_BWRAP {
        let (a, b, c) = bwrap_version;
        return Err(unavailable(
            format!("bubblewrap {a}.{b}.{c} is older than 0.10.0"),
            FIX_BWRAP,
        ));
    }
    run_through_sandbox(&bwrap, crew_bin, landlock_abi)?;
    Ok(Available {
        bwrap,
        bwrap_version,
        landlock_abi,
    })
}

/// `crew --version` inside bubblewrap and the shim, with `/` bound
/// read-only from a pinned descriptor.
fn run_through_sandbox(bwrap: &Path, crew_bin: &Path, abi: u32) -> Result<(), Unavailable> {
    let root = Path::new("/");
    let root_fd = identify(root)
        .map_err(|e| e.to_string())
        .and_then(|id| pin(root, id))
        .map_err(|e| unavailable(format!("opening / for the probe: {e}"), FIX_BWRAP))?;
    let min_abi = abi.min(landlock::RESOLVE_UNIX_ABI).to_string();
    let mut cmd = Command::new(bwrap);
    cmd.args([
        "--unshare-user",
        "--disable-userns",
        "--unshare-pid",
        "--unshare-net",
        "--die-with-parent",
        "--new-session",
        "--ro-bind-fd",
        &root_fd.as_raw_fd().to_string(),
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--",
    ])
    .arg(crew_bin)
    .args(["sandbox-exec", "--min-landlock-abi", &min_abi, "--"])
    .arg(crew_bin)
    .arg("--version")
    .env_clear();
    let out = run_with_deadline(&mut cmd)
        .map_err(|e| unavailable(format!("running bubblewrap: {e}"), FIX_BWRAP))?;
    drop(root_fd);
    if out.status.success() && out.stdout.starts_with(b"crew ") {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let fix = if stderr.contains("namespace") || stderr.contains("uid map") {
        FIX_USERNS
    } else if stderr.contains("crew sandbox-exec") {
        FIX_LANDLOCK
    } else {
        FIX_BWRAP
    };
    Err(unavailable(
        format!(
            "a test sandbox did not start ({:?}: {stderr})",
            out.status.code()
        ),
        fix,
    ))
}

/// The `x.y.z` in `bubblewrap x.y.z`.
fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let version = text.split_whitespace().nth(1)?;
    let mut parts = version.split('.').map(|p| p.parse::<u32>());
    let major = parts.next()?.ok()?;
    let minor = parts.next()?.ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

/// Proof that the Nix daemon socket at `socket` was asked, and does not
/// count the operator as a trusted user. Only [`nix_daemon`] makes one, and
/// the profile builder binds exactly the socket that was asked.
#[derive(Clone, Debug, PartialEq)]
pub struct UntrustedNixDaemon {
    socket: PathBuf,
}

impl UntrustedNixDaemon {
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    #[cfg(test)]
    pub(crate) fn verified_for_test(socket: &Path) -> Self {
        Self {
            socket: socket.to_path_buf(),
        }
    }
}

/// Ask the Nix daemon whether the operator is trusted. A trusted user's
/// settings (such as `post-build-hook`) make the daemon run code as root,
/// so then, or when the answer is unclear, the socket stays out.
pub fn nix_daemon(nix: &Path) -> Result<UntrustedNixDaemon, String> {
    nix_daemon_at(Command::new(nix), Path::new(NIX_DAEMON_SOCKET))
}

/// The probe through `cmd` (the `nix` program, whose environment may already
/// point it elsewhere: that is what is cleared).
fn nix_daemon_at(mut cmd: Command, socket: &Path) -> Result<UntrustedNixDaemon, String> {
    let nix = PathBuf::from(cmd.get_program());
    let store = format!("unix://{}", socket.display());
    cmd.args(["store", "info", "--json", "--store", &store])
        .env_remove("NIX_REMOTE")
        .env_remove("NIX_CONFIG");
    let out = run_with_deadline(&mut cmd).map_err(|e| format!("running {}: {e}", nix.display()))?;
    if !out.status.success() {
        return Err(format!(
            "`nix store info` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let info: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("`nix store info` printed no JSON: {e}"))?;
    let trusted = match &info["trusted"] {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) if n.as_u64() == Some(0) => false,
        serde_json::Value::Number(n) if n.as_u64() == Some(1) => true,
        _ => return Err("the Nix daemon did not say whether you are trusted".into()),
    };
    if trusted {
        return Err(
            "you are a trusted user of the Nix daemon, whose settings can run code as root, \
so its socket stays out of sandboxes"
                .into(),
        );
    }
    Ok(UntrustedNixDaemon {
        socket: socket.to_path_buf(),
    })
}

/// Run a command to completion within the probe deadline, killing it if
/// it overruns.
fn run_with_deadline(cmd: &mut Command) -> std::io::Result<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "did not finish in time",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write an executable script. A child process writes it: had this
    /// (threaded) test process held the write descriptor, a fork in another
    /// test thread could inherit it and make executing the script fail with
    /// ETXTBSY.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let status = Command::new("sh")
            .args(["-c", r#"printf '%s\n' "$2" > "$1" && chmod 755 "$1""#, "sh"])
            .arg(&path)
            .arg(format!("#!/bin/sh\n{body}"))
            .status()
            .unwrap();
        assert!(status.success(), "writing {}", path.display());
        path
    }

    #[test]
    fn bubblewrap_versions_are_parsed() {
        assert_eq!(parse_version("bubblewrap 0.11.0\n"), Some((0, 11, 0)));
        assert_eq!(parse_version("bubblewrap 0.10"), Some((0, 10, 0)));
        assert_eq!(parse_version("bubblewrap"), None);
        assert_eq!(parse_version("bubblewrap x.y"), None);
        assert!(parse_version("bubblewrap 0.9.0").unwrap() < MIN_BWRAP);
    }

    #[test]
    fn a_missing_or_old_bubblewrap_is_unavailable_with_a_fix() {
        if landlock::abi().map_or(true, |abi| abi < landlock::MIN_ABI) {
            eprintln!("skipped: the Landlock check comes first and fails here");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let crew = Path::new("/nonexistent/crew");
        let empty = OsString::from(dir.path());
        let error = probe(crew, Some(&empty)).unwrap_err();
        assert!(error.cause.contains("not on PATH"), "{error}");
        assert!(error.fix.contains("bubblewrap 0.10"), "{error}");
        script(dir.path(), "bwrap", "echo 'bubblewrap 0.9.0'");
        let error = probe(crew, Some(&empty)).unwrap_err();
        assert!(error.cause.contains("0.9.0 is older"), "{error}");
    }

    #[test]
    fn a_bubblewrap_that_cannot_make_namespaces_is_unavailable() {
        if landlock::abi().map_or(true, |abi| abi < landlock::MIN_ABI) {
            eprintln!("skipped: the Landlock check comes first and fails here");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "bwrap",
            r#"if [ "$1" = --version ]; then echo 'bubblewrap 0.11.0'; exit 0; fi
echo 'bwrap: No permissions to create new namespace' >&2; exit 1"#,
        );
        let path = OsString::from(dir.path());
        let error = probe(Path::new("/nonexistent/crew"), Some(&path)).unwrap_err();
        assert!(error.cause.contains("did not start"), "{error}");
        assert_eq!(error.fix, FIX_USERNS);
    }

    #[test]
    fn the_nix_daemon_is_offered_only_to_an_untrusted_user_at_the_asked_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon-socket/socket");
        let log = dir.path().join("log");
        let fake = |answer: &str| {
            script(
                dir.path(),
                "nix",
                &format!(
                    "echo \"$*|${{NIX_REMOTE:-unset}}|${{NIX_CONFIG:-unset}}\" > '{}'\n{answer}",
                    log.display()
                ),
            )
        };
        let nix = fake(r#"echo '{"trusted":0,"url":"daemon"}'"#);
        // Variables that would point nix at another store must not reach it.
        let mut steered = Command::new(&nix);
        steered
            .env("NIX_REMOTE", "daemon")
            .env("NIX_CONFIG", "post-build-hook = /evil");
        let ok = nix_daemon_at(steered, &socket).unwrap();
        assert_eq!(ok.socket(), socket);
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().trim(),
            format!(
                "store info --json --store unix://{}|unset|unset",
                socket.display()
            )
        );
        for answer in [
            r#"echo '{"trusted":1}'"#,
            r#"echo '{"trusted":true}'"#,
            r#"echo '{"url":"daemon"}'"#,
            "echo not-json",
            "exit 1",
        ] {
            let nix = fake(answer);
            assert!(
                nix_daemon_at(Command::new(&nix), &socket).is_err(),
                "{answer}"
            );
        }
        let nix = fake(r#"echo '{"trusted":false}'"#);
        assert!(nix_daemon_at(Command::new(&nix), &socket).is_ok());
    }
}
