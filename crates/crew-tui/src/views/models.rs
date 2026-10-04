//! The model picker: what an agent runs on, chosen by the operator.
//!
//! ```text
//! ┌ orchestrator · model and CLI ─────────────────────────────┐
//! │                                                           │
//! │  1 ✱ Claude Code ●    2 ❂ Codex    3 π pi                 │
//! │                                                           │
//! │  ● Fable 5.1   frontier  $10 / $50 per MTok           now │
//! │ ▸  Opus 5.5    strong    $4 / $20 per MTok                │
//! │    Sonnet 5.5  balanced  $2 / $10 per MTok                │
//! │                                                           │
//! │ For long-running agentic coding and knowledge work. …     │
//! │                                                           │
//! │ same CLI: the session is restarted on the model, resumed… │
//! │ ←→ CLI · ↑↓ model · enter switch · esc cancel             │
//! └───────────────────────────────────────────────────────────┘
//! ```
//!
//! A CLI other than the agent's own starts it a fresh session there (a
//! session cannot move between CLIs), handed where the work stands.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Widget};

use super::help::centered;
use super::question::wrap;
use crate::app::{App, ModelPicker, HARNESSES};
use crate::theme::{runtime_glyph, runtime_label, Theme};
use crate::widgets::canvas::put_str;
use crate::widgets::text::{fit, pad_right, width};

const KEYS: &str = "←→ CLI · ↑↓ model · enter switch · esc cancel";
/// Most model rows shown at once; more scroll.
const MAX_ROWS: usize = 12;
/// Most lines of the highlighted model's description (or why it cannot be
/// started here).
const SUMMARY_LINES: usize = 4;

pub fn render(buf: &mut Buffer, area: Rect, app: &App, picker: &ModelPicker, theme: &Theme) {
    let outer_w = area.width.saturating_sub(4).min(78);
    if outer_w < 30 || area.height < 10 {
        return;
    }
    let cols = usize::from(outer_w) - 4;
    let rows = app.model_rows(picker.runtime);
    let current = app.current_row(&picker.agent, picker.runtime);
    let agent = app.agent(&picker.agent);
    let own = agent.map(|a| a.runtime);
    let highlighted = rows.get(picker.row).copied();
    let summary = highlighted
        .map(|m| match &m.unavailable {
            Some(why) => format!("Cannot be started here: {why}"),
            None => m.summary.clone(),
        })
        .map(|text| wrap(&text, cols).into_iter().take(SUMMARY_LINES).collect::<Vec<_>>())
        .unwrap_or_default();

    // Fixed rows: blank, tabs, blank, (list), blank, (summary and a blank
    // after it), consequence, keys; plus the borders.
    let fixed = 3 + 1 + usize::from(!summary.is_empty()) + 2 + 2;
    let room = usize::from(area.height).saturating_sub(fixed + summary.len()).max(1);
    let visible = rows.len().clamp(1, MAX_ROWS).min(room);
    let height = fixed + summary.len() + visible;
    let rect = centered(area, outer_w, height as u16);
    Clear.render(rect, buf);
    let title = match agent {
        Some(agent) => format!(" {} · model and CLI ", agent.id),
        None => " model and CLI ".to_string(),
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme.fg(theme.muted))
        .title(Span::styled(fit(&title, cols), theme.bold(theme.text)))
        .render(rect, buf);

    let x = i32::from(rect.x) + 2;
    let mut y = i32::from(rect.y) + 2;

    // One tab per CLI; the agent's own is marked.
    let mut tx = x;
    for (i, runtime) in HARNESSES.iter().enumerate() {
        let mark = if Some(*runtime) == own { " ●" } else { "" };
        let label = format!("{} {} {}{mark}", i + 1, runtime_glyph(*runtime), runtime_label(*runtime));
        let style = if *runtime == picker.runtime {
            theme.bold(theme.runtime_color(*runtime)).add_modifier(ratatui::style::Modifier::REVERSED)
        } else {
            theme.fg(theme.muted)
        };
        put_str(buf, rect, tx, y, &format!(" {label} "), style);
        tx += width(&label) as i32 + 4;
    }
    y += 2;

    // The models of the chosen CLI.
    if rows.is_empty() {
        let text = if app.models_pending {
            "asking the daemon what can be started here…".to_string()
        } else if let Some(e) = &app.models_error {
            format!("the models could not be listed: {e}")
        } else {
            format!("no {} models are known", runtime_label(picker.runtime))
        };
        let style = if app.models_error.is_some() { theme.fg(theme.err) } else { theme.fg(theme.dim) };
        put_str(buf, rect, x, y, &fit(&text, cols), style);
        y += 1;
    } else {
        let first = picker.row.saturating_sub(visible - 1).min(rows.len() - visible);
        let name_w = rows.iter().map(|m| width(&m.name)).max().unwrap_or(0).clamp(8, 30);
        let tier_w = rows.iter().map(|m| width(&m.tier)).max().unwrap_or(0);
        for (i, model) in rows.iter().enumerate().skip(first).take(visible) {
            let selected = i == picker.row;
            let now = current == Some(i);
            let cursor = if selected { "▸" } else { " " };
            let mark = if now { "●" } else { " " };
            let mut line = format!("{cursor}{mark} {}", pad_right(&fit(&model.name, name_w), name_w));
            if tier_w > 0 {
                line.push_str(&format!("  {}", pad_right(&model.tier, tier_w)));
            }
            let detail = match &model.unavailable {
                Some(_) => "not here".to_string(),
                None => model.price.clone(),
            };
            if !detail.is_empty() {
                line.push_str(&format!("  {detail}"));
            }
            let tail = if now { "now" } else { "" };
            let line_room = cols.saturating_sub(width(tail) + 1);
            let style = match (&model.unavailable, selected) {
                (Some(_), true) => theme.bold(theme.muted),
                (Some(_), false) => theme.fg(theme.dim),
                (None, true) => theme.bold(theme.accent),
                (None, false) => theme.fg(theme.text),
            };
            put_str(buf, rect, x, y, &fit(&line, line_room), style);
            if now {
                let at = x + cols as i32 - width(tail) as i32;
                put_str(buf, rect, at, y, tail, theme.fg(theme.ok));
            }
            y += 1;
        }
        // Rows above or below the window.
        if rows.len() > visible {
            let note = format!("{}/{}", picker.row + 1, rows.len());
            let at = i32::from(rect.right()) - 3 - width(&note) as i32;
            put_str(buf, rect, at, i32::from(rect.bottom()) - 1, &note, theme.fg(theme.dim));
        }
    }
    y += 1;

    if !summary.is_empty() {
        let style = if highlighted.is_some_and(|m| m.unavailable.is_some()) {
            theme.fg(theme.warn)
        } else {
            theme.fg(theme.muted)
        };
        for line in &summary {
            put_str(buf, rect, x, y, line, style);
            y += 1;
        }
        y += 1;
    }

    // What choosing it does, which differs a lot between the two cases.
    let (consequence, style) = match &picker.notice {
        Some(notice) => (notice.clone(), theme.fg(theme.warn)),
        None if Some(picker.runtime) == own => (
            "same CLI: the session is restarted on the model, resumed where it was".to_string(),
            theme.fg(theme.dim),
        ),
        None => (
            format!(
                "another CLI: a fresh {} session there, handed where the work stands",
                runtime_label(picker.runtime)
            ),
            theme.fg(theme.warn),
        ),
    };
    put_str(buf, rect, x, y, &fit(&consequence, cols), style);
    y += 1;
    put_str(buf, rect, x, y, &fit(KEYS, cols), theme.fg(theme.dim));
}
