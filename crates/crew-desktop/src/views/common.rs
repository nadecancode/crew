use std::{collections::BTreeMap, rc::Rc};

use crew_client::api::EntityKey;
use gtk::prelude::*;

pub type Select = Rc<dyn Fn(EntityKey)>;

/// Persistent native scrolling and selection. Updating projection rows retains
/// the scroll position and returns focus to the same entity where it survives.
#[derive(Clone)]
pub struct EntityList {
    pub root: gtk::ScrolledWindow,
    list: gtk::ListBox,
    keys: Rc<std::cell::RefCell<BTreeMap<i32, EntityKey>>>,
}

impl EntityList {
    pub fn new(label: &str, select: Select) -> Self {
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.update_property(&[gtk::accessible::Property::Label(label)]);
        let keys = Rc::new(std::cell::RefCell::new(BTreeMap::<i32, EntityKey>::new()));
        list.connect_row_activated({
            let keys = keys.clone();
            move |_, row| {
                if let Some(key) = keys.borrow().get(&row.index()) {
                    select(key.clone());
                }
            }
        });
        let root = gtk::ScrolledWindow::builder()
            .child(&list)
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        Self { root, list, keys }
    }

    pub fn replace(&self, rows: Vec<(EntityKey, gtk::Widget, String)>) {
        let offset = self.root.vadjustment().value();
        let selected = self
            .list
            .selected_row()
            .and_then(|row| self.keys.borrow().get(&row.index()).cloned());
        let focused = self
            .list
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| focus.is_ancestor(&self.list));
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.keys.borrow_mut().clear();
        for (index, (key, widget, label)) in rows.into_iter().enumerate() {
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&widget));
            row.update_property(&[gtk::accessible::Property::Label(&label)]);
            self.list.append(&row);
            self.keys.borrow_mut().insert(index as i32, key.clone());
            if selected.as_ref() == Some(&key) {
                self.list.select_row(Some(&row));
                if focused {
                    row.grab_focus();
                }
            }
        }
        self.root.vadjustment().set_value(offset);
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.root
    }
}

pub fn column() -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for edge in [
        gtk::PositionType::Top,
        gtk::PositionType::Bottom,
        gtk::PositionType::Left,
        gtk::PositionType::Right,
    ] {
        match edge {
            gtk::PositionType::Top => column.set_margin_top(16),
            gtk::PositionType::Bottom => column.set_margin_bottom(16),
            gtk::PositionType::Left => column.set_margin_start(16),
            _ => column.set_margin_end(16),
        }
    }
    column
}

pub fn text(value: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .build()
}

pub fn heading(value: &str) -> gtk::Label {
    let label = text(value);
    label.add_css_class("title-2");
    label
}

pub fn fact(container: &gtk::Box, name: &str, value: &str) {
    let line = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let label = text(name);
    label.add_css_class("crew-muted");
    line.append(&label);
    line.append(&text(value));
    container.append(&line);
}

pub fn scroll(widget: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .child(widget)
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build()
}

pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

pub fn timestamp(at_ms: i64) -> String {
    gtk::glib::DateTime::from_unix_local(at_ms.div_euclid(1000))
        .and_then(|date| date.format("%Y-%m-%d %H:%M:%S %Z"))
        .map_or_else(
            |_| format!("Timestamp {at_ms} ms"),
            |value| value.to_string(),
        )
}

pub fn buffer_text(buffer: &gtk::TextBuffer) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}
