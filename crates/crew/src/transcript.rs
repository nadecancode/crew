//! An agent's session, read from the file its runtime keeps.
//!
//! Claude Code and Codex both record a session as JSON lines: what the agent
//! was told, what it wrote, the tools it called. That record is the
//! transcript the dashboard shows. It is complete and already structured,
//! where a terminal's scrollback is neither (and, on herdr, slow to read).
//! Only the end of the file is read: sessions grow to many megabytes.

use std::path::Path;

use crew_core::rpc::TranscriptResult;
use crew_core::types::{TranscriptEntry, TranscriptKind};
use crew_core::util::{one_line, truncate_chars};
use crew_core::RuntimeKind;
use serde_json::Value;

/// How much of the end of a session file is read.
const TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// The longest text kept of one entry.
const MAX_ENTRY_CHARS: usize = 6000;
/// The longest one-line account of a tool call.
const MAX_TOOL_CHARS: usize = 200;

/// How Claude Code introduces what crew's plugin put into the session.
const BRIDGE_PREFIX: &str = "The crew-bridge plugin sent a message:";
/// And how it ends such a prompt, in a paragraph of its own.
const BRIDGE_SUFFIX: &str = "\nThis is how Claude Code surfaces a prompt a plugin submits";

/// The newest `limit` entries of the session file at `path`.
pub fn read(path: &Path, runtime: RuntimeKind, limit: usize) -> TranscriptResult {
    let whole = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let Some(tail) = crate::limits::read_tail(path, TAIL_BYTES) else {
        return TranscriptResult::default();
    };
    let mut entries = match runtime {
        RuntimeKind::Claude => claude_entries(&tail),
        RuntimeKind::Codex => codex_entries(&tail),
        RuntimeKind::Pi => pi_entries(&tail),
        RuntimeKind::Shell => Vec::new(),
    };
    let mut truncated = whole > TAIL_BYTES;
    if entries.len() > limit {
        entries.drain(..entries.len() - limit);
        truncated = true;
    }
    TranscriptResult {
        entries,
        available: true,
        truncated,
    }
}

fn timestamp(line: &Value) -> Option<i64> {
    let text = line.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn entry(kind: TranscriptKind, at: Option<i64>, text: &str) -> Option<TranscriptEntry> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let total = text.chars().count();
    let text = if total > MAX_ENTRY_CHARS {
        let shown = truncate_chars(text, MAX_ENTRY_CHARS);
        format!("{shown}\n[{} more characters]", total + 1 - MAX_ENTRY_CHARS)
    } else {
        text.to_string()
    };
    Some(TranscriptEntry { kind, at, text })
}

/// `mcp__crew__agent_send` reads better as `crew.agent_send`.
fn tool_name(name: &str) -> String {
    name.strip_prefix("mcp__")
        .unwrap_or(name)
        .replace("__", ".")
}

/// A tool call in one line: its name and what it was pointed at.
fn tool_line(name: &str, input: &Value) -> String {
    let about = match input {
        Value::String(text) => {
            // Codex passes some tools their arguments as a JSON string.
            match serde_json::from_str::<Value>(text) {
                Ok(parsed @ Value::Object(_)) => return tool_line(name, &parsed),
                _ => text
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .to_string(),
            }
        }
        Value::Object(fields) => {
            let named = [
                "command",
                "cmd",
                "file_path",
                "path",
                "pattern",
                "query",
                "url",
            ]
            .iter()
            .find_map(|key| fields.get(*key));
            match named {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ if fields.is_empty() => String::new(),
                _ => input.to_string(),
            }
        }
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let line = format!("{}  {}", tool_name(name), one_line(&about));
    truncate_chars(line.trim_end(), MAX_TOOL_CHARS)
}

/// Text that the runtime put into the session for itself, not a prompt.
fn machine_text(text: &str) -> bool {
    let text = text.trim_start();
    [
        "<local-command-",
        "<command-",
        "<system-reminder>",
        "<task-notification>",
        "<environment_context>",
        "<user_instructions>",
    ]
    .iter()
    .any(|tag| text.starts_with(tag))
}

/// What crew's plugin submitted, without the sentences Claude Code puts
/// around a prompt that comes from a plugin.
fn unbridged(text: &str) -> &str {
    let Some(body) = text.trim_start().strip_prefix(BRIDGE_PREFIX) else {
        return text;
    };
    match body.rfind(BRIDGE_SUFFIX) {
        Some(at) => &body[..at],
        None => body,
    }
}

/// A prompt that was pasted into Claude Code's terminal is recorded inside
/// `<pasted_content id="…">` tags; the prompt is what is between them.
fn unpasted(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("<pasted_content") else {
        return text;
    };
    let Some((_, body)) = rest.split_once('>') else {
        return text;
    };
    body.strip_suffix("</pasted_content>").unwrap_or(body)
}

/// The entries of a Claude Code transcript (`<session>.jsonl`).
pub fn claude_entries(text: &str) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let Ok(line) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // A subagent's turns are its own conversation.
        if line.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let at = timestamp(&line);
        let kind = line.get("type").and_then(Value::as_str).unwrap_or("");
        let content = line.pointer("/message/content");
        match kind {
            "system" if line.get("subtype").and_then(Value::as_str) == Some("compact_boundary") => {
                entries.extend(entry(TranscriptKind::Note, at, "context compacted"));
            }
            "user" => {
                let internal = ["isMeta", "isCompactSummary"]
                    .iter()
                    .any(|flag| line.get(*flag).and_then(Value::as_bool) == Some(true));
                if internal {
                    continue;
                }
                let texts: Vec<&str> = match content {
                    Some(Value::String(text)) => vec![text.as_str()],
                    // Tool results are part of the call, not a prompt.
                    Some(Value::Array(blocks)) => blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .collect(),
                    _ => Vec::new(),
                };
                for text in texts {
                    if machine_text(text) {
                        continue;
                    }
                    entries.extend(entry(TranscriptKind::Prompt, at, unpasted(unbridged(text))));
                }
            }
            "assistant" => {
                let Some(Value::Array(blocks)) = content else {
                    continue;
                };
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            entries.extend(entry(TranscriptKind::Reply, at, text));
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                            let input = block.get("input").unwrap_or(&Value::Null);
                            entries.extend(entry(
                                TranscriptKind::Tool,
                                at,
                                &tool_line(name, input),
                            ));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    entries
}

/// The entries of a Codex session file (`rollout-….jsonl`).
pub fn codex_entries(text: &str) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let Ok(line) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = timestamp(&line);
        match line.get("type").and_then(Value::as_str) {
            Some("compacted") => {
                entries.extend(entry(TranscriptKind::Note, at, "context compacted"))
            }
            Some("response_item") => {}
            // A call to an MCP server (crew's tools among them) is recorded
            // only as an item, not as a response item.
            Some("event_msg") => {
                let item = line.pointer("/payload/item");
                let is_call = line.pointer("/payload/type").and_then(Value::as_str)
                    == Some("item_completed")
                    && item.and_then(|i| i.get("type")).and_then(Value::as_str)
                        == Some("McpToolCall");
                if let (true, Some(item)) = (is_call, item) {
                    let field = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or("");
                    let name = format!("{}.{}", field("server"), field("tool"));
                    let input = item.get("arguments").unwrap_or(&Value::Null);
                    entries.extend(entry(TranscriptKind::Tool, at, &tool_line(&name, input)));
                }
                continue;
            }
            _ => continue,
        }
        let Some(item) = line.get("payload") else {
            continue;
        };
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let kind = match item.get("role").and_then(Value::as_str) {
                    Some("user") => TranscriptKind::Prompt,
                    Some("assistant") => TranscriptKind::Reply,
                    // Instructions from the harness.
                    _ => continue,
                };
                let Some(Value::Array(blocks)) = item.get("content") else {
                    continue;
                };
                for text in blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                {
                    if kind == TranscriptKind::Prompt && machine_text(text) {
                        continue;
                    }
                    entries.extend(entry(kind, at, text));
                }
            }
            Some("function_call" | "custom_tool_call" | "local_shell_call") => {
                let name = item.get("name").and_then(Value::as_str).unwrap_or("shell");
                let input = ["arguments", "input", "action"]
                    .iter()
                    .find_map(|key| item.get(*key))
                    .unwrap_or(&Value::Null);
                entries.extend(entry(TranscriptKind::Tool, at, &tool_line(name, input)));
            }
            _ => {}
        }
    }
    entries
}

/// The entries of a pi session file (`<time>_<id>.jsonl`): each message is
/// an entry with its role; a tool call sits in the assistant's content.
pub fn pi_entries(text: &str) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let Ok(line) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = timestamp(&line);
        match line.get("type").and_then(Value::as_str) {
            Some("compaction") => {
                entries.extend(entry(TranscriptKind::Note, at, "context compacted"));
                continue;
            }
            Some("message") => {}
            _ => continue,
        }
        let Some(message) = line.get("message") else {
            continue;
        };
        let kind = match message.get("role").and_then(Value::as_str) {
            Some("user") => TranscriptKind::Prompt,
            Some("assistant") => TranscriptKind::Reply,
            // Tool results are part of the call.
            _ => continue,
        };
        match message.get("content") {
            Some(Value::String(text)) => entries.extend(entry(kind, at, text)),
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            entries.extend(entry(kind, at, text));
                        }
                        Some("toolCall") if kind == TranscriptKind::Reply => {
                            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                            let input = ["arguments", "args", "input"]
                                .iter()
                                .find_map(|key| block.get(*key))
                                .unwrap_or(&Value::Null);
                            entries.extend(entry(
                                TranscriptKind::Tool,
                                at,
                                &tool_line(name, input),
                            ));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    entries
}

/// The latest turn of a session: what it was given, its last reply, and
/// how many tools it called.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LastTurn {
    pub input: Vec<String>,
    pub reply: String,
    /// When the last reply was written, as the runtime recorded it.
    pub reply_at: Option<i64>,
    pub tools: usize,
    /// What it wrote after its last tool call: the answer the turn ends
    /// with (empty when it ended on a tool call).
    pub final_reply: String,
}

/// The latest turn in a session's `entries`: the prompts since the turn
/// before, and what came after them. `None` before the first prompt.
pub fn last_turn(entries: &[TranscriptEntry]) -> Option<LastTurn> {
    let start = entries
        .iter()
        .rposition(|e| e.kind == TranscriptKind::Prompt)?;
    let first = entries[..start]
        .iter()
        .rposition(|e| e.kind != TranscriptKind::Prompt)
        .map_or(0, |at| at + 1);
    let mut turn = LastTurn {
        input: entries[first..=start]
            .iter()
            .map(|e| e.text.clone())
            .collect(),
        ..LastTurn::default()
    };
    let mut closing: Vec<&str> = Vec::new();
    for entry in &entries[start + 1..] {
        match entry.kind {
            TranscriptKind::Reply => {
                turn.reply = entry.text.clone();
                turn.reply_at = entry.at;
                closing.push(&entry.text);
            }
            TranscriptKind::Tool => {
                turn.tools += 1;
                closing.clear();
            }
            _ => {}
        }
    }
    turn.final_reply = closing.join("\n\n");
    Some(turn)
}

/// A turn's final reply, split by whom it speaks to: a line that starts
/// with one or more `@<name>`, each one of `names` (or `@all`, everyone
/// but the operator), separated by spaces or commas and then `:` or a space,
/// opens a part for each of them, running to the next such line. What comes
/// before the first is for whoever the turn answers. Lines inside a code
/// fence are text. One entry per name and part.
pub fn addressed(reply: &str, names: &[&str]) -> (String, Vec<(String, String)>) {
    let mut parts = Vec::new();
    let (lead, sections) = sections(reply, names);
    for (to, text) in sections {
        for name in to {
            parts.push((name, text.clone()));
        }
    }
    (lead, parts)
}

/// The names a line opens with (`@db @lead:`, `@db, @lead`), and the rest
/// of the line; `None` when it opens with no known name.
fn header(line: &str, names: &[&str]) -> Option<(Vec<String>, String)> {
    let mut rest = line.trim_start();
    let mut found: Vec<String> = Vec::new();
    while let Some(after) = rest.strip_prefix('@') {
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(after.len());
        let (name, tail) = after.split_at(end);
        let named: Vec<&str> = match name {
            "all" => names.iter().copied().filter(|n| *n != "operator").collect(),
            _ if names.contains(&name) => vec![name],
            _ => break,
        };
        // What follows a name: more names, the colon, or the text.
        if !(tail.is_empty() || tail.starts_with([':', ' ', ','])) {
            break;
        }
        for n in named {
            if !found.iter().any(|f| f == n) {
                found.push(n.to_string());
            }
        }
        rest = tail.trim_start_matches([' ', ',']);
    }
    if found.is_empty() {
        return None;
    }
    Some((
        found,
        rest.strip_prefix(':')
            .unwrap_or(rest)
            .trim_start()
            .to_string(),
    ))
}

fn sections(reply: &str, names: &[&str]) -> (String, Vec<(Vec<String>, String)>) {
    let mut lead: Vec<&str> = Vec::new();
    let mut parts: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    let mut fenced = false;
    for line in reply.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
        }
        let opened = if fenced { None } else { header(line, names) };
        match opened {
            Some((to, first)) => parts.push((to, vec![first])),
            None => match parts.last_mut() {
                Some((_, lines)) => lines.push(line.to_string()),
                None => lead.push(line),
            },
        }
    }
    let parts = parts
        .into_iter()
        .map(|(name, lines)| (name, lines.join("\n").trim().to_string()))
        .filter(|(_, text)| !text.is_empty())
        .collect();
    (lead.join("\n").trim().to_string(), parts)
}

/// Whether `text` is a runtime's notice that the model refused the turn,
/// and the kind: Claude Code's "…'s safeguards flagged this message" (with
/// the category the API gave, `cyber` and the like, else `safeguards`).
/// A model's own words are not read into: what a turn did says more.
pub fn runtime_refusal(text: &str) -> Option<String> {
    let text = text.replace('\u{2019}', "'");
    if !text.contains("safeguards flagged this message")
        && !text.contains("can't help with this. Start a new session")
    {
        return None;
    }
    let category = text
        .split_once("Details: `[")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(category, _)| category.trim().to_string())
        .filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'));
    Some(category.unwrap_or_else(|| "safeguards".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(values: &[Value]) -> String {
        values.iter().map(|v| format!("{v}\n")).collect()
    }

    fn kinds(entries: &[TranscriptEntry]) -> Vec<(TranscriptKind, &str)> {
        entries.iter().map(|e| (e.kind, e.text.as_str())).collect()
    }

    fn at(kind: TranscriptKind, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            kind,
            at: None,
            text: text.to_string(),
        }
    }

    #[test]
    fn a_reply_speaks_to_whoever_it_names_at_the_start_of_a_line() {
        let names = ["db", "lead", "operator"];
        let reply = "Port 8080.\n@db: schema ok, a1b2c3.\nRebase first.\n@lead migration lands today\n```java\n@Override\n@db: not a message\n```\n@nobody: just text";
        let (lead, parts) = addressed(reply, &names);
        assert_eq!(lead, "Port 8080.");
        assert_eq!(
            parts[0],
            (
                "db".to_string(),
                "schema ok, a1b2c3.\nRebase first.".to_string()
            )
        );
        assert_eq!(parts[1].0, "lead");
        assert!(parts[1].1.starts_with("migration lands today\n```java\n@Override\n@db: not a message\n```\n@nobody: just text"), "{parts:?}");
        assert_eq!(parts.len(), 2);
        // An email address, an annotation or a name nobody has is text.
        assert_eq!(
            addressed("Mail me@db.io or use @Override.", &names),
            ("Mail me@db.io or use @Override.".to_string(), Vec::new())
        );
        assert_eq!(
            addressed("@operator: done", &names).1,
            [("operator".to_string(), "done".to_string())]
        );
        // Several at once, or everyone but the operator.
        let both = [
            ("db".to_string(), "schema changed".to_string()),
            ("lead".to_string(), "schema changed".to_string()),
        ];
        assert_eq!(addressed("@db @lead: schema changed", &names).1, both);
        assert_eq!(addressed("@db, @lead schema changed", &names).1, both);
        assert_eq!(
            addressed("@all: freeze main", &names)
                .1
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["db", "lead"]
        );
        // A name that is not one stops the list: the rest is text.
        assert_eq!(
            addressed("@db: see @lead's note", &names).1,
            [("db".to_string(), "see @lead's note".to_string())]
        );
    }

    #[test]
    fn the_last_turn_is_its_prompts_and_what_followed() {
        use TranscriptKind::*;
        let entries = [
            at(Prompt, "first"),
            at(Reply, "done"),
            at(Prompt, "[crew] 2 new messages: one"),
            at(Prompt, "and two"),
            at(Tool, "Bash  ls"),
            at(Reply, "looking"),
            at(Tool, "Read  a.rs"),
            at(Reply, "all good"),
        ];
        let turn = last_turn(&entries).unwrap();
        assert_eq!(turn.input, ["[crew] 2 new messages: one", "and two"]);
        assert_eq!((turn.reply.as_str(), turn.tools), ("all good", 2));
        // What it ended with: the text after its last tool call.
        assert_eq!(turn.final_reply, "all good");
        let mut ended_on_a_tool = entries.to_vec();
        ended_on_a_tool.push(at(Tool, "Bash  ls"));
        assert_eq!(last_turn(&ended_on_a_tool).unwrap().final_reply, "");
        // Not answered yet: the input, and nothing else.
        let turn = last_turn(&entries[..4]).unwrap();
        assert_eq!(
            (turn.input.len(), turn.reply.as_str(), turn.tools),
            (2, "", 0)
        );
        assert_eq!(last_turn(&[at(Reply, "hello")]), None);
    }

    #[test]
    fn a_refusal_is_the_runtimes_notice_not_the_models_wording() {
        let flagged = "API Error: Opus 4.8's safeguards flagged this message (https://www.anthropic.com/legal/aup). Our intentionally broad safeguards allow us to deliver more capabilities faster, but can sometimes flag legitimate coding and cybersecurity tasks. Claude Code can't respond to this message with Opus 4.8. Details: `[cyber]` Request ID: req_1";
        assert_eq!(runtime_refusal(flagged).as_deref(), Some("cyber"));
        assert_eq!(
            runtime_refusal(
                "API Error: Claude can't help with this. Start a new session to continue."
            )
            .as_deref(),
            Some("safeguards")
        );
        for words in [
            "I can't help with that.",
            "Done: the parser handles empty input now.",
        ] {
            assert_eq!(runtime_refusal(words), None, "{words}");
        }
    }

    #[test]
    fn claude_transcripts_give_prompts_replies_and_tool_calls() {
        let text = lines(&[
            json!({ "type": "permission-mode", "permissionMode": "bypassPermissions" }),
            json!({ "type": "user", "timestamp": "2026-10-02T19:46:31.613Z", "isSidechain": false,
                "message": { "role": "user", "content": "The crew-bridge plugin sent a message:\n[crew] Message from orchestrator (type task, id m-1):\n\nStress-test the design.\n\nThis is how Claude Code surfaces a prompt a plugin submits between turns — it starts this turn in the user's place. Address the message above." } }),
            json!({ "type": "assistant", "timestamp": "2026-10-02T19:46:40.000Z",
                "message": { "role": "assistant", "content": [{ "type": "thinking", "thinking": "hm" }] } }),
            json!({ "type": "assistant", "timestamp": "2026-10-02T19:46:41.000Z",
                "message": { "role": "assistant", "content": [
                    { "type": "text", "text": "I'll read the proposal first." },
                    { "type": "tool_use", "name": "Read", "input": { "file_path": "/work/README.md" } },
                ] } }),
            json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "content": "# readme" }] } }),
            json!({ "type": "assistant", "message": { "role": "assistant", "content": [
                { "type": "tool_use", "name": "Bash", "input": { "command": "cargo test\n  --workspace", "description": "run" } },
                { "type": "tool_use", "name": "mcp__crew__agent_send", "input": { "to": "skeptic", "body": "done" } },
                { "type": "tool_use", "name": "mcp__crew__agents_list", "input": {} },
            ] } }),
            // Not part of the conversation: a subagent's turn, a meta line,
            // what the harness told itself, the summary a compaction left.
            json!({ "type": "assistant", "isSidechain": true, "message": { "content": [{ "type": "text", "text": "subagent" }] } }),
            json!({ "type": "user", "isMeta": true, "message": { "content": [{ "type": "text", "text": "meta" }] } }),
            json!({ "type": "user", "message": { "content": "<local-command-stdout>ok</local-command-stdout>" } }),
            json!({ "type": "system", "subtype": "compact_boundary", "timestamp": "2026-10-02T20:00:00Z" }),
            json!({ "type": "user", "isCompactSummary": true, "message": { "content": "This session is being continued…" } }),
            json!({ "type": "user", "message": { "content": [{ "type": "text", "text": "<pasted_content id=\"0cb4\">\n[crew] and now the tests\n</pasted_content>" }] } }),
        ]);
        let entries = claude_entries(&format!("not json\n{text}"));
        assert_eq!(
            kinds(&entries),
            [
                (TranscriptKind::Prompt, "[crew] Message from orchestrator (type task, id m-1):\n\nStress-test the design."),
                (TranscriptKind::Reply, "I'll read the proposal first."),
                (TranscriptKind::Tool, "Read  /work/README.md"),
                (TranscriptKind::Tool, "Bash  cargo test --workspace"),
                (TranscriptKind::Tool, "crew.agent_send  {\"body\":\"done\",\"to\":\"skeptic\"}"),
                (TranscriptKind::Tool, "crew.agents_list"),
                (TranscriptKind::Note, "context compacted"),
                (TranscriptKind::Prompt, "[crew] and now the tests"),
            ]
        );
        assert_eq!(entries[0].at, Some(1_790_970_391_613));
        assert_eq!(entries[7].at, None);
    }

    #[test]
    fn codex_sessions_give_prompts_replies_and_tool_calls() {
        let item = |payload: Value| json!({ "timestamp": "2026-10-02T19:46:31.613Z", "type": "response_item", "payload": payload });
        let text = lines(&[
            json!({ "type": "session_meta", "payload": { "id": "t" } }),
            item(
                json!({ "type": "message", "role": "developer", "content": [{ "type": "input_text", "text": "rules" }] }),
            ),
            item(
                json!({ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "<environment_context>cwd</environment_context>" }] }),
            ),
            item(
                json!({ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "[crew] Review the report." }] }),
            ),
            item(json!({ "type": "reasoning", "summary": [] })),
            item(
                json!({ "type": "custom_tool_call", "name": "exec", "input": "\nconst r = await tools.exec_command({cmd: \"ls\"});\ntext(r);" }),
            ),
            item(json!({ "type": "custom_tool_call_output", "output": "files" })),
            item(
                json!({ "type": "function_call", "name": "shell", "arguments": "{\"command\":[\"git\",\"status\"]}" }),
            ),
            item(
                json!({ "type": "local_shell_call", "action": { "command": ["cargo", "check"] } }),
            ),
            json!({ "type": "event_msg", "payload": { "type": "token_count" } }),
            // A call to crew's MCP server is recorded only as an item.
            json!({ "type": "event_msg", "payload": { "type": "item_completed", "item": {
                "type": "McpToolCall", "server": "crew", "tool": "agent_send", "arguments": { "to": "lead", "body": "done" } } } }),
            json!({ "type": "event_msg", "payload": { "type": "item_completed", "item": { "type": "Reasoning" } } }),
            json!({ "type": "compacted", "timestamp": "2026-10-02T20:00:00Z", "payload": { "message": "…" } }),
            item(
                json!({ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "Approved." }] }),
            ),
        ]);
        let entries = codex_entries(&text);
        assert_eq!(
            kinds(&entries),
            [
                (TranscriptKind::Prompt, "[crew] Review the report."),
                (
                    TranscriptKind::Tool,
                    "exec  const r = await tools.exec_command({cmd: \"ls\"});"
                ),
                (TranscriptKind::Tool, "shell  git status"),
                (TranscriptKind::Tool, "shell  cargo check"),
                (
                    TranscriptKind::Tool,
                    "crew.agent_send  {\"body\":\"done\",\"to\":\"lead\"}"
                ),
                (TranscriptKind::Note, "context compacted"),
                (TranscriptKind::Reply, "Approved."),
            ]
        );
        assert_eq!(entries[0].at, Some(1_790_970_391_613));
    }

    #[test]
    fn pi_sessions_give_prompts_replies_and_tool_calls() {
        let message = |value: Value| json!({ "type": "message", "timestamp": "2026-10-02T19:46:31.613Z", "message": value });
        let text = lines(&[
            json!({ "type": "session", "version": 3, "id": "s", "cwd": "/w" }),
            json!({ "type": "model_change", "provider": "anthropic", "modelId": "claude-opus-4-6" }),
            json!({ "type": "thinking_level_change", "thinkingLevel": "high" }),
            message(
                json!({ "role": "user", "content": [{ "type": "text", "text": "[crew] Review the report." }] }),
            ),
            message(json!({ "role": "assistant", "content": [
                { "type": "thinking", "text": "hm" },
                { "type": "text", "text": "Reading it." },
                { "type": "toolCall", "name": "read", "args": "{\"path\":\"report.md\"}" },
            ], "usage": { "input": 1, "output": 2, "cacheRead": 3, "cacheWrite": 0, "totalTokens": 6 } })),
            message(
                json!({ "role": "toolResult", "toolCallId": "c1", "content": [{ "type": "text", "text": "# Report" }] }),
            ),
            message(json!({ "role": "assistant", "content": [
                { "type": "toolCall", "name": "bash", "arguments": { "command": "cargo check" } },
            ] })),
            json!({ "type": "compaction", "summary": "…", "tokensBefore": 100000 }),
            message(json!({ "role": "user", "content": "plain text prompt" })),
            message(
                json!({ "role": "assistant", "content": [{ "type": "text", "text": "Approved." }] }),
            ),
        ]);
        let entries = pi_entries(&text);
        assert_eq!(
            kinds(&entries),
            [
                (TranscriptKind::Prompt, "[crew] Review the report."),
                (TranscriptKind::Reply, "Reading it."),
                (TranscriptKind::Tool, "read  report.md"),
                (TranscriptKind::Tool, "bash  cargo check"),
                (TranscriptKind::Note, "context compacted"),
                (TranscriptKind::Prompt, "plain text prompt"),
                (TranscriptKind::Reply, "Approved."),
            ]
        );
        assert_eq!(entries[0].at, Some(1_790_970_391_613));
    }

    #[test]
    fn only_the_newest_entries_are_returned_and_long_ones_are_cut() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let long = "x".repeat(MAX_ENTRY_CHARS + 25);
        let mut values: Vec<Value> = (0..10)
            .map(|i| json!({ "type": "user", "message": { "content": format!("prompt {i}") } }))
            .collect();
        values.push(json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": long }] } }));
        std::fs::write(&path, lines(&values)).unwrap();

        let all = read(&path, RuntimeKind::Claude, 100);
        assert!(all.available && !all.truncated);
        assert_eq!(all.entries.len(), 11);
        let last = &all.entries[10].text;
        assert!(
            last.ends_with("\n[26 more characters]"),
            "{}",
            &last[last.len() - 40..]
        );

        let newest = read(&path, RuntimeKind::Claude, 3);
        assert!(newest.truncated);
        assert_eq!(newest.entries.len(), 3);
        assert_eq!(newest.entries[0].text, "prompt 8");

        // A shell agent has no session file to speak of; a missing file is
        // simply no transcript.
        assert!(read(&path, RuntimeKind::Shell, 10).entries.is_empty());
        let none = read(&dir.path().join("missing.jsonl"), RuntimeKind::Claude, 10);
        assert!(!none.available && none.entries.is_empty());
        // A tool's line is kept short.
        assert!(
            tool_line("Bash", &json!({ "command": "y".repeat(500) }))
                .chars()
                .count()
                <= MAX_TOOL_CHARS
        );
    }
}
