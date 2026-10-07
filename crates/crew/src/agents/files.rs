//! Finding the files an agent's CLI keeps (transcripts, rollouts), and
//! opening them safely.
//!
//! Without a sandbox the CLIs use the operator's own directories and the
//! files are found there. In a sandbox they live under `<A>/rw`, which the
//! agent can write: the daemon opens them through the agent's root
//! descriptor, beneath `<A>/rw` only, without following a link, and treats
//! what it reads as untrusted. A path a hook reports for a transcript is
//! only ever a way to *name* a file under that tree, never a path to open:
//! `<A>/identity.json` or `/etc/passwd` named there is not read.

use std::fs::File;
#[cfg(test)]
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};

use chrono::{Local, TimeZone};
use crew_core::paths;

/// How the daemon reaches an agent's CLI files.
pub enum AgentFiles {
    /// Not sandboxed: the operator's own CLI directories.
    Operator,
    /// Sandboxed: under `<A>/rw`, through `<A>`'s descriptor.
    Private { agent_dir: PathBuf, root: File },
}

/// A regular file opened for reading, and the absolute path it is known by.
pub struct Opened {
    pub file: File,
    pub path: PathBuf,
}

/// The sub-path of the private home Claude Code and Codex use.
const CLAUDE_PROJECTS: &str = "rw/home/claude/projects";
const CODEX_SESSIONS: &str = "rw/home/codex/sessions";

impl AgentFiles {
    /// The files of an agent whose directories exist: opened beneath the state
    /// directory (`state_root`) without making anything. A reader never creates
    /// what an agent that is running may be writing.
    pub fn open_existing(
        state_root: std::os::fd::BorrowedFd<'_>,
        agent_dir: PathBuf,
        agent_id: &str,
    ) -> anyhow::Result<Self> {
        let rel = Path::new("agents").join(agent_id);
        let root = paths::open_dir_beneath(state_root, &rel)?;
        Ok(Self::private(agent_dir, root))
    }

    /// The size of the file the CLI reported at `path`, opened through these
    /// files (0 when there is none, or it is refused). A compaction is looked
    /// for after this offset.
    pub fn reported_len(&self, path: Option<&Path>) -> u64 {
        path.and_then(|p| self.open_reported(p))
            .and_then(|opened| opened.file.metadata().ok())
            .map_or(0, |meta| meta.len())
    }

    pub fn private(agent_dir: PathBuf, root: std::os::fd::OwnedFd) -> Self {
        Self::Private {
            agent_dir,
            root: File::from(root),
        }
    }

    /// A file the CLI or its hooks named by absolute path.
    pub fn open_reported(&self, path: &Path) -> Option<Opened> {
        match self {
            Self::Operator => {
                let file = File::open(path).ok()?;
                file.metadata().ok()?.is_file().then(|| Opened {
                    file,
                    path: path.to_path_buf(),
                })
            }
            Self::Private { agent_dir, root } => {
                let rel = path.strip_prefix(agent_dir).ok()?;
                // Only what the CLI writes: never the daemon's own files in <A>.
                if !rel.starts_with("rw") {
                    return None;
                }
                let file = paths::open_beneath(root, rel).ok()?;
                Some(Opened {
                    file,
                    path: path.to_path_buf(),
                })
            }
        }
    }

    /// Claude Code's transcript of `session`: `projects/<project>/<session>.jsonl`.
    pub fn claude_transcript(&self, session: &str) -> Option<Opened> {
        match self {
            Self::Operator => {
                let path = crate::limits::claude_transcript_path(session)?;
                self.open_reported(&path)
            }
            Self::Private { agent_dir, root } => {
                let name = session_file_name(session)?;
                let projects = paths::list_dir_beneath(root, Path::new(CLAUDE_PROJECTS)).ok()?;
                list(&projects).into_iter().find_map(|project| {
                    let rel = Path::new(CLAUDE_PROJECTS).join(&project).join(&name);
                    let file = paths::open_beneath(root, &rel).ok()?;
                    Some(Opened {
                        file,
                        path: agent_dir.join(rel),
                    })
                })
            }
        }
    }

    /// Codex's rollout of `thread`, started around `started_ms`:
    /// `sessions/YYYY/MM/DD/rollout-<time>-<thread>.jsonl`.
    pub fn codex_rollout(&self, thread: &str, started_ms: i64) -> Option<Opened> {
        match self {
            Self::Operator => {
                let path = crate::limits::codex_rollout_path(thread, started_ms)?;
                self.open_reported(&path)
            }
            Self::Private { agent_dir, root } => {
                session_file_name(thread)?;
                let suffix = format!("-{thread}.jsonl");
                [0i64, -1, 1].iter().find_map(|day| {
                    let date = Local
                        .timestamp_millis_opt(started_ms + day * 86_400_000)
                        .single()?;
                    let dir = Path::new(CODEX_SESSIONS).join(date.format("%Y/%m/%d").to_string());
                    let listing = paths::list_dir_beneath(root, &dir).ok()?;
                    list(&listing).into_iter().find_map(|name| {
                        if !(name.starts_with("rollout-") && name.ends_with(&suffix)) {
                            return None;
                        }
                        let rel = dir.join(&name);
                        let file = paths::open_beneath(root, &rel).ok()?;
                        Some(Opened {
                            file,
                            path: agent_dir.join(rel),
                        })
                    })
                })
            }
        }
    }
}

/// `<session>.jsonl` for an id that is a plain name, never a path.
fn session_file_name(session: &str) -> Option<String> {
    (!session.is_empty()
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| format!("{session}.jsonl"))
}

/// The names in a directory held open by descriptor (enumerated through
/// that descriptor, whatever its path now names).
fn list(dir: &File) -> Vec<String> {
    use std::os::fd::AsFd;
    crate::os::list_dir(dir.as_fd()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct Fixture {
        dir: tempfile::TempDir,
        agent: PathBuf,
        files: AgentFiles,
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().canonicalize().unwrap().join("ws/.crew/agents/a");
        std::fs::create_dir_all(agent.join("rw/home")).unwrap();
        write(&agent.join("identity.json"), r#"{"token":"secret"}"#);
        let root = std::fs::File::open(&agent).unwrap();
        Fixture {
            dir,
            files: AgentFiles::private(agent.clone(), root.into()),
            agent,
        }
    }

    fn text(opened: Option<Opened>) -> Option<String> {
        use std::io::Read;
        let mut s = String::new();
        opened?.file.read_to_string(&mut s).ok()?;
        Some(s)
    }

    #[test]
    fn a_private_agents_claude_transcript_and_codex_rollout_are_found_under_its_home() {
        let f = fixture();
        write(
            &f.agent.join("rw/home/claude/projects/-w/abc-1.jsonl"),
            "claude line",
        );
        let thread = "0199-aa";
        let started = chrono::Local
            .with_ymd_and_hms(2026, 10, 3, 12, 0, 0)
            .unwrap()
            .timestamp_millis();
        write(
            &f.agent.join(format!(
                "rw/home/codex/sessions/2026/10/03/rollout-2026-10-03T12-00-00-{thread}.jsonl"
            )),
            "codex line",
        );
        let found = f.files.claude_transcript("abc-1").unwrap();
        assert_eq!(
            found.path,
            f.agent.join("rw/home/claude/projects/-w/abc-1.jsonl")
        );
        assert_eq!(text(Some(found)).as_deref(), Some("claude line"));
        assert_eq!(
            text(f.files.codex_rollout(thread, started)).as_deref(),
            Some("codex line")
        );
        assert!(f.files.claude_transcript("nope").is_none());
        assert!(f.files.claude_transcript("../../identity").is_none());
    }

    #[test]
    fn a_reported_path_outside_the_agents_writable_tree_is_not_opened() {
        let f = fixture();
        write(
            &f.agent.join("rw/home/claude/projects/-w/s.jsonl"),
            "inside",
        );
        let inside = f.agent.join("rw/home/claude/projects/-w/s.jsonl");
        assert_eq!(
            text(f.files.open_reported(&inside)).as_deref(),
            Some("inside")
        );
        // The daemon's own files, another tree, a traversal.
        for outside in [
            f.agent.join("identity.json"),
            PathBuf::from("/etc/passwd"),
            f.agent.join("rw/../identity.json"),
            f.dir.path().join("elsewhere"),
        ] {
            write(&f.dir.path().join("elsewhere"), "x");
            assert!(
                f.files.open_reported(&outside).is_none(),
                "{}",
                outside.display()
            );
        }
    }

    #[test]
    fn the_size_of_a_rollout_is_measured_on_the_file_opened_through_the_agents_files() {
        let f = fixture();
        let real = f.agent.join("rw/home/codex/sessions/rollout.jsonl");
        write(&real, "0123456789");
        assert_eq!(f.files.reported_len(Some(&real)), 10);
        // A link to a bigger file elsewhere: the offset is 0, never that size.
        let outside = f.dir.path().join("big");
        write(&outside, &"x".repeat(500));
        let linked = f.agent.join("rw/home/codex/sessions/linked.jsonl");
        symlink(&outside, &linked).unwrap();
        assert_eq!(f.files.reported_len(Some(&linked)), 0);
        assert_eq!(
            f.files.reported_len(Some(&f.agent.join("identity.json"))),
            0
        );
        assert_eq!(f.files.reported_len(None), 0);
    }

    #[test]
    fn opening_an_agents_files_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let state = base.join("ws/.crew");
        std::fs::create_dir_all(state.join("agents")).unwrap();
        let root = std::fs::File::open(&state).unwrap();
        // No directory for the agent yet: a read is an error, not a mkdir.
        assert!(AgentFiles::open_existing(root.as_fd(), state.join("agents/a"), "a").is_err());
        assert!(!state.join("agents/a").exists());
        // With the layout in place, files are reachable and nothing is added.
        std::fs::create_dir_all(state.join("agents/a/rw")).unwrap();
        let files = AgentFiles::open_existing(root.as_fd(), state.join("agents/a"), "a").unwrap();
        assert!(files.claude_transcript("s").is_none());
        assert!(!state.join("agents/a/rw/home").exists());
    }

    #[test]
    fn links_the_agent_planted_are_not_followed() {
        let f = fixture();
        let secret = f.dir.path().join("secret.txt");
        write(&secret, "operator secret");
        // The transcript itself is a link.
        write(&f.agent.join("rw/home/claude/projects/-w/other.jsonl"), "x");
        symlink(
            &secret,
            f.agent.join("rw/home/claude/projects/-w/linked.jsonl"),
        )
        .unwrap();
        assert!(f.files.claude_transcript("linked").is_none());
        // A project directory is a link to a directory holding the session.
        let outside = f.dir.path().join("outside-project");
        write(&outside.join("hidden.jsonl"), "operator secret");
        symlink(&outside, f.agent.join("rw/home/claude/projects/-evil")).unwrap();
        assert!(f.files.claude_transcript("hidden").is_none());
        // The projects directory itself.
        let g = fixture();
        let real = g.dir.path().join("real-projects");
        write(&real.join("-p/s.jsonl"), "operator secret");
        std::fs::create_dir_all(g.agent.join("rw/home/claude")).unwrap();
        symlink(&real, g.agent.join("rw/home/claude/projects")).unwrap();
        assert!(g.files.claude_transcript("s").is_none());
        // A reported path through a link.
        assert!(text(
            f.files
                .open_reported(&f.agent.join("rw/home/claude/projects/-w/linked.jsonl"))
        )
        .is_none());
    }
}
