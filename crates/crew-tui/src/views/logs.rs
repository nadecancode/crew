//! The structured event stream, scrollable, optionally filtered to one agent.

use crew_core::types::Event;
use crew_core::util::{fmt_clock, one_line};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::app::App;
use crate::theme::Theme;
use crate::widgets::canvas::{fill_row, put_str};
use crate::widgets::text::{fit, pad_right, width};

const HINTS: &str =
    "esc back · ↑↓ PgUp PgDn scroll · Home/End · f filter to selected agent · t tasks · q quit";

/// Events shown in the log view, oldest first.
pub fn visible_events(app: &App) -> Vec<&Event> {
    match app.logs.filter_agent.as_deref() {
        Some(agent) => app
            .events
            .iter()
            .filter(|e| e.agent_id.as_deref() == Some(agent))
            .collect(),
        None => app.events.iter().collect(),
    }
}

/// Number of event rows the log view shows for a terminal of this height.
pub fn page_rows(height: u16) -> usize {
    usize::from(height.saturating_sub(2))
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    if area.height < 3 || area.width < 10 {
        return;
    }
    let x = i32::from(area.x) + 1;
    let events = visible_events(app);
    let rows = page_rows(area.height);
    let max_scroll = events.len().saturating_sub(rows);
    let scroll = app.logs.scroll.min(max_scroll);

    let mut title = String::from("event log");
    if let Some(agent) = &app.logs.filter_agent {
        title.push_str(&format!(" · {agent}"));
    }
    put_str(
        buf,
        area,
        x,
        i32::from(area.y),
        &title,
        theme.bold(theme.text),
    );
    let state = if scroll == 0 {
        format!("{} events · following", events.len())
    } else {
        format!("{} events · ↑{scroll} (End to follow)", events.len())
    };
    put_str(
        buf,
        area,
        x + width(&title) as i32 + 2,
        i32::from(area.y),
        &state,
        theme.fg(if scroll == 0 { theme.muted } else { theme.warn }),
    );

    let footer_y = area.bottom() - 1;
    fill_row(buf, area, footer_y, Style::default().bg(theme.bar_bg));
    put_str(
        buf,
        area,
        x,
        i32::from(footer_y),
        &fit(HINTS, usize::from(area.width).saturating_sub(2)),
        Style::default().fg(theme.dim).bg(theme.bar_bg),
    );

    if events.is_empty() {
        put_str(
            buf,
            area,
            x,
            i32::from(area.y) + 2,
            "no events",
            theme.fg(theme.dim),
        );
        return;
    }
    let end = events.len() - scroll;
    let start = end.saturating_sub(rows);
    let source_w = events[start..end]
        .iter()
        .map(|e| width(&e.source))
        .max()
        .unwrap_or(6)
        .clamp(6, 10);
    let type_w = events[start..end]
        .iter()
        .map(|e| width(&e.event_type))
        .max()
        .unwrap_or(10)
        .clamp(10, 22);
    let agent_w = events[start..end]
        .iter()
        .filter_map(|e| e.agent_id.as_deref())
        .map(width)
        .max()
        .unwrap_or(0)
        .min(14);

    for (i, event) in events[start..end].iter().enumerate() {
        let y = i32::from(area.y) + 1 + i as i32;
        let level = theme.level_color(event.level);
        let mut cx = x;
        put_str(
            buf,
            area,
            cx,
            y,
            &fmt_clock(event.timestamp),
            theme.fg(theme.dim),
        );
        cx += 14;
        put_str(
            buf,
            area,
            cx,
            y,
            &pad_right(event.level.as_str(), 5),
            theme.bold(level),
        );
        cx += 7;
        put_str(
            buf,
            area,
            cx,
            y,
            &pad_right(&event.source, source_w),
            theme.fg(theme.muted),
        );
        cx += source_w as i32 + 2;
        put_str(
            buf,
            area,
            cx,
            y,
            &pad_right(&event.event_type, type_w),
            theme.fg(theme.orch),
        );
        cx += type_w as i32 + 2;
        if agent_w > 0 {
            let agent = event.agent_id.as_deref().unwrap_or("");
            put_str(
                buf,
                area,
                cx,
                y,
                &pad_right(agent, agent_w),
                theme.fg(theme.name_color(&app.agents, agent)),
            );
            cx += agent_w as i32 + 2;
        }
        let room = (i32::from(area.right()) - 1 - cx).max(0) as usize;
        let color = match event.level {
            crew_core::types::EventLevel::Warn | crew_core::types::EventLevel::Error => level,
            _ => theme.text,
        };
        put_str(
            buf,
            area,
            cx,
            y,
            &fit(&one_line(&event.message), room),
            theme.fg(color),
        );
    }
}
