//! Laying an agent's session transcript out in lines.
//!
//! The inspector shows the live terminal; scrolled up, it shows what the
//! agent was told, wrote and did, as its runtime recorded it. This turns
//! those entries into the lines of a pane of a given width.

use crew_core::types::{TranscriptEntry, TranscriptKind};
use crew_core::util::fmt_clock;

use crate::views::question::wrap;
use crate::widgets::text::{fit, width};

/// What a line of the laid-out transcript is, which decides its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The rule above a prompt, or a note on the session.
    Rule,
    Prompt,
    Reply,
    Tool,
    Blank,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TLine {
    pub part: Part,
    pub text: String,
}

fn line(part: Part, text: impl Into<String>) -> TLine {
    TLine {
        part,
        text: text.into(),
    }
}

/// `── prompt · 15:22:45 ─────`, as wide as the pane.
fn rule(label: &str, at: Option<i64>, cols: usize) -> String {
    let at = at
        .map(|at| format!(" · {}", &fmt_clock(at)[..8]))
        .unwrap_or_default();
    let head = fit(&format!("── {label}{at} "), cols);
    let fill = cols.saturating_sub(width(&head));
    format!("{head}{}", "─".repeat(fill))
}

/// Wrap to `cols`, keeping line breaks and the indentation of each line
/// (code and lists stay readable).
fn wrap_text(text: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let raw = raw.trim_end().replace('\t', "    ");
        if width(&raw) <= cols {
            out.push(raw);
            continue;
        }
        let indent: String = raw.chars().take_while(|c| *c == ' ').collect();
        let indent = if indent.len() * 2 > cols {
            String::new()
        } else {
            indent
        };
        for part in wrap(raw.trim_start(), cols - indent.len()) {
            out.push(format!("{indent}{part}"));
        }
    }
    out
}

/// The transcript as lines at most `cols` wide, oldest first.
pub fn layout(entries: &[TranscriptEntry], truncated: bool, cols: usize) -> Vec<TLine> {
    let cols = cols.max(8);
    let mut lines = Vec::new();
    if truncated {
        lines.push(line(
            Part::Rule,
            fit("── earlier entries are not shown", cols),
        ));
    }
    let mut last: Option<TranscriptKind> = None;
    for entry in entries {
        // The tool calls of a turn stay together; everything else is set
        // apart by a blank line.
        let together = last == Some(TranscriptKind::Tool) && entry.kind == TranscriptKind::Tool;
        if !lines.is_empty() && !together {
            lines.push(line(Part::Blank, ""));
        }
        match entry.kind {
            TranscriptKind::Prompt => {
                lines.push(line(Part::Rule, rule("prompt", entry.at, cols)));
                lines.extend(
                    wrap_text(&entry.text, cols)
                        .into_iter()
                        .map(|t| line(Part::Prompt, t)),
                );
            }
            TranscriptKind::Reply => {
                lines.extend(
                    wrap_text(&entry.text, cols)
                        .into_iter()
                        .map(|t| line(Part::Reply, t)),
                );
            }
            TranscriptKind::Tool => {
                lines.push(line(Part::Tool, fit(&format!("▸ {}", entry.text), cols)));
            }
            TranscriptKind::Note => {
                lines.push(line(Part::Rule, rule(&entry.text, entry.at, cols)));
            }
        }
        last = Some(entry.kind);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: TranscriptKind, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            kind,
            at: None,
            text: text.to_string(),
        }
    }

    #[test]
    fn a_transcript_is_laid_out_turn_by_turn() {
        let entries = [
            entry(
                TranscriptKind::Prompt,
                "[crew] Review the report and say what is wrong with it.",
            ),
            entry(TranscriptKind::Reply, "Reading it first.\n\n    let x = 1;"),
            entry(TranscriptKind::Tool, "Read  /work/report.md"),
            entry(
                TranscriptKind::Tool,
                "Bash  cargo test --workspace --all-targets -- --nocapture",
            ),
            entry(TranscriptKind::Note, "context compacted"),
            entry(TranscriptKind::Reply, "Approved."),
        ];
        let lines = layout(&entries, true, 30);
        let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            text,
            [
                "── earlier entries are not sh…",
                "",
                "── prompt ────────────────────",
                "[crew] Review the report and",
                "say what is wrong with it.",
                "",
                "Reading it first.",
                "",
                "    let x = 1;",
                "",
                "▸ Read  /work/report.md",
                "▸ Bash  cargo test --workspac…",
                "",
                "── context compacted ─────────",
                "",
                "Approved.",
            ]
        );
        assert!(lines.iter().all(|l| width(&l.text) <= 30), "{lines:?}");
        assert_eq!(lines[3].part, Part::Prompt);
        assert_eq!(lines[10].part, Part::Tool);
        assert_eq!(lines[13].part, Part::Rule);

        // A prompt's time is in its rule; an indented line that must wrap
        // keeps its indent.
        let timed = [TranscriptEntry {
            kind: TranscriptKind::Prompt,
            at: Some(1_790_970_391_613),
            text: "  - one two three four five six seven".to_string(),
        }];
        let lines = layout(&timed, false, 24);
        assert!(
            lines[0].text.starts_with("── prompt · ") && lines[0].text.ends_with("──"),
            "{lines:?}"
        );
        assert_eq!(lines[1].text, "  - one two three four");
        assert_eq!(lines[2].text, "  five six seven");
        assert!(layout(&[], false, 40).is_empty());
    }
}
