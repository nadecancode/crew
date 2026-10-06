//! Window composition and local navigation. No domain records, connection
//! success, or daemon actions are synthesized by this shell.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gtk::{gio, glib, prelude::*};

use crate::{
    adaptive::AdaptiveBox,
    appearance::{Appearance, AppearanceController},
    application,
    navigation::Destination,
    views,
};

type DestinationObserver = Box<dyn Fn(Destination)>;

/// The sidebar keeps this width whatever its status text says.
pub const SIDEBAR_WIDTH: i32 = 184;

struct Inner {
    window: gtk::ApplicationWindow,
    adaptive: AdaptiveBox,
    sidebar: gtk::Box,
    navigation: gtk::ListBox,
    rows: [gtk::ListBoxRow; 5],
    content: gtk::Paned,
    main_stack: gtk::Stack,
    detail_stack: gtk::Stack,
    main_slots: [gtk::Box; 5],
    detail_slots: [gtk::Box; 5],
    detail_titles: RefCell<[String; 5]>,
    detail_heading: gtk::Label,
    detail_container: gtk::Box,
    destination_button: gtk::MenuButton,
    destination_label: gtk::Label,
    workspace_slot: gtk::Box,
    status_slot: gtk::Box,
    back_button: gtk::Button,
    destination: Cell<Destination>,
    sidebar_requested: Cell<bool>,
    compact_sidebar: Cell<bool>,
    detail_requested: [Cell<bool>; 5],
    main_focus: [glib::WeakRef<gtk::Widget>; 5],
    observers: RefCell<Vec<DestinationObserver>>,
    appearance: Rc<AppearanceController>,
    sidebar_action: gio::SimpleAction,
    detail_action: gio::SimpleAction,
    back_action: gio::SimpleAction,
    appearance_action: gio::SimpleAction,
}

/// Stable native widget slots for independently implemented feature views.
/// Clone handles refer to the same window; all methods run on GTK's main thread.
#[derive(Clone)]
pub struct Shell(Rc<Inner>);

impl Shell {
    pub fn new(app: &gtk::Application) -> Self {
        application::register_resources();
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Crew")
            .default_width(1120)
            .default_height(720)
            .build();
        window.add_css_class("crew-window");
        window.set_icon_name(Some(application::APPLICATION_ID));
        gtk::IconTheme::for_display(&gtk::prelude::WidgetExt::display(&window))
            .add_resource_path("/dev/crew/Crew/icons");
        let appearance = AppearanceController::new(&gtk::prelude::WidgetExt::display(&window));

        let main_stack = page_stack();
        main_stack.add_css_class("crew-main");
        let detail_stack = page_stack();
        let main_slots = std::array::from_fn(|index| {
            let destination = Destination::ALL[index];
            let slot = view_slot();
            slot.append(&views::disconnected(destination));
            main_stack.add_named(&slot, Some(destination.id()));
            slot
        });
        let detail_slots = std::array::from_fn(|index| {
            let slot = view_slot();
            slot.append(&views::no_selection());
            detail_stack.add_named(&slot, Some(Destination::ALL[index].id()));
            slot
        });

        let navigation = gtk::ListBox::new();
        navigation.add_css_class("navigation-sidebar");
        navigation.add_css_class("crew-navigation");
        navigation.set_selection_mode(gtk::SelectionMode::Single);
        navigation.update_property(&[gtk::accessible::Property::Label("Destinations")]);
        let rows = std::array::from_fn(|index| {
            let destination = Destination::ALL[index];
            let row = gtk::ListBoxRow::new();
            let body = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let icon = gtk::Image::builder()
                .icon_name(destination.icon())
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build();
            body.append(&icon);
            let label = gtk::Label::new(Some(destination.title()));
            label.set_xalign(0.0);
            body.append(&label);
            row.set_child(Some(&body));
            row.update_property(&[gtk::accessible::Property::Label(destination.title())]);
            navigation.append(&row);
            row
        });
        let nav_scroll = gtk::ScrolledWindow::builder()
            .child(&navigation)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let status_slot = gtk::Box::new(gtk::Orientation::Vertical, 4);
        status_slot.add_css_class("crew-status");
        let status = gtk::Label::new(Some("Disconnected"));
        status.set_xalign(0.0);
        status.add_css_class("crew-title");
        status_slot.append(&status);
        let status_detail = gtk::Label::new(Some("No workspace connected"));
        status_detail.set_xalign(0.0);
        status_detail.set_wrap(true);
        status_detail.add_css_class("crew-muted");
        status_slot.append(&status_detail);
        // The status text wraps inside the sidebar. Without a horizontal
        // scrollbar the clip requests only its child's minimum width, so a
        // long status line cannot widen the sidebar and shift the main pane.
        let status_clip = gtk::ScrolledWindow::builder()
            .child(&status_slot)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .build();
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.set_width_request(SIDEBAR_WIDTH);
        sidebar.add_css_class("crew-sidebar");
        sidebar.append(&nav_scroll);
        sidebar.append(&status_clip);

        let detail_heading = gtk::Label::new(Some("Details"));
        detail_heading.set_xalign(0.0);
        detail_heading.set_wrap(true);
        detail_heading.add_css_class("crew-title");
        detail_heading.add_css_class("crew-detail-heading");
        let detail_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        detail_container.add_css_class("crew-detail");
        detail_container.append(&detail_heading);
        detail_container.append(&detail_stack);
        let content = gtk::Paned::new(gtk::Orientation::Horizontal);
        content.set_hexpand(true);
        content.set_vexpand(true);
        content.set_resize_start_child(true);
        content.set_resize_end_child(true);
        content.set_shrink_start_child(false);
        content.set_shrink_end_child(false);
        content.set_start_child(Some(&main_stack));
        content.set_end_child(Some(&detail_container));
        content.set_position(620);
        let adaptive = AdaptiveBox::new();
        adaptive.set_hexpand(true);
        adaptive.set_vexpand(true);
        adaptive.append(&sidebar);
        adaptive.append(&content);
        window.set_child(Some(&adaptive));

        let sidebar_action = gio::SimpleAction::new_stateful("sidebar", None, &true.to_variant());
        let detail_action = gio::SimpleAction::new_stateful("details", None, &false.to_variant());
        let back_action = gio::SimpleAction::new("back", None);
        let appearance_action = gio::SimpleAction::new_stateful(
            "appearance",
            Some(glib::VariantTy::STRING),
            &"system".to_variant(),
        );
        for action in [
            &sidebar_action,
            &detail_action,
            &back_action,
            &appearance_action,
        ] {
            window.add_action(action);
        }
        let header = gtk::HeaderBar::new();
        let sidebar_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .action_name("win.sidebar")
            .tooltip_text("Show or hide sidebar (Ctrl+B)")
            .build();
        name(&sidebar_button, "Show or hide sidebar");
        header.pack_start(&sidebar_button);
        let back_button = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .action_name("win.back")
            .tooltip_text("Back (Alt+Left)")
            .build();
        name(&back_button, "Back to previous pane");
        header.pack_start(&back_button);
        let destination_label = ellipsized_label("Team", 10);
        let destination_button = gtk::MenuButton::builder()
            .child(&destination_label)
            .menu_model(&destinations_menu())
            .build();
        destination_button.set_always_show_arrow(true);
        name(&destination_button, "Choose destination");
        header.set_title_widget(Some(&destination_button));
        let workspace_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        workspace_slot.append(&workspace_selector());
        header.pack_end(&workspace_slot);
        let details_button = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .action_name("win.details")
            .tooltip_text("Show or hide details (Ctrl+I)")
            .build();
        name(&details_button, "Show or hide details");
        header.pack_end(&details_button);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&application_menu())
            .build();
        name(&menu_button, "Application menu");
        header.pack_end(&menu_button);
        window.set_titlebar(Some(&header));

        let shell = Self(Rc::new(Inner {
            window,
            adaptive,
            sidebar,
            navigation,
            rows,
            content,
            main_stack,
            detail_stack,
            main_slots,
            detail_slots,
            detail_titles: RefCell::new(std::array::from_fn(|_| "Details".to_owned())),
            detail_heading,
            detail_container,
            destination_button,
            destination_label,
            workspace_slot,
            status_slot,
            back_button,
            destination: Cell::new(Destination::Team),
            sidebar_requested: Cell::new(true),
            compact_sidebar: Cell::new(false),
            detail_requested: std::array::from_fn(|_| Cell::new(false)),
            main_focus: std::array::from_fn(|_| glib::WeakRef::new()),
            observers: RefCell::new(Vec::new()),
            appearance,
            sidebar_action,
            detail_action,
            back_action,
            appearance_action,
        }));
        shell.install_actions(app);
        // Only activation (Enter, Space or a click) changes the destination.
        // Arrow keys move focus within the list and Tab leaves it, so the
        // sidebar is a single Tab stop.
        let weak = Rc::downgrade(&shell.0);
        shell.0.navigation.connect_row_activated(move |_, row| {
            if let Some(inner) = weak.upgrade() {
                if let Some(destination) = Destination::ALL.get(row.index() as usize) {
                    Shell(inner).navigate(*destination);
                }
            }
        });
        // GTK's list box selects the row that keyboard focus enters; the
        // selection keeps marking the current destination instead.
        let weak = Rc::downgrade(&shell.0);
        shell.0.navigation.connect_row_selected(move |list, row| {
            if let Some(inner) = weak.upgrade() {
                let current = &inner.rows[inner.destination.get().index()];
                if row != Some(current) {
                    list.select_row(Some(current));
                }
            }
        });
        shell.0.navigation.add_controller(sidebar_keys(&shell));
        let weak = Rc::downgrade(&shell.0);
        shell.0.adaptive.connect_compact_notify(move |_| {
            if let Some(inner) = weak.upgrade() {
                let shell = Shell(inner);
                shell.remember_main_focus();
                let previous = gtk::prelude::GtkWindowExt::focus(&shell.0.window);
                shell.render();
                if previous.is_some_and(|focus| !focus.is_drawable()) {
                    shell.focus_visible_view();
                }
            }
        });
        shell.0.navigation.select_row(Some(&shell.0.rows[0]));
        shell.render();
        shell
    }

    pub fn window(&self) -> &gtk::ApplicationWindow {
        &self.0.window
    }
    pub fn destination(&self) -> Destination {
        self.0.destination.get()
    }
    pub fn is_compact(&self) -> bool {
        self.0.adaptive.compact()
    }
    pub fn is_sidebar_visible(&self) -> bool {
        self.0.sidebar.is_visible()
    }
    pub fn is_detail_visible(&self) -> bool {
        self.0.detail_container.is_visible()
    }
    pub fn appearance(&self) -> Appearance {
        self.0.appearance.choice()
    }

    /// Replace a destination's native main view without recreating other pages.
    pub fn set_main(&self, destination: Destination, widget: &impl IsA<gtk::Widget>) {
        replace_child(&self.0.main_slots[destination.index()], widget);
    }

    /// Updating detail content never opens it or steals focus. Feature views
    /// validate scope/selection before passing a widget to this UI-only slot.
    pub fn set_detail(
        &self,
        destination: Destination,
        title: &str,
        widget: &impl IsA<gtk::Widget>,
    ) {
        replace_child(&self.0.detail_slots[destination.index()], widget);
        self.0.detail_titles.borrow_mut()[destination.index()] = title.to_owned();
        self.render();
    }

    pub fn set_workspace_selector(&self, widget: &impl IsA<gtk::Widget>) {
        replace_child(&self.0.workspace_slot, widget);
    }

    pub fn set_status_widget(&self, widget: &impl IsA<gtk::Widget>) {
        replace_child(&self.0.status_slot, widget);
    }

    pub fn connect_destination_changed(&self, observer: impl Fn(Destination) + 'static) {
        self.0.observers.borrow_mut().push(Box::new(observer));
    }

    pub fn navigate(&self, destination: Destination) {
        if self.destination() == destination && !(self.is_compact() && self.0.compact_sidebar.get())
        {
            return;
        }
        let focus_in_nav = gtk::prelude::GtkWindowExt::focus(&self.0.window)
            .is_some_and(|focus| focus.is_ancestor(&self.0.navigation));
        self.remember_main_focus();
        let changed = self.0.destination.replace(destination) != destination;
        self.0.compact_sidebar.set(false);
        self.0
            .navigation
            .select_row(Some(&self.0.rows[destination.index()]));
        self.render();
        if !focus_in_nav || self.is_compact() {
            self.focus_visible_view();
        }
        if changed {
            for observer in self.0.observers.borrow().iter() {
                observer(destination);
            }
        }
    }

    pub fn set_detail_visible(&self, visible: bool) {
        self.remember_main_focus();
        self.0.detail_requested[self.destination().index()].set(visible);
        self.0.compact_sidebar.set(false);
        self.render();
        if self.is_compact() || !visible {
            self.focus_visible_view();
        }
    }

    pub fn toggle_sidebar(&self) {
        self.remember_main_focus();
        if self.is_compact() {
            self.0.compact_sidebar.set(!self.0.compact_sidebar.get());
        } else {
            self.0
                .sidebar_requested
                .set(!self.0.sidebar_requested.get());
        }
        self.render();
        if self.is_compact() && self.0.compact_sidebar.get() {
            self.0.rows[self.destination().index()].grab_focus();
        } else {
            self.focus_visible_view();
        }
    }

    pub fn back(&self) {
        if self.is_compact() && self.0.compact_sidebar.replace(false) {
            self.render();
            self.focus_visible_view();
        } else if self.is_compact() {
            self.set_detail_visible(false);
        }
    }

    pub fn set_appearance(&self, appearance: Appearance) {
        self.0.appearance.set(appearance);
        self.0
            .appearance_action
            .set_state(&appearance.id().to_variant());
    }

    fn render(&self) {
        let destination = self.destination();
        self.0
            .window
            .set_title(Some(&format!("{} — Crew", destination.title())));
        let compact = self.is_compact();
        let nav_open = compact && self.0.compact_sidebar.get();
        let detail = self.0.detail_requested[destination.index()].get();
        self.0.main_stack.set_visible_child_name(destination.id());
        self.0.detail_stack.set_visible_child_name(destination.id());
        self.0.destination_label.set_text(destination.title());
        name(
            &self.0.destination_button,
            &format!("Destination: {}", destination.title()),
        );
        self.0
            .detail_heading
            .set_text(&self.0.detail_titles.borrow()[destination.index()]);
        self.0.sidebar.set_visible(if compact {
            nav_open
        } else {
            self.0.sidebar_requested.get()
        });
        self.0.sidebar.set_hexpand(nav_open);
        self.0.content.set_visible(!nav_open);
        self.0.main_stack.set_visible(!compact || !detail);
        self.0.detail_container.set_visible(detail);
        let back = compact && (nav_open || detail);
        self.0.back_button.set_visible(back);
        self.0.back_action.set_enabled(back);
        self.0
            .sidebar_action
            .set_state(&self.0.sidebar.is_visible().to_variant());
        self.0.detail_action.set_state(&detail.to_variant());
    }

    /// Focus the current page's first control, or its remembered focus, as
    /// navigation does. For callers whose own focus target has gone.
    pub fn focus_page(&self) {
        self.focus_visible_view();
    }

    fn remember_main_focus(&self) {
        if let Some(focus) = gtk::prelude::GtkWindowExt::focus(&self.0.window) {
            let index = self.destination().index();
            if focus.is_ancestor(&self.0.main_slots[index]) {
                self.0.main_focus[index].set(Some(&focus));
            }
        }
    }

    fn focus_visible_view(&self) {
        let weak = Rc::downgrade(&self.0);
        glib::idle_add_local_once(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let index = inner.destination.get().index();
            if inner.adaptive.compact() && inner.detail_requested[index].get() {
                if inner.detail_slots[index].child_focus(gtk::DirectionType::TabForward) {
                    return;
                }
            } else {
                if let Some(focus) = inner.main_focus[index].upgrade() {
                    if focus.is_ancestor(&inner.main_slots[index]) && focus.grab_focus() {
                        return;
                    }
                }
                if inner.main_slots[index].child_focus(gtk::DirectionType::TabForward) {
                    return;
                }
            }
            inner.destination_button.grab_focus();
        });
    }

    fn install_actions(&self, app: &gtk::Application) {
        let destination = gio::SimpleAction::new("destination", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(&self.0);
        destination.connect_activate(move |_, parameter| {
            if let (Some(inner), Some(id)) =
                (weak.upgrade(), parameter.and_then(glib::Variant::str))
            {
                if let Some(destination) = Destination::from_id(id) {
                    Shell(inner).navigate(destination);
                }
            }
        });
        self.0.window.add_action(&destination);
        for (index, destination) in Destination::ALL.into_iter().enumerate() {
            app.set_accels_for_action(
                &format!("win.destination('{}')", destination.id()),
                &[&format!("<Primary>{}", index + 1)],
            );
        }
        for (name, accel) in [
            ("sidebar", "<Primary>b"),
            ("details", "<Primary>i"),
            ("back", "<Alt>Left"),
            ("close", "<Primary>w"),
            ("shortcuts", "F1"),
        ] {
            app.set_accels_for_action(&format!("win.{name}"), &[accel]);
        }
        let weak = Rc::downgrade(&self.0);
        self.0.sidebar_action.connect_activate(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                Shell(inner).toggle_sidebar();
            }
        });
        let weak = Rc::downgrade(&self.0);
        self.0.detail_action.connect_activate(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                let visible = !inner.detail_requested[inner.destination.get().index()].get();
                Shell(inner).set_detail_visible(visible);
            }
        });
        let weak = Rc::downgrade(&self.0);
        self.0.back_action.connect_activate(move |_, _| {
            if let Some(inner) = weak.upgrade() {
                Shell(inner).back();
            }
        });
        let weak = Rc::downgrade(&self.0);
        self.0
            .appearance_action
            .connect_activate(move |_, parameter| {
                if let (Some(inner), Some(id)) =
                    (weak.upgrade(), parameter.and_then(glib::Variant::str))
                {
                    if let Some(choice) = Appearance::from_id(id) {
                        Shell(inner).set_appearance(choice);
                    }
                }
            });
        for name in ["shortcuts", "about", "close"] {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(&self.0);
            action.connect_activate(move |_, _| {
                if let Some(inner) = weak.upgrade() {
                    match name {
                        "shortcuts" => Shell(inner).show_shortcuts(),
                        "about" => Shell(inner).show_about(),
                        "close" => inner.window.close(),
                        _ => unreachable!(),
                    }
                }
            });
            self.0.window.add_action(&action);
        }
    }

    fn show_shortcuts(&self) {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
        column.set_margin_top(24);
        column.set_margin_bottom(24);
        column.set_margin_start(24);
        column.set_margin_end(24);
        let grid = gtk::Grid::builder()
            .row_spacing(12)
            .column_spacing(32)
            .build();
        for (row, (description, keys)) in [
            (
                "Team / Tasks / Messages / Events / Usage",
                "Ctrl+1 / 2 / 3 / 4 / 5",
            ),
            ("Show or hide sidebar", "Ctrl+B"),
            ("Show or hide details", "Ctrl+I"),
            ("Back from compact navigation or details", "Alt+Left"),
            ("Move between controls", "Tab / Shift+Tab"),
            ("Move between destinations in the sidebar", "Up / Down"),
            ("Open the focused destination", "Enter"),
            (
                "Leave an attached terminal (other keys go to the agent)",
                "F6",
            ),
            ("Keyboard shortcuts", "F1"),
            ("Close window", "Ctrl+W"),
            ("Quit application", "Ctrl+Q"),
        ]
        .into_iter()
        .enumerate()
        {
            let description = gtk::Label::new(Some(description));
            description.set_wrap(true);
            description.set_xalign(0.0);
            let keys = gtk::Label::new(Some(keys));
            keys.set_wrap(true);
            keys.set_xalign(0.0);
            grid.attach(&description, 0, row as i32, 1, 1);
            grid.attach(&keys, 1, row as i32, 1, 1);
        }
        column.append(&grid);
        let close = gtk::Button::with_label("Close");
        close.set_halign(gtk::Align::End);
        column.append(&close);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&column)
            .build();
        let dialog = gtk::Window::builder()
            .title("Keyboard Shortcuts")
            .transient_for(&self.0.window)
            .modal(true)
            .default_width(580)
            .default_height(500)
            .child(&scroller)
            .build();
        close.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.close()
        ));
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak]
            dialog,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                if key == gtk::gdk::Key::Escape
                    && !modifiers.intersects(
                        gtk::gdk::ModifierType::CONTROL_MASK
                            | gtk::gdk::ModifierType::ALT_MASK
                            | gtk::gdk::ModifierType::SUPER_MASK,
                    )
                {
                    dialog.close();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        dialog.add_controller(keys);
        self.restore_after_dialog(&dialog);
        dialog.present();
        close.grab_focus();
    }

    fn show_about(&self) {
        let dialog = gtk::AboutDialog::builder()
            .transient_for(&self.0.window)
            .modal(true)
            .program_name("Crew")
            .version(env!("CARGO_PKG_VERSION"))
            .logo_icon_name(application::APPLICATION_ID)
            .comments(
                "A native workspace client. Closing this application does not stop your team.",
            )
            .license_type(gtk::License::MitX11)
            .build();
        self.restore_after_dialog(&dialog);
        dialog.present();
    }

    fn restore_after_dialog(&self, dialog: &impl IsA<gtk::Window>) {
        let previous =
            gtk::prelude::GtkWindowExt::focus(&self.0.window).map(|focus| focus.downgrade());
        let weak = Rc::downgrade(&self.0);
        dialog.connect_close_request(move |_| {
            if let Some(inner) = weak.upgrade() {
                if !previous
                    .as_ref()
                    .and_then(glib::WeakRef::upgrade)
                    .is_some_and(|focus| focus.grab_focus())
                {
                    Shell(inner).focus_visible_view();
                }
            }
            glib::Propagation::Proceed
        });
    }
}

/// Arrow keys move focus between destinations without selecting one; Tab and
/// Shift+Tab leave the list from whichever destination has focus.
fn sidebar_keys(shell: &Shell) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&shell.0);
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        use gtk::gdk::{Key, ModifierType};
        let Some(inner) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if modifiers.intersects(
            ModifierType::CONTROL_MASK | ModifierType::ALT_MASK | ModifierType::SUPER_MASK,
        ) {
            return glib::Propagation::Proceed;
        }
        let Some(index) = inner.rows.iter().position(|row| row.is_focus()) else {
            return glib::Propagation::Proceed;
        };
        let last = inner.rows.len() - 1;
        let target = match key {
            Key::Up | Key::KP_Up => index.saturating_sub(1),
            Key::Down | Key::KP_Down => (index + 1).min(last),
            Key::Home | Key::KP_Home => 0,
            Key::End | Key::KP_End => last,
            Key::Tab | Key::KP_Tab | Key::ISO_Left_Tab => {
                let backward =
                    key == Key::ISO_Left_Tab || modifiers.contains(ModifierType::SHIFT_MASK);
                focus_past(
                    inner.navigation.upcast_ref(),
                    if backward {
                        gtk::DirectionType::TabBackward
                    } else {
                        gtk::DirectionType::TabForward
                    },
                );
                return glib::Propagation::Stop;
            }
            _ => return glib::Propagation::Proceed,
        };
        inner.rows[target].grab_focus();
        glib::Propagation::Stop
    });
    keys
}

/// Move focus to the next focusable widget outside `widget` in `direction`,
/// wrapping at the window's edge as Tab does.
pub(crate) fn focus_past(widget: &gtk::Widget, direction: gtk::DirectionType) -> bool {
    let forward = direction == gtk::DirectionType::TabForward;
    let mut node = widget.clone();
    while let Some(parent) = node.parent() {
        let mut sibling = if forward {
            node.next_sibling()
        } else {
            node.prev_sibling()
        };
        while let Some(candidate) = sibling {
            if candidate.is_visible() && candidate.child_focus(direction) {
                return true;
            }
            sibling = if forward {
                candidate.next_sibling()
            } else {
                candidate.prev_sibling()
            };
        }
        node = parent;
    }
    match node.downcast::<gtk::Window>() {
        Ok(window) => {
            gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
            window.child_focus(direction)
        }
        Err(_) => false,
    }
}

fn page_stack() -> gtk::Stack {
    gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .hhomogeneous(false)
        .vhomogeneous(false)
        .transition_type(gtk::StackTransitionType::None)
        .build()
}

fn view_slot() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .vexpand(true)
        .build()
}

fn replace_child(slot: &gtk::Box, widget: &impl IsA<gtk::Widget>) {
    while let Some(child) = slot.first_child() {
        slot.remove(&child);
    }
    slot.append(widget);
}

fn name(widget: &impl IsA<gtk::Accessible>, label: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(label)]);
}

fn destinations_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    for destination in Destination::ALL {
        menu.append(
            Some(destination.title()),
            Some(&format!("win.destination('{}')", destination.id())),
        );
    }
    menu
}

fn application_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let view = gio::Menu::new();
    view.append(Some("Show Sidebar"), Some("win.sidebar"));
    view.append(Some("Show Details"), Some("win.details"));
    menu.append_section(Some("View"), &view);
    let appearance = gio::Menu::new();
    for (title, value) in [("System", "system"), ("Light", "light"), ("Dark", "dark")] {
        appearance.append(Some(title), Some(&format!("win.appearance('{value}')")));
    }
    menu.append_submenu(Some("Appearance"), &appearance);
    let help = gio::Menu::new();
    help.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
    help.append(Some("About Crew"), Some("win.about"));
    help.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &help);
    menu
}

fn workspace_selector() -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .child(&ellipsized_label("No workspace", 12))
        .build();
    button.set_always_show_arrow(true);
    name(&button, "Workspaces, none connected");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.add_css_class("crew-workspace");
    let title = gtk::Label::new(Some("No connected workspaces"));
    title.add_css_class("crew-title");
    title.set_wrap(true);
    title.set_xalign(0.0);
    let explanation = gtk::Label::new(Some(
        "Workspace data becomes available after a connection is verified.",
    ));
    explanation.set_wrap(true);
    explanation.set_max_width_chars(32);
    explanation.set_xalign(0.0);
    content.append(&title);
    content.append(&explanation);
    let popover = gtk::Popover::builder().child(&content).build();
    button.set_popover(Some(&popover));
    button
}

fn ellipsized_label(text: &str, width: i32) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(width)
        .build()
}
