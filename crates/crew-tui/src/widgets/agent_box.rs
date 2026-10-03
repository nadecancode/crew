//! The boxes of the topology view: one per worker, plus the orchestrator hub.

use crew_core::types::{Agent, AgentStatus};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crew_core::util::{fmt_elapsed, one_line};

use super::canvas::put_str;
use super::text::{fit, pad_right, width};
use crate::app::WorkOn;
use crate::theme::{agent_icon, runtime_glyph, Theme};
use crate::views::usage::bar_parts;

/// Progress shown for an agent: its reported percentage, or 100 once done.
pub fn progress_of(agent: &Agent) -> Option<u8> {
    if agent.status == AgentStatus::Done {
        Some(100)
    } else {
        agent.progress.map(|p| p.min(100))
    }
}

/// `◐  51%` when progress is known, `○ waiting` otherwise.
pub fn status_tail(agent: &Agent) -> String {
    match progress_of(agent) {
        Some(p) => format!("{p:>3}%"),
        None if agent.paused => "paused".to_string(),
        None => agent.status.as_str().to_string(),
    }
}

fn border_style(agent: &Agent, selected: bool, theme: &Theme) -> Style {
    if selected {
        theme.bold(theme.text)
    } else {
        match agent.status {
            AgentStatus::Failed => theme.fg(theme.err),
            AgentStatus::Blocked => theme.fg(theme.warn),
            _ => theme.fg(theme.dim),
        }
    }
}

/// Top border with an inline title: `┌─ ✱ claude-1 ──────┐`. `reserve`
/// cells at the right end are kept free of the title (for a badge).
fn top_border(
    buf: &mut Buffer,
    clip: Rect,
    rect: Rect,
    border: Style,
    glyph: (&str, Style),
    title: (&str, Style),
    reserve: usize,
) {
    let (x, y, w) = (
        i32::from(rect.x),
        i32::from(rect.y),
        usize::from(rect.width),
    );
    let line = format!("┌{}┐", "─".repeat(w.saturating_sub(2)));
    put_str(buf, clip, x, y, &line, border);
    // `┌─ ✱ name ─┐` needs two border cells and two spaces around the title.
    let room = w.saturating_sub(7 + reserve);
    if room == 0 {
        return;
    }
    let name = fit(title.0, room);
    put_str(buf, clip, x + 2, y, " ", border);
    put_str(buf, clip, x + 3, y, glyph.0, glyph.1);
    put_str(buf, clip, x + 4, y, " ", border);
    put_str(buf, clip, x + 5, y, &name, title.1);
    put_str(buf, clip, x + 5 + width(&name) as i32, y, " ", border);
}

/// Bottom border doubling as a progress bar: the first `percent` of it is
/// drawn heavy in `bar` colour.
fn bottom_border(
    buf: &mut Buffer,
    clip: Rect,
    rect: Rect,
    border: Style,
    bar: Style,
    percent: Option<u8>,
) {
    let (x, w) = (i32::from(rect.x), usize::from(rect.width));
    let y = i32::from(rect.bottom()) - 1;
    let inner = w.saturating_sub(2);
    let filled = match percent {
        Some(p) => {
            let cells = (usize::from(p.min(100)) * inner + 50) / 100;
            // Any progress at all shows at least one cell.
            if p > 0 {
                cells.max(1)
            } else {
                0
            }
        }
        None => 0,
    };
    put_str(buf, clip, x, y, "└", border);
    put_str(buf, clip, x + 1, y, &"━".repeat(filled), bar);
    put_str(
        buf,
        clip,
        x + 1 + filled as i32,
        y,
        &"─".repeat(inner - filled.min(inner)),
        border,
    );
    put_str(buf, clip, x + w as i32 - 1, y, "┘", border);
}

/// What an agent runs on, for the bottom border of its box: `Opus 5.5 · high`,
/// in the longest form that fits `room` columns; `↓ Opus 5.5 · high` when
/// that is not the model the member was given. `None` when the runtime has
/// not said yet, or nothing fits.
pub fn running_label(agent: &Agent, room: usize) -> Option<String> {
    let model = agent.model_label().map(|model| {
        if agent.fallback.is_some() {
            format!("↓ {model}")
        } else {
            model
        }
    });
    let effort = agent.effort.as_deref().filter(|e| !e.is_empty());
    let forms = match (model, effort) {
        (Some(model), Some(effort)) => {
            // A long level has a short spelling for a narrow box.
            let short = if effort == "medium" { "med" } else { effort };
            vec![
                format!("{model} · {effort}"),
                format!("{model} · {short}"),
                format!("{model}·{effort}"),
                format!("{model}·{short}"),
                model,
            ]
        }
        (Some(model), None) => vec![model],
        (None, Some(effort)) => vec![effort.to_string()],
        (None, None) => return None,
    };
    forms
        .iter()
        .find(|form| width(form) <= room)
        .cloned()
        // Not even the model's name fits: as much of it as does.
        .or_else(|| (room >= 5).then(|| fit(forms.last().map_or("", String::as_str), room)))
}

/// The running label is quiet, unless the session is off its model.
fn running_style(agent: &Agent, theme: &Theme) -> Style {
    if agent.fallback.is_some() {
        theme.bold(theme.warn)
    } else {
        theme.fg(theme.muted)
    }
}

/// The cells of the bottom border a label may use: between the corners,
/// less the badge. The label has a space each side.
fn label_span(rect: Rect, reserved: usize) -> usize {
    usize::from(rect.width).saturating_sub(2 + reserved)
}

/// Whether the bottom border of `rect` carries what the agent runs on. It
/// is then a plain line: the few cells a label leaves would show stray
/// pieces of the progress bar, in a different colour from box to box.
fn labelled(rect: Rect, agent: &Agent, reserved: usize) -> bool {
    running_label(agent, label_span(rect, reserved).saturating_sub(2)).is_some()
}

/// Write what the agent runs on into the middle of the bottom border of
/// `rect`, the line running on at both ends: `└──── Opus 5.5 · high ────┘`.
/// `reserved` columns at the right end are left for a badge.
/// `mark` goes in front of the label when there is room for both: the
/// hub's CLI glyph, `└── ✱ Fable 5.1 · high ──┘`.
#[allow(clippy::too_many_arguments)]
fn border_label(
    buf: &mut Buffer,
    clip: Rect,
    rect: Rect,
    agent: &Agent,
    reserved: usize,
    border: Style,
    style: Style,
    mark: Option<(&str, Style)>,
) {
    let span = label_span(rect, reserved);
    let with_mark = mark.and_then(|(glyph, glyph_style)| {
        let label = running_label(agent, span.saturating_sub(3 + width(glyph)))?;
        Some((label, Some((glyph, glyph_style))))
    });
    let Some((label, mark)) =
        with_mark.or_else(|| Some((running_label(agent, span.saturating_sub(2))?, None)))
    else {
        return;
    };
    let mark_w = mark.map_or(0, |(glyph, _)| width(glyph) + 1);
    let taken = width(&label) + 2 + mark_w;
    let x = i32::from(rect.x) + 1 + (span.saturating_sub(taken) / 2) as i32;
    let y = i32::from(rect.bottom()) - 1;
    put_str(buf, clip, x, y, " ", border);
    if let Some((glyph, glyph_style)) = mark {
        put_str(buf, clip, x + 1, y, glyph, glyph_style);
        put_str(buf, clip, x + 1 + width(glyph) as i32, y, " ", border);
    }
    put_str(buf, clip, x + 1 + mark_w as i32, y, &label, style);
    put_str(
        buf,
        clip,
        x + 1 + (mark_w + width(&label)) as i32,
        y,
        " ",
        border,
    );
}

/// A context window's size, short: `1M`, `200k`, `258k`.
pub fn window_size(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let tenths = (tokens + 50_000) / 100_000;
        if tenths % 10 == 0 {
            format!("{}M", tenths / 10)
        } else {
            format!("{}.{}M", tenths / 10, tenths % 10)
        }
    } else if tokens >= 1000 {
        format!("{}k", tokens / 1000)
    } else {
        tokens.to_string()
    }
}

/// The row of a box that shows how full the agent's context window is: the
/// window's size, a bar, the share used.
///
/// ```text
/// │ 1M ████░░░░░░  35% │
/// ```
fn context_row(
    buf: &mut Buffer,
    clip: Rect,
    rect: Rect,
    row: i32,
    agent: Option<&Agent>,
    border: Style,
    theme: &Theme,
) {
    side_borders(buf, clip, rect, row, border);
    let inner = usize::from(rect.width).saturating_sub(4);
    let x = i32::from(rect.x) + 2;
    let Some(context) = agent.and_then(|a| a.context.as_ref()) else {
        // The runtime has not said (or never does: a shell agent).
        put_str(
            buf,
            clip,
            x,
            row,
            &fit("context —", inner),
            theme.fg(theme.dim),
        );
        return;
    };
    if context.window == 0 {
        // The runtime gave the tokens in use but not the window's size.
        let text = format!("{} tokens", window_size(context.tokens));
        put_str(buf, clip, x, row, &fit(&text, inner), theme.fg(theme.muted));
        return;
    }
    let size = window_size(context.window);
    let percent = format!("{:>3}%", context.percent);
    let color = match context.percent {
        90.. => theme.err,
        75..=89 => theme.warn,
        _ => theme.gauge,
    };
    // The size, a space, the bar, a space, the figure.
    let cells = inner.saturating_sub(width(&size) + 2 + width(&percent));
    if cells < 3 {
        let text = format!("{size} {}", percent.trim_start());
        put_str(buf, clip, x, row, &fit(&text, inner), theme.fg(color));
        return;
    }
    put_str(buf, clip, x, row, &size, theme.fg(theme.muted));
    let bar_x = x + width(&size) as i32 + 1;
    let (full, partial, track) = bar_parts(f32::from(context.percent), cells);
    let fill = Style::default().fg(color).bg(theme.gauge_track);
    put_str(buf, clip, bar_x, row, &"█".repeat(full), fill);
    put_str(buf, clip, bar_x + full as i32, row, partial, fill);
    let track_x = bar_x + (cells - track) as i32;
    put_str(
        buf,
        clip,
        track_x,
        row,
        &" ".repeat(track),
        Style::default().bg(theme.gauge_track),
    );
    put_str(
        buf,
        clip,
        bar_x + cells as i32 + 1,
        row,
        &percent,
        theme.fg(theme.text),
    );
}

fn side_borders(buf: &mut Buffer, clip: Rect, rect: Rect, row: i32, border: Style) {
    let x = i32::from(rect.x);
    let inner = usize::from(rect.width).saturating_sub(2);
    put_str(buf, clip, x, row, "│", border);
    put_str(buf, clip, x + 1, row, &" ".repeat(inner), Style::default());
    put_str(buf, clip, x + i32::from(rect.width) - 1, row, "│", border);
}

/// How a worker box is dressed beyond the agent's own state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decor<'a> {
    /// The task the agent is on, or the one it finished last.
    pub work: Option<WorkOn<'a>>,
    /// The box has a row, under the task, for what the agent is doing now.
    pub activity_row: bool,
    /// How long since the agent last did anything, for an idle one.
    pub idle_for: Option<i64>,
    /// The box is a subagent orbiting its parent: titled `↳ id`.
    pub satellite: bool,
    /// Subagents of this agent that are not drawn as satellites (`+N`).
    pub hidden_subagents: usize,
    /// One of those hidden subagents is the selected agent.
    pub hidden_selected: bool,
    /// A question of this agent's is waiting for the operator.
    pub asking: bool,
}

/// How what an agent is doing reads: under way, at rest, or wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Live,
    Quiet,
    Warn,
    Bad,
}

/// What the agent is doing right now, as `(what, for how long, tone)`: the
/// tool it is running (the runtimes report each call) and how long it has
/// been on its task; else its state and how long it has been in it.
pub fn doing(agent: &Agent, decor: &Decor<'_>) -> (String, String, Tone) {
    let elapsed = |ms: Option<i64>| ms.map(fmt_elapsed).unwrap_or_default();
    let under_way = decor.work.filter(|w| !w.done).and_then(|w| w.elapsed);
    if agent.paused {
        return ("paused".into(), String::new(), Tone::Warn);
    }
    let activity = one_line(&agent.activity);
    match agent.status {
        AgentStatus::Spawning => ("starting…".into(), String::new(), Tone::Quiet),
        AgentStatus::Working => {
            let what = if activity.is_empty() {
                "working".to_string()
            } else {
                activity
            };
            (format!("▸ {what}"), elapsed(under_way), Tone::Live)
        }
        AgentStatus::Blocked => {
            let what = if activity.is_empty() {
                agent.block_kind.clone().unwrap_or_else(|| "blocked".into())
            } else {
                activity
            };
            (what, String::new(), Tone::Warn)
        }
        AgentStatus::Failed => {
            let what = agent
                .error
                .as_deref()
                .map(one_line)
                .unwrap_or_else(|| "failed".into());
            (what, String::new(), Tone::Bad)
        }
        AgentStatus::Waiting => ("waiting".into(), elapsed(decor.idle_for), Tone::Quiet),
        AgentStatus::Idle => ("idle".into(), elapsed(decor.idle_for), Tone::Quiet),
        AgentStatus::Done => {
            let ago = decor.work.filter(|w| w.done).and_then(|w| w.elapsed);
            (
                "✓ finished".into(),
                ago.map(|ms| format!("{} ago", fmt_elapsed(ms)))
                    .unwrap_or_default(),
                Tone::Quiet,
            )
        }
        AgentStatus::Stopped => ("stopped".into(), String::new(), Tone::Quiet),
    }
}

/// What the first row of a box says: the task the agent is on, or the one it
/// finished last, else its role; and whether that is current.
pub fn work_line<'a>(agent: &'a Agent, decor: &Decor<'a>) -> (&'a str, bool) {
    match (decor.work, agent.task.as_deref()) {
        (Some(work), _) => (work.title, !work.done),
        (None, Some(task)) => (task, true),
        (None, None) if agent.role.is_empty() => ("—", false),
        (None, None) => (agent.role.as_str(), false),
    }
}

/// The badge of a worker box: `temp` for a temporary member, `⑂N` for
/// in-session subagents running inside the agent, `+N` for subagents that
/// have no satellite box of their own.
pub fn badge(agent: &Agent, decor: &Decor) -> String {
    let mut parts: Vec<String> = Vec::new();
    if agent.temporary {
        parts.push("temp".to_string());
    }
    if agent.subagents > 0 {
        parts.push(format!("⑂{}", agent.subagents));
    }
    if decor.hidden_subagents > 0 {
        parts.push(format!("+{}", decor.hidden_subagents));
    }
    parts.join(" ")
}

/// A worker box:
///
/// ```text
/// ┌─ ✱ claude-1 ─────── ⑂2 ─┐
/// │ Write the parser  ◐  51% │
/// │ ▸ Edit parser.rs      4m │
/// │ 1M ███▌           35%    │
/// └──── Sonnet 5.5 · high ───┘
/// ```
///
/// The first row is the task the agent is on (its role while it has none);
/// the rows under it, when the box has them, what it is doing right now and
/// how full its context window is.
///
/// The glyph before the name and the name's colour are the runtime's: `✱`
/// for Claude Code, `❂` for Codex, `$` for a shell agent.
#[allow(clippy::too_many_arguments)]
pub fn draw_agent(
    buf: &mut Buffer,
    clip: Rect,
    rect: Rect,
    agent: &Agent,
    selected: bool,
    decor: &Decor,
    theme: &Theme,
    now: i64,
) {
    if rect.width < 8 || rect.height < 3 {
        return;
    }
    let border = border_style(agent, selected, theme);
    // Glyph and colour say which CLI the agent runs on.
    let (glyph, own) = theme.member_mark(agent.runtime, decor.satellite);
    let name_style = if selected {
        theme.bold(theme.text)
    } else {
        theme.bold(own)
    };
    // The badge sits at the right end of the top border when the id still
    // fits beside it, and on the bottom border otherwise; either way the
    // content row is left alone.
    let w = usize::from(rect.width);
    let badge = badge(agent, decor);
    let badge_w = width(&badge);
    let badge_style = if decor.hidden_selected {
        theme.bold(theme.text)
    } else {
        theme.fg(theme.muted)
    };
    let id_room_with_badge = w.saturating_sub(7 + badge_w + 3);
    let badge_on_top = badge_w > 0 && id_room_with_badge >= width(&agent.id).min(6);
    top_border(
        buf,
        clip,
        rect,
        border,
        (glyph, theme.bold(own)),
        (&agent.id, name_style),
        if badge_on_top { badge_w + 3 } else { 0 },
    );
    let badge_x = i32::from(rect.right()) - 3 - badge_w as i32;
    if badge_on_top {
        put_str(buf, clip, badge_x - 1, i32::from(rect.y), " ", border);
        put_str(buf, clip, badge_x, i32::from(rect.y), &badge, badge_style);
        put_str(
            buf,
            clip,
            badge_x + badge_w as i32,
            i32::from(rect.y),
            " ",
            border,
        );
    }

    let row = i32::from(rect.y) + 1;
    side_borders(buf, clip, rect, row, border);
    let inner = usize::from(rect.width) - 4;
    let icon = if decor.asking {
        "?"
    } else {
        agent_icon(agent.status, agent.paused, now)
    };
    let mut tail = status_tail(agent);
    if 1 + 1 + width(&tail) + 2 > inner {
        tail.clear();
    }
    let right_w = 1 + if tail.is_empty() { 0 } else { 1 + width(&tail) };
    let work_w = inner.saturating_sub(right_w + 1);
    let (work, current) = work_line(agent, decor);
    let x = i32::from(rect.x) + 2;
    put_str(
        buf,
        clip,
        x,
        row,
        &pad_right(&fit(work, work_w), work_w),
        if current {
            theme.fg(theme.text)
        } else {
            theme.fg(theme.muted)
        },
    );
    let icon_x = x + (inner - right_w) as i32;
    let color = if agent.paused || decor.asking {
        theme.warn
    } else {
        theme.state_color(agent)
    };
    put_str(buf, clip, icon_x, row, icon, theme.bold(color));
    if !tail.is_empty() {
        put_str(buf, clip, icon_x + 2, row, &tail, theme.fg(theme.muted));
    }

    // A taller box has a row for what the agent is doing, then one for the
    // context window.
    let extra = rect.height.saturating_sub(3);
    let mut next = row + 1;
    if decor.activity_row && extra >= 1 {
        side_borders(buf, clip, rect, next, border);
        let (what, how_long, tone) = doing(agent, decor);
        let how_long_w = width(&how_long);
        let what_w = if how_long_w == 0 {
            inner
        } else {
            inner.saturating_sub(how_long_w + 1)
        };
        let style = match tone {
            Tone::Live => theme.fg(theme.text),
            Tone::Quiet => theme.fg(theme.muted),
            Tone::Warn => theme.fg(theme.warn),
            Tone::Bad => theme.fg(theme.err),
        };
        put_str(buf, clip, x, next, &fit(&what, what_w), style);
        if how_long_w > 0 && how_long_w < inner {
            put_str(
                buf,
                clip,
                x + (inner - how_long_w) as i32,
                next,
                &how_long,
                theme.fg(theme.muted),
            );
        }
        next += 1;
    }
    if extra > u16::from(decor.activity_row) {
        context_row(buf, clip, rect, next, Some(agent), border, theme);
    }

    let bar_color = if agent.status == AgentStatus::Failed {
        theme.err
    } else {
        own
    };
    // What the agent runs on sits in the bottom border, beside a badge
    // that had to go there too. Without it the border is the progress bar
    // (the figure is in the row above either way).
    let badge_below = badge_w > 0 && !badge_on_top && w >= badge_w + 6;
    let reserved = if badge_below { badge_w + 3 } else { 0 };
    let progress = if labelled(rect, agent, reserved) {
        None
    } else {
        progress_of(agent)
    };
    bottom_border(buf, clip, rect, border, theme.fg(bar_color), progress);
    border_label(
        buf,
        clip,
        rect,
        agent,
        reserved,
        border,
        running_style(agent, theme),
        None,
    );
    if badge_w > 0 && !badge_on_top && w >= badge_w + 6 {
        let y = i32::from(rect.bottom()) - 1;
        put_str(buf, clip, badge_x - 1, y, " ", border);
        put_str(buf, clip, badge_x, y, &badge, badge_style);
        put_str(buf, clip, badge_x + badge_w as i32, y, " ", border);
    }
}

/// What the hub shows about the session as a whole.
pub struct HubState<'a> {
    pub orchestrator: Option<&'a Agent>,
    pub done: usize,
    pub total: usize,
    pub complete: bool,
    pub selected: bool,
    /// Questions waiting for the operator's answer.
    pub questions: usize,
}

/// The orchestrator hub:
///
/// ```text
/// ┌─ ◆ orchestrator ─────┐
/// │ status · 4/6         │
/// │ ○ waiting            │
/// └── Fable 5.1 · high ──┘
/// ```
pub fn draw_orchestrator(
    buf: &mut Buffer,
    clip: Rect,
    rect: Rect,
    hub: &HubState<'_>,
    theme: &Theme,
    now: i64,
) {
    if rect.width < 8 || rect.height < 4 {
        return;
    }
    // A team that waits for the operator says so where they look first.
    let border = if hub.selected {
        theme.bold(theme.text)
    } else if hub.questions > 0 {
        theme.fg(theme.warn)
    } else {
        theme.fg(theme.dim)
    };
    top_border(
        buf,
        clip,
        rect,
        border,
        ("◆", theme.bold(theme.orch)),
        ("orchestrator", theme.bold(theme.orch)),
        0,
    );
    let x = i32::from(rect.x) + 2;
    let inner = usize::from(rect.width) - 4;

    let row = i32::from(rect.y) + 1;
    side_borders(buf, clip, rect, row, border);
    put_str(buf, clip, x, row, "status", theme.fg(theme.orch));
    put_str(buf, clip, x + 6, row, " · ", theme.fg(theme.dim));
    let count = fit(
        &format!("{}/{}", hub.done, hub.total),
        inner.saturating_sub(9),
    );
    put_str(buf, clip, x + 9, row, &count, theme.bold(theme.text));

    let row = row + 1;
    side_borders(buf, clip, rect, row, border);
    let (icon, label, color) = if hub.questions > 0 {
        let label = match hub.questions {
            1 => "your answer needed".to_string(),
            n => format!("{n} answers needed"),
        };
        ("?", label, theme.warn)
    } else if hub.complete {
        ("✓", "all tasks done".to_string(), theme.ok)
    } else {
        match hub.orchestrator {
            Some(agent) => {
                let label = if agent.paused {
                    "paused".to_string()
                } else {
                    agent.status.as_str().to_string()
                };
                (
                    agent_icon(agent.status, agent.paused, now),
                    label,
                    theme.state_color(agent),
                )
            }
            None => ("·", "scheduler".to_string(), theme.muted),
        }
    };
    put_str(buf, clip, x, row, icon, theme.bold(color));
    let label_style = if hub.questions > 0 {
        theme.bold(theme.warn)
    } else {
        theme.fg(theme.muted)
    };
    put_str(
        buf,
        clip,
        x + 2,
        row,
        &fit(&label, inner.saturating_sub(2)),
        label_style,
    );

    if rect.height > 4 {
        context_row(buf, clip, rect, row + 1, hub.orchestrator, border, theme);
    }

    let percent = if hub.total == 0 {
        None
    } else {
        Some((hub.done * 100 / hub.total) as u8)
    };
    let percent = percent.filter(|_| {
        !hub.orchestrator
            .is_some_and(|agent| labelled(rect, agent, 0))
    });
    bottom_border(buf, clip, rect, border, theme.fg(theme.orch), percent);
    if let Some(agent) = hub.orchestrator {
        // Which CLI the orchestrator is on, as the members' boxes say it.
        let mark = (
            runtime_glyph(agent.runtime),
            theme.bold(theme.runtime_color(agent.runtime)),
        );
        border_label(
            buf,
            clip,
            rect,
            agent,
            0,
            border,
            running_style(agent, theme),
            Some(mark),
        );
    }
}
