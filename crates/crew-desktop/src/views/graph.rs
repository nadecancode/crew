//! Native graph geometry. Domain order, task layers and relationships arrive
//! from the shared projection; GTK owns only coordinates, zoom and focus.
//!
//! Nodes sit in layered columns at one measured size; `layout` places them
//! and routes the edges. Zoom is a uniform transform of the whole drawing, so
//! text scales without rewrapping. Fit puts the whole graph in view; it runs
//! once when a graph first appears and otherwise only on request, never after
//! a push.

pub mod layout;

use super::common::{self, Select};
use crew_client::api::{EntityKey, Tone};
use gtk::{glib, graphene, gsk, prelude::*};
use layout::{Layout, NODE_WIDTH};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

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

struct Inner {
    scroll: gtk::ScrolledWindow,
    fixed: gtk::Fixed,
    canvas: gtk::DrawingArea,
    widgets: RefCell<Vec<gtk::Widget>>,
    keys: RefCell<Vec<Option<EntityKey>>>,
    edges: RefCell<Vec<Edge>>,
    layers: RefCell<Vec<u32>>,
    geometry: RefCell<Layout>,
    /// Titles for layer bands, shown only while layers wrap.
    title: Box<dyn Fn(u32) -> Option<String>>,
    titles: RefCell<Vec<gtk::Label>>,
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
    pub fn new(
        label: &str,
        select: Select,
        title: impl Fn(u32) -> Option<String> + 'static,
    ) -> Self {
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
            layers: RefCell::new(Vec::new()),
            geometry: RefCell::new(Layout::default()),
            title: Box::new(title),
            titles: RefCell::new(Vec::new()),
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
        *inner.layers.borrow_mut() = nodes.iter().map(|n| n.layer).collect();
        *inner.widgets.borrow_mut() = widgets;
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

    /// The current base geometry: bands, columns and edge routes.
    pub fn geometry(&self) -> Layout {
        self.inner.geometry.borrow().clone()
    }

    /// Band titles currently shown, left to right.
    pub fn band_titles(&self) -> Vec<String> {
        self.inner
            .titles
            .borrow()
            .iter()
            .map(|label| label.label().to_string())
            .collect()
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
    /// the nodes and route the edges for the current viewport.
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
        let viewport = (self.scroll.width() > 0 && self.scroll.height() > 0).then(|| {
            (
                f64::from(self.scroll.width()),
                f64::from(self.scroll.height()),
            )
        });
        let edges: Vec<(usize, usize)> =
            self.edges.borrow().iter().map(|e| (e.from, e.to)).collect();
        let geometry = layout::layout(&self.layers.borrow(), &edges, f64::from(height), viewport);
        for label in self.titles.borrow_mut().drain(..) {
            self.fixed.remove(&label);
        }
        if geometry.banded {
            for band in &geometry.bands {
                if let Some(text) = (self.title)(band.layer) {
                    let label = gtk::Label::builder()
                        .label(&text)
                        .xalign(0.0)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .width_request((band.right - band.left - 2.0 * layout::BAND_PAD) as i32)
                        .build();
                    label.add_css_class("crew-graph-band");
                    label.add_css_class("crew-muted");
                    self.fixed.put(&label, 0.0, 0.0);
                    self.titles.borrow_mut().push(label);
                }
            }
        }
        *self.geometry.borrow_mut() = geometry;
        self.apply_zoom();
    }

    fn apply_zoom(&self) {
        let z = self.zoom.get();
        let geometry = self.geometry.borrow();
        let place = |widget: &gtk::Widget, x: f64, y: f64| {
            let transform = gsk::Transform::new()
                .translate(&graphene::Point::new((x * z) as f32, (y * z) as f32))
                .scale(z as f32, z as f32);
            self.fixed.set_child_transform(widget, Some(&transform));
        };
        for (widget, &(x, y)) in self.widgets.borrow().iter().zip(geometry.origins.iter()) {
            place(widget, x, y);
        }
        let titled = geometry
            .bands
            .iter()
            .filter(|band| (self.title)(band.layer).is_some());
        for (label, band) in self.titles.borrow().iter().zip(titled) {
            place(
                label.upcast_ref(),
                band.left + layout::BAND_PAD,
                band.top + layout::BAND_PAD / 2.0,
            );
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

    /// Arrange for the visible region, then scale the whole graph into it,
    /// never above 1:1.
    fn fit(&self) {
        self.layout();
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
        if geometry.banded {
            for band in &geometry.bands {
                if (self.title)(band.layer).is_none() {
                    continue;
                }
                rounded_rect(
                    cr,
                    band.left,
                    band.top,
                    band.right - band.left,
                    band.bottom - band.top,
                    10.0,
                );
                cr.set_source_rgba(r, g, b, 0.035);
                let _ = cr.fill_preserve();
                cr.set_source_rgba(r, g, b, 0.14);
                cr.set_line_width(1.0);
                cr.set_dash(&[], 0.0);
                let _ = cr.stroke();
            }
        }
        for (edge, route) in self.edges.borrow().iter().zip(geometry.routes.iter()) {
            let (alpha, width, dashed) = match edge.kind {
                EdgeKind::Coordination => (0.45, 1.3, false),
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
            rounded(cr, route);
            let _ = cr.stroke();
            if let (EdgeKind::Dependency, Some(&(x, y))) = (edge.kind, route.last()) {
                cr.set_dash(&[], 0.0);
                cr.move_to(x - 8.0, y - 5.0);
                cr.line_to(x, y);
                cr.line_to(x - 8.0, y + 5.0);
                let _ = cr.stroke();
            }
        }
    }
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
    cr.close_path();
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
