//! Native graph geometry. Domain order, task layers and relationships arrive
//! from the shared projection; GTK owns only coordinates, zoom and focus.

use super::common::Select;
use crew_client::api::EntityKey;
use gtk::prelude::*;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

pub struct Node {
    pub key: EntityKey,
    pub label: String,
    pub column: u32,
    pub row: u32,
}

struct FontWatch {
    settings: gtk::Settings,
    handler: RefCell<Option<gtk::glib::SignalHandlerId>>,
}
impl Drop for FontWatch {
    fn drop(&mut self) {
        if let Some(handler) = self.handler.borrow_mut().take() {
            self.settings.disconnect(handler);
        }
    }
}

#[derive(Clone)]
pub struct Graph {
    pub root: gtk::Box,
    scroll: gtk::ScrolledWindow,
    fixed: gtk::Fixed,
    canvas: gtk::DrawingArea,
    nodes: Rc<RefCell<Vec<Node>>>,
    edges: Rc<RefCell<Vec<(usize, usize)>>>,
    buttons: Rc<RefCell<Vec<gtk::Button>>>,
    selected: Rc<RefCell<Option<EntityKey>>>,
    zoom: Rc<Cell<f64>>,
    row_pitch: Rc<Cell<f64>>,
    select: Select,
    font_watch: Rc<FontWatch>,
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
        let fixed = gtk::Fixed::new();
        fixed.update_property(&[gtk::accessible::Property::Label(label)]);
        let canvas = gtk::DrawingArea::new();
        canvas.set_can_target(false);
        fixed.put(&canvas, 0.0, 0.0);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&fixed)
            .hexpand(true)
            .vexpand(true)
            .build();
        root.append(&scroll);
        let this = Self {
            root,
            scroll,
            fixed,
            canvas,
            select,
            nodes: Rc::new(RefCell::new(Vec::new())),
            edges: Rc::new(RefCell::new(Vec::new())),
            buttons: Rc::new(RefCell::new(Vec::new())),
            selected: Rc::new(RefCell::new(None)),
            zoom: Rc::new(Cell::new(1.0)),
            row_pitch: Rc::new(Cell::new(145.0)),
            font_watch: Rc::new(FontWatch {
                settings: gtk::Settings::default().expect("initialized GTK settings"),
                handler: RefCell::new(None),
            }),
        };
        this.canvas.set_draw_func({
            let nodes = this.nodes.clone();
            let edges = this.edges.clone();
            let zoom = this.zoom.clone();
            let row_pitch = this.row_pitch.clone();
            move |area, cr, _, _| {
                let color = area.style_context().color();
                cr.set_source_rgba(
                    color.red().into(),
                    color.green().into(),
                    color.blue().into(),
                    0.55,
                );
                cr.set_line_width(1.5);
                for &(from, to) in edges.borrow().iter() {
                    let nodes = nodes.borrow();
                    if let (Some(a), Some(b)) = (nodes.get(from), nodes.get(to)) {
                        let z = zoom.get();
                        let x1 = (24.0 + f64::from(a.column) * 250.0 + 200.0) * z;
                        let y1 = (24.0
                            + f64::from(a.row) * row_pitch.get()
                            + (row_pitch.get() - 35.0) / 2.0)
                            * z;
                        let x2 = (24.0 + f64::from(b.column) * 250.0) * z;
                        let y2 = (24.0
                            + f64::from(b.row) * row_pitch.get()
                            + (row_pitch.get() - 35.0) / 2.0)
                            * z;
                        cr.move_to(x1, y1);
                        cr.curve_to(x1 + 28.0 * z, y1, x2 - 28.0 * z, y2, x2, y2);
                        let _ = cr.stroke();
                        cr.move_to(x2 - 6.0, y2 - 4.0);
                        cr.line_to(x2, y2);
                        cr.line_to(x2 - 6.0, y2 + 4.0);
                        let _ = cr.stroke();
                    }
                }
            }
        });
        minus.connect_clicked({
            let get = this.handle();
            move |_| {
                if let Some(this) = get() {
                    this.set_zoom(this.zoom.get() / 1.2);
                }
            }
        });
        plus.connect_clicked({
            let get = this.handle();
            move |_| {
                if let Some(this) = get() {
                    this.set_zoom(this.zoom.get() * 1.2);
                }
            }
        });
        fit.connect_clicked({
            let get = this.handle();
            move |_| {
                let Some(this) = get() else {
                    return;
                };
                let columns = this
                    .nodes
                    .borrow()
                    .iter()
                    .map(|n| n.column)
                    .max()
                    .unwrap_or(0);
                this.set_zoom(
                    f64::from(this.scroll.width().max(1)) / (f64::from(columns) * 250.0 + 248.0),
                );
                this.scroll.hadjustment().set_value(0.0);
                this.scroll.vadjustment().set_value(0.0);
            }
        });
        let get = Rc::new(this.handle());
        *this.font_watch.handler.borrow_mut() = Some(
            this.font_watch
                .settings
                .connect_notify_local(Some("gtk-font-name"), move |_, _| {
                    let get = get.clone();
                    gtk::glib::idle_add_local_once(move || {
                        if let Some(graph) = get() {
                            graph.layout();
                        }
                    });
                }),
        );
        this
    }
    fn handle(&self) -> impl Fn() -> Option<Self> + 'static {
        let root = self.root.downgrade();
        let scroll = self.scroll.downgrade();
        let fixed = self.fixed.downgrade();
        let canvas = self.canvas.downgrade();
        let nodes = self.nodes.clone();
        let edges = self.edges.clone();
        let buttons = self.buttons.clone();
        let selected = self.selected.clone();
        let zoom = self.zoom.clone();
        let row_pitch = self.row_pitch.clone();
        let select = self.select.clone();
        let font_watch = Rc::downgrade(&self.font_watch);
        move || {
            Some(Self {
                root: root.upgrade()?,
                scroll: scroll.upgrade()?,
                fixed: fixed.upgrade()?,
                canvas: canvas.upgrade()?,
                nodes: nodes.clone(),
                edges: edges.clone(),
                buttons: buttons.clone(),
                selected: selected.clone(),
                zoom: zoom.clone(),
                row_pitch: row_pitch.clone(),
                select: select.clone(),
                font_watch: font_watch.upgrade()?,
            })
        }
    }

    pub fn update(&self, nodes: Vec<Node>, links: Vec<(String, String)>) {
        let focused = self
            .buttons
            .borrow()
            .iter()
            .position(|b| b.has_focus())
            .and_then(|i| self.nodes.borrow().get(i).map(|n| n.key.clone()));
        let x = self.scroll.hadjustment().value();
        let y = self.scroll.vadjustment().value();
        for button in self.buttons.borrow_mut().drain(..) {
            self.fixed.remove(&button);
        }
        let ids: BTreeMap<_, _> = nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| match &n.key.entity {
                crew_client::api::Entity::Agent(id) | crew_client::api::Entity::Task(id) => {
                    Some((id.clone(), i))
                }
                _ => None,
            })
            .collect();
        *self.edges.borrow_mut() = links
            .iter()
            .filter_map(|(a, b)| Some((*ids.get(a)?, *ids.get(b)?)))
            .collect();
        for node in &nodes {
            let label = gtk::Label::builder()
                .label(&node.label)
                .wrap(true)
                .xalign(0.0)
                .max_width_chars(28)
                .build();
            let button = gtk::Button::builder().child(&label).build();
            button.add_css_class("crew-graph-node");
            button.update_property(&[gtk::accessible::Property::Label(&node.label)]);
            if self.selected.borrow().as_ref() == Some(&node.key) {
                button.add_css_class("suggested-action");
            }
            button.connect_clicked({
                let key = node.key.clone();
                let select = self.select.clone();
                let selected = self.selected.clone();
                let buttons = Rc::downgrade(&self.buttons);
                move |button| {
                    if let Some(buttons) = buttons.upgrade() {
                        for node in buttons.borrow().iter() {
                            node.remove_css_class("suggested-action");
                        }
                    }
                    button.add_css_class("suggested-action");
                    *selected.borrow_mut() = Some(key.clone());
                    select(key.clone());
                }
            });
            self.fixed.put(&button, 0.0, 0.0);
            if focused.as_ref() == Some(&node.key) {
                button.grab_focus();
            }
            self.buttons.borrow_mut().push(button);
        }
        *self.nodes.borrow_mut() = nodes;
        self.layout();
        self.scroll.hadjustment().set_value(x);
        self.scroll.vadjustment().set_value(y);
    }

    fn set_zoom(&self, zoom: f64) {
        self.zoom.set(zoom.clamp(0.5, 2.0));
        self.layout();
    }

    fn layout(&self) {
        let z = self.zoom.get();
        let nodes = self.nodes.borrow();
        // Clear the previous zoom's minimum height before measuring native
        // text, so zoom-out and text-scale changes do not accumulate size.
        for button in self.buttons.borrow().iter() {
            button.set_size_request((200.0 * z) as i32, -1);
        }
        let height = self
            .buttons
            .borrow()
            .iter()
            .map(|button| {
                button
                    .measure(gtk::Orientation::Vertical, (200.0 * z) as i32)
                    .0
            })
            .max()
            .unwrap_or(110)
            .max((110.0 * z) as i32);
        self.row_pitch.set((f64::from(height) + 35.0 * z) / z);
        for (node, button) in nodes.iter().zip(self.buttons.borrow().iter()) {
            button.set_size_request((200.0 * z) as i32, height);
            self.fixed.move_(
                button,
                (24.0 + f64::from(node.column) * 250.0) * z,
                (24.0 + f64::from(node.row) * self.row_pitch.get()) * z,
            );
        }
        let columns = nodes.iter().map(|n| n.column).max().unwrap_or(0);
        let rows = nodes.iter().map(|n| n.row).max().unwrap_or(0);
        let width = ((f64::from(columns) * 250.0 + 248.0) * z).min(f64::from(i32::MAX)) as i32;
        let height = ((f64::from(rows) * self.row_pitch.get() + self.row_pitch.get() + 24.0) * z)
            .min(f64::from(i32::MAX)) as i32;
        self.fixed.set_size_request(width, height);
        self.canvas.set_content_width(width);
        self.canvas.set_content_height(height);
        self.canvas.queue_draw();
    }
}
