//! Pure layered-graph geometry: node positions, layer bands and orthogonal
//! edge routes in base (unzoomed) coordinates. No GTK types.
//!
//! Layers run left to right, each centred vertically; nodes keep the
//! caller's order within a layer. When the whole drawing would need to shrink
//! below a readable scale, wide layers wrap into several columns. Wrapped
//! layers become bands: each layer gets a tinted band with a title and a
//! clearly larger gap to the next layer, so depth stays readable.
//!
//! Every edge is orthogonal. Vertical runs use only the gaps between columns;
//! horizontal runs use only a node's own row (its stub), the channel above or
//! below a layer's rows, or the channel below the drawing for edges that go
//! backwards. No edge runs between sibling rows or across a node. Each source
//! has its own track in every gap and channel, so edges from one source merge
//! into one tree and edges from different sources never share a segment.

use std::collections::BTreeMap;

pub const NODE_WIDTH: f64 = 220.0;
/// Outer margin.
pub const PAD: f64 = 24.0;
/// Between layers when no layer wraps.
const COLUMN_GAP: f64 = 72.0;
/// Between layer bands.
pub const BAND_GAP: f64 = 104.0;
/// Between the wrapped columns of one layer.
pub const SUB_GAP: f64 = 36.0;
pub const ROW_GAP: f64 = 24.0;
/// Inside a band, around its rows.
pub const BAND_PAD: f64 = 12.0;
/// Band title height.
pub const HEADER: f64 = 24.0;
/// Horizontal channel above and below a layer's rows.
const CHANNEL: f64 = 20.0;
/// Below the drawing, for edges that run backwards (dependency cycles).
const BOTTOM: f64 = 20.0;
/// Without a viewport, aim for this width:height.
const ASPECT: f64 = 1.8;
/// Prefer one column per layer while Fit can keep at least this scale.
pub const READABLE: f64 = 0.8;

#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    pub layer: u32,
    /// Global visual column indices, left to right.
    pub columns: Vec<usize>,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub rows_top: f64,
    pub rows_bottom: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub node_height: f64,
    /// Top-left corner per node.
    pub origins: Vec<(f64, f64)>,
    /// Dense layer index per node.
    pub layers: Vec<usize>,
    /// Global visual column per node.
    pub columns: Vec<usize>,
    pub column_x: Vec<f64>,
    pub bands: Vec<Band>,
    /// Whether any layer wraps, so layers are drawn as titled bands.
    pub banded: bool,
    /// One orthogonal polyline per edge, from the source's right side to the
    /// target's left side.
    pub routes: Vec<Vec<(f64, f64)>>,
    pub width: f64,
    pub height: f64,
}

/// `layers` gives each node's layer; `edges` are (from, to) node indices.
pub fn layout(
    layers: &[u32],
    edges: &[(usize, usize)],
    node_height: f64,
    viewport: Option<(f64, f64)>,
) -> Layout {
    let mut values: Vec<u32> = layers.to_vec();
    values.sort_unstable();
    values.dedup();
    let dense: Vec<usize> = layers
        .iter()
        .map(|layer| values.binary_search(layer).unwrap_or(0))
        .collect();
    let mut members = vec![Vec::new(); values.len()];
    for (node, &layer) in dense.iter().enumerate() {
        members[layer].push(node);
    }
    let sizes: Vec<usize> = members.iter().map(Vec::len).collect();
    let backward = edges.iter().any(|&(a, b)| dense[b] <= dense[a] && a != b);
    let rows = rows(&sizes, node_height, backward, viewport);
    place(
        &values,
        &dense,
        &members,
        rows,
        node_height,
        edges,
        backward,
    )
}

struct Spacing {
    banded: bool,
    pad: f64,
    header: f64,
    channel: f64,
    gap: f64,
}

fn spacing(banded: bool) -> Spacing {
    if banded {
        Spacing {
            banded,
            pad: BAND_PAD,
            header: HEADER,
            channel: CHANNEL,
            gap: BAND_GAP,
        }
    } else {
        Spacing {
            banded,
            pad: 0.0,
            header: 0.0,
            channel: CHANNEL,
            gap: COLUMN_GAP,
        }
    }
}

fn block(n: usize, rows: usize, node_height: f64) -> f64 {
    let shown = n.min(rows);
    shown as f64 * node_height + shown.saturating_sub(1) as f64 * ROW_GAP
}

fn extent(sizes: &[usize], rows: usize, node_height: f64, backward: bool) -> (f64, f64) {
    let largest = sizes.iter().copied().max().unwrap_or(0);
    let s = spacing(rows < largest);
    let bands: f64 = sizes
        .iter()
        .map(|&n| {
            let columns = n.div_ceil(rows).max(1);
            2.0 * s.pad + columns as f64 * NODE_WIDTH + (columns - 1) as f64 * SUB_GAP
        })
        .sum();
    let width = 2.0 * PAD + bands + sizes.len().saturating_sub(1) as f64 * s.gap;
    let tallest = sizes
        .iter()
        .map(|&n| block(n, rows, node_height))
        .fold(0.0, f64::max);
    let height = 2.0 * PAD
        + 2.0 * s.pad
        + s.header
        + 2.0 * s.channel
        + tallest
        + if backward { BOTTOM } else { 0.0 };
    (width, height)
}

/// Rows per column: one column per layer while Fit keeps a readable scale,
/// otherwise the wrap that lets Fit show the drawing largest.
fn rows(sizes: &[usize], node_height: f64, backward: bool, viewport: Option<(f64, f64)>) -> usize {
    let largest = sizes.iter().copied().max().unwrap_or(0).max(1);
    let (view_width, view_height) = viewport
        .filter(|(w, h)| *w > 0.0 && *h > 0.0)
        .unwrap_or((ASPECT * 1000.0, 1000.0));
    let scale = |rows: usize| {
        let (width, height) = extent(sizes, rows, node_height, backward);
        (view_width / width).min(view_height / height).min(1.0)
    };
    if scale(largest) >= READABLE {
        return largest;
    }
    // Ties keep fewer columns.
    (1..=largest)
        .rev()
        .fold((largest, scale(largest)), |best, rows| {
            let value = scale(rows);
            if value > best.1 + 1e-9 {
                (rows, value)
            } else {
                best
            }
        })
        .0
}

fn place(
    values: &[u32],
    dense: &[usize],
    members: &[Vec<usize>],
    rows: usize,
    node_height: f64,
    edges: &[(usize, usize)],
    backward: bool,
) -> Layout {
    let largest = members.iter().map(Vec::len).max().unwrap_or(0);
    if largest == 0 {
        return Layout::default();
    }
    let s = spacing(rows < largest);
    let tallest = members
        .iter()
        .map(|m| block(m.len(), rows, node_height))
        .fold(0.0, f64::max);
    let top = PAD + s.pad + s.header + s.channel;
    let mut layout = Layout {
        node_height,
        origins: vec![(0.0, 0.0); dense.len()],
        layers: dense.to_vec(),
        columns: vec![0; dense.len()],
        banded: s.banded,
        ..Layout::default()
    };
    let mut x = PAD;
    for (layer, nodes) in members.iter().enumerate() {
        if layer > 0 {
            x += s.gap;
        }
        let rows_top = top + (tallest - block(nodes.len(), rows, node_height)) / 2.0;
        let mut band = Band {
            layer: values[layer],
            columns: Vec::new(),
            left: x,
            right: 0.0,
            top: rows_top - s.channel - s.header - s.pad,
            bottom: 0.0,
            rows_top,
            rows_bottom: rows_top + block(nodes.len(), rows, node_height),
        };
        band.bottom = band.rows_bottom + s.channel + s.pad;
        x += s.pad;
        for (wrap, chunk) in nodes.chunks(rows.max(1)).enumerate() {
            if wrap > 0 {
                x += NODE_WIDTH + SUB_GAP;
            }
            let column = layout.column_x.len();
            for (row, &node) in chunk.iter().enumerate() {
                layout.origins[node] = (x, rows_top + row as f64 * (node_height + ROW_GAP));
                layout.columns[node] = column;
            }
            layout.column_x.push(x);
            band.columns.push(column);
        }
        x += NODE_WIDTH + s.pad;
        band.right = x;
        layout.bands.push(band);
    }
    layout.width = x + PAD;
    let content_bottom = top + tallest + s.channel + s.pad;
    layout.height = content_bottom + if backward { BOTTOM } else { 0.0 } + PAD;
    layout.routes = route(&layout, edges, content_bottom);
    layout
}

#[derive(Clone, Copy)]
enum X {
    At(f64),
    Gap(usize, usize),
}

#[derive(Clone, Copy)]
enum Y {
    At(f64),
    Channel(usize, bool, usize),
    Bottom(usize),
}

impl Layout {
    fn band_of_column(&self, column: usize) -> usize {
        self.bands
            .iter()
            .position(|band| band.columns.contains(&column))
            .unwrap_or(0)
    }

    /// Gap `i` lies left of global column `i`; gap `columns` is the right
    /// margin.
    fn gap_range(&self, gap: usize) -> (f64, f64) {
        let count = self.column_x.len();
        let left = if gap == 0 {
            0.0
        } else if gap < count && self.band_of_column(gap - 1) == self.band_of_column(gap) {
            self.column_x[gap - 1] + NODE_WIDTH
        } else {
            self.bands[self.band_of_column(gap - 1)].right
        };
        let right = if gap == count {
            self.width
        } else if gap > 0 && self.band_of_column(gap - 1) == self.band_of_column(gap) {
            self.column_x[gap]
        } else {
            self.bands[self.band_of_column(gap)].left
        };
        (left, right)
    }

    fn mid(&self, node: usize) -> f64 {
        self.origins[node].1 + self.node_height / 2.0
    }
}

/// Evenly spread `count` tracks around `centre` inside `span`.
fn track(index: usize, count: usize, centre: f64, span: f64, most: f64) -> f64 {
    if count <= 1 {
        return centre;
    }
    let step = most.min(span / (count - 1) as f64);
    centre + (index as f64 - (count - 1) as f64 / 2.0) * step
}

fn route(layout: &Layout, edges: &[(usize, usize)], content_bottom: f64) -> Vec<Vec<(f64, f64)>> {
    let first = |band: usize| layout.bands[band].columns[0];
    let last = |band: usize| *layout.bands[band].columns.last().expect("nonempty band");
    // One side per source and band, from the source's height, so the edges
    // of one source form a comb rather than a closed frame.
    let side = |band: usize, y: f64| {
        let b = &layout.bands[band];
        y <= (b.rows_top + b.rows_bottom) / 2.0 + 0.5
    };
    let mut paths: Vec<Vec<(X, Y)>> = Vec::with_capacity(edges.len());
    for &(a, b) in edges {
        let (ya, yb) = (layout.mid(a), layout.mid(b));
        let (ca, cb) = (layout.columns[a], layout.columns[b]);
        let (la, lb) = (layout.layers[a], layout.layers[b]);
        let key = a;
        let mut path = vec![(X::At(layout.origins[a].0 + NODE_WIDTH), Y::At(ya))];
        let mut x = X::At(0.0);
        let mut y = Y::At(ya);
        let to_x = |path: &mut Vec<(X, Y)>, nx: X, y: Y, x: &mut X| {
            *x = nx;
            path.push((nx, y));
        };
        if ca != last(la) {
            to_x(&mut path, X::Gap(ca + 1, key), y, &mut x);
            y = Y::Channel(la, side(la, ya), key);
            path.push((x, y));
        }
        to_x(&mut path, X::Gap(last(la) + 1, key), y, &mut x);
        if lb > la {
            for band in la + 1..lb {
                y = Y::Channel(band, side(band, ya), key);
                path.push((x, y));
                to_x(&mut path, X::Gap(last(band) + 1, key), y, &mut x);
            }
        } else {
            y = Y::Bottom(key);
            path.push((x, y));
            to_x(&mut path, X::Gap(first(lb), key), y, &mut x);
        }
        if cb != first(lb) {
            y = Y::Channel(lb, side(lb, ya), key);
            path.push((x, y));
            to_x(&mut path, X::Gap(cb, key), y, &mut x);
        }
        y = Y::At(yb);
        path.push((x, y));
        path.push((X::At(layout.origins[b].0), y));
        paths.push(path);
    }

    // Tracks: one per source in each gap and channel, ordered by source
    // height so neighbouring bundles cross as little as possible.
    let mut gaps: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut channels: BTreeMap<(usize, bool), Vec<usize>> = BTreeMap::new();
    let mut bottom: Vec<usize> = Vec::new();
    for path in &paths {
        for &(x, y) in path {
            if let X::Gap(gap, key) = x {
                gaps.entry(gap).or_default().push(key);
            }
            match y {
                Y::Channel(band, top, key) => channels.entry((band, top)).or_default().push(key),
                Y::Bottom(key) => bottom.push(key),
                Y::At(_) => {}
            }
        }
    }
    let order = |keys: &mut Vec<usize>| {
        keys.sort_by(|&a, &b| layout.mid(a).total_cmp(&layout.mid(b)).then(a.cmp(&b)));
        keys.dedup();
    };
    gaps.values_mut().for_each(order);
    channels.values_mut().for_each(order);
    order(&mut bottom);
    let gap_x = |gap: usize, key: usize| {
        let keys = &gaps[&gap];
        let (left, right) = layout.gap_range(gap);
        let index = keys.iter().position(|&k| k == key).unwrap_or(0);
        track(
            index,
            keys.len(),
            (left + right) / 2.0,
            (right - left - 12.0).max(0.0),
            10.0,
        )
    };
    let channel_y = |band: usize, top: bool, key: usize| {
        let keys = &channels[&(band, top)];
        let b = &layout.bands[band];
        let centre = if top {
            b.rows_top - CHANNEL / 2.0
        } else {
            b.rows_bottom + CHANNEL / 2.0
        };
        let index = keys.iter().position(|&k| k == key).unwrap_or(0);
        track(index, keys.len(), centre, CHANNEL - 6.0, 6.0)
    };
    let bottom_y = |key: usize| {
        let index = bottom.iter().position(|&k| k == key).unwrap_or(0);
        track(
            index,
            bottom.len(),
            content_bottom + BOTTOM / 2.0,
            BOTTOM - 6.0,
            6.0,
        )
    };
    paths
        .into_iter()
        .map(|path| {
            let mut points: Vec<(f64, f64)> = Vec::with_capacity(path.len());
            for (x, y) in path {
                let point = (
                    match x {
                        X::At(v) => v,
                        X::Gap(gap, key) => gap_x(gap, key),
                    },
                    match y {
                        Y::At(v) => v,
                        Y::Channel(band, top, key) => channel_y(band, top, key),
                        Y::Bottom(key) => bottom_y(key),
                    },
                );
                if points.last() != Some(&point) {
                    points.push(point);
                }
            }
            simplify(points)
        })
        .collect()
}

/// Drop points in the middle of a straight run.
fn simplify(points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(points.len());
    for point in points {
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let straight = (a.0 == b.0 && b.0 == point.0) || (a.1 == b.1 && b.1 == point.1);
            if straight {
                out.pop();
            }
        }
        out.push(point);
    }
    out
}

/// Test check: every route is orthogonal, never crosses a node and never runs
/// horizontally between sibling rows of a layer, except a node's own stub.
#[cfg(test)]
pub fn assert_clean(layout: &Layout) {
    let h = layout.node_height;
    let nodes: Vec<(f64, f64, f64, f64)> = layout
        .origins
        .iter()
        .map(|&(x, y)| (x, y, x + NODE_WIDTH, y + h))
        .collect();
    for route in &layout.routes {
        for pair in route.windows(2) {
            let ((x1, y1), (x2, y2)) = (pair[0], pair[1]);
            assert!(x1 == x2 || y1 == y2, "orthogonal segment {pair:?}");
            let (lx, hx) = (x1.min(x2), x1.max(x2));
            let (ly, hy) = (y1.min(y2), y1.max(y2));
            for &(nx1, ny1, nx2, ny2) in &nodes {
                let crosses = lx < nx2 - 0.5 && hx > nx1 + 0.5 && ly < ny2 - 0.5 && hy > ny1 + 0.5;
                assert!(!crosses, "segment {pair:?} crosses node {:?}", (nx1, ny1));
            }
            let stub = route.first() == Some(&pair[0]) || route.last() == Some(&pair[1]);
            if y1 != y2 || stub {
                continue;
            }
            for band in &layout.bands {
                if hx <= band.left + BAND_PAD + 0.5 || lx >= band.right - BAND_PAD - 0.5 {
                    continue;
                }
                let between = band.columns.iter().any(|&column| {
                    let mut tops: Vec<f64> = layout
                        .origins
                        .iter()
                        .zip(&layout.columns)
                        .filter(|(_, &c)| c == column)
                        .map(|(o, _)| o.1)
                        .collect();
                    tops.sort_by(f64::total_cmp);
                    tops.windows(2).any(|t| y1 > t[0] + h && y1 < t[1])
                });
                assert!(!between, "segment {pair:?} runs between sibling rows");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: f64 = 100.0;

    fn clean(layout: &Layout) {
        assert_clean(layout);
    }

    #[test]
    fn readable_layers_stay_single_columns() {
        let layout = layout(&[0, 1, 1], &[(0, 1), (0, 2)], H, Some((1000.0, 600.0)));
        assert!(!layout.banded);
        assert_eq!(layout.column_x.len(), 2);
        let mid = layout.origins[0].1 + H / 2.0;
        let spread = (layout.origins[1].1 + layout.origins[2].1 + H) / 2.0;
        assert!(
            (mid - spread).abs() < 1e-9,
            "layer 0 centred against layer 1"
        );
        clean(&layout);
    }

    #[test]
    fn wide_layers_wrap_into_titled_bands_with_clean_routes() {
        // Six roots, one task on two of them, one on that task and a root.
        let layers = [0, 0, 0, 0, 0, 0, 1, 2];
        let edges = [(1, 6), (4, 6), (6, 7), (2, 7), (5, 6)];
        let layout = layout(&layers, &edges, H, Some((900.0, 420.0)));
        assert!(layout.banded);
        assert_eq!(layout.bands.len(), 3);
        assert!(layout.bands[0].columns.len() > 1, "layer 0 wraps");
        for pair in layout.bands.windows(2) {
            assert!(pair[1].left - pair[0].right >= BAND_GAP - 1e-9);
        }
        let sub = layout.column_x[1] - layout.column_x[0] - NODE_WIDTH;
        assert!(
            BAND_GAP > 2.0 * sub,
            "bands are clearly further apart than wraps"
        );
        clean(&layout);
        // The edge from root 2 skips layer 1 through its channel.
        let band = &layout.bands[1];
        assert!(layout.routes[3].windows(2).any(|pair| {
            let ((x1, y1), (x2, y2)) = (pair[0], pair[1]);
            y1 == y2
                && x1.min(x2) < band.left
                && x1.max(x2) > band.right
                && (y1 < band.rows_top || y1 > band.rows_bottom)
        }));
    }

    #[test]
    fn one_source_fans_out_as_one_tree() {
        let mut layers = vec![0];
        layers.extend(std::iter::repeat_n(1, 13));
        let edges: Vec<_> = (1..14).map(|to| (0, to)).collect();
        let layout = layout(&layers, &edges, H, Some((925.0, 430.0)));
        assert!(layout.banded && layout.bands[1].columns.len() > 1);
        clean(&layout);
        // Every edge leaves the hub on the same trunk and shares one spine
        // per gap: all vertical runs inside one gap sit on one x.
        let mut xs: BTreeMap<i64, ()> = BTreeMap::new();
        for route in &layout.routes {
            for pair in route.windows(2) {
                if pair[0].0 == pair[1].0 && pair[0].0 < layout.bands[1].left {
                    xs.insert((pair[0].0 * 100.0) as i64, ());
                }
            }
        }
        assert_eq!(xs.len(), 1, "one trunk into the member band");
        // A comb, not a frame: the fan-out uses one channel of the band.
        let band = &layout.bands[1];
        let below = layout
            .routes
            .iter()
            .flatten()
            .any(|&(_, y)| y > band.rows_bottom);
        assert!(!below, "no channel below the members");
    }

    #[test]
    fn backward_edges_return_below_the_drawing() {
        let layout = layout(&[0, 1], &[(0, 1), (1, 0)], H, None);
        clean(&layout);
        let back = &layout.routes[1];
        let lowest = back.iter().map(|p| p.1).fold(f64::MIN, f64::max);
        assert!(
            lowest
                > layout
                    .bands
                    .iter()
                    .map(|b| b.bottom)
                    .fold(f64::MIN, f64::max)
        );
        assert!(lowest < layout.height);
    }
}
