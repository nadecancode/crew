//! Native view slots. Feature views replace these honest disconnected states
//! after the shared client service has been integrated.

use gtk::prelude::*;

use crate::navigation::Destination;

pub mod agent;
pub mod common;
pub mod events;
pub mod evidence;
pub mod graph;
pub mod messages;
pub mod overview;
mod tab_bar;
pub mod task_detail;
pub mod tasks;
pub mod team;
pub mod usage;

pub fn disconnected(destination: Destination) -> gtk::ScrolledWindow {
    let (title, description) = match destination {
        Destination::Team => (
            "No team loaded",
            "Connect to a workspace to see its agents and current work.",
        ),
        Destination::Tasks => (
            "No task board loaded",
            "Connect to a workspace to inspect tasks and their dependencies.",
        ),
        Destination::Messages => (
            "No messages loaded",
            "Connect to a workspace to read team messages and questions.",
        ),
        Destination::Events => (
            "No events loaded",
            "Connect to a workspace to read its activity and evidence.",
        ),
        Destination::Usage => (
            "No usage reports loaded",
            "Connect to a workspace to see reported tokens, context, and limits.",
        ),
    };
    let (page, column) = empty_page(destination.icon(), title, description);
    // The way out of the empty state is a connection: Choose workspace opens
    // Connections (`win.connections`, installed by the controller).
    let choose = gtk::Button::builder()
        .label("Choose workspace")
        .action_name("win.connections")
        .halign(gtk::Align::Center)
        .build();
    choose.add_css_class("suggested-action");
    column.append(&choose);
    // Unlike static empty prose, this page has a control to focus, while it
    // is available. A scrolled window open to focus with nothing focusable
    // inside reports success without taking focus.
    choose
        .bind_property("sensitive", &page, "can-focus")
        .sync_create()
        .build();
    page
}

/// Append a control to a flowing row of controls. The row's own child
/// wrapper is not a focus stop, so Tab visits each control once.
pub fn add_control(row: &gtk::FlowBox, control: &impl IsA<gtk::Widget>) {
    row.insert(control, -1);
    if let Some(wrapper) = control.parent() {
        wrapper.set_focusable(false);
    }
}

pub fn no_selection() -> gtk::ScrolledWindow {
    empty_state(
        "view-reveal-symbolic",
        "Nothing selected",
        "Choose an item in the main view to read its details.",
    )
}

pub fn empty_state(icon: &str, title: &str, description: &str) -> gtk::ScrolledWindow {
    empty_page(icon, title, description).0
}

/// An empty state and the column holding its prose, for a caller that adds an
/// action below it.
fn empty_page(icon: &str, title: &str, description: &str) -> (gtk::ScrolledWindow, gtk::Box) {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_halign(gtk::Align::Center);
    column.set_valign(gtk::Align::Center);
    column.set_margin_top(32);
    column.set_margin_bottom(32);
    column.set_margin_start(24);
    column.set_margin_end(24);
    let image = gtk::Image::builder()
        .icon_name(icon)
        .pixel_size(40)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .build();
    image.add_css_class("crew-muted");
    column.append(&image);
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("title-2");
    heading.set_wrap(true);
    heading.set_justify(gtk::Justification::Center);
    column.append(&heading);
    let body = gtk::Label::new(Some(description));
    body.set_wrap(true);
    body.set_max_width_chars(42);
    body.set_justify(gtk::Justification::Center);
    // Empty-state prose is not an entity/evidence focus target. Keyboard
    // navigation falls back to the destination control when no row exists.
    body.add_css_class("crew-muted");
    column.append(&body);
    let page = gtk::ScrolledWindow::builder()
        // Static empty prose contributes no keyboard target. Let the shell
        // restore the named destination control; feature scrollers stay native.
        .can_focus(false)
        .focusable(false)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .hexpand(true)
        .vexpand(true)
        .child(&column)
        .build();
    (page, column)
}
