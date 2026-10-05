//! Native view slots. Feature views replace these honest disconnected states
//! after the shared client service has been integrated.

use gtk::prelude::*;

use crate::navigation::Destination;

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
    empty_state(destination.icon(), title, description)
}

pub fn no_selection() -> gtk::ScrolledWindow {
    empty_state(
        "view-reveal-symbolic",
        "Nothing selected",
        "Choose an item in the main view to read its details.",
    )
}

pub fn empty_state(icon: &str, title: &str, description: &str) -> gtk::ScrolledWindow {
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
    body.set_selectable(true);
    body.add_css_class("crew-muted");
    column.append(&body);
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .hexpand(true)
        .vexpand(true)
        .child(&column)
        .build()
}
