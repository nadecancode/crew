//! Agent inspection: identity and process facts, its tasks, its messages, a
//! live capture of its terminal (scrolled up: the transcript of its
//! session), and anything that went wrong.

use crew_core::types::{Agent, AgentToolbox, Message, MessageType, RuntimeKind, Task};
use crew_core::util::{fmt_clock, fmt_datetime, fmt_elapsed, one_line, printable};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::app::App;
use crate::theme::{agent_icon, task_icon, Theme};
use crate::transcript::Part;
use crate::widgets::agent_box::status_tail;
use crate::widgets::ansi;
use crate::widgets::canvas::{fill_row, put_str};
use crate::widgets::text::{fit, printable_lines, width};

const HINTS: &str =
    "esc back · tab next agent · click terminal to type · f full · i type · m message · r refresh · d diff · T toolbox · p pause · R restart · k kill";
/// While keys go to the agent's terminal.
const TYPING_HINTS: &str =
    "ctrl-] stop typing (right-click too) · every other key goes to the agent";

/// What a click at column `x`, row `y` lands on: the terminal pane, or
/// nothing of interest. The toolbox pane in its place is read-only: a click
/// there types into nothing.
pub fn hit(app: &App, x: u16, y: u16) -> Option<super::dashboard::Hit> {
    let pane = app.detail.terminal_rect.get();
    (!app.detail.show_toolbox
        && app.agent(&app.detail.agent).is_some()
        && pane.contains(ratatui::layout::Position { x, y }))
    .then_some(super::dashboard::Hit::Terminal)
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    if area.height < 3 || area.width < 10 {
        return;
    }
    let Some(agent) = app.agent(&app.detail.agent) else {
        put_str(
            buf,
            area,
            i32::from(area.x) + 1,
            i32::from(area.y),
            "this agent no longer exists — esc to go back",
            theme.fg(theme.warn),
        );
        return;
    };
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(buf, header, agent, theme, now);

    if app.detail.full {
        // The terminal alone, as large as the inspector.
        render_output(buf, body, app, theme, now);
    } else if body.width >= 90 && body.height >= 12 {
        let left_w = (body.width * 2 / 5).clamp(38, 54);
        let [left, right] =
            Layout::horizontal([Constraint::Length(left_w), Constraint::Min(0)]).areas(body);
        let info = info_lines(app, agent, theme, now);
        let info_h = (info.len() as u16 + 2).min(left.height.saturating_sub(4));
        let [info_area, tasks_area] =
            Layout::vertical([Constraint::Length(info_h), Constraint::Min(0)]).areas(left);
        panel(buf, info_area, "agent", info, theme);
        panel(
            buf,
            tasks_area,
            "tasks",
            task_lines(app, agent, theme, now),
            theme,
        );

        let problems = problem_lines(app, agent, theme);
        let problems_h = (problems.len() as u16 + 2).clamp(3, 7);
        let messages_h = (right.height / 3).clamp(4, 11);
        let [output_area, messages_area, problems_area] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(messages_h),
            Constraint::Length(problems_h),
        ])
        .areas(right);
        render_output(buf, output_area, app, theme, now);
        let rows = usize::from(messages_area.height.saturating_sub(2));
        panel(
            buf,
            messages_area,
            "messages",
            message_lines(app, agent, rows, theme),
            theme,
        );
        panel(
            buf,
            problems_area,
            "errors",
            tail(problems, usize::from(problems_h) - 2),
            theme,
        );
    } else {
        // Narrow terminal: the facts on top, the terminal below.
        let info = info_lines(app, agent, theme, now);
        let info_h = (info.len() as u16 + 2).min(body.height / 2).max(3);
        let [info_area, output_area] =
            Layout::vertical([Constraint::Length(info_h), Constraint::Min(0)]).areas(body);
        panel(buf, info_area, "agent", info, theme);
        render_output(buf, output_area, app, theme, now);
    }

    fill_row(buf, footer, footer.y, Style::default().bg(theme.bar_bg));
    let hint = match &app.status {
        Some(status) => {
            let color = if status.is_error { theme.err } else { theme.ok };
            (status.text.as_str(), color)
        }
        None if app.detail.typing => (TYPING_HINTS, theme.accent),
        None => (HINTS, theme.dim),
    };
    put_str(
        buf,
        footer,
        i32::from(footer.x) + 1,
        i32::from(footer.y),
        &fit(hint.0, usize::from(footer.width).saturating_sub(2)),
        Style::default().fg(hint.1).bg(theme.bar_bg),
    );
}

fn render_header(buf: &mut Buffer, area: Rect, agent: &Agent, theme: &Theme, now: i64) {
    let y = i32::from(area.y);
    let mut x = i32::from(area.x) + 1;
    let (glyph, color) = if agent.orchestrator {
        ("◆", theme.orch)
    } else {
        theme.member_mark(agent.runtime, agent.parent.is_some())
    };
    put_str(buf, area, x, y, glyph, theme.bold(color));
    x += 2;
    put_str(buf, area, x, y, &agent.id, theme.bold(color));
    x += width(&agent.id) as i32;
    if !agent.role.is_empty() && agent.role != agent.id {
        let role = format!(" · {}", agent.role);
        put_str(buf, area, x, y, &role, theme.fg(theme.text));
        x += width(&role) as i32;
    }
    let right = format!(
        "{} {}",
        agent_icon(agent.status, agent.paused, now),
        state_text(agent)
    );
    let right_x = i32::from(area.right()) - 1 - width(&right) as i32;
    if right_x > x + 1 {
        put_str(
            buf,
            area,
            right_x,
            y,
            &right,
            theme.bold(theme.state_color(agent)),
        );
    }
}

fn state_text(agent: &Agent) -> String {
    let mut s = agent.status.as_str().to_string();
    if agent.paused {
        s.push_str(" (paused)");
    }
    if let Some(kind) = &agent.block_kind {
        s.push_str(&format!(" · {kind}"));
    }
    if agent.progress.is_some() {
        s.push_str(&format!(" · {}", status_tail(agent).trim()));
    }
    s
}

/// A bordered panel of pre-built lines; long lines are clipped, not wrapped.
fn panel(buf: &mut Buffer, area: Rect, title: &str, lines: Vec<Line<'static>>, theme: &Theme) {
    panel_in(buf, area, title, lines, theme.fg(theme.dim), theme);
}

/// A panel with a border of its own colour (the terminal pane being typed
/// into stands out).
fn panel_in(
    buf: &mut Buffer,
    area: Rect,
    title: &str,
    mut lines: Vec<Line<'static>>,
    border: Style,
    theme: &Theme,
) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Span::styled(
            format!(" {} ", printable(title)),
            theme.fg(theme.muted),
        ));
    printable_lines(&mut lines);
    Paragraph::new(lines).block(block).render(area, buf);
}

fn tail<T>(mut items: Vec<T>, keep: usize) -> Vec<T> {
    let skip = items.len().saturating_sub(keep);
    items.drain(..skip);
    items
}

fn field(label: &str, value: impl Into<String>, style: Style, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {label:<10}"), theme.fg(theme.muted)),
        Span::styled(value.into(), style),
    ])
}

fn compact_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{}k", n / 1000)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn info_lines(app: &App, agent: &Agent, theme: &Theme, now: i64) -> Vec<Line<'static>> {
    let text = theme.fg(theme.text);
    let server_now = app.server_now(now);
    let mut lines = vec![field(
        "status",
        format!(
            "{} {}",
            agent_icon(agent.status, agent.paused, now),
            state_text(agent)
        ),
        theme.bold(theme.state_color(agent)),
        theme,
    )];
    if let Some(error) = &agent.error {
        lines.push(field("error", one_line(error), theme.fg(theme.err), theme));
    }
    lines.push(field(
        "task",
        agent.task.clone().unwrap_or_else(|| "—".into()),
        text,
        theme,
    ));
    if !agent.activity.is_empty() && crate::widgets::agent_box::provider_error(agent).is_none() {
        lines.push(field("activity", one_line(&agent.activity), text, theme));
    }
    // The runtime, the model (as named in the team, else as the session
    // reports it) and the reasoning effort.
    let mut runtime = agent.runtime.to_string();
    for part in [agent.model_label(), agent.effort.clone()]
        .into_iter()
        .flatten()
    {
        runtime.push_str(" · ");
        runtime.push_str(&part);
    }
    lines.push(field("runtime", runtime, text, theme));
    // The session was moved to another model than the member was given.
    if let Some(fallback) = &agent.fallback {
        let mut value = format!(
            "↓ on {} instead of {}",
            agent
                .model_label()
                .unwrap_or_else(|| "another model".into()),
            fallback.from
        );
        if !fallback.note.is_empty() {
            value.push_str(" — ");
            value.push_str(&fallback.note);
        }
        lines.push(field("fallback", value, theme.fg(theme.warn), theme));
    }
    // The effective toolbox: the summary and a pending change here; `T`
    // shows the rest in the terminal pane (notes/toolboxes.md §Display).
    if let Some(toolbox) = &agent.toolbox {
        // `3f9a0c21e7b4 · 4 items`, then what is not available below it: the
        // panel can be 36 columns wide.
        let summary = toolbox_summary(toolbox);
        let mut parts = summary.splitn(3, " · ");
        let head = [parts.next(), parts.next()].into_iter().flatten();
        lines.push(field(
            "toolbox",
            head.collect::<Vec<_>>().join(" · "),
            text,
            theme,
        ));
        if let Some(rest) = parts.next() {
            lines.push(field("", rest, theme.fg(theme.warn), theme));
        }
        if toolbox.pending_fingerprint.is_some() {
            lines.push(Line::from(Span::styled(
                format!(" {} (R restart)", crew_core::types::TOOLBOX_PENDING),
                theme.fg(theme.warn),
            )));
        }
        lines.push(Line::from(Span::styled(
            " T shows its items, sources and digest",
            theme.fg(theme.dim),
        )));
    }
    let backend = match &agent.backend_ref {
        Some(reference) => format!("{} · {reference}", agent.backend),
        None => agent.backend.clone(),
    };
    lines.push(field("backend", backend, text, theme));
    if !agent.delivery.is_empty() {
        lines.push(field("delivery", agent.delivery.clone(), text, theme));
    }
    if let Some(parent) = &agent.parent {
        lines.push(field("parent", parent.clone(), text, theme));
    }
    let children: Vec<String> = app
        .agents
        .iter()
        .filter(|a| a.parent.as_deref() == Some(agent.id.as_str()))
        .map(|a| format!("{} ({})", a.id, a.status))
        .collect();
    if !children.is_empty() {
        lines.push(field("subagents", children.join(", "), text, theme));
    }
    if agent.subagents > 0 {
        lines.push(Line::from(vec![
            Span::styled(" in-session subagents: ", theme.fg(theme.muted)),
            Span::styled(agent.subagents.to_string(), text),
        ]));
    }
    let pid = match (agent.pid, agent.exit_code) {
        (Some(pid), None) => pid.to_string(),
        (Some(pid), Some(code)) => format!("{pid} (exited {code})"),
        (None, Some(code)) => format!("exited {code}"),
        (None, None) => "—".to_string(),
    };
    lines.push(field("pid", pid, text, theme));
    lines.push(field("workdir", agent.workdir.clone(), text, theme));
    lines.push(field(
        "branch",
        agent.branch.clone().unwrap_or_else(|| "—".into()),
        text,
        theme,
    ));
    lines.push(field(
        "session",
        agent.session_id.clone().unwrap_or_else(|| "—".into()),
        text,
        theme,
    ));
    let started = match agent.started_at {
        Some(at) => format!("{} · up {}", fmt_datetime(at), fmt_elapsed(server_now - at)),
        None => "not started".to_string(),
    };
    lines.push(field("started", started, text, theme));
    lines.push(field(
        "last seen",
        format!("{} ago", fmt_elapsed(server_now - agent.last_activity_at)),
        text,
        theme,
    ));
    lines.push(field("restarts", agent.restarts.to_string(), text, theme));
    lines.push(field(
        "mailbox",
        format!(
            "{} unread of {} · {} sent",
            agent.inbox_unread, agent.inbox_total, agent.outbox_total
        ),
        text,
        theme,
    ));
    match &agent.usage {
        Some(u) => {
            lines.push(field(
                "tokens",
                format!(
                    "{} in · {} out · {} turns",
                    compact_count(u.input_tokens),
                    compact_count(u.output_tokens),
                    u.turns
                ),
                text,
                theme,
            ));
            lines.push(field(
                "cache",
                format!(
                    "{} read · {} written",
                    compact_count(u.cache_read_tokens),
                    compact_count(u.cache_creation_tokens)
                ),
                text,
                theme,
            ));
        }
        None => lines.push(field("tokens", "not reported", theme.fg(theme.dim), theme)),
    }
    // How full the context window is; a compaction that is queued or was done.
    if let Some(c) = &agent.context {
        let style = match c.percent {
            90.. => theme.fg(theme.err),
            75..=89 => theme.fg(theme.warn),
            _ => text,
        };
        let fill = if c.window > 0 {
            format!(
                "{}% full · {} of {}",
                c.percent,
                compact_count(c.tokens),
                compact_count(c.window)
            )
        } else {
            format!(
                "{} tokens (window size not reported)",
                compact_count(c.tokens)
            )
        };
        lines.push(field("context", fill, style, theme));
    }
    let compaction = match (agent.compact_pending, agent.compactions) {
        (false, 0) => None,
        (true, 0) => Some("queued for the end of this turn".to_string()),
        (true, done) => Some(format!("queued · done {done}× so far")),
        (false, done) => Some(format!("done {done}×")),
    };
    if let Some(compaction) = compaction {
        lines.push(field("compact", compaction, text, theme));
    }
    lines
}

/// `3f9a0c21e7b4 · 4 items · 1 not available`: the summary after its label.
fn toolbox_summary(toolbox: &AgentToolbox) -> String {
    let summary = toolbox.summary();
    summary
        .strip_prefix("Toolbox ")
        .unwrap_or(&summary)
        .to_string()
}

/// How a toolbox pane row is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolboxTone {
    Heading,
    Text,
    Muted,
    Dim,
    Warn,
}

/// The toolbox pane (`T`): the summary, a pending change, the items by kind,
/// the runtime's sources and the definition facts, wrapped to `cols` so long
/// commands and URLs stay whole. Groups without entries are left out; status
/// is always the daemon's words (notes/toolboxes.md §Display).
pub(crate) fn toolbox_rows(toolbox: &AgentToolbox, cols: usize) -> Vec<(String, ToolboxTone)> {
    use crew_core::types::{
        toolbox_origin_label, ToolboxItemCode, ToolboxItemKind, ToolboxSourceCode, TOOLBOX_PENDING,
        TOOLBOX_UNPINNED,
    };
    use ToolboxTone::*;
    let mut rows = Vec::new();
    let mut add = |text: String, tone| wrap_into(&mut rows, &text, cols, tone);
    add(format!(" {}", toolbox_summary(toolbox)), Text);
    if toolbox.pending_fingerprint.is_some() {
        add(format!(" {TOOLBOX_PENDING} (R restart)"), Warn);
    }
    if toolbox.definitions_unpinned() {
        add(format!(" {TOOLBOX_UNPINNED}"), Dim);
    }
    let mut kinds: Vec<ToolboxItemKind> = toolbox.items.iter().map(|item| item.kind).collect();
    kinds.sort();
    kinds.dedup();
    for kind in kinds {
        add(String::new(), Text);
        add(format!(" {}", kind.heading()), Heading);
        for item in toolbox.items.iter().filter(|item| item.kind == kind) {
            add(format!("  {}", item.name), Text);
            let tone = match item.status.code {
                ToolboxItemCode::Active | ToolboxItemCode::Planned => Muted,
                ToolboxItemCode::NotApplicable => Dim,
                _ => Warn,
            };
            let required = if item.required { " · required" } else { "" };
            add(
                format!(
                    "    {} · {}{required}",
                    toolbox_origin_label(&item.origin),
                    item.status.text
                ),
                tone,
            );
            if let Some(detail) = &item.detail {
                add(format!("    {detail}"), Dim);
            }
        }
    }
    if !toolbox.sources.is_empty() {
        add(String::new(), Text);
        add(" Sources".into(), Heading);
        for source in &toolbox.sources {
            let mut line = format!("  {}: {}", source.label, source.status.text);
            if !source.names.is_empty() {
                line.push_str(&format!(" ({})", source.names.join(", ")));
            }
            let tone = if source.status.code == ToolboxSourceCode::CannotBeExcluded {
                Warn
            } else {
                Text
            };
            add(line, tone);
            if let Some(detail) = &source.detail {
                add(format!("    {detail}"), Dim);
            }
        }
    }
    add(String::new(), Text);
    add(" Definitions".into(), Heading);
    let unknown = |value: Option<String>| value.unwrap_or_else(|| "unknown".into());
    for (label, value) in [
        ("fingerprint", toolbox.fingerprint.clone()),
        (
            "after restart",
            toolbox.pending_fingerprint.clone().unwrap_or_default(),
        ),
        (
            "definition digest",
            toolbox
                .definition_digest
                .clone()
                .unwrap_or_else(|| "unavailable".into()),
        ),
        ("tools", unknown(toolbox.tool_count.map(|n| n.to_string()))),
        (
            "definition bytes",
            unknown(toolbox.definition_bytes.map(|n| format!("{n} bytes"))),
        ),
    ] {
        if !value.is_empty() {
            add(format!("  {label}: {value}"), Text);
        }
    }
    rows
}

/// `text` as rows of at most `cols` columns, broken at spaces, and inside a
/// word only when the word alone is wider than a row. Continuation rows keep
/// the text's indent.
fn wrap_into(rows: &mut Vec<(String, ToolboxTone)>, text: &str, cols: usize, tone: ToolboxTone) {
    let cols = cols.max(16);
    let body = text.trim_start_matches(' ');
    let indent = " ".repeat(text.len() - body.len());
    let mut line = indent.clone();
    for word in body.split(' ') {
        let started = line.len() > indent.len();
        if started && width(&line) + 1 + width(word) > cols {
            rows.push((std::mem::replace(&mut line, indent.clone()), tone));
        } else if started {
            line.push(' ');
        }
        for ch in word.chars() {
            let mut buf = [0; 4];
            if line.len() > indent.len() && width(&line) + width(ch.encode_utf8(&mut buf)) > cols {
                rows.push((std::mem::replace(&mut line, indent.clone()), tone));
            }
            line.push(ch);
        }
    }
    rows.push((line, tone));
}

/// The pane showing the toolbox, scrolled like the diff: ↑ and PgUp go
/// toward the top, where it opens.
fn render_toolbox(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    let detail = &app.detail;
    let Some(toolbox) = app.agent(&detail.agent).and_then(|a| a.toolbox.as_ref()) else {
        let none = Line::from(Span::styled(
            " this member has no toolbox",
            theme.fg(theme.dim),
        ));
        panel(buf, area, "toolbox · T closes", vec![none], theme);
        return;
    };
    let rows = toolbox_rows(toolbox, detail.cols.get());
    let visible = usize::from(area.height.saturating_sub(2));
    let scroll = detail.scroll.min(rows.len().saturating_sub(visible));
    let end = rows.len() - scroll;
    let start = end.saturating_sub(visible);
    let lines = rows[start..end]
        .iter()
        .map(|(text, tone)| {
            let style = match tone {
                ToolboxTone::Heading => theme.bold(theme.muted),
                ToolboxTone::Text => theme.fg(theme.text),
                ToolboxTone::Muted => theme.fg(theme.muted),
                ToolboxTone::Dim => theme.fg(theme.dim),
                ToolboxTone::Warn => theme.fg(theme.warn),
            };
            Line::from(Span::styled(text.clone(), style))
        })
        .collect();
    let title = if rows.len() > visible {
        format!(
            "toolbox · rows {}–{end} of {} · ↑↓ scroll · T closes",
            start + 1,
            rows.len()
        )
    } else {
        "toolbox · T closes".to_string()
    };
    panel(buf, area, &title, lines, theme);
}

fn task_lines(app: &App, agent: &Agent, theme: &Theme, now: i64) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for task in app.tasks_for(&agent.id) {
        lines.push(task_line(task, agent, theme, now));
        if !task.description.is_empty() && task.description != task.title {
            lines.push(Line::from(Span::styled(
                format!("   {}", one_line(&task.description)),
                theme.fg(theme.muted),
            )));
        }
        if let Some(note) = &task.note {
            lines.push(Line::from(Span::styled(
                format!("   {}", one_line(note)),
                theme.fg(theme.task_color(task.status)),
            )));
        }
        for artifact in &task.artifacts {
            let mut text = format!("   ⬡ {} {}", artifact.kind, artifact.value);
            if !artifact.description.is_empty() {
                text.push_str(&format!(" — {}", one_line(&artifact.description)));
            }
            lines.push(Line::from(Span::styled(text, theme.fg(theme.ok))));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no tasks assigned",
            theme.fg(theme.dim),
        )));
    }
    lines
}

fn task_line(task: &Task, agent: &Agent, theme: &Theme, now: i64) -> Line<'static> {
    let color = theme.task_color(task.status);
    let mut spans = vec![
        Span::styled(
            format!(" {} ", task_icon(task.status, now)),
            theme.bold(color),
        ),
        Span::styled(task.id.clone(), theme.bold(theme.text)),
        Span::styled(format!("  {}", task.status), theme.fg(color)),
    ];
    if task.reviewer.as_deref() == Some(agent.id.as_str())
        && task.assigned_agent.as_deref() != Some(agent.id.as_str())
    {
        spans.push(Span::styled("  (reviewing)", theme.fg(theme.orch)));
    }
    if !task.dependencies.is_empty() {
        spans.push(Span::styled(
            format!("  ← {}", task.dependencies.join(", ")),
            theme.fg(theme.muted),
        ));
    }
    if let Some(reviewer) = task.reviewer.as_deref().filter(|r| *r != agent.id) {
        spans.push(Span::styled(
            format!("  review: {reviewer}"),
            theme.fg(theme.muted),
        ));
    }
    Line::from(spans)
}

fn message_lines(app: &App, agent: &Agent, rows: usize, theme: &Theme) -> Vec<Line<'static>> {
    let all: Vec<&Message> = app.messages_for(&agent.id).collect();
    let mut lines: Vec<Line<'static>> = all
        .iter()
        .skip(all.len().saturating_sub(rows))
        .map(|m| message_line(app, agent, m, theme))
        .collect();
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no messages yet",
            theme.fg(theme.dim),
        )));
    }
    lines
}

fn message_line(app: &App, agent: &Agent, msg: &Message, theme: &Theme) -> Line<'static> {
    let outgoing = msg.from == agent.id;
    let (arrow, peer) = if outgoing {
        ("→", msg.to.as_str())
    } else {
        ("←", msg.from.as_str())
    };
    let color = if msg.is_p2p() {
        theme.accent
    } else {
        theme.orch
    };
    let mut spans = vec![
        Span::styled(
            format!(" {} ", fmt_clock(msg.timestamp)),
            theme.fg(theme.dim),
        ),
        Span::styled(format!("{arrow} "), theme.bold(color)),
        Span::styled(format!("{peer}  "), theme.fg(color)),
    ];
    if !outgoing {
        // Delivery state of incoming mail: injected into the session, then read.
        let mark = match app.detail.delivery.get(&msg.id) {
            Some((_, true)) => "✓✓ ",
            Some((true, false)) => "✓ ",
            _ => "",
        };
        spans.push(Span::styled(mark.to_string(), theme.fg(theme.ok)));
    }
    if msg.kind != MessageType::Message {
        spans.push(Span::styled(
            format!("‹{}› ", msg.kind.as_str().replace('_', " ")),
            theme.fg(theme.dim),
        ));
    }
    spans.push(Span::styled(one_line(&msg.body), theme.fg(theme.text)));
    Line::from(spans)
}

fn problem_lines(app: &App, agent: &Agent, theme: &Theme) -> Vec<Line<'static>> {
    let problems = app.problems_for(&agent.id);
    if problems.is_empty() {
        return vec![Line::from(Span::styled(" none", theme.fg(theme.dim)))];
    }
    problems
        .into_iter()
        .map(|e| {
            Line::from(vec![
                Span::styled(format!(" {} ", fmt_clock(e.timestamp)), theme.fg(theme.dim)),
                Span::styled(
                    format!("{} ", e.event_type),
                    theme.fg(theme.level_color(e.level)),
                ),
                Span::styled(one_line(&e.message), theme.fg(theme.text)),
            ])
        })
        .collect()
}

/// The pane scrolled up: the agent's session transcript, `scroll` lines
/// above its end. False when there is no transcript to show.
fn render_transcript(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) -> bool {
    let detail = &app.detail;
    let Some(lines) = detail.transcript_lines(detail.cols.get()) else {
        return false;
    };
    let rows = usize::from(area.height.saturating_sub(2));
    // One step up from the live terminal is the end of the transcript.
    let scroll = (detail.scroll - 1).min(lines.len().saturating_sub(rows));
    let end = lines.len() - scroll;
    let start = end.saturating_sub(rows);
    let shown: Vec<Line<'static>> = lines[start..end]
        .iter()
        .map(|line| {
            let style = match line.part {
                Part::Rule => theme.fg(theme.dim),
                Part::Prompt => theme.fg(theme.orch),
                Part::Tool => theme.fg(theme.muted),
                Part::Reply | Part::Blank => theme.fg(theme.text),
            };
            Line::from(Span::styled(format!(" {}", printable(&line.text)), style))
        })
        .collect();
    let age = detail
        .transcript_at
        .map(|at| format!(" · read {} ago", fmt_elapsed(now - at)))
        .unwrap_or_default();
    let title = format!(
        "transcript{age} · ↑{} of {} · ↓ live terminal",
        scroll + 1,
        lines.len()
    );
    panel(buf, area, &title, shown, theme);
    true
}

fn render_output(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    let detail = &app.detail;
    let rows = usize::from(area.height.saturating_sub(2));
    // Border, margin and a spare column on the right.
    detail
        .cols
        .set(usize::from(area.width.saturating_sub(4)).max(8));
    detail.terminal_rect.set(area);
    if detail.show_toolbox {
        render_toolbox(buf, area, app, theme);
        return;
    }
    if !detail.show_diff && detail.scroll > 0 && render_transcript(buf, area, app, theme, now) {
        return;
    }
    // Claude Code and Codex keep a session file; a shell agent has only
    // its terminal.
    let has_transcript = app
        .agent(&detail.agent)
        .is_some_and(|a| a.runtime != RuntimeKind::Shell);
    let (title, source) = if detail.show_diff {
        (
            "diff (uncommitted)".to_string(),
            detail
                .diff
                .clone()
                .unwrap_or_else(|| "loading diff…".to_string()),
        )
    } else {
        let state = if detail.output_at.is_none() {
            "loading"
        } else if detail.alive {
            "live"
        } else {
            "process not running"
        };
        let age = detail
            .output_at
            .map(|at| format!(" · {} ago", fmt_elapsed(now - at)))
            .unwrap_or_default();
        let more = if detail.typing {
            " · typing: keys go to the agent · ctrl-] stops"
        } else if detail.scroll > 0 && detail.transcript_pending {
            " · reading the transcript…"
        } else if detail.scroll == 0 && has_transcript {
            " · ↑ transcript · click to type"
        } else {
            ""
        };
        (
            format!("terminal · {state}{age}{more}"),
            detail.output.clone(),
        )
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    if let (false, Some(error)) = (detail.show_diff, &detail.output_error) {
        lines.push(Line::from(Span::styled(
            format!(" output unavailable: {}", one_line(error)),
            theme.fg(theme.err),
        )));
    }
    if let (false, true, Some(error)) = (
        detail.show_diff,
        detail.scroll > 0,
        &detail.transcript_error,
    ) {
        lines.push(Line::from(Span::styled(
            format!(" transcript unavailable: {}", one_line(error)),
            theme.fg(theme.err),
        )));
    }
    let mut text: Vec<&str> = source.lines().collect();
    while text.last().is_some_and(|l| l.trim().is_empty()) {
        text.pop();
    }
    let keep = rows.saturating_sub(lines.len());
    let max_scroll = text.len().saturating_sub(keep);
    let scroll = detail.scroll.min(max_scroll);
    let end = text.len() - scroll;
    let start = end.saturating_sub(keep);
    for line in &text[start..end] {
        if detail.show_diff {
            let style = match line.as_bytes().first() {
                Some(b'+') => theme.fg(theme.ok),
                Some(b'-') => theme.fg(theme.err),
                Some(b'@') => theme.fg(theme.orch),
                _ => theme.fg(theme.text),
            };
            lines.push(Line::from(Span::styled(
                format!(" {}", printable(line)),
                style,
            )));
        } else {
            // The terminal's own colours, as the daemon read them.
            let mut styled = ansi::line(line, theme.fg(theme.text));
            styled.spans.insert(0, Span::raw(" "));
            lines.push(styled);
        }
    }
    if lines.is_empty() {
        let placeholder = if detail.pending {
            " loading…"
        } else {
            " (no output)"
        };
        lines.push(Line::from(Span::styled(placeholder, theme.fg(theme.dim))));
    }
    let title = if scroll > 0 {
        format!("{title} · ↑{scroll}")
    } else {
        title
    };
    let border = if detail.typing && !detail.show_diff {
        theme.fg(theme.accent)
    } else {
        theme.fg(theme.dim)
    };
    panel_in(buf, area, &title, lines, border, theme);
}
