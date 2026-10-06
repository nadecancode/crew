//! Native graph geometry. Domain order, task layers and relationships arrive
//! from the shared projection; GTK owns only coordinates, zoom and focus.
//!
//! Nodes sit in layered columns, centred per column, at one measured size.
//! Zoom is a uniform transform of the whole drawing, so text scales without
//! rewrapping. Fit puts the whole graph in view; it runs once when a graph
//! first appears and otherwise only on request, never after a push.

use super::common::{self, Select};
use crew_client::api::{EntityKey, Tone};
use gtk::{glib, graphene, gsk, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

const NODE_WIDTH: f64 = 220.0;
const PAD: f64 = 24.0;
const COLUMN_GAP: f64 = 72.0;
/// Between the wrapped columns of one layer.
const SUB_GAP: f64 = 32.0;
const ROW_GAP: f64 = 24.0;
/// Preferred width:height of the whole drawing; wide layers wrap towards it.
const ASPECT: f64 = 1.8;
const MIN_ZOOM: f64 = 0.25;
const MAX_ZOOM: f64 = 2.0;
const ZOOM_STEP: f64 = 1.2;

pub struct Node {
    /// `None` for a structural node without an entity, such as the scheduler
    /// that coordinates a team without an orchestrator agent.
    pub key: Option<EntityKey>,
    pub title: String,
    pub status: Option<(String, Tone)>,
    pub lines: Vec<String>,
    pub layer: u32,
    /// Accessible name, matching the list row for the same entity.
    pub label: String,
    /// Textual alternative for this node's edges.
    pub relations: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// A team member coordinated by the orchestrator or scheduler.
    Coordination,
    /// A subagent tethered to its parent.
    Subagent,
    /// A task dependency, drawn towards the dependent task.
    Dependency,
}

#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub kind: EdgeKind,
}

/// Base (unzoomed) geometry of the current nodes.
#[derive(Default)]
struct Geometry {
    /// Dense layer index per node.
    layers: Vec<usize>,
    /// Visual column per node; a wide layer wraps into several columns.
    columns: Vec<usize>,
    /// Left edge per visual column.
    column_x: Vec<f64>,
    /// Nodes per visual column, top to bottom.
    column_nodes: Vec<Vec<usize>>,
    origins: Vec<(f64, f64)>,
    node_height: f64,
    width: f64,
    height: f64,
}

enum Route {
    Curve((f64, f64), (f64, f64)),
    Lines(Vec<(f64, f64)>),
}

impl Geometry {
    fn gap_after(&self, column: usize) -> f64 {
        self.column_x
            .get(column + 1)
            .map_or(PAD, |next| next - self.column_x[column] - NODE_WIDTH)
    }

    fn gap_before(&self, column: usize) -> f64 {
        column.checked_sub(1).map_or(PAD, |previous| {
            self.column_x[column] - self.column_x[previous] - NODE_WIDTH
        })
    }

    /// The horizontal channel through `column` nearest to `y`: above,
    /// between or below its nodes.
    fn row_channel(&self, column: usize, y: f64) -> f64 {
        let nodes = &self.column_nodes[column];
        let mut channels = Vec::with_capacity(nodes.len() + 1);
        if let Some(&first) = nodes.first() {
            channels.push(self.origins[first].1 - ROW_GAP / 2.0);
        }
        for &node in nodes {
            channels.push(self.origins[node].1 + self.node_height + ROW_GAP / 2.0);
        }
        channels
            .into_iter()
            .min_by(|a, b| (a - y).abs().total_cmp(&(b - y).abs()))
            .unwrap_or(y)
    }

    /// Adjacent columns join with a curve. Longer and backward edges run
    /// through the column gaps and between rows, never across a node.
    fn route(&self, from: usize, to: usize) -> Route {
        let (a, b) = (self.columns[from], self.columns[to]);
        let half = self.node_height / 2.0;
        let start = (
            self.origins[from].0 + NODE_WIDTH,
            self.origins[from].1 + half,
        );
        let end = (self.origins[to].0, self.origins[to].1 + half);
        if b == a + 1 {
            return Route::Curve(start, end);
        }
        let mut x = self.column_x[a] + NODE_WIDTH + self.gap_after(a) / 2.0;
        let mut points = vec![start, (x, start.1)];
        let crossed: Vec<usize> = if b > a {
            (a + 1..b).collect()
        } else {
            (b..=a).rev().collect()
        };
        for column in crossed {
            let y = self.row_channel(column, end.1);
            points.push((x, y));
            x = if b > a {
                self.column_x[column] + NODE_WIDTH + self.gap_after(column) / 2.0
            } else {
                self.column_x[column] - self.gap_before(column) / 2.0
            };
            points.push((x, y));
        }
        points.push((x, end.1));
        points.push(end);
        Route::Lines(points)
    }
}

/// Rows per visual column: the wrap whose drawing is closest to ASPECT.
fn rows_per_column(layer_sizes: &[usize], node_height: f64) -> usize {
    let largest = layer_sizes.iter().copied().max().unwrap_or(1).max(1);
    let size = |rows: usize| {
        let columns: usize = layer_sizes.iter().map(|n| n.div_ceil(rows)).sum();
        let width = 2.0 * PAD
            + columns as f64 * NODE_WIDTH
            + layer_sizes.len().saturating_sub(1) as f64 * COLUMN_GAP
            + columns.saturating_sub(layer_sizes.len()) as f64 * SUB_GAP;
        let shown = rows.min(largest);
        let height =
            2.0 * PAD + shown as f64 * node_height + shown.saturating_sub(1) as f64 * ROW_GAP;
        (width / height / ASPECT).ln().abs()
    };
    (1..=largest)
        .min_by(|&a, &b| size(a).total_cmp(&size(b)))
        .unwrap_or(1)
}

struct Inner {
    scroll: gtk::ScrolledWindow,
    fixed: gtk::Fixed,
    canvas: gtk::DrawingArea,
    widgets: RefCell<Vec<gtk::Widget>>,
    keys: RefCell<Vec<Option<EntityKey>>>,
    edges: RefCell<Vec<Edge>>,
    geometry: RefCell<Geometry>,
    selected: RefCell<Option<EntityKey>>,
    zoom: Cell<f64>,
    fitted: Cell<bool>,
    select: Select,
    font: RefCell<Option<(gtk::Settings, glib::SignalHandlerId)>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some((settings, handler)) = self.font.borrow_mut().take() {
            settings.disconnect(handler);
        }
    }
}

#[derive(Clone)]
pub struct Graph {
    pub root: gtk::Box,
    inner: Rc<Inner>,
}

impl Graph {
    pub fn new(label: &str, select: Select) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let minus = gtk::Button::with_label("Zoom out");
        let plus = gtk::Button::with_label("Zoom in");
        let fit = gtk::Button::with_label("Fit graph");
        controls.append(&minus);
        controls.append(&plus);
        controls.append(&fit);
        root.append(&controls);
        let fixed = gtk::Fixed::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        fixed.update_property(&[
            gtk::accessible::Property::Label(label),
            gtk::accessible::Property::Description(
                "Arrow keys move between nodes; Enter opens the selected node",
            ),
        ]);
        let canvas = gtk::DrawingArea::new();
        canvas.set_can_target(false);
        fixed.put(&canvas, 0.0, 0.0);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&fixed)
            .hexpand(true)
            .vexpand(true)
            .build();
        root.append(&scroll);
        let inner = Rc::new(Inner {
            scroll,
            fixed,
            canvas,
            widgets: RefCell::new(Vec::new()),
            keys: RefCell::new(Vec::new()),
            edges: RefCell::new(Vec::new()),
            geometry: RefCell::new(Geometry::default()),
            selected: RefCell::new(None),
            zoom: Cell::new(1.0),
            fitted: Cell::new(false),
            select,
            font: RefCell::new(None),
        });
        let weak = Rc::downgrade(&inner);
        inner.canvas.set_draw_func({
            let weak = weak.clone();
            move |area, cr, _, _| {
                if let Some(inner) = weak.upgrade() {
                    inner.draw(area, cr);
                }
            }
        });
        minus.connect_clicked({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.fitted.set(true);
                    inner.set_zoom(inner.zoom.get() / ZOOM_STEP);
                }
            }
        });
        plus.connect_clicked({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.fitted.set(true);
                    inner.set_zoom(inner.zoom.get() * ZOOM_STEP);
                }
            }
        });
        fit.connect_clicked({
            let weak = weak.clone();
            move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.fit();
                }
            }
        });
        // The first allocation with nodes fits the graph once. Later pushes,
        // resizes and navigation keep the operator's zoom and position.
        inner.scroll.hadjustment().connect_changed({
            let weak = weak.clone();
            move |adjustment| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                if inner.fitted.get()
                    || adjustment.page_size() <= 0.0
                    || inner.widgets.borrow().is_empty()
                {
                    return;
                }
                inner.fitted.set(true);
                let weak = Rc::downgrade(&inner);
                glib::idle_add_local_once(move || {
                    if let Some(inner) = weak.upgrade() {
                        inner.fit();
                    }
                });
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let weak = weak.clone();
            move |_, key, _, _| {
                weak.upgrade()
                    .map_or(glib::Propagation::Proceed, |inner| inner.key(key))
            }
        });
        inner.fixed.add_controller(keys);
        if let Some(settings) = gtk::Settings::default() {
            let handler = settings.connect_notify_local(Some("gtk-font-name"), {
                let weak = weak.clone();
                move |_, _| {
                    let weak = weak.clone();
                    glib::idle_add_local_once(move || {
                        if let Some(inner) = weak.upgrade() {
                            inner.layout();
                        }
                    });
                }
            });
            *inner.font.borrow_mut() = Some((settings, handler));
        }
        Self { root, inner }
    }

    /// Replace the nodes and edges. Order within a layer is the caller's list
    /// order. Selection, keyboard focus, zoom and scroll position survive.
    pub fn update(&self, nodes: Vec<Node>, edges: Vec<Edge>) {
        let inner = &self.inner;
        let focused = inner
            .widgets
            .borrow()
            .iter()
            .position(|w| w.is_focus())
            .and_then(|i| inner.keys.borrow().get(i).cloned().flatten());
        let x = inner.scroll.hadjustment().value();
        let y = inner.scroll.vadjustment().value();
        for widget in inner.widgets.borrow_mut().drain(..) {
            inner.fixed.remove(&widget);
        }
        if nodes.is_empty() {
            inner.fitted.set(false);
        }
        let weak = Rc::downgrade(inner);
        let mut widgets = Vec::with_capacity(nodes.len());
        for node in &nodes {
            let widget = node_widget(node, &weak);
            inner.fixed.put(&widget, 0.0, 0.0);
            widgets.push(widget);
        }
        *inner.edges.borrow_mut() = edges
            .into_iter()
            .filter(|e| e.from < nodes.len() && e.to < nodes.len() && e.from != e.to)
            .collect();
        *inner.keys.borrow_mut() = nodes.iter().map(|n| n.key.clone()).collect();
        let layers = layers(&nodes);
        *inner.widgets.borrow_mut() = widgets;
        inner.geometry.borrow_mut().layers = layers;
        inner.mark_selection();
        inner.layout();
        inner.scroll.hadjustment().set_value(x);
        inner.scroll.vadjustment().set_value(y);
        if let Some(key) = focused {
            let index = inner
                .keys
                .borrow()
                .iter()
                .position(|k| k.as_ref() == Some(&key));
            if let Some(index) = index {
                inner.focus(index);
            }
        }
    }

    /// Mark an entity selected elsewhere (for example in the list) so both
    /// presentations show the same selection.
    pub fn set_selected(&self, key: Option<&EntityKey>) {
        *self.inner.selected.borrow_mut() = key.cloned();
        self.inner.mark_selection();
    }

    pub fn zoom(&self) -> f64 {
        self.inner.zoom.get()
    }

    /// Base (unzoomed) content size.
    pub fn extent(&self) -> (f64, f64) {
        let geometry = self.inner.geometry.borrow();
        (geometry.width, geometry.height)
    }

    /// Node widgets in caller order.
    pub fn nodes(&self) -> Vec<gtk::Widget> {
        self.inner.widgets.borrow().clone()
    }

    pub fn fit(&self) {
        self.inner.fit();
    }

    /// Base (unzoomed) top-left corner of each node, in caller order.
    pub fn origins(&self) -> Vec<(f64, f64)> {
        self.inner.geometry.borrow().origins.clone()
    }

    pub fn edges(&self) -> Vec<Edge> {
        self.inner.edges.borrow().clone()
    }

    /// The handler the graph's key controller runs for a key pressed while a
    /// node has focus. Returns whether the key was consumed.
    pub fn navigate(&self, key: gtk::gdk::Key) -> bool {
        self.inner.key(key) == glib::Propagation::Stop
    }
}

/// Map each node's layer to a dense index, so an unused layer never leaves an
/// empty column.
fn layers(nodes: &[Node]) -> Vec<usize> {
    let mut layers: Vec<u32> = nodes.iter().map(|n| n.layer).collect();
    layers.sort_unstable();
    layers.dedup();
    nodes
        .iter()
        .map(|n| layers.binary_search(&n.layer).unwrap_or(0))
        .collect()
}

fn node_widget(node: &Node, inner: &Weak<Inner>) -> gtk::Widget {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let title = gtk::Label::builder()
        .label(&node.title)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    title.add_css_class("crew-title");
    body.append(&title);
    if let Some((text, tone)) = &node.status {
        body.append(&common::status_chip(text, *tone));
    }
    for line in &node.lines {
        let label = gtk::Label::builder()
            .label(line)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .build();
        label.add_css_class("crew-muted");
        body.append(&label);
    }
    let widget: gtk::Widget = match &node.key {
        Some(key) => {
            let button = gtk::Button::builder().child(&body).focusable(false).build();
            button.connect_clicked({
                let key = key.clone();
                let inner = inner.clone();
                move |button| {
                    if let Some(inner) = inner.upgrade() {
                        *inner.selected.borrow_mut() = Some(key.clone());
                        inner.mark_selection();
                        button.grab_focus();
                        (inner.select)(key.clone());
                    }
                }
            });
            button.upcast()
        }
        None => {
            body.add_css_class("crew-graph-hub");
            body.upcast()
        }
    };
    widget.add_css_class("crew-graph-node");
    widget.update_property(&[
        gtk::accessible::Property::Label(&node.label),
        gtk::accessible::Property::Description(&node.relations),
    ]);
    widget
}

impl Inner {
    fn interactive(&self, index: usize) -> bool {
        self.keys.borrow().get(index).is_some_and(Option::is_some)
    }

    /// One tab stop: the selected node, else the first interactive node.
    /// Arrow keys move between nodes from there.
    fn mark_selection(&self) {
        let keys = self.keys.borrow();
        let selected = self.selected.borrow();
        let stop = keys
            .iter()
            .position(|k| k.is_some() && k.as_ref() == selected.as_ref())
            .or_else(|| keys.iter().position(Option::is_some));
        for (index, (widget, key)) in self.widgets.borrow().iter().zip(keys.iter()).enumerate() {
            let is_selected = key.is_some() && key.as_ref() == selected.as_ref();
            if is_selected {
                widget.add_css_class("selected");
            } else {
                widget.remove_css_class("selected");
            }
            if key.is_some() {
                widget.update_state(&[gtk::accessible::State::Selected(Some(is_selected))]);
            }
            widget.set_focusable(Some(index) == stop);
        }
    }

    fn focus(&self, index: usize) {
        if !self.interactive(index) {
            return;
        }
        for (i, widget) in self.widgets.borrow().iter().enumerate() {
            widget.set_focusable(i == index);
        }
        if let Some(widget) = self.widgets.borrow().get(index) {
            widget.grab_focus();
        }
    }

    fn key(&self, key: gtk::gdk::Key) -> glib::Propagation {
        let Some(current) = self.widgets.borrow().iter().position(|w| w.is_focus()) else {
            return glib::Propagation::Proceed;
        };
        let target = match key {
            gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => self.step_in_column(current, false),
            gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => self.step_in_column(current, true),
            gtk::gdk::Key::Left | gtk::gdk::Key::KP_Left => self.step_across(current, false),
            gtk::gdk::Key::Right | gtk::gdk::Key::KP_Right => self.step_across(current, true),
            gtk::gdk::Key::Home | gtk::gdk::Key::KP_Home => {
                (0..self.widgets.borrow().len()).find(|&i| self.interactive(i))
            }
            gtk::gdk::Key::End | gtk::gdk::Key::KP_End => (0..self.widgets.borrow().len())
                .rev()
                .find(|&i| self.interactive(i)),
            _ => return glib::Propagation::Proceed,
        };
        if let Some(target) = target {
            self.focus(target);
        }
        glib::Propagation::Stop
    }

    /// Up and Down follow list order within a layer, across its wrapped
    /// columns.
    fn step_in_column(&self, current: usize, forward: bool) -> Option<usize> {
        let geometry = self.geometry.borrow();
        let layer = *geometry.layers.get(current)?;
        let same: Vec<usize> = (0..geometry.layers.len())
            .filter(|&i| geometry.layers[i] == layer && self.interactive(i))
            .collect();
        let at = same.iter().position(|&i| i == current)?;
        if forward {
            same.get(at + 1).copied()
        } else {
            at.checked_sub(1).map(|at| same[at])
        }
    }

    /// The nearest interactive node by vertical position in the closest
    /// populated column on that side.
    fn step_across(&self, current: usize, forward: bool) -> Option<usize> {
        let geometry = self.geometry.borrow();
        let column = *geometry.columns.get(current)?;
        let y = geometry.origins.get(current)?.1;
        let last = geometry.columns.iter().copied().max()?;
        let mut next = column;
        loop {
            next = if forward {
                if next >= last {
                    return None;
                }
                next + 1
            } else {
                next.checked_sub(1)?
            };
            let best = (0..geometry.columns.len())
                .filter(|&i| geometry.columns[i] == next && self.interactive(i))
                .min_by(|&a, &b| {
                    (geometry.origins[a].1 - y)
                        .abs()
                        .total_cmp(&(geometry.origins[b].1 - y).abs())
                });
            if best.is_some() {
                return best;
            }
        }
    }

    /// Measure one node height at the base width from native text, then place
    /// layers left to right, each centred vertically and wrapped into columns
    /// when that brings the drawing closer to ASPECT.
    fn layout(&self) {
        let widgets = self.widgets.borrow();
        let width = NODE_WIDTH as i32;
        let mut height = 0;
        for widget in widgets.iter() {
            // Clear the previous request first so text-scale changes can
            // shrink nodes as well as grow them.
            widget.set_size_request(width, -1);
            height = height.max(widget.measure(gtk::Orientation::Vertical, width).1);
        }
        for widget in widgets.iter() {
            widget.set_size_request(width, height);
        }
        drop(widgets);
        let mut geometry = self.geometry.borrow_mut();
        let node_height = f64::from(height);
        let count = geometry.layers.iter().copied().max().map_or(0, |l| l + 1);
        let mut members = vec![Vec::new(); count];
        for (node, &layer) in geometry.layers.iter().enumerate() {
            members[layer].push(node);
        }
        let sizes: Vec<usize> = members.iter().map(Vec::len).collect();
        let rows = rows_per_column(&sizes, node_height);
        let block = |n: usize| {
            let n = n.min(rows);
            n as f64 * node_height + n.saturating_sub(1) as f64 * ROW_GAP
        };
        let tallest = sizes.iter().map(|&n| block(n)).fold(0.0, f64::max);
        let mut origins = vec![(0.0, 0.0); geometry.layers.len()];
        let mut columns = vec![0; geometry.layers.len()];
        let mut column_x = Vec::new();
        let mut column_nodes = Vec::new();
        let mut x = PAD;
        for (layer, nodes) in members.iter().enumerate() {
            let top = PAD + (tallest - block(nodes.len())) / 2.0;
            for (wrap, chunk) in nodes.chunks(rows).enumerate() {
                if wrap > 0 {
                    x += NODE_WIDTH + SUB_GAP;
                } else if layer > 0 {
                    x += NODE_WIDTH + COLUMN_GAP;
                }
                for (row, &node) in chunk.iter().enumerate() {
                    origins[node] = (x, top + row as f64 * (node_height + ROW_GAP));
                    columns[node] = column_x.len();
                }
                column_x.push(x);
                column_nodes.push(chunk.to_vec());
            }
        }
        geometry.width = if column_x.is_empty() {
            0.0
        } else {
            x + NODE_WIDTH + PAD
        };
        geometry.height = if column_x.is_empty() {
            0.0
        } else {
            2.0 * PAD + tallest
        };
        geometry.origins = origins;
        geometry.columns = columns;
        geometry.column_x = column_x;
        geometry.column_nodes = column_nodes;
        geometry.node_height = node_height;
        drop(geometry);
        self.apply_zoom();
    }

    fn apply_zoom(&self) {
        let z = self.zoom.get();
        let geometry = self.geometry.borrow();
        for (widget, &(x, y)) in self.widgets.borrow().iter().zip(geometry.origins.iter()) {
            let transform = gsk::Transform::new()
                .translate(&graphene::Point::new((x * z) as f32, (y * z) as f32))
                .scale(z as f32, z as f32);
            self.fixed.set_child_transform(widget, Some(&transform));
        }
        let width = (geometry.width * z).ceil().min(f64::from(i32::MAX)) as i32;
        let height = (geometry.height * z).ceil().min(f64::from(i32::MAX)) as i32;
        self.canvas.set_content_width(width);
        self.canvas.set_content_height(height);
        self.fixed.set_size_request(width, height);
        self.canvas.queue_draw();
    }

    /// Zoom about the centre of the visible region.
    fn set_zoom(&self, zoom: f64) {
        let old = self.zoom.get();
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let h = self.scroll.hadjustment();
        let v = self.scroll.vadjustment();
        let cx = (h.value() + h.page_size() / 2.0) / old;
        let cy = (v.value() + v.page_size() / 2.0) / old;
        self.zoom.set(zoom);
        self.apply_zoom();
        let (width, height) = {
            let geometry = self.geometry.borrow();
            (geometry.width * zoom, geometry.height * zoom)
        };
        for (adjustment, centre, extent) in [(&h, cx, width), (&v, cy, height)] {
            let upper = extent.max(adjustment.page_size());
            adjustment.set_upper(upper);
            adjustment.set_value(
                (centre * zoom - adjustment.page_size() / 2.0)
                    .clamp(0.0, upper - adjustment.page_size()),
            );
        }
    }

    /// Scale the whole graph into the visible region, never above 1:1.
    fn fit(&self) {
        let (width, height) = {
            let geometry = self.geometry.borrow();
            (geometry.width, geometry.height)
        };
        let view_width = f64::from(self.scroll.width());
        let view_height = f64::from(self.scroll.height());
        self.fitted.set(true);
        if width <= 0.0 || height <= 0.0 || view_width <= 0.0 || view_height <= 0.0 {
            return;
        }
        let zoom = ((view_width / width).min(view_height / height) * 0.99).clamp(MIN_ZOOM, 1.0);
        self.zoom.set(zoom);
        self.apply_zoom();
        self.scroll.hadjustment().set_value(0.0);
        self.scroll.vadjustment().set_value(0.0);
    }

    fn draw(&self, area: &gtk::DrawingArea, cr: &gtk::cairo::Context) {
        let geometry = self.geometry.borrow();
        let z = self.zoom.get();
        #[allow(deprecated)]
        let color = area.style_context().color();
        let (r, g, b) = (
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
        );
        cr.scale(z, z);
        for edge in self.edges.borrow().iter() {
            if edge.from >= geometry.origins.len() || edge.to >= geometry.origins.len() {
                continue;
            }
            let (alpha, width, dashed) = match edge.kind {
                EdgeKind::Coordination => (0.35, 1.2, false),
                EdgeKind::Subagent => (0.6, 1.4, true),
                EdgeKind::Dependency => (0.7, 1.6, false),
            };
            cr.set_source_rgba(r, g, b, alpha);
            cr.set_line_width(width);
            if dashed {
                cr.set_dash(&[5.0, 4.0], 0.0);
            } else {
                cr.set_dash(&[], 0.0);
            }
            let end = match geometry.route(edge.from, edge.to) {
                Route::Curve(start, end) => {
                    let reach = (end.0 - start.0) / 2.0;
                    cr.move_to(start.0, start.1);
                    cr.curve_to(start.0 + reach, start.1, end.0 - reach, end.1, end.0, end.1);
                    end
                }
                Route::Lines(points) => {
                    rounded(cr, &points);
                    *points.last().expect("routed edge")
                }
            };
            let _ = cr.stroke();
            if edge.kind == EdgeKind::Dependency {
                cr.set_dash(&[], 0.0);
                cr.move_to(end.0 - 8.0, end.1 - 5.0);
                cr.line_to(end.0, end.1);
                cr.line_to(end.0 - 8.0, end.1 + 5.0);
                let _ = cr.stroke();
            }
        }
    }
}

/// A polyline with softened corners.
fn rounded(cr: &gtk::cairo::Context, points: &[(f64, f64)]) {
    let Some(&(x, y)) = points.first() else {
        return;
    };
    cr.move_to(x, y);
    for window in points.windows(3) {
        let [a, p, b] = [window[0], window[1], window[2]];
        let (da, db) = (
            ((p.0 - a.0).abs() + (p.1 - a.1).abs()) / 2.0,
            ((b.0 - p.0).abs() + (b.1 - p.1).abs()) / 2.0,
        );
        let radius = 6.0_f64.min(da).min(db);
        let toward = |from: (f64, f64), to: (f64, f64), distance: f64| {
            let length = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
            if length == 0.0 {
                from
            } else {
                (
                    from.0 + (to.0 - from.0) / length * distance,
                    from.1 + (to.1 - from.1) / length * distance,
                )
            }
        };
        let before = toward(p, a, radius);
        let after = toward(p, b, radius);
        cr.line_to(before.0, before.1);
        cr.curve_to(p.0, p.1, p.0, p.1, after.0, after.1);
    }
    if let Some(&(x, y)) = points.last() {
        cr.line_to(x, y);
    }
}
