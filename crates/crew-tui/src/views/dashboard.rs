//! The main view: agents around the orchestrator, the message bus log, and a
//! one-line footer.

use crew_core::types::{Agent, Message, MessageType, RuntimeKind, ORCHESTRATOR_ID};
use crew_core::util::{fmt_clock, fmt_elapsed, one_line};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::anim::{Link, LinkKind};
use crate::app::App;
use crate::layout::{self, Topology};
use crate::theme::{agent_icon, Theme};
use crate::widgets::agent_box::{draw_agent, draw_orchestrator, status_tail, Decor, HubState};
use crate::widgets::canvas::{fill_row, put_str};
use crate::widgets::text::{fit, pad_left, pad_right, width};

/// Split the screen into (topology, message bus, footer).
pub fn split(area: Rect) -> (Rect, Rect, Rect) {
    let footer_h = u16::from(area.height >= 3);
    let body_h = area.height - footer_h;
    let bus_h = match body_h {
        34.. => 10,
        26..=33 => 8,
        20..=25 => 7,
        14..=19 => 5,
        9..=13 => 3,
        _ => 0,
    };
    let topo = Rect {
        height: body_h - bus_h,
        ..area
    };
    let bus = Rect {
        y: topo.bottom(),
        height: bus_h,
        ..area
    };
    let footer = Rect {
        y: bus.bottom(),
        height: footer_h,
        ..area
    };
    (topo, bus, footer)
}

pub fn render(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    if area.is_empty() {
        return;
    }
    // The usage strip takes its rows off the top, when there is something
    // to show and room to show it.
    let strip_h = super::usage::height(app, area);
    if strip_h > 0 {
        let strip = Rect {
            height: strip_h,
            ..area
        };
        super::usage::render(buf, strip, app, theme, now);
    }
    let (topo, bus, footer) = regions(app, area);
    if !topo.is_empty() {
        render_topology(buf, topo, app, theme, now);
    }
    if !bus.is_empty() {
        render_bus(buf, bus, app, theme, now);
    }
    if !footer.is_empty() {
        render_footer(buf, footer, app, theme, now);
    }
}

// ---- pointing ---------------------------------------------------------------

/// What a click on the dashboard lands on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    /// An agent's box (or its row in the compact list).
    Agent(String),
    /// The bottom border of an agent's box, where its model is written.
    Model(String),
    /// A row of the message bus, by message id.
    Message(String),
    /// The band announcing questions for the operator.
    Questions,
    /// The inspector's terminal pane.
    Terminal,
}

/// The dashboard's regions inside `area`: (topology, message bus, footer),
/// below the usage strip.
fn regions(app: &App, area: Rect) -> (Rect, Rect, Rect) {
    let strip_h = super::usage::height(app, area);
    let warning = super::sandbox_warning(app).is_some();
    let (mut topo, mut bus, mut footer) = split(Rect {
        y: area.y + strip_h,
        height: (area.height - strip_h).saturating_add(u16::from(warning)),
        ..area
    });
    // Spend the warning's row on history before shrinking the agent boxes.
    if warning {
        if bus.height > 0 {
            bus.height -= 1;
        } else {
            topo.height = topo.height.saturating_sub(1);
        }
        bus.y = topo.bottom();
        footer.y = bus.bottom();
    }
    (topo, bus, footer)
}

/// The ring of boxes in `area`, and whether its boxes have a row for what
/// each agent is doing right now. Rows are added under the task's — that
/// one first, then (when any agent reports how full its context window is)
/// one for the window — as far as they cost no width.
fn ring(app: &App, area: Rect, n: usize) -> Option<(layout::Topology, bool)> {
    let plain = layout::radial(area, n)?;
    let context = u16::from(app.agents.iter().any(|a| a.context.is_some()));
    let width_of = |t: &layout::Topology| t.agents.first().map_or(t.center.width, |r| r.width);
    let tries = [
        (layout::AGENT_H + 1 + context, layout::CENTER_H + context),
        (layout::AGENT_H + 1, layout::CENTER_H),
    ];
    for heights in tries {
        if let Some(tall) = layout::radial_sized(area, n, heights) {
            if width_of(&tall) >= width_of(&plain) {
                return Some((tall, true));
            }
        }
    }
    Some((plain, false))
}

/// Every agent drawn in `topo` and the cells it occupies: the same boxes
/// (or compact rows) `render_topology` draws.
fn agent_cells(app: &App, topo: Rect) -> Vec<(String, Rect)> {
    let tops: Vec<&Agent> = app.top_level().collect();
    let Some((layout, _)) = ring(app, topo, tops.len()) else {
        // The compact list: one row each, the selection kept in view.
        let rows = usize::from(topo.height);
        let mut entries: Vec<&Agent> = Vec::new();
        entries.extend(app.orchestrator());
        entries.extend(app.workers());
        let selected = entries
            .iter()
            .position(|a| app.is_selected(&a.id))
            .unwrap_or(0);
        let first = selected.saturating_sub(rows.saturating_sub(1));
        return entries
            .iter()
            .skip(first)
            .take(rows)
            .enumerate()
            .map(|(row, agent)| {
                let cell = Rect::new(topo.x, topo.y + row as u16, topo.width, 1);
                (agent.id.clone(), cell)
            })
            .collect();
    };
    let mut cells = Vec::new();
    if let Some(orchestrator) = app.orchestrator() {
        cells.push((orchestrator.id.clone(), layout.center));
    }
    let counts: Vec<usize> = tops
        .iter()
        .map(|parent| app.subagents_of(&parent.id).count())
        .collect();
    let satellites = layout::satellites(topo, &layout, &counts);
    for (i, (agent, rect)) in tops.iter().zip(&layout.agents).enumerate() {
        cells.push((agent.id.clone(), *rect));
        for (child, rect) in app.subagents_of(&agent.id).zip(&satellites[i]) {
            cells.push((child.id.clone(), *rect));
        }
    }
    cells
}

/// Every agent on the dashboard drawn into `area`, with its cells.
pub fn agent_rects(app: &App, area: Rect) -> Vec<(String, Rect)> {
    if area.is_empty() {
        return Vec::new();
    }
    agent_cells(app, regions(app, area).0)
}

/// What is at column `x`, row `y` of the dashboard drawn into `area`.
pub fn hit(app: &App, area: Rect, x: u16, y: u16) -> Option<Hit> {
    if area.is_empty() {
        return None;
    }
    let at = ratatui::layout::Position::new(x, y);
    let (topo, bus, _) = regions(app, area);
    if topo.contains(at) {
        return agent_cells(app, topo)
            .into_iter()
            .find(|(_, cell)| cell.contains(at))
            .map(|(id, cell)| {
                // A box's bottom border carries what it runs on.
                let runs_a_model = app
                    .agent(&id)
                    .is_some_and(|a| a.runtime != RuntimeKind::Shell);
                if runs_a_model && cell.height >= 3 && y == cell.bottom() - 1 {
                    Hit::Model(id)
                } else {
                    Hit::Agent(id)
                }
            });
    }
    if bus.contains(at) && y > bus.y {
        // Under the heading, the newest messages that fit, oldest first.
        let rows = usize::from(bus.height - 1);
        let first = app.messages.len().saturating_sub(rows);
        return app
            .messages
            .get(first + usize::from(y - bus.y - 1))
            .map(|m| Hit::Message(m.id.clone()));
    }
    None
}

// ---- topology ---------------------------------------------------------------

fn render_topology(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    // The ring is laid out from the top-level workers alone, so it does not
    // shift when a subagent appears; subagents orbit their parent.
    let tops: Vec<&Agent> = app.top_level().collect();
    let Some((topo, activity_row)) = ring(app, area, tops.len()) else {
        let workers: Vec<&Agent> = app.workers().collect();
        render_compact(buf, area, app, &workers, theme, now);
        return;
    };
    let server_now = app.server_now(now);
    let idle_for = |agent: &Agent| {
        (agent.last_activity_at > 0).then(|| (server_now - agent.last_activity_at).max(0))
    };
    let subs: Vec<Vec<&Agent>> = tops
        .iter()
        .map(|parent| app.subagents_of(&parent.id).collect())
        .collect();
    let counts: Vec<usize> = subs.iter().map(Vec::len).collect();
    let satellites = layout::satellites(area, &topo, &counts);

    let hub_id = app
        .orchestrator()
        .map_or(ORCHESTRATOR_ID, |a| a.id.as_str());
    let rect_of = |id: &str| -> Option<Rect> {
        if id == hub_id || id == ORCHESTRATOR_ID {
            return Some(topo.center);
        }
        if let Some(i) = tops.iter().position(|a| a.id == id) {
            return Some(topo.agents[i]);
        }
        for (i, children) in subs.iter().enumerate() {
            if let Some(j) = children.iter().position(|a| a.id == id) {
                // A subagent without a box of its own is reached at its parent.
                return Some(satellites[i].get(j).copied().unwrap_or(topo.agents[i]));
            }
        }
        None
    };
    let boxes: Vec<Rect> = std::iter::once(topo.center)
        .chain(topo.agents.iter().copied())
        .chain(satellites.iter().flatten().copied())
        .collect();

    draw_structure(buf, area, &topo, &satellites, theme);

    // Live links, oldest first so the newest ends up on top.
    let mut pulses: Vec<Pulse> = Vec::new();
    for link in app.anims.links.iter().filter(|l| l.is_live(now)) {
        let (Some(from), Some(to)) = (rect_of(&link.from), rect_of(&link.to)) else {
            continue;
        };
        if from == to {
            continue;
        }
        let path = link_path(link, from, to, &boxes, area);
        let color = link_color(link, theme);
        for (x, y) in &path {
            put_str(
                buf,
                area,
                i32::from(*x),
                i32::from(*y),
                "·",
                theme.bold(color),
            );
        }
        if let Some(t) = link.pulse(now) {
            pulses.push(Pulse { path, t, color });
        }
    }

    let questions = app.open_questions();
    let asking = |id: &str| {
        questions
            .iter()
            .any(|q| q.from == id || q.asked_by() == Some(id))
    };
    for (i, (agent, rect)) in tops.iter().zip(&topo.agents).enumerate() {
        let hidden = &subs[i][satellites[i].len()..];
        let decor = Decor {
            work: app.work_of(&agent.id, now),
            activity_row,
            idle_for: idle_for(agent),
            asking: asking(&agent.id),
            // A subagent of the orchestrator has no parent box to orbit and
            // sits on the ring; it still reads as a subagent.
            satellite: agent.parent.is_some(),
            hidden_subagents: hidden.len(),
            hidden_selected: hidden.iter().any(|a| app.is_selected(&a.id)),
        };
        draw_agent(
            buf,
            area,
            *rect,
            agent,
            app.is_selected(&agent.id),
            &decor,
            theme,
            now,
        );
        for (child, rect) in subs[i].iter().zip(&satellites[i]) {
            let decor = Decor {
                work: app.work_of(&child.id, now),
                satellite: true,
                asking: asking(&child.id),
                ..Decor::default()
            };
            draw_agent(
                buf,
                area,
                *rect,
                child,
                app.is_selected(&child.id),
                &decor,
                theme,
                now,
            );
        }
    }
    let (done, total) = app.task_counts();
    let hub = HubState {
        orchestrator: app.orchestrator(),
        done,
        total,
        complete: app.session.completed_at.is_some(),
        selected: app.orchestrator().is_some_and(|a| app.is_selected(&a.id)),
        questions: questions.len(),
    };
    draw_orchestrator(buf, area, topo.center, &hub, theme, now);
    if tops.is_empty() {
        draw_empty_hint(buf, area, topo.center, app, theme);
    }

    let boxed = |x: u16, y: u16| {
        let p = ratatui::layout::Position::new(x, y);
        boxes.iter().any(|r| r.contains(p))
    };
    for Pulse { path, t, color } in pulses {
        if path.is_empty() {
            continue;
        }
        let head = ((path.len() - 1) as f64 * t).round() as usize;
        if head > 0 {
            let (x, y) = path[head - 1];
            if !boxed(x, y) {
                put_str(buf, area, i32::from(x), i32::from(y), "•", theme.fg(color));
            }
        }
        let (x, y) = path[head];
        if !boxed(x, y) {
            put_str(
                buf,
                area,
                i32::from(x),
                i32::from(y),
                "●",
                theme.bold(color),
            );
        }
    }
}

/// With nobody on the ring yet, say why under the hub.
fn draw_empty_hint(buf: &mut Buffer, area: Rect, hub: Rect, app: &App, theme: &Theme) {
    let hint = if app.orchestrator().is_some() {
        "the orchestrator is assembling the team…"
    } else if app.session.project.is_some() {
        "no agents on the team yet"
    } else if app.loaded {
        "no team loaded · crew run <team.yaml>"
    } else {
        "waiting for the daemon…"
    };
    let y = i32::from(hub.bottom()) + 1;
    if y >= i32::from(area.bottom()) {
        return;
    }
    let text = fit(hint, usize::from(area.width).saturating_sub(2));
    let x = i32::from(area.x) + (i32::from(area.width) - width(&text) as i32) / 2;
    put_str(buf, area, x, y, &text, theme.fg(theme.dim));
}

/// A message in flight: the cells of its link and how far along it is.
struct Pulse {
    path: Vec<(u16, u16)>,
    t: f64,
    color: Color,
}

/// The dim scaffolding: spokes from the hub, a ring through the agents, and a
/// tether from each satellite to its parent.
fn draw_structure(
    buf: &mut Buffer,
    area: Rect,
    topo: &Topology,
    satellites: &[Vec<Rect>],
    theme: &Theme,
) {
    let dotted = |buf: &mut Buffer, path: &[(u16, u16)], every: usize, style: Style| {
        for (i, (x, y)) in path.iter().enumerate() {
            if i % every == 0 {
                put_str(buf, area, i32::from(*x), i32::from(*y), "·", style);
            }
        }
    };
    // Spokes are sparse and faint; the ring is the dominant structure.
    for rect in &topo.agents {
        let path = layout::route(topo.center, *rect, &[], area);
        dotted(buf, &path, 3, theme.fg(theme.faint));
    }
    let n = topo.agents.len();
    if n >= 3 {
        for i in 0..n {
            let path = layout::route(topo.agents[i], topo.agents[(i + 1) % n], &[], area);
            dotted(buf, &path, 1, theme.fg(theme.dim));
        }
    }
    for (parent, children) in topo.agents.iter().zip(satellites) {
        for child in children {
            let path = layout::route(*parent, *child, &[], area);
            dotted(buf, &path, 1, theme.fg(theme.muted));
        }
    }
}

fn link_path(link: &Link, from: Rect, to: Rect, boxes: &[Rect], area: Rect) -> Vec<(u16, u16)> {
    match link.kind {
        // Hub traffic runs straight along its spoke.
        LinkKind::Orchestrator => layout::route(from, to, &[], area),
        // Worker-to-worker links avoid every other box, the hub above all. A
        // subagent and its parent are neighbours, so theirs is the tether.
        LinkKind::P2p | LinkKind::Broadcast => {
            let obstacles: Vec<Rect> = boxes
                .iter()
                .copied()
                .filter(|r| *r != from && *r != to)
                .collect();
            layout::route(from, to, &obstacles, area)
        }
    }
}

fn link_color(link: &Link, theme: &Theme) -> Color {
    match link.kind {
        LinkKind::P2p | LinkKind::Broadcast => theme.accent,
        LinkKind::Orchestrator => theme.orch,
    }
}

/// Fallback for terminals too small for the radial layout: one row per agent.
fn render_compact(
    buf: &mut Buffer,
    area: Rect,
    app: &App,
    workers: &[&Agent],
    theme: &Theme,
    now: i64,
) {
    let rows = usize::from(area.height);
    let w = usize::from(area.width);
    let mut entries: Vec<&Agent> = Vec::with_capacity(workers.len() + 1);
    entries.extend(app.orchestrator());
    entries.extend(workers.iter().copied());
    if rows == 0 || w == 0 {
        return;
    }
    // Keep the selected row in view.
    let selected = entries
        .iter()
        .position(|a| app.is_selected(&a.id))
        .unwrap_or(0);
    let first = selected.saturating_sub(rows.saturating_sub(1));
    let id_w = entries
        .iter()
        .map(|a| width(&a.id))
        .max()
        .unwrap_or(0)
        .min(w / 2);
    let (done, total) = app.task_counts();
    let questions = app.open_questions();
    for (row, agent) in entries.iter().skip(first).take(rows).enumerate() {
        let y = i32::from(area.y) + row as i32;
        let x = i32::from(area.x);
        let is_hub = agent.orchestrator || agent.id == ORCHESTRATOR_ID;
        let selected = app.is_selected(&agent.id);
        put_str(
            buf,
            area,
            x,
            y,
            if selected { "▸" } else { " " },
            theme.bold(theme.text),
        );
        let (glyph, color) = if is_hub {
            ("◆", theme.orch)
        } else {
            theme.member_mark(agent.runtime, agent.parent.is_some())
        };
        put_str(buf, area, x + 1, y, glyph, theme.bold(color));
        put_str(
            buf,
            area,
            x + 3,
            y,
            &pad_right(&agent.id, id_w),
            theme.bold(color),
        );
        let status_x = x + 4 + id_w as i32;
        // The hub stands for the team: it waits when anything is asked.
        let waits = if is_hub {
            !questions.is_empty()
        } else {
            questions
                .iter()
                .any(|q| q.from == agent.id || q.asked_by() == Some(agent.id.as_str()))
        };
        let (icon, icon_color) = if waits {
            ("?", theme.warn)
        } else {
            (
                agent_icon(agent.status, agent.paused, now),
                theme.state_color(agent),
            )
        };
        put_str(buf, area, status_x, y, icon, theme.bold(icon_color));
        let tail = if is_hub {
            format!("{done}/{total}")
        } else {
            status_tail(agent)
        };
        put_str(buf, area, status_x + 2, y, &tail, theme.fg(theme.muted));
        let work_x = status_x + 3 + width(&tail) as i32;
        let room = (i32::from(area.right()) - work_x).max(0) as usize;
        if !is_hub && room > 2 {
            let decor = Decor {
                work: app.work_of(&agent.id, now),
                ..Decor::default()
            };
            let (work, current) = crate::widgets::agent_box::work_line(agent, &decor);
            let work = fit(work, room);
            put_str(
                buf,
                area,
                work_x,
                y,
                &work,
                if current {
                    theme.fg(theme.text)
                } else {
                    theme.fg(theme.muted)
                },
            );
            // What it is doing, while it works and room is left.
            let (what, _, tone) = crate::widgets::agent_box::doing(agent, &decor);
            let after = work_x + width(&work) as i32 + 3;
            let left = (i32::from(area.right()) - after).max(0) as usize;
            if tone == crate::widgets::agent_box::Tone::Live && left > 6 {
                put_str(buf, area, after - 2, y, "·", theme.fg(theme.dim));
                put_str(
                    buf,
                    area,
                    after,
                    y,
                    &fit(&what, left),
                    theme.fg(theme.muted),
                );
            }
        }
    }
}

// ---- message bus -------------------------------------------------------------

fn render_bus(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    let x = i32::from(area.x);
    let y = i32::from(area.y);
    let p2p_live = app.anims.p2p_live(now);

    let label = "message bus ── ";
    put_str(buf, area, x + 1, y, label, theme.fg(theme.muted));
    let state_x = x + 1 + width(label) as i32;
    if p2p_live {
        put_str(
            buf,
            area,
            state_x,
            y,
            "private channel · orchestrator not in loop",
            theme.bold(theme.p2p),
        );
    } else {
        let p2p = app.messages.iter().filter(|m| m.is_p2p()).count();
        let summary = format!("{} messages · {} peer-to-peer", app.messages.len(), p2p);
        put_str(buf, area, state_x, y, &summary, theme.fg(theme.dim));
    }

    let rows = usize::from(area.height.saturating_sub(1));
    if rows == 0 {
        return;
    }
    let visible: Vec<&Message> = app
        .messages
        .iter()
        .skip(app.messages.len().saturating_sub(rows))
        .collect();
    if visible.is_empty() {
        put_str(
            buf,
            area,
            x + 1,
            y + 1,
            "no messages yet",
            theme.fg(theme.dim),
        );
        return;
    }
    let id_w = visible
        .iter()
        .flat_map(|m| [width(&m.from), width(&m.to)])
        .max()
        .unwrap_or(8)
        .clamp(8, 14);
    for (i, msg) in visible.iter().enumerate() {
        bus_row(buf, area, y + 1 + i as i32, msg, id_w, &app.agents, theme);
    }
}

/// `09:20:00.006      claude-3 → claude-5      your trait bound broke`
fn bus_row(
    buf: &mut Buffer,
    area: Rect,
    y: i32,
    msg: &Message,
    id_w: usize,
    agents: &[Agent],
    theme: &Theme,
) {
    // Names wear the colour of the CLI the agent runs on.
    let from_color = theme.name_color(agents, &msg.from);
    let mut x = i32::from(area.x) + 1;
    let clock = fmt_clock(msg.timestamp);
    put_str(buf, area, x, y, &clock, theme.fg(theme.dim));
    x += width(&clock) as i32 + 2;

    let p2p = msg.is_p2p();
    let (arrow, arrow_color) = if msg.is_broadcast() {
        ("⇶", from_color)
    } else if p2p {
        ("→", theme.accent)
    } else {
        ("⇉", theme.orch)
    };
    put_str(
        buf,
        area,
        x,
        y,
        &pad_left(&msg.from, id_w),
        theme.fg(from_color),
    );
    x += id_w as i32 + 1;
    put_str(buf, area, x, y, arrow, theme.bold(arrow_color));
    x += 2;
    let to_color = if msg.is_broadcast() {
        theme.muted
    } else {
        theme.name_color(agents, &msg.to)
    };
    put_str(
        buf,
        area,
        x,
        y,
        &pad_right(&msg.to, id_w),
        theme.fg(to_color),
    );
    x += id_w as i32 + 2;

    let room = (i32::from(area.right()) - 1 - x).max(0) as usize;
    if room == 0 {
        return;
    }
    let mut used = 0;
    if msg.kind != MessageType::Message {
        let tag = format!("‹{}› ", msg.kind.as_str().replace('_', " "));
        if width(&tag) + 4 < room {
            put_str(buf, area, x, y, &tag, theme.fg(theme.dim));
            used = width(&tag);
        }
    }
    let body_color = if p2p { theme.text } else { theme.muted };
    put_str(
        buf,
        area,
        x + used as i32,
        y,
        &fit(&one_line(&msg.body), room - used),
        theme.fg(body_color),
    );
}

// ---- footer ------------------------------------------------------------------

/// Key hints, longest first; the footer shows the longest one that fits.
const HINTS: &[&str] = &[
    "? help · tab select · ⏎ inspect · m message · / command · t tasks · l logs · q quit",
    "? help · tab select · ⏎ inspect · m message · / command · q quit",
    "? help · ⏎ inspect · m message · / command · q quit",
    "? help · / command · q quit",
    "? help · q quit",
];

fn render_footer(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme, now: i64) {
    let y = area.y;
    let bar = Style::default().bg(theme.bar_bg);
    fill_row(buf, area, y, bar);
    let x = i32::from(area.x) + 1;
    let w = usize::from(area.width).saturating_sub(2);
    let yi = i32::from(y);

    if app.stopping {
        let text = "stopping the team: agents first, then the daemon…";
        put_str(
            buf,
            area,
            x,
            yi,
            &fit(text, w),
            theme.bold(theme.warn).bg(theme.bar_bg),
        );
        return;
    }
    if let Some(status) = &app.status {
        let color = if status.is_error { theme.err } else { theme.ok };
        put_str(buf, area, x, yi, &fit(&status.text, w), bar.fg(color));
        return;
    }
    if app.anims.p2p_live(now) {
        let text = "p2p channel OPEN · agents talking directly";
        put_str(
            buf,
            area,
            x,
            yi,
            &fit(text, w),
            theme.bold(theme.accent).bg(theme.bar_bg),
        );
        return;
    }

    let mut left = String::from("crew");
    if let Some(project) = &app.session.project {
        left.push_str(" · ");
        left.push_str(project);
    }
    if let Some(style) = &app.session.style {
        left.push_str(" · ");
        left.push_str(style);
    }
    if !app.session.backend.is_empty() {
        left.push_str(" · ");
        left.push_str(&app.session.backend);
    }
    if let Some(elapsed) = app.elapsed_ms(now) {
        left.push_str(" · ");
        left.push_str(&fmt_elapsed(elapsed));
    }
    let (done, total) = app.task_counts();
    if total > 0 {
        left.push_str(&format!(" · {done}/{total} tasks"));
    }
    let left = fit(&left, w);
    put_str(buf, area, x, yi, &left, bar.fg(theme.muted));
    let mut cursor = x + width(&left) as i32;

    if app.session.completed_at.is_some() {
        let mut done_text = String::from("  ✓ all tasks done");
        // Done, but for what the operator still has to decide.
        let waiting = app.open_questions().len();
        if waiting > 0 {
            done_text.push_str(&match waiting {
                1 => " · ? your answer is needed".to_string(),
                n => format!(" · ? {n} answers are needed"),
            });
        } else if let Some(summary) = &app.session.summary {
            done_text.push_str(" · ");
            done_text.push_str(&one_line(summary));
        }
        let room = (i32::from(area.right()) - 1 - cursor).max(0) as usize;
        let done_text = fit(&done_text, room);
        let color = if waiting > 0 { theme.warn } else { theme.ok };
        put_str(buf, area, cursor, yi, &done_text, bar.fg(color));
        cursor += width(&done_text) as i32;
    }

    // Key hints go on the right: the longest variant that still fits.
    for hints in HINTS {
        let hints_x = i32::from(area.right()) - 1 - width(hints) as i32;
        if hints_x > cursor + 2 {
            put_str(buf, area, hints_x, yi, hints, bar.fg(theme.dim));
            break;
        }
    }
}
