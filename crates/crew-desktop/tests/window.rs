#![cfg(all(feature = "native-ui", target_os = "linux"))]

use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use crew_desktop::{appearance::Appearance, application, navigation::Destination, shell::Shell};
use gtk::{gio, glib, prelude::*};

// GTK initialization and every assertion stay on this single test thread.
#[test]
fn native_navigation_layout_focus_and_lifecycle() {
    gtk::init().expect("run with the isolated native window test runner");
    application::register_resources();
    let app = application::build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let shell = Shell::new(&app);
    shell.window().present();
    until(|| shell.window().is_mapped());
    assert_eq!(shell.destination(), Destination::Team);
    assert!(!shell.is_compact());
    assert!(shell.is_sidebar_visible());
    assert!(!shell.is_detail_visible());
    let navigation = descendants(shell.window().upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::ListBox>().ok())
        .unwrap();
    assert!(gtk::test_accessible_has_role(
        &navigation,
        gtk::AccessibleRole::List
    ));
    let selected = navigation.selected_row().unwrap();
    assert!(gtk::test_accessible_has_role(
        &selected,
        gtk::AccessibleRole::ListItem
    ));
    assert!(gtk::test_accessible_has_property(
        &selected,
        gtk::AccessibleProperty::Label
    ));
    assert!(gtk::test_accessible_has_state(
        &selected,
        gtk::AccessibleState::Selected
    ));
    let icon_theme = gtk::IconTheme::for_display(&gtk::prelude::WidgetExt::display(shell.window()));
    for destination in Destination::ALL {
        assert!(
            icon_theme.has_icon(destination.icon()),
            "{} is a native symbolic icon",
            destination.icon()
        );
    }
    assert_eq!(
        app.accels_for_action("win.destination('tasks')").as_slice(),
        ["<Control>2"]
    );

    // Static disconnected pages have no row or reading control to restore.
    // Navigation, compact detail and Back must retain named visible context.
    shell.navigate(Destination::Tasks);
    settle();
    let destination_control = descendants(shell.window().upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::MenuButton>().ok())
        .find(|button| {
            button
                .child()
                .and_then(|child| child.downcast::<gtk::Label>().ok())
                .is_some_and(|label| label.text() == "Tasks")
        })
        .unwrap();
    assert!(gtk::test_accessible_has_property(
        &destination_control,
        gtk::AccessibleProperty::Label
    ));
    shell.set_detail_visible(true);
    shell.window().set_default_size(560, 600);
    until(|| shell.is_compact());
    settle();
    assert!(destination_control.is_drawable());
    assert!(
        has_focus(shell.window(), &destination_control),
        "empty Tasks detail must focus the visible named destination control"
    );
    gtk::prelude::WidgetExt::activate_action(shell.window(), "win.back", None).unwrap();
    settle();
    assert!(!shell.is_detail_visible());
    assert!(destination_control.is_drawable());
    assert!(
        has_focus(shell.window(), &destination_control),
        "empty Tasks main after Back must focus the visible named destination control"
    );
    shell.window().set_default_size(1120, 720);
    until(|| !shell.is_compact());
    shell.navigate(Destination::Team);
    settle();

    // Real native controls exercise page persistence and return focus. These are
    // UI widgets, not a second domain model or a simulated successful service.
    let team_entry = gtk::Entry::builder().text("retained team draft").build();
    let task_entry = gtk::Entry::builder().text("retained task draft").build();
    let detail_button = gtk::Button::with_label("Copy detail");
    shell.set_main(Destination::Team, &team_entry);
    shell.set_main(Destination::Tasks, &task_entry);
    shell.set_detail(Destination::Team, "Selected item", &detail_button);
    team_entry.grab_focus();
    until(|| has_focus(shell.window(), &team_entry));
    let changes = Rc::new(Cell::new(0));
    shell.connect_destination_changed({
        let changes = changes.clone();
        move |_| changes.set(changes.get() + 1)
    });
    for destination in Destination::ALL.into_iter().skip(1) {
        gtk::prelude::WidgetExt::activate_action(
            shell.window(),
            "win.destination",
            Some(&destination.id().to_variant()),
        )
        .unwrap();
        settle();
        assert_eq!(shell.destination(), destination);
    }
    shell.navigate(Destination::Team);
    until(|| has_focus(shell.window(), &team_entry));
    assert_eq!(changes.get(), 5);
    assert_eq!(team_entry.text(), "retained team draft");
    assert_eq!(task_entry.text(), "retained task draft");

    shell.window().set_default_size(560, 600);
    until(|| shell.is_compact());
    assert!(!shell.is_sidebar_visible());
    assert!(has_focus(shell.window(), &team_entry));
    shell.set_detail_visible(true);
    until(|| has_focus(shell.window(), &detail_button));
    assert!(shell.is_detail_visible());
    gtk::prelude::WidgetExt::activate_action(shell.window(), "win.back", None).unwrap();
    until(|| has_focus(shell.window(), &team_entry));
    assert!(!shell.is_detail_visible());
    shell.toggle_sidebar();
    settle();
    assert!(shell.is_sidebar_visible());
    shell.navigate(Destination::Tasks);
    until(|| has_focus(shell.window(), &task_entry));
    assert!(!shell.is_sidebar_visible());

    shell.window().set_default_size(1024, 600);
    until(|| !shell.is_compact());
    assert_eq!(shell.destination(), Destination::Tasks);
    assert!(shell.is_sidebar_visible());
    shell.set_detail_visible(true);
    assert!(shell.is_detail_visible());
    assert!(has_focus(shell.window(), &task_entry));
    shell.toggle_sidebar();
    assert!(!shell.is_sidebar_visible());
    shell.toggle_sidebar();
    assert!(shell.is_sidebar_visible());

    for choice in [Appearance::Dark, Appearance::Light, Appearance::System] {
        gtk::prelude::WidgetExt::activate_action(
            shell.window(),
            "win.appearance",
            Some(&choice.id().to_variant()),
        )
        .unwrap();
        assert_eq!(shell.appearance(), choice);
        settle();
    }
    let settings = gtk::Settings::for_display(&gtk::prelude::WidgetExt::display(shell.window()));
    let original_theme = settings.gtk_theme_name();
    settings.set_gtk_theme_name(Some("HighContrast"));
    settle();
    assert!(
        shell
            .window()
            .style_context()
            .lookup_color("crew_canvas")
            .is_none(),
        "system high contrast must retain toolkit colors"
    );
    settings.set_gtk_theme_name(original_theme.as_deref());
    let original_font = settings.gtk_font_name().unwrap();
    settings.set_gtk_font_name(Some("Sans 24"));
    until(|| shell.is_compact());
    assert_eq!(shell.destination(), Destination::Tasks);
    settings.set_gtk_font_name(Some(&original_font));
    until(|| !shell.is_compact());

    task_entry.grab_focus();
    until(|| has_focus(shell.window(), &task_entry));

    gtk::prelude::WidgetExt::activate_action(shell.window(), "win.shortcuts", None).unwrap();
    until(|| find_window("Keyboard Shortcuts").is_some());
    let dialog = find_window("Keyboard Shortcuts").unwrap();
    assert!(dialog.is_modal());
    dialog.close();
    settle();
    assert!(has_focus(shell.window(), &task_entry));

    // A local close destroys this window. No client/domain/daemon API exists in
    // the foundation, so there is no stop-team side effect to dispatch.
    shell.window().close();
    until(|| !shell.window().is_visible());
    assert!(app.windows().is_empty());
}

fn has_focus(window: &gtk::ApplicationWindow, widget: &impl IsA<gtk::Widget>) -> bool {
    gtk::prelude::GtkWindowExt::focus(window)
        .is_some_and(|focus| focus == *widget.as_ref() || focus.is_ancestor(widget))
}

#[track_caller]
fn until(condition: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < end, "native UI condition timed out");
        settle();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle() {
    let context = glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
}

fn find_window(title: &str) -> Option<gtk::Window> {
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
        .find(|window| window.title().as_deref() == Some(title))
}

fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = Vec::new();
    let mut child = widget.first_child();
    while let Some(widget) = child {
        result.extend(descendants(&widget));
        child = widget.next_sibling();
        result.push(widget);
    }
    result
}
