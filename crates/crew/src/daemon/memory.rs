//! The daemon's side of the workspace memory (`crate::memory`): the calls
//! behind the `memory_*` tools and `crew memory`, and what crew remembers
//! by itself — the operator's answers (decisions, in so many words) and
//! each session's outcome.

use std::time::Duration;

use crew_core::config::MemoryConfig;
use crew_core::rpc::{RpcError, TranscriptResult};
use crew_core::util::{now_ms, one_line, truncate_chars};
use crew_core::{
    Memory, MemoryKind, Message, RuntimeKind, Task, MAX_MEMORY_BODY_CHARS, OPERATOR_ID,
};
use serde_json::{json, Value};

use super::handlers::internal;
use super::server::Who;
use super::Daemon;
use crate::memory::{self, MemoryStore};
use crate::persistence::NewEvent;

/// Who crew writes as, for what it remembers by itself.
const CREW: &str = "crew";

impl Daemon {
    fn memory_config(&self) -> MemoryConfig {
        self.model()
            .session
            .team
            .as_ref()
            .map(|t| t.memory.clone())
            .unwrap_or_default()
    }

    /// The workspace's memory as the team configures it; `None` when the
    /// team keeps none.
    pub(crate) fn memory_store(&self) -> Option<MemoryStore> {
        let config = self.memory_config();
        if !config.enabled {
            return None;
        }
        let dir = match config.dir {
            Some(dir) => self.workspace.join(dir),
            None => self.state_dir.join(memory::DEFAULT_DIR),
        };
        Some(MemoryStore::new(dir))
    }

    fn memory_on(&self) -> Result<MemoryStore, RpcError> {
        self.memory_store()
            .ok_or_else(|| RpcError::invalid("this team keeps no memory (memory.enabled: false)"))
    }

    fn may_replace_anyones(&self, who: &Who) -> bool {
        match who {
            Who::Operator => true,
            Who::Agent(id) => self.is_orchestrator(id),
        }
    }

    pub(crate) fn memory_list(&self) -> Result<Vec<Memory>, RpcError> {
        self.memory_on()?.list().map_err(internal)
    }

    pub(crate) fn memory_get(&self, name: &str) -> Result<Memory, RpcError> {
        let store = self.memory_on()?;
        memory::validate_name(name).map_err(RpcError::invalid)?;
        store
            .get(name)
            .map_err(internal)?
            .ok_or_else(|| RpcError::not_found(format!("no memory '{name}'")))
    }

    pub(crate) fn memory_search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Memory>, RpcError> {
        let store = self.memory_on()?;
        if query.trim().is_empty() {
            return Err(RpcError::invalid("say what to search for"));
        }
        store
            .search(query, limit.unwrap_or(memory::SEARCH_LIMIT))
            .map_err(internal)
    }

    /// Remember something, as `who`. A memory of the same name by someone
    /// else is replaced only by the orchestrator or the operator.
    pub(crate) fn memory_add(
        &self,
        who: &Who,
        name: String,
        description: String,
        kind: Option<MemoryKind>,
        body: String,
        source: Option<String>,
    ) -> Result<Value, RpcError> {
        let store = self.memory_on()?;
        let by = who.sender().to_string();
        let memory = Memory {
            name: name.trim().to_string(),
            description: description.trim().to_string(),
            kind: kind.unwrap_or_default(),
            by: by.clone(),
            at: now_ms(),
            source: source
                .map(|s| truncate_chars(s.trim(), 200))
                .filter(|s| !s.is_empty()),
            body: body.trim().to_string(),
        };
        memory::check(&memory).map_err(RpcError::invalid)?;
        if memory.kind == MemoryKind::Session && !self.may_replace_anyones(who) {
            return Err(RpcError::invalid(
                "a session memory is crew's own, written when a session completes; pick another kind",
            ));
        }
        if let Some(existing) = store.get(&memory.name).map_err(internal)? {
            if existing.by != by && !self.may_replace_anyones(who) {
                return Err(RpcError::forbidden(format!(
                    "memory '{}' was written by {}; add yours under another name, or ask the \
orchestrator to replace it",
                    memory.name, existing.by
                )));
            }
        }
        let replaced = store
            .put(&memory)
            .map_err(|e| RpcError::invalid(format!("{e:#}")))?;
        refresh_index(&store);
        let mut event = NewEvent::new(
            "daemon",
            if replaced {
                "memory.replaced"
            } else {
                "memory.added"
            },
            format!(
                "{by} {} {}: {}",
                if replaced { "rewrote" } else { "remembered" },
                memory.name,
                truncate_chars(&memory.description, 160)
            ),
        )
        .data(json!({ "name": memory.name, "kind": memory.kind, "replaced": replaced }));
        if let Who::Agent(id) = who {
            event = event.agent(id);
        }
        self.emit(event);
        Ok(json!({ "memory": memory, "replaced": replaced }))
    }

    /// Drop a memory (the caller is the orchestrator or the operator).
    pub(crate) fn memory_forget(&self, who: &Who, name: &str) -> Result<Value, RpcError> {
        let store = self.memory_on()?;
        memory::validate_name(name).map_err(RpcError::invalid)?;
        if !store.forget(name).map_err(internal)? {
            return Err(RpcError::not_found(format!("no memory '{name}'")));
        }
        refresh_index(&store);
        let mut event = NewEvent::new(
            "daemon",
            "memory.forgotten",
            format!("{} forgot {name}", who.sender()),
        )
        .data(json!({ "name": name }));
        if let Who::Agent(id) = who {
            event = event.agent(id);
        }
        self.emit(event);
        Ok(json!({ "ok": true, "name": name }))
    }

    /// What the briefings carry: the newest memories, as many as the team
    /// briefs. `None` when the team keeps no memory.
    pub(crate) fn memories_for_briefing(&self) -> Option<Vec<Memory>> {
        let store = self.memory_store()?;
        let brief = self.memory_config().brief;
        // The index Claude Code loads at the start of this session lists
        // what a member saved on its own since the last write too.
        refresh_index(&store);
        match store.list() {
            Ok(mut all) => {
                all.truncate(brief);
                Some(all)
            }
            Err(e) => {
                tracing::warn!("reading the memory for a briefing: {e:#}");
                Some(Vec::new())
            }
        }
    }

    /// crew's own: the operator's answer to the team's question is a
    /// decision, remembered as one. A question whose answer reads like a
    /// secret is not remembered.
    pub(crate) fn remember_answer(&self, question: &Message, answer: &str) {
        let Some(store) = self.memory_store() else {
            return;
        };
        let question_text = question.body.trim();
        let gist = |text: &str, max| truncate_chars(&one_line(text), max);
        let mut base = memory::slug(question_text, 5);
        if base.is_empty() {
            base = "question".to_string();
        }
        let name = match store.free_name(&format!("decision-{base}")) {
            Ok(name) => name,
            Err(e) => {
                tracing::warn!("naming a decision memory: {e:#}");
                return;
            }
        };
        let body = truncate_chars(
            &format!(
                "Question ({}): {question_text}\n\nAnswer (operator): {}",
                question.asker(),
                answer.trim()
            ),
            MAX_MEMORY_BODY_CHARS,
        );
        let memory = Memory {
            name,
            description: format!("{} → {}", gist(question_text, 110), gist(answer, 80)),
            kind: MemoryKind::Decision,
            by: OPERATOR_ID.to_string(),
            at: now_ms(),
            source: Some(question.id.clone()),
            body,
        };
        self.remember(store, memory);
    }

    /// crew's own: what a session set out to do and came to.
    pub(crate) fn remember_session(&self, summary: &str) {
        let Some(store) = self.memory_store() else {
            return;
        };
        let (objective, team) = {
            let model = self.model();
            let objective = model
                .session
                .team
                .as_ref()
                .map(|t| t.objective.trim().to_string())
                .unwrap_or_default();
            let team: Vec<String> = model
                .agents
                .iter()
                .filter(|a| !a.orchestrator && a.parent.is_none())
                .map(|a| {
                    if a.role.is_empty() {
                        a.id.clone()
                    } else {
                        format!("{} ({})", a.id, truncate_chars(&one_line(&a.role), 60))
                    }
                })
                .collect();
            (objective, team)
        };
        let date = memory::date_of(now_ms());
        let mut base = format!("session-{date}");
        let slug = memory::slug(&objective, 4);
        if !slug.is_empty() {
            base = format!("{base}-{slug}");
        }
        let name = match store.free_name(&base) {
            Ok(name) => name,
            Err(e) => {
                tracing::warn!("naming a session memory: {e:#}");
                return;
            }
        };
        let mut body = String::new();
        if !objective.is_empty() {
            body.push_str(&format!("Objective: {objective}\n\n"));
        }
        body.push_str(&format!("Outcome: {}", summary.trim()));
        if !team.is_empty() {
            body.push_str(&format!("\n\nTeam: {}", team.join(", ")));
        }
        let memory = Memory {
            name,
            description: truncate_chars(&one_line(summary), 200),
            kind: MemoryKind::Session,
            by: CREW.to_string(),
            at: now_ms(),
            source: None,
            body: truncate_chars(&body, MAX_MEMORY_BODY_CHARS),
        };
        self.remember(store, memory);
    }

    fn remember(&self, store: MemoryStore, memory: Memory) {
        if let Err(why) = memory::check(&memory) {
            tracing::info!("not remembering {}: {why}", memory.name);
            return;
        }
        match store.put(&memory) {
            Ok(_) => self.emit(
                NewEvent::new(
                    "daemon",
                    "memory.added",
                    format!(
                        "crew remembered {} ({}): {}",
                        memory.name,
                        memory.kind,
                        truncate_chars(&memory.description, 160)
                    ),
                )
                .data(json!({ "name": memory.name, "kind": memory.kind, "replaced": false })),
            ),
            Err(e) => tracing::warn!("remembering {}: {e:#}", memory.name),
        }
        refresh_index(&store);
    }
}

/// The `MEMORY.md` index Claude Code reads, brought up to date after a
/// change; a failure is logged, the files are the memory.
fn refresh_index(store: &MemoryStore) {
    if let Err(e) = store.write_index() {
        tracing::warn!("writing the memory index: {e:#}");
    }
}

/// How much of a card and its member's transcript a distillation reads.
const DISTILL_TRANSCRIPT_ENTRIES: usize = 200;
const DISTILL_TRANSCRIPT_CHARS: usize = 40_000;
const DISTILL_ENTRY_CHARS: usize = 1_500;
const DISTILL_FIELD_CHARS: usize = 2_000;
const DISTILL_INDEX_LINES: usize = 60;
/// Most memories one card yields.
const DISTILL_MAX: usize = 5;
/// How long the model gets.
const DISTILL_TIMEOUT: Duration = Duration::from_secs(180);

/// What the model answers with: a memory to keep.
#[derive(serde::Deserialize)]
struct Candidate {
    name: String,
    description: String,
    #[serde(default)]
    kind: Option<String>,
    body: String,
}

#[derive(serde::Deserialize)]
struct Candidates {
    #[serde(default)]
    memories: Vec<Candidate>,
}

impl Daemon {
    /// A finished card distilled into memories, in the background: what a
    /// later session could not get from the code, the commits or the card
    /// itself, read off the card and its member's transcript by the model
    /// `memory.extract` names (Codex's own memory works the same way, from
    /// a session's rollout once it has rested). Off with `extract: off`,
    /// and where Claude Code is not installed.
    pub(crate) fn distill_card(&self, task: Task, author: String) {
        let config = self.memory_config();
        if !config.enabled
            || config.extract.trim().eq_ignore_ascii_case("off")
            || task.result.as_deref().is_none_or(|r| r.trim().is_empty())
        {
            return;
        }
        if crate::models::Availability::runtime(RuntimeKind::Claude).is_err() {
            tracing::debug!("not distilling {}: Claude Code is not installed", task.id);
            return;
        }
        let daemon = self.clone();
        tokio::spawn(async move {
            daemon.distill_card_now(task, author, config.extract).await;
        });
    }

    async fn distill_card_now(&self, task: Task, author: String, model: String) {
        let Some(store) = self.memory_store() else {
            return;
        };
        let transcript = self
            .agent_transcript(&author, DISTILL_TRANSCRIPT_ENTRIES)
            .await
            .unwrap_or_default();
        let known = store.list().unwrap_or_default();
        let prompt = distill_prompt(&task, &author, &transcript, &known);
        let answer = match self.run_claude_print(&model, &prompt).await {
            Ok(answer) => answer,
            Err(why) => {
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "memory.distill_failed",
                        format!("distilling card {} into memories: {why}", task.id),
                    )
                    .task(&task.id)
                    .warn(),
                );
                return;
            }
        };
        let candidates = match parse_candidates(&answer) {
            Ok(candidates) => candidates,
            Err(why) => {
                self.emit(
                    NewEvent::new(
                        "daemon",
                        "memory.distill_failed",
                        format!(
                            "distilling card {}: the model's answer was not the expected JSON ({why})",
                            task.id
                        ),
                    )
                    .task(&task.id)
                    .warn(),
                );
                return;
            }
        };
        let mut kept = Vec::new();
        for candidate in candidates.into_iter().take(DISTILL_MAX) {
            let kind = candidate
                .kind
                .as_deref()
                .and_then(|k| k.parse::<MemoryKind>().ok())
                .filter(|k| *k != MemoryKind::Session)
                .unwrap_or_default();
            let mut memory = Memory {
                name: candidate.name.trim().to_lowercase(),
                description: candidate.description.trim().to_string(),
                kind,
                by: CREW.to_string(),
                at: now_ms(),
                source: Some(task.id.clone()),
                body: candidate.body.trim().to_string(),
            };
            if let Err(why) = memory::check(&memory) {
                tracing::info!("distilled memory dropped ({why})");
                continue;
            }
            // Somebody else's memory of that name stays: this one goes
            // next to it.
            match store.get(&memory.name) {
                Ok(Some(existing)) if existing.by != CREW => {
                    memory.name = match store.free_name(&memory.name) {
                        Ok(name) => name,
                        Err(_) => continue,
                    };
                }
                Err(e) => {
                    tracing::warn!("reading memory {}: {e:#}", memory.name);
                    continue;
                }
                _ => {}
            }
            match store.put(&memory) {
                Ok(_) => kept.push(memory),
                Err(e) => tracing::warn!("keeping a distilled memory: {e:#}"),
            }
        }
        refresh_index(&store);
        if kept.is_empty() {
            return;
        }
        for memory in &kept {
            self.emit(
                NewEvent::new(
                    "daemon",
                    "memory.added",
                    format!(
                        "crew distilled {} ({}) from card {}: {}",
                        memory.name,
                        memory.kind,
                        task.id,
                        truncate_chars(&memory.description, 160)
                    ),
                )
                .task(&task.id)
                .data(json!({ "name": memory.name, "kind": memory.kind, "replaced": false, "distilled": true })),
            );
        }
    }

    /// One answer from Claude Code's print mode: no session kept, no MCP
    /// server, none of the operator's own memory, the model named.
    async fn run_claude_print(&self, model: &str, prompt: &str) -> Result<String, String> {
        let mut cmd = tokio::process::Command::new("claude");
        cmd.args([
            "-p",
            prompt,
            "--model",
            model,
            "--output-format",
            "json",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--mcp-config",
            r#"{"mcpServers":{}}"#,
        ])
        .current_dir(&self.workspace)
        .env_remove(crew_core::paths::IDENTITY_ENV)
        .env("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
        let child = cmd.spawn().map_err(|e| format!("starting claude: {e}"))?;
        let output = tokio::time::timeout(DISTILL_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| format!("claude took longer than {}s", DISTILL_TIMEOUT.as_secs()))?
            .map_err(|e| format!("claude: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "claude exited with {}: {}",
                output.status,
                truncate_chars(&one_line(&String::from_utf8_lossy(&output.stderr)), 200)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// The ask: the card, what is remembered already, the transcript, and the
/// shape of the answer.
fn distill_prompt(
    task: &Task,
    author: &str,
    transcript: &TranscriptResult,
    known: &[Memory],
) -> String {
    let field = |s: &str| truncate_chars(s.trim(), DISTILL_FIELD_CHARS);
    let mut out = String::new();
    out.push_str(
        "You distil a finished card of a team of coding agents into memories for the team's later \
sessions on this project. Keep only what a later session could not get from the code, the commits \
or the card itself: a decision and why, a convention this project follows, a pitfall met (what \
went wrong, what to do instead), where something stands or lives, a preference of the operator's. \
Not the narrative of the work, not what the diff says, never a secret, nothing already \
remembered. Write 0 to 5 memories; none is a fine answer.\n\n",
    );
    out.push_str(&format!(
        "Card {}: {}\nDone by: {author}\n",
        task.id,
        field(&task.title)
    ));
    if !task.description.trim().is_empty() {
        out.push_str(&format!("Brief: {}\n", field(&task.description)));
    }
    if let Some(result) = &task.result {
        out.push_str(&format!("Result: {}\n", field(result)));
    }
    let commits: Vec<&str> = task
        .artifacts
        .iter()
        .filter(|a| a.kind == "commit")
        .map(|a| a.value.as_str())
        .collect();
    if !commits.is_empty() {
        out.push_str(&format!("Commits: {}\n", commits.join(", ")));
    }
    if !known.is_empty() {
        out.push_str("\nAlready remembered (do not repeat or restate):\n");
        for m in known.iter().take(DISTILL_INDEX_LINES) {
            out.push_str(&format!("- {} — {}\n", m.name, m.description));
        }
    }
    out.push_str(
        "\nAnswer with JSON only, nothing around it: {\"memories\": [{\"name\": \"[a-z0-9-], at most \
64 characters, like docs-host-quill\", \"description\": \"one line, at most 200 characters: the \
gist\", \"kind\": \"decision | convention | preference | fact | pitfall\", \"body\": \"at most 2000 \
characters: what, and why\"}]} — {\"memories\": []} when nothing is worth keeping.\n",
    );
    if transcript.entries.is_empty() {
        return out;
    }
    let mut lines: Vec<String> = transcript
        .entries
        .iter()
        .map(|e| {
            format!(
                "[{}] {}",
                e.kind,
                truncate_chars(e.text.trim(), DISTILL_ENTRY_CHARS)
            )
        })
        .collect();
    // The newest part of a long transcript: the end is where the card
    // was finished.
    let mut total = 0;
    let mut keep = lines.len();
    for (i, line) in lines.iter().enumerate().rev() {
        total += line.len() + 1;
        if total > DISTILL_TRANSCRIPT_CHARS {
            keep = lines.len() - i - 1;
            break;
        }
    }
    lines.drain(..lines.len() - keep);
    out.push_str("\nThe member's transcript, oldest first:\n");
    out.push_str(&lines.join("\n"));
    out.push('\n');
    out
}

/// The model's answer, as Claude Code's print mode wraps it (`result` of a
/// JSON envelope) or bare, with or without a code fence.
fn parse_candidates(answer: &str) -> Result<Vec<Candidate>, String> {
    let text = match serde_json::from_str::<Value>(answer.trim()) {
        Ok(Value::Object(envelope)) if envelope.contains_key("result") => envelope["result"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| envelope["result"].to_string()),
        _ => answer.trim().to_string(),
    };
    let start = text.find('{').ok_or("no JSON object")?;
    let end = text.rfind('}').ok_or("no JSON object")?;
    if end < start {
        return Err("no JSON object".into());
    }
    serde_json::from_str::<Candidates>(&text[start..=end])
        .map(|c| c.memories)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_answer_is_read_from_the_print_envelope_or_bare() {
        let bare = r#"{"memories":[{"name":"docs-host-quill","description":"Docs go to Quill","kind":"decision","body":"Chosen over Notion."}]}"#;
        assert_eq!(parse_candidates(bare).unwrap()[0].name, "docs-host-quill");
        let envelope =
            json!({ "type": "result", "result": format!("Here you go:\n```json\n{bare}\n```") })
                .to_string();
        let got = parse_candidates(&envelope).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind.as_deref(), Some("decision"));
        assert!(parse_candidates(r#"{"memories":[]}"#).unwrap().is_empty());
        assert!(parse_candidates("nothing here").is_err());
        assert!(
            parse_candidates(r#"{"type":"result","result":"no memories worth keeping"}"#).is_err()
        );
    }

    #[test]
    fn a_long_transcript_is_cut_from_the_front() {
        let task = Task {
            epic: None,
            id: "t".into(),
            title: "Title".into(),
            description: String::new(),
            assigned_agent: Some("a".into()),
            status: crew_core::TaskStatus::Done,
            dependencies: vec![],
            created_at: 0,
            started_at: None,
            completed_at: None,
            artifacts: vec![],
            reviewer: None,
            result: Some("done".into()),
            progress: None,
            note: None,
            timeout_secs: None,
        };
        let entries = (0..100)
            .map(|i| crew_core::TranscriptEntry {
                kind: crew_core::TranscriptKind::Reply,
                at: None,
                text: format!("entry {i} {}", "x".repeat(800)),
            })
            .collect();
        let transcript = TranscriptResult {
            entries,
            available: true,
            truncated: false,
        };
        let prompt = distill_prompt(&task, "a", &transcript, &[]);
        assert!(!prompt.contains("entry 0 "), "the start is cut");
        assert!(prompt.contains("entry 99 "), "the end is kept");
        assert!(
            prompt.len() < DISTILL_TRANSCRIPT_CHARS + 2_000,
            "{}",
            prompt.len()
        );
        assert!(prompt.contains("Card t: Title"));
    }
}
