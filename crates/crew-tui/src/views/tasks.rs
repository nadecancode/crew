//! The task graph, laid out in dependency layers.

use crew_core::types::{Task, TaskStatus};
use crew_core::util::one_line;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::app::{task_layers, App};
use crate::theme::{task_icon, Theme};
use crate::widgets::canvas::{fill_row, put_str};
use crate::widgets::text::{fit, pad_left, pad_right, width};

const HINTS: &str = "esc back · ↑↓ scroll · l logs · q quit";

enum Row<'a> {
    Layer(usize, usize),
    Task(&'a Task),
    Note(&'a Task),
    Blank,
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    if area.height < 3 || area.width < 10 {
        return;
    }
    let x = i32::from(area.x) + 1;
    let y = i32::from(area.y);
    let (done, total) = app.task_counts();
    put_str(buf, area, x, y, "task graph", theme.bold(theme.text));
    let counts = summary(app);
    put_str(
        buf,
        area,
        x + 12,
        y,
        &fit(
            &format!("{done}/{total} done{counts}"),
            usize::from(area.width).saturating_sub(14),
        ),
        theme.fg(theme.muted),
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

    let body = Rect {
        y: area.y + 1,
        height: area.height - 2,
        ..area
    };
    if app.tasks.is_empty() {
        put_str(
            buf,
            body,
            x,
            i32::from(body.y) + 1,
            "no tasks yet",
            theme.fg(theme.dim),
        );
        return;
    }

    let layers = task_layers(&app.tasks);
    let mut rows: Vec<Row<'_>> = Vec::new();
    for (depth, layer) in layers.iter().enumerate() {
        rows.push(Row::Blank);
        rows.push(Row::Layer(depth, layer.len()));
        for &i in layer {
            let task = &app.tasks[i];
            rows.push(Row::Task(task));
            if task.note.is_some()
                && matches!(task.status, TaskStatus::Blocked | TaskStatus::Failed)
            {
                rows.push(Row::Note(task));
            }
        }
    }

    let id_w = app
        .tasks
        .iter()
        .map(|t| width(&t.id))
        .max()
        .unwrap_or(4)
        .clamp(4, 24);
    // A subagent is named together with the agent it works for.
    let assignee = |task: &Task| -> Option<String> {
        let id = task.assigned_agent.as_deref()?;
        Some(match app.agent(id).and_then(|a| a.parent.as_deref()) {
            Some(parent) => format!("{id} (sub of {parent})"),
            None => id.to_string(),
        })
    };
    let agent_w = app
        .tasks
        .iter()
        .filter_map(assignee)
        .map(|a| width(&a))
        .max()
        .unwrap_or(10)
        .clamp(10, 36);

    let visible = usize::from(body.height);
    let scroll = app.tasks_scroll.min(rows.len().saturating_sub(visible));
    for (i, row) in rows.iter().skip(scroll).take(visible).enumerate() {
        let y = i32::from(body.y) + i as i32;
        match row {
            Row::Blank => {}
            Row::Layer(depth, count) => {
                let label = if *depth == 0 {
                    format!("layer 0 · no dependencies · {count}")
                } else {
                    format!("layer {depth} · {count}")
                };
                put_str(buf, body, x, y, &label, theme.fg(theme.dim));
            }
            Row::Task(task) => {
                let who = assignee(task);
                let who = who.as_deref().map(|id| (id, theme.name_color(&app.agents, id)));
                task_row(buf, body, x, y, task, who, id_w, agent_w, theme, now)
            }
            Row::Note(task) => {
                let note = format!("    ↳ {}", one_line(task.note.as_deref().unwrap_or("")));
                put_str(
                    buf,
                    body,
                    x,
                    y,
                    &fit(&note, usize::from(body.width).saturating_sub(2)),
                    theme.fg(theme.task_color(task.status)),
                );
            }
        }
    }
}

fn summary(app: &App) -> String {
    let mut out = String::new();
    for status in [
        TaskStatus::Working,
        TaskStatus::Review,
        TaskStatus::Ready,
        TaskStatus::Queued,
        TaskStatus::Waiting,
        TaskStatus::Blocked,
        TaskStatus::Failed,
    ] {
        let n = app.tasks.iter().filter(|t| t.status == status).count();
        if n > 0 {
            out.push_str(&format!(" · {n} {status}"));
        }
    }
    out
}

/// `  ◐ billing   claude-1    working   51%  ← ledger, queue  review: claude-2  ⬡1`
#[allow(clippy::too_many_arguments)]
fn task_row(
    buf: &mut Buffer,
    clip: Rect,
    x: i32,
    y: i32,
    task: &Task,
    assignee: Option<(&str, Color)>,
    id_w: usize,
    agent_w: usize,
    theme: &Theme,
    now: i64,
) {
    let color = theme.task_color(task.status);
    let mut cx = x + 2;
    put_str(
        buf,
        clip,
        cx,
        y,
        task_icon(task.status, now),
        theme.bold(color),
    );
    cx += 2;
    put_str(
        buf,
        clip,
        cx,
        y,
        &pad_right(&task.id, id_w),
        theme.bold(theme.text),
    );
    cx += id_w as i32 + 2;
    let (assignee, assignee_color) = assignee.unwrap_or(("unassigned", theme.dim));
    put_str(
        buf,
        clip,
        cx,
        y,
        &pad_right(assignee, agent_w),
        theme.fg(assignee_color),
    );
    cx += agent_w as i32 + 2;
    put_str(
        buf,
        clip,
        cx,
        y,
        &pad_right(task.status.as_str(), 8),
        theme.fg(color),
    );
    cx += 10;
    let percent = match (task.status, task.progress) {
        (TaskStatus::Done, _) => "100%".to_string(),
        (_, Some(p)) => format!("{}%", p.min(100)),
        _ => String::new(),
    };
    put_str(
        buf,
        clip,
        cx,
        y,
        &pad_left(&percent, 4),
        theme.fg(theme.muted),
    );
    cx += 6;

    let mut rest = String::new();
    if !task.dependencies.is_empty() {
        rest.push_str(&format!("← {}", task.dependencies.join(", ")));
    }
    if let Some(reviewer) = &task.reviewer {
        if !rest.is_empty() {
            rest.push_str("  ");
        }
        rest.push_str(&format!("review: {reviewer}"));
    }
    if !task.artifacts.is_empty() {
        if !rest.is_empty() {
            rest.push_str("  ");
        }
        rest.push_str(&format!("⬡{}", task.artifacts.len()));
    }
    if task.title != task.id && !task.title.is_empty() {
        if !rest.is_empty() {
            rest.push_str("  ");
        }
        rest.push_str(&one_line(&task.title));
    }
    let room = (i32::from(clip.right()) - 1 - cx).max(0) as usize;
    put_str(buf, clip, cx, y, &fit(&rest, room), theme.fg(theme.muted));
}
