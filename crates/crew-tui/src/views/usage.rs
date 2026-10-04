//! The usage strip at the top of the dashboard: how much of each account's
//! usage windows is spent, and which agents are paused at a limit.
//!
//! The bars form a grid: one row per runtime, one column per kind of window
//! (session, week, …), so the same window lines up across runtimes and every
//! bar starts, ends and is labelled at the same place. Columns that do not
//! fit beside the others on the screen wrap onto a further row of the grid,
//! so nothing is cut off. A runtime with paused agents gets a row of its own
//! above its bars saying what happened, when it resets, when crew resumes
//! it, and the keys to continue now.

use crew_core::rpc::Quota;
use crew_core::types::{Agent, RuntimeKind};
use crew_core::util::fmt_elapsed;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::app::App;
use crate::theme::{runtime_glyph, Theme};
use crate::widgets::canvas::{fill_row, put_str};
use crate::widgets::text::{fit, width};

/// The bar is as wide as the row allows, within these bounds.
const BAR_MIN: usize = 6;
const BAR_MAX: usize = 24;
/// Left-aligned partial blocks, for a bar edge that is exact to an eighth of
/// a cell.
const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
const RUNTIMES: [RuntimeKind; 3] = [RuntimeKind::Claude, RuntimeKind::Codex, RuntimeKind::Pi];

/// One runtime's row.
struct Row<'a> {
    runtime: RuntimeKind,
    quotas: Vec<&'a Quota>,
    paused: Vec<&'a Agent>,
}

fn rows(app: &App) -> Vec<Row<'_>> {
    RUNTIMES
        .into_iter()
        .map(|runtime| Row {
            runtime,
            quotas: app.session.quotas.iter().filter(|q| q.runtime == runtime).collect(),
            paused: app
                .agents
                .iter()
                .filter(|a| a.runtime == runtime && a.limit.is_some())
                .collect(),
        })
        .filter(|row| !row.quotas.is_empty() || !row.paused.is_empty())
        .collect()
}

/// The strip has a cell's margin on either side.
fn strip_width(area: Rect) -> usize {
    usize::from(area.width).saturating_sub(2)
}

/// How many rows the strip needs on a screen of `area` (0: nothing to show,
/// or no room to spare).
pub fn height(app: &App, area: Rect) -> u16 {
    if area.height < 14 || area.width < 40 {
        return 0;
    }
    let rows = rows(app);
    let layout = layout(&rows, strip_width(area));
    rows.iter()
        .map(|row| usize::from(!row.paused.is_empty()) + layout.grid_rows(row).len())
        .sum::<usize>() as u16
}

/// One column of the grid: a kind of window, and whether its reset time is
/// shown. Room is kept for the reset text at its widest, so the grid does
/// not shift when a reset that was "tomorrow" becomes "today".
struct Column {
    label: String,
    resets: bool,
}

/// A figure older than this is drawn muted: it may no longer be true.
const STALE_MS: i64 = 20 * 60_000;
/// Columns of text in a grid cell besides the bar itself.
const USED_WIDTH: usize = 5;
const GAP: usize = 3;
/// `  ↻ 13d23h`: the reset text at its widest.
const RESETS_WIDTH: usize = 10;
/// `✱ claude `: the runtime's glyph and name, in its colour.
const NAME_WIDTH: usize = 9;

impl Column {
    /// What a cell of the column takes besides its bar.
    fn fixed(&self) -> usize {
        let resets = if self.resets { RESETS_WIDTH } else { 0 };
        width(&self.label) + 1 + USED_WIDTH + resets + GAP
    }
}

/// The windows the runtimes report, session first, then week, then the rest.
fn columns(rows: &[Row<'_>]) -> Vec<Column> {
    let mut columns: Vec<Column> = Vec::new();
    for quota in rows.iter().flat_map(|row| row.quotas.iter()) {
        let label = window_label(&quota.window);
        let resets = quota.resets_at.is_some();
        match columns.iter_mut().find(|c| c.label == label) {
            Some(column) => column.resets |= resets,
            None => columns.push(Column { label, resets }),
        }
    }
    columns.sort_by_key(|c| match c.label.as_str() {
        "session" => 0,
        "week" => 1,
        _ => 2,
    });
    columns
}

/// The grid's shape on a strip of a given width: its columns, the grid row
/// each sits in, and the width of every bar.
struct Layout {
    columns: Vec<Column>,
    /// The grid row of each column: a column that does not fit beside the
    /// ones before it starts the next row.
    row_of: Vec<usize>,
    cells: usize,
}

fn layout(rows: &[Row<'_>], width: usize) -> Layout {
    let columns = columns(rows);
    let room = width.saturating_sub(NAME_WIDTH);
    let mut row_of = Vec::with_capacity(columns.len());
    let (mut row, mut used) = (0, 0);
    for column in &columns {
        let needed = column.fixed() + BAR_MIN;
        if used > 0 && used + needed > room {
            row += 1;
            used = 0;
        }
        used += needed;
        row_of.push(row);
    }
    // Every bar is the same width: what the fullest row leaves, shared out.
    let cells = (0..=row)
        .map(|r| {
            let in_row: Vec<&Column> = columns
                .iter()
                .zip(&row_of)
                .filter(|(_, at)| **at == r)
                .map(|(column, _)| column)
                .collect();
            let fixed: usize = in_row.iter().map(|c| c.fixed()).sum();
            room.saturating_sub(fixed) / in_row.len().max(1)
        })
        .min()
        .unwrap_or(BAR_MAX)
        .clamp(BAR_MIN, BAR_MAX);
    Layout { columns, row_of, cells }
}

impl Layout {
    fn column_of(&self, window: &str) -> Option<usize> {
        let label = window_label(window);
        self.columns.iter().position(|c| c.label == label)
    }

    /// The grid rows a runtime's windows fall in, in order.
    fn grid_rows(&self, row: &Row<'_>) -> Vec<usize> {
        let mut rows: Vec<usize> = row
            .quotas
            .iter()
            .filter_map(|q| self.column_of(&q.window))
            .map(|c| self.row_of[c])
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    }
}

/// Whether any agent is paused at a usage limit.
pub fn any_paused(app: &App) -> bool {
    app.agents.iter().any(|a| a.limit.is_some())
}

/// The fill colour: the bar's own until the window is nearly spent.
fn level_color(theme: &Theme, percent: f32) -> Color {
    match percent {
        p if p >= 90.0 => theme.err,
        p if p >= 70.0 => theme.warn,
        _ => theme.gauge,
    }
}

/// What a window is called on screen.
/// `session`, `week`, or a model's own weekly window as `Fable week`.
fn window_label(window: &str) -> String {
    match window {
        "5h" => "session".to_string(),
        other => match other.strip_prefix("week/") {
            Some(model) => format!("{model} week"),
            None => other.to_string(),
        },
    }
}

/// A bar of `cells` cells: how many are full, the partial block at the edge
/// (empty if the edge falls between cells), and how many are track.
pub fn bar_parts(percent: f32, cells: usize) -> (usize, &'static str, usize) {
    let total = cells * 8;
    let mut filled = ((percent.clamp(0.0, 100.0) / 100.0) * total as f32).round() as usize;
    // Anything spent is visible.
    if percent > 0.0 {
        filled = filled.max(1);
    }
    let full = filled / 8;
    let partial = EIGHTHS[filled % 8];
    (full, partial, cells - full - usize::from(!partial.is_empty()))
}

/// How long until a reset, short: `45m`, `2h14m`, `1d14h`; never wider than
/// six columns (`99d+` beyond that: no usage window is that long).
pub fn until(ms: i64) -> String {
    let minutes = (ms.max(0) + 59_999) / 60_000;
    let hours = minutes / 60;
    match (hours / 24, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h{:02}m", minutes % 60),
        (days, _) if days > 99 => "99d+".to_string(),
        (days, _) => format!("{days}d{}h", hours % 24),
    }
}

/// The text beside a bar: how much is used, `33%`, and how long until the
/// window resets, `↻ 2h14m`.
fn gauge_text(quota: &Quota, now: i64) -> (String, String) {
    // Right-aligned, so the figures line up down a column.
    let used = format!(" {:>3.0}%", quota.percent_used.max(0.0));
    let resets = match quota.resets_at.filter(|at| *at > now) {
        Some(at) => format!("  ↻ {}", until(at - now)),
        None => String::new(),
    };
    (used, resets)
}

/// `session ████████▌        33%  ↻ 2h14m`: a solid bar on
/// a darker track, as wide as `cells`. A figure nobody has reported for a
/// while is muted instead of bold.
fn gauge(quota: &Quota, cells: usize, theme: &Theme, now: i64) -> Vec<(String, Style)> {
    let percent = quota.percent_used.max(0.0);
    let (full, partial, track) = bar_parts(percent, cells);
    let fill = Style::default().fg(level_color(theme, percent)).bg(theme.gauge_track);
    let (used, resets) = gauge_text(quota, now);
    vec![
        (format!("{} ", window_label(&quota.window)), theme.fg(theme.muted)),
        ("█".repeat(full), fill),
        (partial.to_string(), fill),
        (" ".repeat(track), Style::default().bg(theme.gauge_track)),
        (
            used,
            if now - quota.updated_at > STALE_MS { theme.fg(theme.muted) } else { theme.bold(theme.text) },
        ),
        (resets, theme.fg(theme.dim)),
    ]
}

/// `⊘ usage limit reached · 2 paused (a, b) · resets in 1h58m · resumes in 1h59m · u continue now`
fn paused_text(paused: &[&Agent], now: i64) -> String {
    let limits: Vec<_> = paused.iter().filter_map(|a| a.limit.as_ref()).collect();
    let what = match limits.first().map(|l| l.kind.as_str()) {
        Some("credits") => "out of credits",
        Some("auth") => "sign-in needed",
        _ => "usage limit reached",
    };
    let ids: Vec<&str> = paused.iter().map(|a| a.id.as_str()).collect();
    let mut text = format!("⊘ {what} · {} paused ({})", ids.len(), ids.join(", "));
    if let Some(at) = limits.iter().filter_map(|l| l.resets_at).min() {
        text.push_str(&format!(" · resets in {}", until(at - now)));
    }
    match limits.iter().filter_map(|l| l.retry_at).min() {
        Some(at) if at > now => text.push_str(&format!(" · resumes in {}", fmt_elapsed(at - now))),
        Some(_) => text.push_str(" · resuming"),
        None => text.push_str(" · waiting for you"),
    }
    text.push_str(" · u continue now · U restart on a new login");
    text
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    // The figures carry the daemon's times.
    let now = app.server_now(now);
    let rows = rows(app);
    let layout = layout(&rows, strip_width(area));
    let cells = layout.cells;
    let left = i32::from(area.x) + 1;
    let right = i32::from(area.right()) - 1;

    let bar = Style::default().bg(theme.bar_bg);
    let mut y = area.y;
    let mut line = |buf: &mut Buffer, parts: Vec<(String, Style)>| {
        if y >= area.bottom() {
            return;
        }
        fill_row(buf, area, y, bar);
        let mut x = left;
        for (text, style) in parts {
            let text = fit(&text, (right - x).max(0) as usize);
            // The strip's background, unless the cell brings its own (a bar).
            let style = if style.bg.is_none() { style.bg(theme.bar_bg) } else { style };
            put_str(buf, area, x, i32::from(y), &text, style);
            x += width(&text) as i32;
        }
        y += 1;
    };

    for row in &rows {
        let name = (
            format!("{:<NAME_WIDTH$}", format!("{} {}", runtime_glyph(row.runtime), row.runtime)),
            theme.bold(theme.runtime_color(row.runtime)),
        );
        if !row.paused.is_empty() {
            let color = match row.paused[0].limit.as_ref().map(|l| l.kind.as_str()) {
                Some("auth") | Some("credits") => theme.err,
                _ => theme.warn,
            };
            line(buf, vec![name.clone(), (paused_text(&row.paused, now), theme.bold(color))]);
        }
        // One line per grid row the runtime has a window in; the name is
        // on the first.
        for (n, grid_row) in layout.grid_rows(row).into_iter().enumerate() {
            let mut parts = vec![if n == 0 {
                name.clone()
            } else {
                (" ".repeat(NAME_WIDTH), Style::default())
            }];
            for (column, _) in layout.columns.iter().zip(&layout.row_of).filter(|(_, at)| **at == grid_row) {
                match row.quotas.iter().find(|q| window_label(&q.window) == column.label) {
                    Some(quota) => {
                        let mut gauge = gauge(quota, cells, theme, now);
                        // Pad the reset text to the column, then the gap.
                        let resets = if column.resets { RESETS_WIDTH } else { 0 };
                        let pad = resets.saturating_sub(width(&gauge[5].0)) + GAP;
                        gauge.push((" ".repeat(pad), Style::default()));
                        parts.extend(gauge);
                    }
                    // This runtime has no such window: leave its column empty.
                    None => parts.push((" ".repeat(column.fixed() + cells), Style::default())),
                }
            }
            line(buf, parts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use crate::widgets::canvas::buffer_text;
    use crew_core::types::UsageLimit;

    const NOW: i64 = 1_790_960_400_000;

    fn app() -> App {
        let mut app = App::new();
        app.apply_snapshot(fixture::snapshot(NOW), NOW);
        app
    }

    fn quota(runtime: RuntimeKind, window: &str, percent: f32, resets_in: Option<i64>) -> Quota {
        Quota {
            runtime,
            window: window.to_string(),
            percent_used: percent,
            resets_at: resets_in.map(|ms| NOW + ms),
            updated_at: NOW,
        }
    }

    fn draw(app: &App, w: u16, h: u16) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(&mut buf, area, app, &Theme::truecolor(), NOW);
        buffer_text(&buf)
    }

    #[test]
    fn nothing_known_takes_no_room() {
        let app = app();
        assert_eq!(height(&app, Rect::new(0, 0, 120, 40)), 0);
        assert!(!any_paused(&app));
    }

    #[test]
    fn gauges_show_each_window_of_each_account() {
        let mut app = app();
        app.session.quotas = vec![
            quota(RuntimeKind::Claude, "5h", 62.4, Some(3_600_000)),
            quota(RuntimeKind::Claude, "week", 21.0, None),
            quota(RuntimeKind::Codex, "week", 96.0, Some(86_400_000 * 3)),
        ];
        assert_eq!(height(&app, Rect::new(0, 0, 120, 40)), 2);
        // No room on a small screen: the agents matter more.
        assert_eq!(height(&app, Rect::new(0, 0, 120, 12)), 0);
        // The bar as plain text: full blocks, the partial block at its
        // edge, and the track (a background colour, so blank here).
        let plain = |percent: f32, cells: usize| {
            let (full, partial, track) = bar_parts(percent, cells);
            format!("{}{partial}{}", "█".repeat(full), " ".repeat(track))
        };
        let text = draw(&app, 150, 2);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with(" ✱ claude session █"), "{text}");
        // A wide screen gives each bar its full width.
        assert!(lines[0].contains(&format!("session {}  62%  ↻ 1h00m", plain(62.4, BAR_MAX))), "{text}");
        assert!(lines[0].contains(&format!("week {}  21%", plain(21.0, BAR_MAX))), "{text}");
        assert!(lines[1].contains(&format!("week {}  96%  ↻ 3d0h", plain(96.0, BAR_MAX))), "{text}");
        // The grid: Codex has no session window, so that column stays empty
        // and its week bar sits right under Claude's.
        let column = |line: &str, what: &str| line.chars().collect::<String>().find(what).map(|at| width(&line[..at]));
        assert!(lines[1].starts_with(" ❂ codex   "), "{text}");
        assert_eq!(column(lines[0], "week "), column(lines[1], "week "), "{text}");
        assert!(column(lines[0], "week ").is_some());
        // The figures are right-aligned, so they end in the same column too.
        let week = |line: &str| column(&line[line.find("week ").unwrap()..], "%");
        assert_eq!(week(lines[0]), week(lines[1]), "{text}");
        // The edge of the bar is exact to an eighth of a cell.
        assert_eq!(bar_parts(50.0, 10), (5, "", 5));
        assert_eq!(bar_parts(33.0, 24), (7, "▉", 16));
        assert_eq!(bar_parts(100.0, 24), (24, "", 0));
        assert_eq!(bar_parts(140.0, 8), (8, "", 0), "an exceeded spend limit is a full bar");
        // A sliver of use is visible; nothing used is all track.
        assert_eq!(bar_parts(0.2, 10), (0, "▏", 9));
        assert_eq!(bar_parts(0.0, 10), (0, "", 10));
        // The fill is the bar's own colour until the window is nearly spent,
        // on a darker track.
        let theme = Theme::truecolor();
        assert_eq!(level_color(&theme, 62.0), theme.gauge);
        assert_eq!(level_color(&theme, 75.0), theme.warn);
        assert_eq!(level_color(&theme, 96.0), theme.err);
        let parts = gauge(&quota(RuntimeKind::Claude, "5h", 33.0, None), 24, &theme, NOW);
        assert_eq!(parts[4].1, theme.bold(theme.text));
        // A figure from long ago may no longer be true: it is muted.
        let mut old = quota(RuntimeKind::Claude, "week/Fable", 72.0, None);
        old.updated_at = NOW - 8 * 3_600_000;
        assert_eq!(gauge(&old, 24, &theme, NOW)[4].1, theme.fg(theme.muted));
        assert_eq!(parts[0].0, "session ");
        assert_eq!((parts[1].1.fg, parts[1].1.bg), (Some(theme.gauge), Some(theme.gauge_track)));
        assert_eq!((parts[3].0.len(), parts[3].1.bg), (16, Some(theme.gauge_track)));
        assert_eq!(parts[4].0, "  33%");
        // How long until a reset, at a glance.
        assert_eq!((until(30_000), until(45 * 60_000), until(134 * 60_000), until(38 * 3_600_000)), ("1m".into(), "45m".into(), "2h14m".into(), "1d14h".into()));
        assert_eq!(until(400 * 86_400_000), "99d+");
        // With less room the bars shrink together, down to a minimum. Each
        // column keeps room for the reset text at its widest (10), so the
        // grid stays put as the countdown shortens.
        let narrower = draw(&app, 90, 2);
        let cells = (90 - 2 - 9 - (7 + 1 + 5 + 10 + 3) - (4 + 1 + 5 + 10 + 3)) / 2;
        assert!(narrower.lines().next().unwrap().contains(&format!("session {}  62%", plain(62.4, cells))), "{narrower}");
        let tiny = draw(&app, 40, 3);
        assert!(tiny.lines().next().unwrap().contains(&format!("session {}  62%", plain(62.4, BAR_MIN))), "{tiny}");
        // Narrow screens cut the row off; they never overflow it.
        assert!(draw(&app, 44, 2).lines().all(|l| width(l) <= 44));
    }

    #[test]
    fn windows_that_do_not_fit_beside_the_others_wrap_onto_a_further_row() {
        let mut app = app();
        app.session.quotas = vec![
            quota(RuntimeKind::Claude, "5h", 1.0, Some(3_600_000)),
            quota(RuntimeKind::Claude, "week", 74.0, Some(86_400_000)),
            quota(RuntimeKind::Claude, "week/Fable", 5.0, Some(86_400_000)),
            quota(RuntimeKind::Codex, "week", 62.0, Some(86_400_000 * 3)),
        ];
        // Three windows do not fit on 100 columns with their countdowns:
        // the model's week goes under Claude's other two, and Codex's week
        // still sits under Claude's.
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(height(&app, area), 3);
        let text = draw(&app, 100, 3);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with(" ✱ claude session ") && lines[0].contains(" week "), "{text}");
        assert!(!lines[0].contains("Fable"), "{text}");
        assert!(lines[1].starts_with("          Fable week ") && lines[1].contains("   5%  ↻ 1d0h"), "{text}");
        assert!(lines[2].starts_with(" ❂ codex   ") && lines[2].contains(" week "), "{text}");
        let column = |line: &str, what: &str| line.find(what).map(|at| width(&line[..at]));
        assert_eq!(column(lines[0], "week "), column(lines[2], "week "), "{text}");
        assert!(lines.iter().all(|l| width(l) <= 100 && !l.contains('…')), "{text}");
        // Every bar is the same width, on every row: what the fullest row
        // leaves its two columns.
        let plain = |percent: f32, cells: usize| {
            let (full, partial, track) = bar_parts(percent, cells);
            format!("{}{partial}{}", "█".repeat(full), " ".repeat(track))
        };
        let cells = (100 - 2 - 9 - (7 + 1 + 5 + 10 + 3) - (4 + 1 + 5 + 10 + 3)) / 2;
        assert!(lines[0].contains(&format!("session {}   1%", plain(1.0, cells))), "{text}");
        assert!(lines[1].contains(&format!("Fable week {}   5%", plain(5.0, cells))), "{text}");
        assert!(lines[2].contains(&format!("week {}  62%", plain(62.0, cells))), "{text}");

        // Where there is room they share one row.
        let wide = Rect::new(0, 0, 200, 40);
        assert_eq!(height(&app, wide), 2);
        let text = draw(&app, 200, 2);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains("Fable week ") && lines[0].contains("   5%  ↻ 1d0h"), "{text}");
        assert!(lines[1].starts_with(" ❂ codex   "), "{text}");
        assert_eq!(column(lines[0], "week "), column(lines[1], "week "), "{text}");
    }

    #[test]
    fn paused_agents_lead_their_runtimes_row() {
        let mut app = app();
        let mut park = |id: &str, kind: &str, resets: Option<i64>, retry: Option<i64>| {
            let agent = app.agents.iter_mut().find(|a| a.id == id).expect("fixture agent");
            agent.limit = Some(UsageLimit {
                kind: kind.to_string(),
                message: "You've hit your usage limit.".to_string(),
                since: NOW - 60_000,
                resets_at: resets.map(|ms| NOW + ms),
                retry_at: retry.map(|ms| NOW + ms),
                attempts: 0,
            });
        };
        park("claude-1", "usage", Some(7_140_000), Some(7_200_000));
        park("claude-3", "usage", None, Some(9_000_000));
        app.session.quotas = vec![quota(RuntimeKind::Claude, "5h", 100.0, Some(7_140_000))];
        assert!(any_paused(&app));
        // The notice has a row to itself, above the runtime's bars.
        assert_eq!(height(&app, Rect::new(0, 0, 200, 40)), 2);
        let text = draw(&app, 200, 2);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with(" ✱ claude ⊘ usage limit reached · 2 paused (claude-1, claude-3) · resets "), "{text}");
        assert!(lines[0].ends_with(" · resumes in 2h00m · u continue now · U restart on a new login"), "{text}");
        assert!(lines[1].starts_with(" ✱ claude session ██████") && lines[1].contains("█ 100%  ↻ 1h59m"), "{text}");

        // A login problem does not lift by waiting.
        let mut app = self::app();
        let agent = app.agents.iter_mut().find(|a| a.id == "claude-2").unwrap();
        agent.limit = Some(UsageLimit {
            kind: "auth".to_string(),
            ..UsageLimit::default()
        });
        let text = draw(&app, 160, 1);
        assert!(text.contains("⊘ sign-in needed · 1 paused (claude-2) · waiting for you"), "{text}");
    }
}
