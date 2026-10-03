//! What one sandboxed launch gets: the mounts, the environment and the
//! namespaces, in the form the daemon hands to `crew sandbox-run`.
//!
//! The spec travels as a command-line argument, so it holds paths,
//! identities and non-secret values only. Secret values (endpoint keys)
//! reach the sandbox by name through `pass_env`: `crew sandbox-run` takes
//! them from its own environment, which the caller builds with
//! [`launcher_env`].

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::landlock;
use super::pin::Identity;
use super::validate::is_plain_absolute;

/// The largest spec `crew sandbox-run` accepts, in bytes.
pub const MAX_SPEC_LEN: usize = 64 * 1024;

/// Inherited from the daemon when present: locale, terminal, user name and
/// the CA bundle. `LC_*` is inherited too.
const INHERITED: &[&str] = &[
    "LANG",
    "TZ",
    "TERM",
    "COLORTERM",
    "USER",
    "LOGNAME",
    "SSL_CERT_FILE",
    "NIX_SSL_CERT_FILE",
];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// The bubblewrap binary the probe found.
    pub bwrap: PathBuf,
    /// The crew binary; bound read-only at this path, it runs the shim.
    pub crew_bin: PathBuf,
    /// In order. A mount may lie inside an earlier one, never above it.
    pub mounts: Vec<Mount>,
    /// Variables crew sets, with their values. Never secret.
    pub env: Vec<(String, String)>,
    /// Variables passed by name from `crew sandbox-run`'s environment.
    pub pass_env: Vec<String>,
    pub cwd: PathBuf,
    /// Keep the host network (agents) or get none (helpers).
    pub network: bool,
    /// A new terminal session (helpers, which have no PTY).
    pub new_session: bool,
    pub min_landlock_abi: u32,
    /// Directories inside the sandbox beneath which outside pathname
    /// sockets may be reached (Landlock ABI 9 and newer).
    pub allow_unix: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mount {
    /// The host object at `path`, pinned to `id`, bound at the same path.
    Bind {
        path: PathBuf,
        id: Identity,
        writable: bool,
    },
    /// An empty tmpfs.
    Tmpfs {
        path: PathBuf,
    },
    /// An empty directory only the user can enter.
    Dir {
        path: PathBuf,
    },
    Symlink {
        path: PathBuf,
        target: PathBuf,
    },
    /// An empty read-only file over a socket found in a bound tree.
    Mask {
        path: PathBuf,
    },
}

impl Mount {
    pub fn path(&self) -> &Path {
        match self {
            Mount::Bind { path, .. }
            | Mount::Tmpfs { path }
            | Mount::Dir { path }
            | Mount::Symlink { path, .. }
            | Mount::Mask { path } => path,
        }
    }
}

impl Spec {
    /// Everything `crew sandbox-run` relies on before it touches a path.
    pub fn check(&self) -> Result<(), String> {
        for (what, path) in [
            ("bwrap", &self.bwrap),
            ("crew_bin", &self.crew_bin),
            ("cwd", &self.cwd),
        ] {
            plain(what, path)?;
        }
        for path in &self.allow_unix {
            plain("allow_unix", path)?;
        }
        for (i, mount) in self.mounts.iter().enumerate() {
            let path = mount.path();
            plain("mount", path)?;
            if path == Path::new("/") {
                return Err("a mount over / is not allowed".into());
            }
            if let Mount::Symlink { target, .. } = mount {
                if target.as_os_str().is_empty() {
                    return Err(format!("symlink {} has no target", path.display()));
                }
            }
            if let Some(hidden) = self.mounts[..i].iter().find(|m| m.path().starts_with(path)) {
                return Err(format!(
                    "mount {} would hide the earlier {}",
                    path.display(),
                    hidden.path().display()
                ));
            }
        }
        let mut names = HashSet::new();
        for name in self.env.iter().map(|(n, _)| n).chain(&self.pass_env) {
            check_name(name)?;
            if !names.insert(name.as_str()) {
                return Err(format!("variable {name} is listed twice"));
            }
        }
        if self.min_landlock_abi < landlock::MIN_ABI {
            return Err(format!(
                "a minimum Landlock ABI of {} is below the floor of {}",
                self.min_landlock_abi,
                landlock::MIN_ABI
            ));
        }
        Ok(())
    }

    /// Whether `name` gets through the allow-list. A name `check_name`
    /// refuses never does, whatever the spec lists.
    fn passes(&self, name: &str) -> bool {
        check_name(name).is_ok() && (is_inherited(name) || self.pass_env.iter().any(|n| n == name))
    }

    /// The environment of the sandboxed program: what passes the
    /// allow-list from `own` (sandbox-run's environment), then crew's
    /// values.
    pub fn sandbox_env(
        &self,
        own: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> = own
            .into_iter()
            .filter(|(name, _)| name.to_str().is_some_and(|n| self.passes(n)))
            .filter(|(name, _)| !self.env.iter().any(|(n, _)| name == n.as_str()))
            .collect();
        env.extend(
            self.env
                .iter()
                .map(|(n, v)| (OsString::from(n), OsString::from(v))),
        );
        env
    }
}

/// The exact environment to start `crew sandbox-run` with (the caller
/// clears everything else): the inherited names from `host` (the daemon's
/// environment), and those plus the spec's `pass_env` names from `launch`
/// (the runtime's variables, e.g. endpoint keys), `launch` winning. Nothing
/// that steers the host's loader or libc gets through: such names are
/// filtered here even from a spec that lists them (and [`Spec::check`]
/// refuses that spec).
pub fn launcher_env(
    spec: &Spec,
    host: impl IntoIterator<Item = (OsString, OsString)>,
    launch: &[(String, String)],
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = host
        .into_iter()
        .filter(|(name, _)| name.to_str().is_some_and(is_inherited))
        .collect();
    for (name, value) in launch.iter().filter(|(n, _)| spec.passes(n)) {
        env.retain(|(n, _)| n != name.as_str());
        env.push((name.into(), value.into()));
    }
    env
}

fn is_inherited(name: &str) -> bool {
    INHERITED.contains(&name) || name.starts_with("LC_")
}

/// A variable a launch may set or pass. Names that steer the dynamic
/// loader, libc's allocator, iconv, locale files or the resolver are
/// refused: `crew sandbox-run` and bubblewrap run on the host with this
/// environment before any confinement exists.
pub fn check_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let identifier = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !identifier {
        return Err(format!("{name:?} is not a variable name"));
    }
    const PREFIXES: &[&str] = &["LD_", "GLIBC_", "MALLOC_"];
    const NAMES: &[&str] = &[
        "GCONV_PATH",
        "LOCPATH",
        "NLSPATH",
        "HOSTALIASES",
        "LOCALDOMAIN",
        "RES_OPTIONS",
        "RESOLV_HOST_CONF",
    ];
    if PREFIXES.iter().any(|p| name.starts_with(p)) || NAMES.contains(&name) {
        return Err(format!(
            "{name} cannot be passed into a sandbox: it would steer the host processes \
that set the sandbox up"
        ));
    }
    Ok(())
}

fn plain(what: &str, path: &Path) -> Result<(), String> {
    if is_plain_absolute(path) {
        Ok(())
    } else {
        Err(format!(
            "{what} path {:?} is not a plain absolute path",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::pin::Kind;

    fn id() -> Identity {
        Identity {
            dev: 1,
            ino: 2,
            kind: Kind::Dir,
        }
    }

    fn spec() -> Spec {
        Spec {
            bwrap: "/usr/bin/bwrap".into(),
            crew_bin: "/opt/crew/bin/crew".into(),
            mounts: vec![
                Mount::Tmpfs {
                    path: "/home/u".into(),
                },
                Mount::Bind {
                    path: "/home/u/.rustup".into(),
                    id: id(),
                    writable: false,
                },
                Mount::Mask {
                    path: "/home/u/.rustup/sock".into(),
                },
            ],
            env: vec![("HOME".into(), "/ws/.crew/agents/a/rw/home".into())],
            pass_env: vec![
                "ANTHROPIC_API_KEY".into(),
                "CREW_IDENTITY".into(),
                "OPENAI_API_KEY".into(),
            ],
            cwd: "/ws/.crew/worktrees/a".into(),
            network: true,
            new_session: false,
            min_landlock_abi: 6,
            allow_unix: vec!["/run/user/1000/crew".into()],
        }
    }

    fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(n, v)| (OsString::from(n), OsString::from(v)))
            .collect()
    }

    #[test]
    fn a_well_formed_spec_round_trips_and_passes() {
        let spec = spec();
        spec.check().unwrap();
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<Spec>(&json).unwrap(), spec);
        assert!(serde_json::from_str::<Spec>(&json.replacen('{', "{\"extra\":1,", 1)).is_err());
    }

    #[test]
    fn malformed_specs_are_refused() {
        type Breakage = fn(&mut Spec);
        let cases: [(&str, Breakage); 9] = [
            ("relative", |s| s.cwd = "ws".into()),
            ("dotdot", |s| s.crew_bin = "/opt/../bin/crew".into()),
            ("root mount", |s| {
                s.mounts.push(Mount::Tmpfs { path: "/".into() })
            }),
            ("hides an earlier mount", |s| {
                s.mounts.push(Mount::Tmpfs {
                    path: "/home".into(),
                })
            }),
            ("bad name", |s| s.pass_env.push("A-B".into())),
            ("loader variable", |s| s.pass_env.push("LD_PRELOAD".into())),
            ("twice", |s| s.pass_env.push("HOME".into())),
            ("abi", |s| s.min_landlock_abi = 5),
            ("allow_unix", |s| s.allow_unix.push("run/crew".into())),
        ];
        for (label, break_it) in cases {
            let mut spec = spec();
            break_it(&mut spec);
            assert!(spec.check().is_err(), "{label} was accepted");
        }
    }

    #[test]
    fn loader_and_libc_controls_are_refused_by_name() {
        for name in [
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "GLIBC_TUNABLES",
            "MALLOC_CHECK_",
            "GCONV_PATH",
            "LOCPATH",
            "HOSTALIASES",
            "RES_OPTIONS",
            "",
            "1X",
            "A=B",
        ] {
            assert!(check_name(name).is_err(), "{name}");
        }
        for name in ["ANTHROPIC_API_KEY", "CREW_IDENTITY", "LC_ALL", "_X"] {
            check_name(name).unwrap();
        }
    }

    #[test]
    fn the_launcher_gets_only_allow_listed_names_with_the_launch_values() {
        let spec = spec();
        let host = os(&[
            ("PATH", "/usr/bin"),
            ("LD_PRELOAD", "/tmp/evil.so"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
            ("SSH_AUTH_SOCK", "/run/user/1000/ssh"),
            ("LANG", "C.UTF-8"),
            ("LC_TIME", "en_GB.UTF-8"),
            ("TERM", "dumb"),
            ("ANTHROPIC_API_KEY", "host-key"),
            ("OPENAI_API_KEY", "only-the-launch-passes-keys"),
        ]);
        let launch = [
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-secret".to_string()),
            (
                "CREW_IDENTITY".to_string(),
                "/ws/.crew/agents/a/identity.json".to_string(),
            ),
            ("LD_LIBRARY_PATH".to_string(), "/tmp".to_string()),
        ];
        let mut env = launcher_env(&spec, host, &launch);
        env.sort();
        assert_eq!(
            env,
            os(&[
                ("ANTHROPIC_API_KEY", "sk-secret"),
                ("CREW_IDENTITY", "/ws/.crew/agents/a/identity.json"),
                ("LANG", "C.UTF-8"),
                ("LC_TIME", "en_GB.UTF-8"),
                ("TERM", "xterm-256color"),
            ])
        );
    }

    #[test]
    fn loader_names_never_pass_even_when_a_spec_lists_them() {
        let mut spec = spec();
        spec.pass_env.push("LD_PRELOAD".into());
        assert!(spec.check().is_err());
        let launch = [("LD_PRELOAD".to_string(), "/tmp/evil.so".to_string())];
        let env = launcher_env(&spec, os(&[("LD_PRELOAD", "/tmp/host.so")]), &launch);
        assert!(env.iter().all(|(n, _)| n != "LD_PRELOAD"), "{env:?}");
        let env = spec.sandbox_env(os(&[("LD_PRELOAD", "/tmp/evil.so")]));
        assert!(env.iter().all(|(n, _)| n != "LD_PRELOAD"), "{env:?}");
    }

    #[test]
    fn the_sandbox_env_is_the_allow_list_plus_crews_values() {
        let spec = spec();
        let own = os(&[
            ("HOME", "/home/u"),
            ("PATH", "/usr/bin"),
            ("ANTHROPIC_API_KEY", "sk-secret"),
            ("LANG", "C.UTF-8"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        let mut env = spec.sandbox_env(own);
        env.sort();
        assert_eq!(
            env,
            os(&[
                ("ANTHROPIC_API_KEY", "sk-secret"),
                ("HOME", "/ws/.crew/agents/a/rw/home"),
                ("LANG", "C.UTF-8"),
            ])
        );
    }

    #[test]
    fn the_spec_never_carries_passed_values() {
        let json = serde_json::to_string(&spec()).unwrap();
        assert!(json.contains("ANTHROPIC_API_KEY"));
        assert!(!json.contains("sk-"));
    }
}
