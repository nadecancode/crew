//! The workspace's memory: what earlier sessions and teammates learned
//! about the project, kept across sessions so that a terse objective or
//! card is read with the project's history in mind.
//!
//! One memory is one markdown file under the memory directory
//! (`.crew/memory/` unless the team says otherwise), named after the
//! memory, with its particulars in a front matter block and the note
//! below: a decision and why, a convention, the operator's preference, a
//! fact about where something stands, a pitfall, or a session's outcome.
//! The files are the store — readable and editable by hand, like notes —
//! and the daemon reads them afresh for every call, so an edit on disk
//! counts at once. Every member's briefing carries the index (name and
//! description); the crew tools read one in full, search, add and forget.
//!
//! Limits hold the memory to what a briefing can carry: a name of at most
//! 64 characters in `[a-z0-9_-]`, a one-line description, a body of at most
//! 4000 characters, at most 500 memories; and a note that looks like a
//! secret (a key, a token, a private key block) is refused.
//!
//! The directory is also Claude Code's own memory for every member of the
//! team (its `autoMemoryDirectory`), so what a Claude member saves on its
//! own lands here in Claude's format (a `type` of user, feedback, project
//! or reference; `modified`), which is read as a memory too, and crew keeps
//! the `MEMORY.md` index Claude Code loads at the start of a session, with
//! every memory in it whoever wrote it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use crew_core::util::{now_ms, one_line, truncate_chars};
use crew_core::{
    Memory, MemoryKind, MAX_MEMORIES, MAX_MEMORY_BODY_CHARS, MAX_MEMORY_DESCRIPTION_CHARS,
    MAX_MEMORY_NAME_CHARS,
};
use serde::{Deserialize, Serialize};

/// The directory under the state dir, when the team names none.
pub const DEFAULT_DIR: &str = "memory";
/// The index Claude Code reads at the start of a session: one line per
/// memory, kept by crew.
pub const INDEX_FILE: &str = "MEMORY.md";
/// Most lines the index carries (Claude Code reads its first 200).
const INDEX_LINES: usize = 200;
/// What a search returns when the caller does not say.
pub const SEARCH_LIMIT: usize = 10;
/// The most a search returns.
pub const MAX_SEARCH_LIMIT: usize = 50;

/// The memory directory, read afresh at every call.
#[derive(Clone, Debug)]
pub struct MemoryStore {
    dir: PathBuf,
}

/// The front matter of a memory file.
#[derive(Serialize, Deserialize, Default)]
struct Front {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    kind: Option<MemoryKind>,
    #[serde(default)]
    by: Option<String>,
    /// RFC 3339 when written by crew; a number of milliseconds is read too.
    #[serde(default)]
    at: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    /// Claude Code's own fields, at the top or under `metadata`: its
    /// `type` (user, feedback, project, reference) and `modified`.
    #[serde(default, rename = "type", skip_serializing)]
    type_: Option<String>,
    #[serde(default, skip_serializing)]
    modified: Option<String>,
    #[serde(default, skip_serializing)]
    metadata: Option<serde_yaml::Value>,
}

/// Who a file by Claude Code's own hand is from.
const CLAUDE: &str = "claude";

/// Claude Code's memory types as kinds: what the user is and wants
/// (`user`), how to work for them (`feedback`), the project and pointers
/// to things (`project`, `reference`).
fn kind_of_claude_type(kind: &str) -> MemoryKind {
    match kind {
        "user" => MemoryKind::Preference,
        "feedback" => MemoryKind::Convention,
        _ => MemoryKind::Fact,
    }
}

impl MemoryStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_of(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.md"))
    }

    /// Every memory, newest first.
    pub fn list(&self) -> Result<Vec<Memory>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.dir.display())),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            // Only a memory file by a valid name; the rest (a README, an
            // editor's leftovers) is not the memory's.
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".md"))
            else {
                continue;
            };
            if validate_name(name).is_err() || !entry.file_type()?.is_file() {
                continue;
            }
            match self.read(name, &path) {
                Ok(memory) => out.push(memory),
                Err(e) => tracing::warn!("memory {}: {e:#}; skipped", path.display()),
            }
        }
        out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    pub fn count(&self) -> Result<usize> {
        Ok(self.list()?.len())
    }

    pub fn get(&self, name: &str) -> Result<Option<Memory>> {
        validate_name(name).map_err(|e| anyhow!(e))?;
        let path = self.path_of(name);
        if !path.is_file() {
            return Ok(None);
        }
        self.read(name, &path).map(Some)
    }

    fn read(&self, name: &str, path: &Path) -> Result<Memory> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let modified = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .map(|t| DateTime::<Utc>::from(t).timestamp_millis());
        parse(name, &text, modified)
    }

    /// Write a memory; says whether one of that name was replaced. The
    /// memory is checked first (`check`), and the write is atomic.
    pub fn put(&self, memory: &Memory) -> Result<bool> {
        check(memory).map_err(|e| anyhow!(e))?;
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let path = self.path_of(&memory.name);
        let existed = path.is_file();
        if !existed && self.count()? >= MAX_MEMORIES {
            bail!("the memory is full ({MAX_MEMORIES} memories): forget one before adding another");
        }
        let tmp = self.dir.join(format!(".{}.md.tmp", memory.name));
        fs::write(&tmp, render(memory)).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(existed)
    }

    /// Drop a memory; says whether there was one.
    pub fn forget(&self, name: &str) -> Result<bool> {
        validate_name(name).map_err(|e| anyhow!(e))?;
        let path = self.path_of(name);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }

    /// The memories matching the words of `query`, best first: a word in
    /// the name counts most, in the description next, in the body least.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Memory>> {
        let words: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .filter(|w| w.chars().count() >= 2)
            .map(|w| w.to_lowercase())
            .collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut scored: Vec<(usize, Memory)> = self
            .list()?
            .into_iter()
            .filter_map(|m| {
                let score = score(&m, &words);
                (score > 0).then_some((score, m))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.at.cmp(&a.1.at)));
        Ok(scored
            .into_iter()
            .take(limit.clamp(1, MAX_SEARCH_LIMIT))
            .map(|(_, m)| m)
            .collect())
    }

    /// A name for a new memory made from `base`: `base`, or `base-2`,
    /// `base-3`, … when taken.
    pub fn free_name(&self, base: &str) -> Result<String> {
        let base = truncate_chars(base, MAX_MEMORY_NAME_CHARS - 4);
        let base = base.trim_end_matches('-').to_string();
        if self.get(&base)?.is_none() {
            return Ok(base);
        }
        for n in 2.. {
            let name = format!("{base}-{n}");
            if self.get(&name)?.is_none() {
                return Ok(name);
            }
        }
        unreachable!()
    }

    /// Write the `MEMORY.md` index from the files: one line per memory,
    /// newest first, as Claude Code's own index is written (`- [name](file)
    /// — gist`). Says whether it changed. Claude Code adds a line for a
    /// memory it saves; the next write lists that memory from its file.
    pub fn write_index(&self) -> Result<bool> {
        let all = self.list()?;
        let mut text = String::new();
        for m in all.iter().take(INDEX_LINES) {
            text.push_str(&format!(
                "- [{}]({}.md) — {} ({}, {}, {})\n",
                m.name,
                m.name,
                m.description,
                m.kind,
                m.by,
                date_of(m.at)
            ));
        }
        let path = self.dir.join(INDEX_FILE);
        if fs::read_to_string(&path).is_ok_and(|current| current == text) {
            return Ok(false);
        }
        if all.is_empty() && !path.exists() {
            return Ok(false);
        }
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let tmp = self.dir.join(".MEMORY.md.tmp");
        fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(true)
    }
}

fn score(memory: &Memory, words: &[String]) -> usize {
    let name = memory.name.to_lowercase();
    let description = memory.description.to_lowercase();
    let body = memory.body.to_lowercase();
    words
        .iter()
        .map(|w| {
            let mut s = 0;
            if name.contains(w.as_str()) {
                s += 4;
            }
            if description.contains(w.as_str()) {
                s += 3;
            }
            s += body.matches(w.as_str()).count().min(5);
            s
        })
        .sum()
}

/// A memory's name: `[a-z0-9]`, `-` and `_` (Claude Code's own names use
/// it), starting and ending with a letter or digit, at most
/// [`MAX_MEMORY_NAME_CHARS`] characters.
pub fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= MAX_MEMORY_NAME_CHARS
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && !name.starts_with(['-', '_'])
        && !name.ends_with(['-', '_'])
        && !name.contains("--");
    if ok {
        Ok(())
    } else {
        Err(format!(
            "'{}' is not a memory name (expected [a-z0-9_-], a letter or digit first and last, \
at most {MAX_MEMORY_NAME_CHARS} characters, like docs-host-quill)",
            truncate_chars(&one_line(name), 40)
        ))
    }
}

/// What a memory must be to be kept: a valid name, a one-line description
/// within its limit, a body within its limit, and nothing that looks like
/// a secret.
pub fn check(memory: &Memory) -> Result<(), String> {
    validate_name(&memory.name)?;
    let description = memory.description.trim();
    if description.is_empty() {
        return Err(
            "a memory needs a one-line description: the gist, read in every briefing".into(),
        );
    }
    if description.contains('\n') || description.chars().count() > MAX_MEMORY_DESCRIPTION_CHARS {
        return Err(format!(
            "the description is one line of at most {MAX_MEMORY_DESCRIPTION_CHARS} characters"
        ));
    }
    if memory.body.trim().is_empty() {
        return Err("a memory needs a body: what a later session needs to know, and why".into());
    }
    if memory.body.chars().count() > MAX_MEMORY_BODY_CHARS {
        return Err(format!(
            "the body is at most {MAX_MEMORY_BODY_CHARS} characters; point to a file or a commit for the rest"
        ));
    }
    for text in [description, memory.body.as_str()] {
        if let Some(what) = looks_like_secret(text) {
            return Err(format!(
                "the memory looks like it holds {what}; a memory is read by every member of every \
later session, and must not carry a secret"
            ));
        }
    }
    Ok(())
}

/// Whether `text` carries something that reads as a credential: a private
/// key block, or a token by the shape well-known ones have.
pub fn looks_like_secret(text: &str) -> Option<&'static str> {
    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY") {
        return Some("a private key");
    }
    const PREFIXES: &[(&str, &str)] = &[
        ("sk-ant-", "an Anthropic API key"),
        ("sk-", "an API key"),
        ("ghp_", "a GitHub token"),
        ("gho_", "a GitHub token"),
        ("github_pat_", "a GitHub token"),
        ("glpat-", "a GitLab token"),
        ("xoxb-", "a Slack token"),
        ("xoxp-", "a Slack token"),
        ("lin_api_", "a Linear API key"),
        ("AKIA", "an AWS access key"),
        ("AIza", "a Google API key"),
    ];
    for word in text.split(|c: char| c.is_whitespace() || "\"'`<>()[]{},;".contains(c)) {
        if word.len() < 20 {
            continue;
        }
        if let Some((_, what)) = PREFIXES.iter().find(|(p, _)| word.starts_with(p)) {
            let rest = &word[word.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(0)..];
            if rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Some(what);
            }
        }
    }
    None
}

/// A name made from free text: its first words, lowercased, joined by `-`.
pub fn slug(text: &str, max_words: usize) -> String {
    let mut out = String::new();
    let mut words = 0;
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        let word: String = word
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word);
        words += 1;
        if words == max_words || out.len() >= MAX_MEMORY_NAME_CHARS - 8 {
            break;
        }
    }
    truncate_chars(&out, MAX_MEMORY_NAME_CHARS - 8)
        .trim_end_matches('-')
        .to_string()
}

/// `YYYY-MM-DD`, UTC, for the briefing.
pub fn date_of(at: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(at)
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn render(memory: &Memory) -> String {
    let front = Front {
        name: Some(memory.name.clone()),
        description: memory.description.trim().to_string(),
        kind: Some(memory.kind),
        by: Some(memory.by.clone()),
        // Milliseconds: two memories written in the same second still
        // list newest first.
        at: Some(serde_yaml::Value::String(
            DateTime::<Utc>::from_timestamp_millis(memory.at)
                .unwrap_or_default()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        )),
        source: memory.source.clone(),
        type_: None,
        modified: None,
        metadata: None,
    };
    let yaml = serde_yaml::to_string(&front).unwrap_or_default();
    format!("---\n{yaml}---\n\n{}\n", memory.body.trim())
}

/// A memory file: a front matter block and the body. A file with no front
/// matter (written by hand) is a fact whose description is its first line.
fn parse(name: &str, text: &str, modified: Option<i64>) -> Result<Memory> {
    let (front, body) = match split_front(text) {
        Some((yaml, body)) => (
            serde_yaml::from_str::<Front>(yaml).context("front matter")?,
            body,
        ),
        None => (Front::default(), text),
    };
    let body = body.trim().to_string();
    let description = if front.description.trim().is_empty() {
        truncate_chars(
            &one_line(body.lines().find(|l| !l.trim().is_empty()).unwrap_or("")),
            MAX_MEMORY_DESCRIPTION_CHARS,
        )
    } else {
        front.description.trim().to_string()
    };
    // Claude Code's own fields, where crew's are not there.
    let meta = |key: &str| -> Option<String> {
        front
            .metadata
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(|v| match v {
                serde_yaml::Value::String(s) => Some(s.clone()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
    };
    let claude_type = front.type_.clone().or_else(|| meta("type"));
    let claudes = claude_type.is_some() || meta("originSessionId").is_some();
    let stamp = |s: &str| {
        DateTime::parse_from_rfc3339(s.trim())
            .map(|t| t.timestamp_millis())
            .ok()
    };
    let at = match front.at {
        Some(serde_yaml::Value::String(s)) => stamp(&s),
        Some(serde_yaml::Value::Number(n)) => n.as_i64(),
        _ => None,
    }
    .or_else(|| front.modified.as_deref().and_then(stamp))
    .or_else(|| meta("modified").as_deref().and_then(stamp))
    .or(modified)
    .unwrap_or_else(now_ms);
    let kind = front.kind.unwrap_or_else(|| {
        claude_type
            .as_deref()
            .map(kind_of_claude_type)
            .unwrap_or_default()
    });
    Ok(Memory {
        name: name.to_string(),
        description,
        kind,
        by: front
            .by
            .unwrap_or_else(|| if claudes { CLAUDE } else { "operator" }.to_string()),
        at,
        source: front.source,
        body,
    })
}

fn split_front(text: &str) -> Option<(&str, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---")?;
    let yaml = &rest[..end];
    let after = &rest[end + 4..];
    let body = after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after);
    Some((yaml, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(name: &str, description: &str, body: &str) -> Memory {
        Memory {
            name: name.into(),
            description: description.into(),
            kind: MemoryKind::Decision,
            by: "orchestrator".into(),
            at: 1_791_700_000_000,
            source: Some("m-1".into()),
            body: body.into(),
        }
    }

    #[test]
    fn a_memory_goes_to_a_file_and_comes_back_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(tmp.path().join("memory"));
        assert!(store.list().unwrap().is_empty());
        let m = memory("docs-host-quill", "Planning docs that leave the repo go to Quill", "Chosen over Notion by the operator on 2026-10-11: Quill has an API we can test against.\n\nSee [[linear-mirror]].");
        assert!(!store.put(&m).unwrap());
        let text = fs::read_to_string(tmp.path().join("memory/docs-host-quill.md")).unwrap();
        assert!(text.starts_with("---\nname: docs-host-quill\n"), "{text}");
        assert!(text.contains("at: 2026-10-11T"), "{text}");
        assert!(text.ends_with("See [[linear-mirror]].\n"), "{text}");
        assert_eq!(store.get("docs-host-quill").unwrap().unwrap(), m);
        assert_eq!(store.list().unwrap(), vec![m.clone()]);
        // The same name again replaces it.
        let mut again = m.clone();
        again.body = "Changed.".into();
        assert!(store.put(&again).unwrap());
        assert_eq!(
            store.get("docs-host-quill").unwrap().unwrap().body,
            "Changed."
        );
        assert!(store.forget("docs-host-quill").unwrap());
        assert!(!store.forget("docs-host-quill").unwrap());
        assert!(store.get("docs-host-quill").unwrap().is_none());
    }

    #[test]
    fn a_file_written_by_hand_is_a_memory_too() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("memory");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("no-force-push.md"),
            "Never force-push main.\n\nThe CI mirrors it.\n",
        )
        .unwrap();
        fs::write(dir.join("README.txt"), "not a memory").unwrap();
        fs::write(dir.join("Bad Name.md"), "not a memory either").unwrap();
        fs::write(dir.join("MEMORY.md"), "- the index, not a memory\n").unwrap();
        let all = store_of(&dir).list().unwrap();
        assert_eq!(all.len(), 1);
        let m = &all[0];
        assert_eq!(m.name, "no-force-push");
        assert_eq!(m.description, "Never force-push main.");
        assert_eq!(m.kind, MemoryKind::Fact);
        assert_eq!(m.by, "operator");
        assert!(m.body.contains("The CI mirrors it."));
    }

    fn store_of(dir: &Path) -> MemoryStore {
        MemoryStore::new(dir.to_path_buf())
    }

    #[test]
    fn a_file_claude_code_saved_is_a_memory_in_its_own_terms() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("memory");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("feedback_testing.md"),
            "---\nname: feedback_testing\ndescription: Run the e2e suite alone, it is load-sensitive\nmetadata:\n  node_type: memory\n  type: feedback\n  originSessionId: 33430311-defb-4030-9c59-db43bcf477fd\n  modified: 2026-10-08T07:10:14.496Z\n---\n\nThe full team suite fails under load; rerun the failed ones alone.\n",
        )
        .unwrap();
        fs::write(
            dir.join("user_role.md"),
            "---\ntype: user\nmodified: 2026-10-09T10:00:00Z\n---\n\nThe operator is the project's only developer.\n",
        )
        .unwrap();
        // Claude's own index is not a memory.
        fs::write(dir.join("MEMORY.md"), "- [x](x.md) — y\n").unwrap();
        let all = store_of(&dir).list().unwrap();
        assert_eq!(
            all.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["user_role", "feedback_testing"]
        );
        let feedback = &all[1];
        assert_eq!(feedback.kind, MemoryKind::Convention);
        assert_eq!(feedback.by, "claude");
        assert_eq!(
            feedback.description,
            "Run the e2e suite alone, it is load-sensitive"
        );
        assert_eq!(date_of(feedback.at), "2026-10-08");
        let user = &all[0];
        assert_eq!(user.kind, MemoryKind::Preference);
        assert_eq!(user.by, "claude");
        assert_eq!(
            user.description,
            "The operator is the project's only developer."
        );
        assert_eq!(date_of(user.at), "2026-10-09");
    }

    #[test]
    fn the_index_lists_every_memory_for_claude_code_to_read() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_of(&tmp.path().join("memory"));
        assert!(!store.write_index().unwrap());
        assert!(!tmp.path().join("memory/MEMORY.md").exists());
        store
            .put(&memory("docs-host-quill", "Docs go to Quill", "b"))
            .unwrap();
        fs::write(
            tmp.path().join("memory/user_role.md"),
            "---\ntype: user\n---\n\nThe operator is the only developer.\n",
        )
        .unwrap();
        assert!(store.write_index().unwrap());
        let index = fs::read_to_string(tmp.path().join("memory/MEMORY.md")).unwrap();
        assert!(
            index.contains(
                "- [docs-host-quill](docs-host-quill.md) — Docs go to Quill (decision, \
orchestrator, 2026-10-11)"
            ) && index.contains(
                "- [user_role](user_role.md) — The operator is the only developer. (preference, \
claude, "
            ),
            "{index}"
        );
        assert!(!store.write_index().unwrap(), "rewritten unchanged");
        store.forget("docs-host-quill").unwrap();
        assert!(store.write_index().unwrap());
        assert!(!fs::read_to_string(tmp.path().join("memory/MEMORY.md"))
            .unwrap()
            .contains("docs-host-quill"));
    }

    #[test]
    fn names_and_sizes_are_held_to_their_limits() {
        assert!(validate_name("docs-host-quill").is_ok());
        assert!(validate_name("a1").is_ok());
        assert!(validate_name("feedback_testing").is_ok());
        for bad in [
            "",
            "-x",
            "x-",
            "_x",
            "a--b",
            "Docs",
            "a b",
            "../x",
            "x/y",
            &"a".repeat(65),
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        let mut m = memory("ok", "gist", "body");
        assert!(check(&m).is_ok());
        m.description = "x\ny".into();
        assert!(check(&m).is_err());
        m.description = "g".repeat(201);
        assert!(check(&m).is_err());
        m.description = "gist".into();
        m.body = "b".repeat(4001);
        assert!(check(&m).is_err());
        m.body = "  ".into();
        assert!(check(&m).is_err());
    }

    #[test]
    fn a_secret_is_refused() {
        assert_eq!(
            looks_like_secret("the key is sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"),
            Some("an Anthropic API key")
        );
        assert_eq!(
            looks_like_secret("token ghp_abcdefghijklmnopqrstuvwxyz012345"),
            Some("a GitHub token")
        );
        assert_eq!(
            looks_like_secret("-----BEGIN RSA PRIVATE KEY-----\nMIIE..."),
            Some("a private key")
        );
        assert_eq!(
            looks_like_secret("the config key is sk-ant- shaped, never written here"),
            None
        );
        assert_eq!(looks_like_secret("AKIA is the prefix"), None);
        let m = memory(
            "leak",
            "gist",
            "use ghp_abcdefghijklmnopqrstuvwxyz012345 to push",
        );
        assert!(check(&m).unwrap_err().contains("GitHub token"));
    }

    #[test]
    fn search_ranks_the_name_over_the_description_over_the_body() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_of(&tmp.path().join("memory"));
        store
            .put(&memory(
                "linear-mirror",
                "Where the Linear mirror lives",
                "crates/crew-integrations.",
            ))
            .unwrap();
        store
            .put(&memory(
                "docs-host",
                "Docs go to Quill",
                "The Linear import is separate.",
            ))
            .unwrap();
        store
            .put(&memory(
                "no-force-push",
                "Never force-push main",
                "CI mirrors it.",
            ))
            .unwrap();
        let found = store.search("linear", 10).unwrap();
        assert_eq!(
            found.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["linear-mirror", "docs-host"]
        );
        assert!(store.search("", 10).unwrap().is_empty());
        assert!(store.search("zzz", 10).unwrap().is_empty());
        assert_eq!(store.search("mirror linear", 1).unwrap().len(), 1);
    }

    #[test]
    fn free_names_and_slugs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_of(&tmp.path().join("memory"));
        assert_eq!(
            slug("Which document host should crew integrate first?", 5),
            "which-document-host-should-crew"
        );
        assert_eq!(slug("  ... ", 5), "");
        assert_eq!(
            store.free_name("decision-which-host").unwrap(),
            "decision-which-host"
        );
        store.put(&memory("decision-which-host", "g", "b")).unwrap();
        assert_eq!(
            store.free_name("decision-which-host").unwrap(),
            "decision-which-host-2"
        );
    }

    #[test]
    fn the_memory_has_a_ceiling() {
        let tmp = tempfile::tempdir().unwrap();
        let store = store_of(&tmp.path().join("memory"));
        for n in 0..MAX_MEMORIES {
            store.put(&memory(&format!("m-{n}"), "g", "b")).unwrap();
        }
        let err = store.put(&memory("one-more", "g", "b")).unwrap_err();
        assert!(err.to_string().contains("full"), "{err}");
        // Replacing one is still fine.
        assert!(store.put(&memory("m-0", "g", "b2")).unwrap());
    }
}
