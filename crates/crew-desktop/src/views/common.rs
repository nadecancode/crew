use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

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
        list.add_css_class("crew-list");
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

    pub fn list(&self) -> &gtk::ListBox {
        &self.list
    }
}

type TabSwitched = Rc<dyn Fn(u32)>;

/// Pages under a one-line row of tabs. Tabs that do not fit are offered in a
/// More menu at the end of the row instead of scrolling out of reach.
#[derive(Clone)]
pub struct Tabs {
    pub root: gtk::Box,
    bar: super::tab_bar::TabBar,
    stack: gtk::Stack,
    buttons: Rc<RefCell<Vec<gtk::ToggleButton>>>,
    switched: Rc<RefCell<Vec<TabSwitched>>>,
}

impl Tabs {
    pub fn new(label: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let bar = super::tab_bar::TabBar::new(label);
        bar.set_margin_start(20);
        bar.set_margin_end(20);
        bar.add_css_class("crew-tabs");
        root.append(&bar);
        let stack = gtk::Stack::builder()
            .hexpand(true)
            .vexpand(true)
            .hhomogeneous(false)
            .vhomogeneous(false)
            .transition_type(gtk::StackTransitionType::None)
            .build();
        stack.add_css_class("crew-tab-pages");
        root.append(&stack);
        Self {
            root,
            bar,
            stack,
            buttons: Rc::default(),
            switched: Rc::default(),
        }
    }

    /// Add a page; the first one is shown. Returns its index.
    pub fn append(&self, title: &str, page: &impl IsA<gtk::Widget>) -> u32 {
        let mut buttons = self.buttons.borrow_mut();
        let index = buttons.len() as u32;
        let name = index.to_string();
        self.stack.add_named(page, Some(&name));
        let button = gtk::ToggleButton::builder()
            .label(title)
            .accessible_role(gtk::AccessibleRole::Tab)
            .build();
        button.add_css_class("flat");
        button.add_css_class("crew-tab");
        match buttons.first() {
            Some(first) => button.set_group(Some(first)),
            None => button.set_active(true),
        }
        button.update_state(&[gtk::accessible::State::Selected(Some(button.is_active()))]);
        button.connect_toggled({
            let stack = self.stack.clone();
            let switched = self.switched.clone();
            move |button| {
                button.update_state(&[gtk::accessible::State::Selected(Some(button.is_active()))]);
                if button.is_active() {
                    stack.set_visible_child_name(&name);
                    let callbacks = switched.borrow().clone();
                    for callback in callbacks {
                        callback(index);
                    }
                }
            }
        });
        self.bar.append(&button);
        buttons.push(button);
        index
    }

    pub fn select(&self, index: u32) {
        if let Some(button) = self.buttons.borrow().get(index as usize) {
            button.set_active(true);
        }
    }

    pub fn connect_switched(&self, callback: impl Fn(u32) + 'static) {
        self.switched.borrow_mut().push(Rc::new(callback));
    }
}

/// A pane or page body: 24 pixels from the pane edge, 12 between items.
pub fn column() -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_margin_top(24);
    column.set_margin_bottom(24);
    column.set_margin_start(24);
    column.set_margin_end(24);
    column
}

/// One list row's body: 12 pixels above and below, aligned with the page's
/// 24-pixel edge, 4 between its lines.
pub fn row() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.set_margin_top(12);
    row.set_margin_bottom(12);
    row.set_margin_start(24);
    row.set_margin_end(24);
    row
}

/// A row's first line: its identity, then its status chip at the end.
pub fn row_title(title: &str, chip: Option<gtk::Label>) -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let label = text(title);
    label.add_css_class("heading");
    label.set_hexpand(true);
    line.append(&label);
    if let Some(chip) = chip {
        chip.set_valign(gtk::Align::Start);
        line.append(&chip);
    }
    line
}

/// A row for a place to connect to: its title with its state at the end, the
/// detail below, then the actions that fit that state. Local and gateway
/// builds present Connections with this one shape.
pub fn entry_row(title: &str, detail: &str, state: &str, actions: &gtk::Box) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    row.add_css_class("crew-row");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let name = text(title);
    name.add_css_class("heading");
    name.add_css_class("crew-row-title");
    name.set_hexpand(true);
    head.append(&name);
    let state_label = muted(state);
    state_label.set_wrap(false);
    state_label.set_xalign(1.0);
    head.append(&state_label);
    row.append(&head);
    if !detail.is_empty() {
        row.append(&muted(detail));
    }
    actions.set_margin_top(4);
    row.append(actions);
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{title}, {state}"
    ))]);
    row
}

/// Plain wrapped text. Not selectable: list rows, status and reason lines
/// must not take the pointer click or become a focus stop.
pub fn text(value: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build()
}

/// Copyable text for inspector facts, evidence and detail bodies.
pub fn selectable(value: &str) -> gtk::Label {
    let label = text(value);
    label.set_selectable(true);
    label
}

/// Secondary text.
pub fn muted(value: &str) -> gtk::Label {
    let label = text(value);
    label.add_css_class("crew-muted");
    label
}

/// A page or entity title.
pub fn title(value: &str) -> gtk::Label {
    let label = text(value);
    label.add_css_class("title-3");
    label
}

/// A section title within a page or pane.
pub fn heading(value: &str) -> gtk::Label {
    let label = text(value);
    label.add_css_class("heading");
    label
}

/// One fact as a muted name above its copyable value, for forms and setup
/// pages that read top to bottom.
pub fn fact(container: &gtk::Box, name: &str, value: &str) {
    let line = gtk::Box::new(gtk::Orientation::Vertical, 4);
    line.append(&muted(name));
    line.append(&selectable(value));
    container.append(&line);
}

/// A reason line for unavailable or failed controls: muted, and hidden while
/// it has nothing to say so it takes no room.
pub fn reason() -> gtk::Label {
    let label = muted("");
    label.set_visible(false);
    label.connect_label_notify(|label| label.set_visible(!label.label().is_empty()));
    label
}

/// A warning: an icon and the warning colour, with the text itself carrying
/// the meaning.
pub fn warning(value: &str) -> gtk::Box {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.add_css_class("crew-warning");
    let icon = gtk::Image::builder()
        .icon_name("dialog-warning-symbolic")
        .accessible_role(gtk::AccessibleRole::Presentation)
        .valign(gtk::Align::Start)
        .build();
    line.append(&icon);
    let label = text(value);
    label.set_hexpand(true);
    line.append(&label);
    line
}

/// Facts as two columns: muted names, copyable values. Rows are kept and
/// updated in place, so a pushed update does not move a reader's text
/// selection; absent and empty values are left out.
#[derive(Clone)]
pub struct Facts {
    pub root: gtk::Grid,
    rows: Rc<RefCell<Vec<(gtk::Label, gtk::Label)>>>,
}

impl Default for Facts {
    fn default() -> Self {
        Self::new()
    }
}

impl Facts {
    pub fn new() -> Self {
        let root = gtk::Grid::builder()
            .column_spacing(24)
            .row_spacing(6)
            .build();
        Self {
            root,
            rows: Rc::default(),
        }
    }

    pub fn with(facts: &[(&str, Option<String>)]) -> Self {
        let grid = Self::new();
        grid.set(facts);
        grid
    }

    pub fn set(&self, facts: &[(&str, Option<String>)]) {
        let present: Vec<(&str, &str)> = facts
            .iter()
            .filter_map(|(name, value)| {
                value
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .map(|value| (*name, value))
            })
            .collect();
        let mut rows = self.rows.borrow_mut();
        while rows.len() < present.len() {
            let name = muted("");
            name.set_valign(gtk::Align::Start);
            name.set_wrap(false);
            let value = selectable("");
            value.set_hexpand(true);
            let index = rows.len() as i32;
            self.root.attach(&name, 0, index, 1, 1);
            self.root.attach(&value, 1, index, 1, 1);
            rows.push((name, value));
        }
        for (index, (name, value)) in rows.iter().enumerate() {
            let fact = present.get(index);
            name.set_visible(fact.is_some());
            value.set_visible(fact.is_some());
            if let Some((fact_name, fact_value)) = fact {
                if name.label() != *fact_name {
                    name.set_label(fact_name);
                }
                if value.label() != *fact_value {
                    value.set_label(fact_value);
                }
            }
        }
        self.root.set_visible(!present.is_empty());
    }
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

/// One task-status colour mapping for task rows and graph nodes. Agent tone
/// comes from the shared presentation; the status word always accompanies it.
pub fn task_tone(status: crew_protocol::TaskStatus) -> crew_client::api::Tone {
    use crew_client::api::Tone;
    use crew_protocol::TaskStatus;
    match status {
        TaskStatus::Working | TaskStatus::Review => Tone::Info,
        TaskStatus::Done => Tone::Success,
        TaskStatus::Failed => Tone::Danger,
        TaskStatus::Blocked => Tone::Warning,
        TaskStatus::Queued | TaskStatus::Ready | TaskStatus::Waiting => Tone::Neutral,
    }
}

/// A status chip: the status text plus its semantic colour, never colour alone.
pub fn status_chip(text: &str, tone: crew_client::api::Tone) -> gtk::Label {
    let chip = gtk::Label::builder()
        .label(text)
        .selectable(false)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .build();
    chip.add_css_class("crew-chip");
    chip.add_css_class(tone_class(tone));
    chip
}

const TONE_CLASSES: [&str; 5] = [
    "tone-neutral",
    "tone-info",
    "tone-success",
    "tone-warning",
    "tone-danger",
];

fn tone_class(tone: crew_client::api::Tone) -> &'static str {
    use crew_client::api::Tone;
    match tone {
        Tone::Neutral => TONE_CLASSES[0],
        Tone::Info => TONE_CLASSES[1],
        Tone::Success => TONE_CLASSES[2],
        Tone::Warning => TONE_CLASSES[3],
        Tone::Danger => TONE_CLASSES[4],
    }
}

/// Change a chip in place when its status changes.
pub fn set_status_chip(chip: &gtk::Label, text: &str, tone: crew_client::api::Tone) {
    if chip.label() != text {
        chip.set_label(text);
    }
    for class in TONE_CLASSES {
        chip.remove_css_class(class);
    }
    chip.add_css_class(tone_class(tone));
}

/// An elapsed time in seconds, minutes or hours and minutes.
pub fn duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    match seconds {
        0..=59 => format!("{seconds} s"),
        60..=3599 => format!("{} min", seconds / 60),
        _ => format!("{} h {} min", seconds / 3600, seconds % 3600 / 60),
    }
}
