//! Radial geometry: where the orchestrator and agent boxes go, and the cell
//! paths of the links between them.

use ratatui::layout::Rect;

pub const AGENT_H: u16 = 3;
pub const CENTER_H: u16 = 4;
/// The same boxes with one more row, for the context window.
pub const AGENT_H_CONTEXT: u16 = AGENT_H + 1;
pub const CENTER_H_CONTEXT: u16 = CENTER_H + 1;

/// Candidate (agent box width, orchestrator box width) pairs, widest first.
/// The widest leave room for a task's title and what the agent is doing.
const WIDTHS: &[(u16, u16)] = &[
    (30, 26),
    (26, 24),
    (22, 24),
    (20, 22),
    (18, 22),
    (16, 20),
    (14, 18),
    (12, 16),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topology {
    pub center: Rect,
    /// One rectangle per worker, in display order.
    pub agents: Vec<Rect>,
}

/// Place `n` agent boxes on an ellipse around a centred orchestrator box.
///
/// The first agent sits top-left and the rest follow clockwise. Returns `None`
/// when the area cannot hold the boxes without overlap even at the smallest
/// size; callers then fall back to a compact list.
pub fn radial(area: Rect, n: usize) -> Option<Topology> {
    radial_sized(area, n, (AGENT_H, CENTER_H))
}

/// `radial` with boxes of the given (agent, orchestrator) heights.
pub fn radial_sized(area: Rect, n: usize, heights: (u16, u16)) -> Option<Topology> {
    for &(agent_w, center_w) in WIDTHS {
        // Prefer a pleasantly round ellipse; stretch to the full width only
        // when that is what it takes to avoid overlap.
        //
        // "Spread" scales the ring so the outermost agents reach the edges of
        // the area; when that crowds the boxes, the plain ellipse is tried.
        for (capped, spread) in [(true, true), (false, true), (true, false), (false, false)] {
            if let Some(t) = try_radial(area, n, agent_w, center_w, capped, spread, heights) {
                return Some(t);
            }
        }
    }
    None
}

fn angle(i: usize, n: usize) -> f64 {
    let degrees = if n == 1 {
        0.0
    } else {
        -180.0 / n as f64 + i as f64 * 360.0 / n as f64
    };
    degrees.to_radians()
}

fn try_radial(
    area: Rect,
    n: usize,
    agent_w: u16,
    center_w: u16,
    capped: bool,
    spread: bool,
    heights: (u16, u16),
) -> Option<Topology> {
    let (agent_h, center_h) = heights;
    if area.width < center_w + 2 || area.height < center_h {
        return None;
    }
    let cx = f64::from(area.x) + f64::from(area.width) / 2.0;
    let cy = f64::from(area.y) + f64::from(area.height) / 2.0;
    // The hub's first content row lines up with the content row of agents
    // sitting level with it, so their spokes are straight.
    let center = Rect {
        x: (cx - f64::from(center_w) / 2.0).round() as u16,
        y: ((cy - f64::from(agent_h) / 2.0).round() as u16).min(area.bottom() - center_h),
        width: center_w,
        height: center_h,
    };
    if n == 0 {
        return Some(Topology {
            center,
            agents: Vec::new(),
        });
    }
    if area.width < agent_w || area.height < agent_h {
        return None;
    }
    // Keep a row of air above and below when the area can afford it.
    let margin = if area.height >= 18 { 1.0 } else { 0.0 };
    let max_ry = (f64::from(area.height) - f64::from(agent_h)) / 2.0 - margin;
    let max_rx = (f64::from(area.width) - f64::from(agent_w)) / 2.0;
    let max_rx = if capped {
        // Terminal cells are roughly twice as tall as wide, so a visually
        // round ring has rx ≈ 2·ry; allow up to ~4.4 before it looks squashed.
        let clear_of_center = f64::from(agent_w) / 2.0 + f64::from(center_w) / 2.0 + 3.0;
        max_rx.min((max_ry * 4.4).max(clear_of_center))
    } else {
        max_rx
    };
    let (mut rx, mut ry) = (max_rx, max_ry);
    if spread {
        let reach_x = (0..n).map(|i| angle(i, n).sin().abs()).fold(0.0, f64::max);
        let reach_y = (0..n).map(|i| angle(i, n).cos().abs()).fold(0.0, f64::max);
        if reach_x > 0.2 {
            rx /= reach_x;
        }
        if reach_y > 0.2 {
            ry /= reach_y;
        }
    }

    let min_x = f64::from(area.x);
    let max_x = f64::from(area.right() - agent_w);
    let min_y = f64::from(area.y);
    let max_y = f64::from(area.bottom() - agent_h);
    let mut agents = Vec::with_capacity(n);
    for i in 0..n {
        let a = angle(i, n);
        let px = cx + rx * a.sin();
        let py = cy - ry * a.cos();
        let x = (px - f64::from(agent_w) / 2.0).round().clamp(min_x, max_x);
        let y = (py - f64::from(agent_h) / 2.0).round().clamp(min_y, max_y);
        agents.push(Rect {
            x: x as u16,
            y: y as u16,
            width: agent_w,
            height: agent_h,
        });
    }

    for i in 0..n {
        if crowded(agents[i], center) {
            return None;
        }
        for j in i + 1..n {
            if crowded(agents[i], agents[j]) {
                return None;
            }
        }
    }
    Some(Topology { center, agents })
}

/// Smallest topology area in which subagents are drawn as satellite boxes;
/// below it their parent carries a `+N` badge instead.
pub const SATELLITE_MIN_W: u16 = 100;
pub const SATELLITE_MIN_H: u16 = 22;
/// How far (in columns / rows, centre to centre) a satellite may sit from its
/// parent before it stops reading as attached.
const SATELLITE_REACH_X: i32 = 44;
const SATELLITE_REACH_Y: i32 = 9;

/// Width of a satellite box next to agent boxes of width `agent_w`: a
/// little narrower, and never wider than the compact box it is (one row).
pub fn satellite_width(agent_w: u16) -> u16 {
    agent_w.saturating_sub(4).clamp(14, 18)
}

/// Place satellite boxes for subagents: `counts[i]` of them for worker `i`.
///
/// The ring itself is never moved. Each satellite takes the free spot nearest
/// its parent, preferring the side away from the hub, with a row of air around
/// it so the link to the parent stays visible. Returns the rectangles placed
/// per worker, in order; a worker gets fewer than it asked for when the space
/// around it runs out (the caller badges the parent with the remainder), and
/// nothing is placed at all in areas too small for satellites.
pub fn satellites(area: Rect, topo: &Topology, counts: &[usize]) -> Vec<Vec<Rect>> {
    let mut placed: Vec<Vec<Rect>> = vec![Vec::new(); counts.len()];
    if area.width < SATELLITE_MIN_W || area.height < SATELLITE_MIN_H {
        return placed;
    }
    let Some(agent_w) = topo.agents.first().map(|r| r.width) else {
        return placed;
    };
    let width = satellite_width(agent_w);
    let mut taken: Vec<Rect> = topo.agents.clone();
    taken.push(topo.center);
    let hub = center_of(topo.center);
    for (i, &count) in counts.iter().enumerate() {
        let Some(&parent) = topo.agents.get(i) else {
            break;
        };
        for _ in 0..count {
            match satellite_spot(area, parent, hub, width, &taken) {
                Some(rect) => {
                    taken.push(rect);
                    placed[i].push(rect);
                }
                None => break,
            }
        }
    }
    placed
}

/// Two boxes with less than a column of air between them horizontally or a
/// row vertically.
fn cramped(a: Rect, b: Rect) -> bool {
    let x_overlap =
        i32::from(a.x) - 1 < i32::from(b.right()) && i32::from(b.x) < i32::from(a.right()) + 1;
    let y_overlap =
        i32::from(a.y) - 1 < i32::from(b.bottom()) && i32::from(b.y) < i32::from(a.bottom()) + 1;
    x_overlap && y_overlap
}

fn satellite_spot(
    area: Rect,
    parent: Rect,
    hub: (i32, i32),
    width: u16,
    taken: &[Rect],
) -> Option<Rect> {
    if area.width < width || area.height < AGENT_H {
        return None;
    }
    let (px, py) = center_of(parent);
    // Unit vector pointing from the hub through the parent ("outwards"), in
    // visual space where a row is two columns tall.
    let (ox, oy) = {
        let dx = f64::from(px - hub.0) / 2.0;
        let dy = f64::from(py - hub.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 0.5 {
            (0.0, 0.0)
        } else {
            (dx / len, dy / len)
        }
    };
    let x_lo = (px - SATELLITE_REACH_X - i32::from(width) / 2).max(i32::from(area.x));
    let x_hi = (px + SATELLITE_REACH_X - i32::from(width) / 2)
        .min(i32::from(area.right()) - i32::from(width));
    let y_lo = (py - SATELLITE_REACH_Y - 1).max(i32::from(area.y));
    let y_hi = (py + SATELLITE_REACH_Y - 1).min(i32::from(area.bottom()) - i32::from(AGENT_H));

    let mut best: Option<(f64, Rect)> = None;
    for y in y_lo..=y_hi {
        for x in x_lo..=x_hi {
            let rect = Rect {
                x: x as u16,
                y: y as u16,
                width,
                height: AGENT_H,
            };
            if taken.iter().any(|t| cramped(rect, *t)) {
                continue;
            }
            let (cx, cy) = center_of(rect);
            let dx = f64::from(cx - px) / 2.0;
            let dy = f64::from(cy - py);
            let distance = (dx * dx + dy * dy).sqrt();
            let outward = if distance > 0.0 {
                (dx * ox + dy * oy) / distance
            } else {
                0.0
            };
            // Nearest wins; among near-equals, the side away from the hub.
            let cost = distance - 1.5 * outward;
            if best.as_ref().is_none_or(|(c, _)| cost < *c - 1e-9) {
                best = Some((cost, rect));
            }
        }
    }
    best.map(|(_, rect)| rect)
}

/// True when two boxes overlap or touch side by side (one column of air is
/// required horizontally; vertically adjacent rows are fine).
pub fn crowded(a: Rect, b: Rect) -> bool {
    let a_left = i32::from(a.x) - 1;
    let a_right = i32::from(a.right()) + 1;
    let x_overlap = a_left < i32::from(b.right()) && i32::from(b.x) < a_right;
    let y_overlap = a.y < b.bottom() && b.y < a.bottom();
    x_overlap && y_overlap
}

/// The cell links attach to: the middle of a box's first content row.
fn center_of(r: Rect) -> (i32, i32) {
    (
        i32::from(r.x) + i32::from(r.width) / 2,
        i32::from(r.y) + (i32::from(r.height) - 1) / 2,
    )
}

fn inside(r: Rect, c: (i32, i32)) -> bool {
    c.0 >= i32::from(r.x)
        && c.0 < i32::from(r.right())
        && c.1 >= i32::from(r.y)
        && c.1 < i32::from(r.bottom())
}

/// Bresenham line, endpoints included.
pub fn line_cells(from: (i32, i32), to: (i32, i32)) -> Vec<(i32, i32)> {
    let (mut x, mut y) = from;
    let dx = (to.0 - x).abs();
    let dy = -(to.1 - y).abs();
    let sx = if x < to.0 { 1 } else { -1 };
    let sy = if y < to.1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut out = Vec::with_capacity((dx.max(-dy) + 1) as usize);
    loop {
        out.push((x, y));
        if (x, y) == to {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
    out
}

/// Quadratic curve from `a` to `b` bowing `bow` rows sideways at its midpoint.
fn curve_cells(a: (i32, i32), b: (i32, i32), bow: f64) -> Vec<(i32, i32)> {
    let (ax, ay, bx, by) = (
        f64::from(a.0),
        f64::from(a.1),
        f64::from(b.0),
        f64::from(b.1),
    );
    // Work in "visual" space where a row is two columns tall.
    let dx = bx - ax;
    let dy = (by - ay) * 2.0;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let (nx, ny) = (-dy / len, dx / len);
    // The control point sits twice as far out as the curve's apex.
    let ctrl_x = (ax + bx) / 2.0 + nx * bow * 4.0;
    let ctrl_y = (ay + by) / 2.0 + ny * bow * 2.0;

    const STEPS: usize = 48;
    let mut out: Vec<(i32, i32)> = Vec::new();
    let mut prev = a;
    out.push(a);
    for step in 1..=STEPS {
        let t = step as f64 / STEPS as f64;
        let u = 1.0 - t;
        let x = u * u * ax + 2.0 * u * t * ctrl_x + t * t * bx;
        let y = u * u * ay + 2.0 * u * t * ctrl_y + t * t * by;
        let next = (x.round() as i32, y.round() as i32);
        if next != prev {
            out.extend(line_cells(prev, next).into_iter().skip(1));
            prev = next;
        }
    }
    out
}

/// Cells of the link from box `from` to box `to`, in travel order, excluding
/// the cells covered by the two boxes themselves.
///
/// A straight line is used when it is clear of `obstacles`; otherwise the link
/// bows around them, so a peer-to-peer link between opposite agents visibly
/// goes *around* the orchestrator rather than through it.
pub fn route(from: Rect, to: Rect, obstacles: &[Rect], bounds: Rect) -> Vec<(u16, u16)> {
    let a = center_of(from);
    let b = center_of(to);
    let strip = |cells: Vec<(i32, i32)>| -> Vec<(i32, i32)> {
        cells
            .into_iter()
            .filter(|c| !inside(from, *c) && !inside(to, *c))
            .collect()
    };
    let blocked = |cells: &[(i32, i32)]| {
        cells
            .iter()
            .any(|c| obstacles.iter().any(|o| inside(*o, *c)))
    };
    let in_bounds = |cells: &[(i32, i32)]| cells.iter().all(|c| inside(bounds, *c));
    let finish = |cells: Vec<(i32, i32)>| -> Vec<(u16, u16)> {
        cells
            .into_iter()
            .filter(|c| inside(bounds, *c))
            .map(|(x, y)| (x as u16, y as u16))
            .collect()
    };

    let straight = strip(line_cells(a, b));
    if !blocked(&straight) {
        return finish(straight);
    }
    for bow in [3.0, -3.0, 4.0, -4.0, 6.0, -6.0, 8.0, -8.0, 11.0, -11.0] {
        let path = strip(curve_cells(a, b, bow));
        if in_bounds(&path) && !blocked(&path) {
            return finish(path);
        }
    }
    finish(straight)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Topology area the dashboard uses for a terminal of the given size.
    fn topo_area(width: u16, height: u16) -> Rect {
        crate::views::dashboard::split(Rect::new(0, 0, width, height)).0
    }

    #[test]
    fn boxes_never_overlap_and_stay_inside() {
        for (w, h) in [(120u16, 40u16), (80, 24), (100, 30), (160, 50), (200, 60)] {
            let area = topo_area(w, h);
            for n in 2..=10usize {
                let t = radial(area, n).unwrap_or_else(|| panic!("no layout for n={n} at {w}x{h}"));
                assert_eq!(t.agents.len(), n);
                let mut all = t.agents.clone();
                all.push(t.center);
                for (i, a) in all.iter().enumerate() {
                    assert!(
                        a.x >= area.x
                            && a.right() <= area.right()
                            && a.y >= area.y
                            && a.bottom() <= area.bottom(),
                        "box {i} {a:?} escapes {area:?} (n={n}, {w}x{h})"
                    );
                    for b in all.iter().skip(i + 1) {
                        assert!(!a.intersects(*b), "{a:?} overlaps {b:?} (n={n}, {w}x{h})");
                        assert!(!crowded(*a, *b), "{a:?} touches {b:?} (n={n}, {w}x{h})");
                    }
                }
            }
        }
    }

    /// Ways to hand `k` subagents to `n` workers: all on one, or spread.
    fn distributions(n: usize, k: usize) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        for parent in [0, n / 2, n - 1] {
            let mut counts = vec![0; n];
            counts[parent] = k;
            out.push(counts);
        }
        let mut spread = vec![0; n];
        for i in 0..k {
            spread[(i * 3) % n] += 1;
        }
        out.push(spread);
        out
    }

    #[test]
    fn satellites_fit_beside_their_parents_without_overlap() {
        for (w, h) in [(120u16, 40u16), (160, 50), (200, 60)] {
            let area = topo_area(w, h);
            for n in 2..=10usize {
                let topo = radial(area, n).unwrap();
                for k in 1..=4usize {
                    for counts in distributions(n, k) {
                        let placed = satellites(area, &topo, &counts);
                        let ctx = format!("n={n} counts={counts:?} at {w}x{h}");
                        assert_eq!(
                            placed.iter().map(Vec::len).collect::<Vec<_>>(),
                            counts,
                            "every subagent gets a box ({ctx})"
                        );
                        let mut all: Vec<Rect> = topo.agents.clone();
                        all.push(topo.center);
                        all.extend(placed.iter().flatten().copied());
                        for (i, a) in all.iter().enumerate() {
                            assert!(
                                a.x >= area.x
                                    && a.right() <= area.right()
                                    && a.y >= area.y
                                    && a.bottom() <= area.bottom(),
                                "{a:?} escapes {area:?} ({ctx})"
                            );
                            for b in all.iter().skip(i + 1) {
                                assert!(!a.intersects(*b), "{a:?} overlaps {b:?} ({ctx})");
                                assert!(!crowded(*a, *b), "{a:?} touches {b:?} ({ctx})");
                            }
                        }
                        // Attached: each satellite is within reach of its parent.
                        for (parent, children) in topo.agents.iter().zip(&placed) {
                            let (px, py) = center_of(*parent);
                            for child in children {
                                let (cx, cy) = center_of(*child);
                                assert!(
                                    (cx - px).abs() <= SATELLITE_REACH_X
                                        && (cy - py).abs() <= SATELLITE_REACH_Y,
                                    "{child:?} drifted from {parent:?} ({ctx})"
                                );
                                assert!(child.width < parent.width, "satellites are smaller");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_ring_does_not_move_when_subagents_appear() {
        let area = topo_area(120, 40);
        let topo = radial(area, 6).unwrap();
        let before = topo.clone();
        let one = satellites(area, &topo, &[0, 0, 1, 0, 0, 0]);
        let two = satellites(area, &topo, &[0, 0, 2, 0, 0, 0]);
        assert_eq!(topo, before);
        // A second subagent does not displace the first.
        assert_eq!(one[2][0], two[2][0]);
    }

    #[test]
    fn small_areas_fall_back_to_badges() {
        let area = topo_area(80, 24);
        for n in 2..=10usize {
            let topo = radial(area, n).unwrap();
            let mut counts = vec![0; n];
            counts[0] = 2;
            let placed = satellites(area, &topo, &counts);
            assert!(placed.iter().all(Vec::is_empty), "n={n}");
            assert_eq!(placed.len(), n);
        }
        // Degenerate inputs never panic.
        let topo = radial(Rect::new(0, 0, 200, 60), 0).unwrap();
        assert!(satellites(Rect::new(0, 0, 200, 60), &topo, &[]).is_empty());
        let topo = radial(Rect::new(0, 0, 120, 30), 3).unwrap();
        assert_eq!(satellites(Rect::new(0, 0, 120, 30), &topo, &[1, 1, 1, 9]).len(), 4);
    }

    #[test]
    fn six_agents_match_the_reference_arrangement() {
        let area = topo_area(120, 40);
        let t = radial(area, 6).unwrap();
        let c = center_of(t.center);
        let pos: Vec<(i32, i32)> = t.agents.iter().map(|r| center_of(*r)).collect();
        // 1 top-left, 2 top-right, 3 right, 4 bottom-right, 5 bottom-left, 6 left.
        assert!(pos[0].0 < c.0 && pos[0].1 < c.1);
        assert!(pos[1].0 > c.0 && pos[1].1 < c.1);
        assert!(pos[2].0 > pos[1].0 && (pos[2].1 - c.1).abs() <= 1);
        assert!(pos[3].0 > c.0 && pos[3].1 > c.1);
        assert!(pos[4].0 < c.0 && pos[4].1 > c.1);
        assert!(pos[5].0 < pos[0].0 && (pos[5].1 - c.1).abs() <= 1);
    }

    #[test]
    fn tiny_areas_yield_no_layout_instead_of_panicking() {
        for (w, h) in [(0u16, 0u16), (1, 1), (10, 3), (20, 8), (30, 5), (12, 40)] {
            for n in 0..=10 {
                let _ = radial(Rect::new(0, 0, w, h), n);
            }
        }
        assert!(radial(Rect::new(0, 0, 20, 6), 6).is_none());
    }

    #[test]
    fn lines_connect_their_endpoints() {
        let cells = line_cells((0, 0), (7, 3));
        assert_eq!(cells.first(), Some(&(0, 0)));
        assert_eq!(cells.last(), Some(&(7, 3)));
        for pair in cells.windows(2) {
            assert!((pair[1].0 - pair[0].0).abs() <= 1 && (pair[1].1 - pair[0].1).abs() <= 1);
        }
        assert_eq!(line_cells((3, 3), (3, 3)), vec![(3, 3)]);
    }

    #[test]
    fn opposite_agents_route_around_the_orchestrator() {
        let area = topo_area(120, 40);
        let t = radial(area, 6).unwrap();
        // claude-6 (left) to claude-3 (right): the straight line crosses the centre box.
        let (left, right) = (t.agents[5], t.agents[2]);
        let path = route(left, right, &[t.center], area);
        assert!(!path.is_empty());
        for (x, y) in &path {
            assert!(
                !inside(t.center, (i32::from(*x), i32::from(*y))),
                "path crosses the orchestrator at {x},{y}"
            );
            assert!(!inside(left, (i32::from(*x), i32::from(*y))));
            assert!(!inside(right, (i32::from(*x), i32::from(*y))));
        }
        // It starts next to the sender and ends next to the receiver.
        assert!(i32::from(path[0].0) - i32::from(left.right()) <= 1);
        assert!(i32::from(right.x) - i32::from(path[path.len() - 1].0) <= 1);
    }

    #[test]
    fn unobstructed_routes_are_straight() {
        let a = Rect::new(0, 0, 10, 3);
        let b = Rect::new(30, 0, 10, 3);
        let path = route(a, b, &[], Rect::new(0, 0, 40, 10));
        assert_eq!(path.len(), 20);
        assert!(path.iter().all(|(_, y)| *y == 1));
        assert_eq!(path[0], (10, 1));
        assert_eq!(path[19], (29, 1));
    }
}
