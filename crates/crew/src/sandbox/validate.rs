//! Mount validation (design-b3 §3.3): which host paths a sandbox may bind.
//!
//! Every bind source except the daemon-generated ones is checked against a
//! protected set S, after canonicalization so aliases and PATH symlinks are
//! judged by their target. A source may not
//! 1. equal, or be an ancestor of, a member of S or `$HOME` (that rejects
//!    `/`, `/home`, `$HOME` and the workspace itself), nor
//! 2. lie inside a member of S, except inside that member's carve-out
//!    (`~/.cargo/bin` inside `~/.cargo`).
//!
//! `$HOME` itself is not in S, because toolchains live under it.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
struct Member {
    path: PathBuf,
    carve_out: Option<PathBuf>,
}

/// The protected set S, plus `$HOME`, which only counts for rule 1.
#[derive(Clone, Debug)]
pub struct Protected {
    home: PathBuf,
    members: Vec<Member>,
}

impl Protected {
    pub fn new(home: &Path) -> Self {
        Self {
            home: resolve(home),
            members: Vec::new(),
        }
    }

    /// Protect `path`, under its own name and its resolved one.
    pub fn add(&mut self, path: &Path) {
        self.push(path, None);
    }

    /// Protect `path` except what lies inside `carve_out`.
    pub fn add_except(&mut self, path: &Path, carve_out: &Path) {
        self.push(path, Some(carve_out));
    }

    fn push(&mut self, path: &Path, carve_out: Option<&Path>) {
        let mut forms = vec![path.to_path_buf()];
        let canonical = resolve(path);
        if canonical != path {
            forms.push(canonical);
        }
        for form in forms {
            self.members.push(Member {
                carve_out: carve_out.map(|c| form.join(c.strip_prefix(path).unwrap_or(c))),
                path: form,
            });
        }
    }

    /// Rules 1 and 2 for an already canonical path.
    pub fn check(&self, candidate: &Path) -> Result<(), String> {
        if self.home.starts_with(candidate) {
            return Err(format!(
                "{} would expose all of {}",
                candidate.display(),
                self.home.display()
            ));
        }
        for member in &self.members {
            if member.path.starts_with(candidate) {
                return Err(format!(
                    "{} would expose the protected {}",
                    candidate.display(),
                    member.path.display()
                ));
            }
            let carved_out = member
                .carve_out
                .as_ref()
                .is_some_and(|c| candidate.starts_with(c));
            if candidate.starts_with(&member.path) && !carved_out {
                return Err(format!(
                    "{} lies inside the protected {}",
                    candidate.display(),
                    member.path.display()
                ));
            }
        }
        Ok(())
    }

    /// Rule 3, then rules 1 and 2: the canonical path of an acceptable
    /// source.
    pub fn validate(&self, path: &Path) -> Result<PathBuf, String> {
        if !path.is_absolute() {
            return Err(format!("{} is not an absolute path", path.display()));
        }
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.check(&canonical)?;
        Ok(canonical)
    }
}

/// The canonical form of a path that may not exist (yet): its longest
/// existing ancestor canonicalized, with the rest appended. A path created
/// later under a symlinked parent is then still recognised.
fn resolve(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut ancestor = path;
    loop {
        if let Ok(canonical) = ancestor.canonicalize() {
            return rest.iter().rev().fold(canonical, |p, name| p.join(name));
        }
        match (ancestor.parent(), ancestor.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                ancestor = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Where the operator's secrets and crew's own state live on this host:
/// everything in S that does not depend on the workspace.
pub fn host_members(home: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> Protected {
    let mut protected = Protected::new(home);
    let var = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    for rel in [
        ".claude",
        ".claude.json",
        ".codex",
        ".pi",
        ".ssh",
        ".gnupg",
        ".config",
        ".local/share/keyrings",
    ] {
        protected.add(&home.join(rel));
    }
    for name in [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "GNUPGHOME",
        "XDG_RUNTIME_DIR",
    ] {
        if let Some(path) = var(name) {
            protected.add(&path);
        }
    }
    let config = var("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    protected.add(&config.join("crew"));
    if let Some(global) = var("CREW_GLOBAL_CONFIG") {
        protected.add(&global);
    }
    let state = var("XDG_STATE_HOME").unwrap_or_else(|| home.join(".local/state"));
    protected.add(&state.join("crew"));
    // Every session's cargo cache and the store-scan cache.
    let cache = var("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"));
    protected.add(&cache.join("crew"));
    let cargo = var("CARGO_HOME").unwrap_or_else(|| home.join(".cargo"));
    protected.add_except(&cargo, &cargo.join("bin"));
    for path in ["/run", "/tmp", "/nix/var/nix/daemon-socket"] {
        protected.add(Path::new(path));
    }
    protected
}

/// Whether `path` is absolute and spelled plainly: no `.` or `..`, no
/// repeated or trailing separator. Component-wise comparisons of such paths
/// mean what they say.
pub fn is_plain_absolute(path: &Path) -> bool {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return false;
    }
    let mut rebuilt = PathBuf::from("/");
    for component in components {
        match component {
            Component::Normal(name) => rebuilt.push(name),
            _ => return false,
        }
    }
    rebuilt.as_os_str() == path.as_os_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A fixture home with a toolchain and protected neighbours, and a
    /// workspace beside it.
    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
        ws: PathBuf,
        protected: Protected,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("home/u");
        let ws = root.join("ws");
        for sub in [
            ".cargo/bin",
            ".cargo/registry",
            ".rustup",
            ".config/crew",
            ".ssh",
        ] {
            std::fs::create_dir_all(home.join(sub)).unwrap();
        }
        std::fs::create_dir_all(&ws).unwrap();
        let mut protected = Protected::new(&home);
        protected.add(&ws);
        protected.add(&home.join(".config"));
        protected.add(&home.join(".ssh"));
        protected.add(&home.join(".pi"));
        protected.add_except(&home.join(".cargo"), &home.join(".cargo/bin"));
        Fixture {
            _dir: dir,
            root,
            home,
            ws,
            protected,
        }
    }

    #[test]
    fn ancestors_of_home_or_of_a_protected_path_are_refused() {
        let f = fixture();
        for path in [
            PathBuf::from("/"),
            f.root.clone(),
            f.root.join("home"),
            f.home.clone(),
            f.ws.clone(),
            f.home.join(".cargo"),
        ] {
            let error = f.protected.validate(&path).unwrap_err();
            assert!(error.contains("would expose"), "{path:?}: {error}");
        }
        // $HOME counts for rule 1 even with nothing protected inside it.
        let bare = Protected::new(&f.home);
        assert!(bare.validate(&f.home).is_err());
        assert!(bare.validate(&f.root.join("home")).is_err());
        assert!(bare.validate(&f.home.join(".rustup")).is_ok());
    }

    #[test]
    fn paths_inside_a_protected_path_are_refused_except_its_carve_out() {
        let f = fixture();
        for path in [f.home.join(".cargo/registry"), f.home.join(".config/crew")] {
            let error = f.protected.validate(&path).unwrap_err();
            assert!(error.contains("lies inside"), "{path:?}: {error}");
        }
        assert_eq!(
            f.protected.validate(&f.home.join(".cargo/bin")).unwrap(),
            f.home.join(".cargo/bin")
        );
        assert_eq!(
            f.protected.validate(&f.home.join(".rustup")).unwrap(),
            f.home.join(".rustup")
        );
    }

    #[test]
    fn aliases_are_judged_by_their_target() {
        let f = fixture();
        let bin = f.home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // A PATH entry that is a symlink into S, and an alias of the workspace.
        symlink(f.home.join(".config/crew"), bin.join("tools")).unwrap();
        symlink(&f.ws, f.root.join("ws-alias")).unwrap();
        symlink(f.home.join(".rustup"), bin.join("rustup-alias")).unwrap();
        assert!(f.protected.validate(&bin.join("tools")).is_err());
        assert!(f.protected.validate(&f.root.join("ws-alias")).is_err());
        assert_eq!(
            f.protected.validate(&bin.join("rustup-alias")).unwrap(),
            f.home.join(".rustup")
        );
        assert!(f.protected.validate(Path::new("relative/bin")).is_err());
    }

    #[test]
    fn a_protected_path_created_later_under_a_symlinked_parent_is_still_protected() {
        let f = fixture();
        let link = f.root.join("home-link");
        symlink(&f.home, &link).unwrap();
        let mut protected = Protected::new(&link);
        protected.add(&link.join(".pi"));
        std::fs::create_dir_all(f.home.join(".pi/agent")).unwrap();
        assert!(protected.validate(&f.home.join(".pi/agent")).is_err());
        assert!(protected.validate(&f.home).is_err());
    }

    #[test]
    fn the_host_set_covers_secrets_state_and_shared_sockets() {
        let home = Path::new("/nonexistent-home/u");
        let env = |name: &str| match name {
            "XDG_CACHE_HOME" => Some("/nonexistent-cache".into()),
            "CARGO_HOME" => Some("/nonexistent-cargo".into()),
            _ => None,
        };
        let protected = host_members(home, &env);
        for path in [
            "/run/user/1000/bus",
            "/tmp/x",
            "/nix/var/nix/daemon-socket/socket",
            "/nonexistent-home/u/.claude/.credentials.json",
            "/nonexistent-home/u/.claude.json",
            "/nonexistent-home/u/.codex/auth.json",
            "/nonexistent-home/u/.ssh/id_ed25519",
            "/nonexistent-home/u/.local/state/crew/sessions",
            "/nonexistent-cache/crew/nix-store-scan",
            "/nonexistent-cargo/registry",
        ] {
            assert!(protected.check(Path::new(path)).is_err(), "{path}");
        }
        assert!(protected.check(Path::new("/nonexistent-cache")).is_err());
        assert!(protected.check(Path::new("/nonexistent-cargo/bin")).is_ok());
        assert!(protected
            .check(Path::new("/nonexistent-home/u/.local/bin"))
            .is_ok());
    }

    #[test]
    fn plain_absolute_paths() {
        for good in ["/", "/a", "/a/b c/d"] {
            assert!(is_plain_absolute(Path::new(good)), "{good}");
        }
        for bad in [
            "", "a", "a/b", "/a/", "//a", "/a//b", "/a/./b", "/a/../b", "/..",
        ] {
            assert!(!is_plain_absolute(Path::new(bad)), "{bad}");
        }
    }
}
