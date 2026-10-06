//! A row of tabs that stays on one line. Tabs that do not fit move into a
//! More menu at the end of the row; the selected tab always stays in the row.

use std::cell::{OnceCell, RefCell};

use gtk::{glib, prelude::*, subclass::prelude::*};

const SPACING: i32 = 4;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TabBar {
        pub tabs: RefCell<Vec<gtk::ToggleButton>>,
        pub items: RefCell<Vec<gtk::Button>>,
        pub more: OnceCell<gtk::MenuButton>,
        pub menu: OnceCell<gtk::Box>,
        /// A tab chosen from the menu, focused once it is back in the row.
        pub focus: RefCell<Option<gtk::ToggleButton>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TabBar {
        const NAME: &'static str = "CrewTabBar";
        type Type = super::TabBar;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TabBar {
        fn constructed(&self) {
            self.parent_constructed();
            let menu = gtk::Box::new(gtk::Orientation::Vertical, 4);
            menu.set_margin_top(8);
            menu.set_margin_bottom(8);
            menu.set_margin_start(8);
            menu.set_margin_end(8);
            let popover = gtk::Popover::builder().child(&menu).build();
            let more = gtk::MenuButton::builder()
                .label("More")
                .has_frame(false)
                .popover(&popover)
                .build();
            more.add_css_class("crew-tab-more");
            more.update_property(&[gtk::accessible::Property::Label("More sections")]);
            // The menu offers only the tabs that are out of the row now.
            more.set_create_popup_func({
                let bar = self.obj().downgrade();
                move |_| {
                    if let Some(bar) = bar.upgrade() {
                        let imp = bar.imp();
                        for (tab, item) in imp.tabs.borrow().iter().zip(imp.items.borrow().iter()) {
                            item.set_visible(tab.is_visible() && !tab.is_child_visible());
                        }
                    }
                }
            });
            more.set_parent(&*self.obj());
            more.set_child_visible(false);
            self.more.set(more).expect("one More menu");
            self.menu.set(menu).expect("one More menu");
        }

        fn dispose(&self) {
            for tab in self.tabs.borrow().iter() {
                tab.unparent();
            }
            if let Some(more) = self.more.get() {
                more.unparent();
            }
        }
    }

    impl WidgetImpl for TabBar {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let tabs = self.tabs.borrow();
            let shown = tabs.iter().filter(|tab| tab.is_visible());
            if orientation == gtk::Orientation::Vertical {
                let (min, natural) = shown
                    .map(|tab| tab.upcast_ref::<gtk::Widget>().clone())
                    .chain(std::iter::once(self.more().clone().upcast()))
                    .map(|child| child.measure(orientation, -1))
                    .fold((0, 0), |(min, natural), child| {
                        (min.max(child.0), natural.max(child.1))
                    });
                return (min, natural, -1, -1);
            }
            let naturals: Vec<i32> = shown.map(|tab| tab.measure(orientation, -1).1).collect();
            let natural = row_width(&naturals);
            // At the least, the widest tab and the More menu side by side.
            let widest = naturals.iter().copied().max().unwrap_or(0);
            let min = match naturals.len() {
                0 | 1 => natural,
                _ => natural.min(widest + SPACING + self.more().measure(orientation, -1).1),
            };
            (min, natural, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let tabs = self.tabs.borrow();
            let naturals: Vec<i32> = tabs
                .iter()
                .map(|tab| {
                    if tab.is_visible() {
                        tab.measure(gtk::Orientation::Horizontal, -1).1
                    } else {
                        0
                    }
                })
                .collect();
            let widths: Vec<i32> = tabs
                .iter()
                .zip(&naturals)
                .filter(|(tab, _)| tab.is_visible())
                .map(|(_, width)| *width)
                .collect();
            let overflow = row_width(&widths) > width;
            let more = self.more();
            let more_width = more.measure(gtk::Orientation::Horizontal, -1).1;
            let mut in_row: Vec<bool> = tabs.iter().map(|tab| tab.is_visible()).collect();
            if overflow {
                // The selected tab first, then tabs in order while they fit.
                let selected = tabs.iter().position(|tab| tab.is_active());
                let mut room = width - more_width;
                if let Some(selected) = selected {
                    room -= naturals[selected] + SPACING;
                }
                let mut full = false;
                for (index, shown) in in_row.iter_mut().enumerate() {
                    if Some(index) == selected || !*shown {
                        continue;
                    }
                    let cost = naturals[index] + SPACING;
                    full = full || cost > room;
                    *shown = !full;
                    if *shown {
                        room -= cost;
                    }
                }
            }
            let mut x = 0;
            for ((tab, natural), shown) in tabs.iter().zip(&naturals).zip(&in_row) {
                if !tab.is_visible() {
                    continue;
                }
                tab.set_child_visible(*shown);
                if *shown {
                    tab.size_allocate(&gtk::Allocation::new(x, 0, *natural, height), -1);
                    x += natural + SPACING;
                }
            }
            more.set_child_visible(overflow);
            if overflow {
                more.size_allocate(&gtk::Allocation::new(x, 0, more_width, height), -1);
            }
            // Focus moves to a tab chosen from the menu once it is drawn in
            // the row, outside layout.
            let chosen = self.focus.borrow().clone();
            if let Some(tab) = chosen.filter(|tab| tab.is_child_visible()) {
                self.focus.replace(None);
                glib::idle_add_local_once(move || {
                    tab.grab_focus();
                });
            }
        }
    }

    impl TabBar {
        pub fn more(&self) -> &gtk::MenuButton {
            self.more.get().expect("constructed More menu")
        }
    }
}

/// Tabs and the spacing between them.
fn row_width(widths: &[i32]) -> i32 {
    let gaps = widths.len().saturating_sub(1) as i32;
    widths.iter().sum::<i32>() + gaps * SPACING
}

glib::wrapper! {
    pub struct TabBar(ObjectSubclass<imp::TabBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TabBar {
    pub fn new(label: &str) -> Self {
        let bar: Self = glib::Object::builder()
            .property("accessible-role", gtk::AccessibleRole::TabList)
            .build();
        bar.update_property(&[gtk::accessible::Property::Label(label)]);
        bar
    }

    /// Add a tab at the end of the row, and its entry in the More menu.
    pub fn append(&self, tab: &gtk::ToggleButton) {
        let imp = self.imp();
        tab.insert_before(self, Some(imp.more()));
        tab.connect_toggled({
            let bar = self.downgrade();
            move |tab| {
                if !tab.is_active() {
                    return;
                }
                if let Some(bar) = bar.upgrade() {
                    bar.queue_allocate();
                }
            }
        });
        let item = gtk::Button::builder()
            .label(tab.label().unwrap_or_default())
            .has_frame(false)
            .visible(false)
            .build();
        if let Some(label) = item.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
        }
        item.connect_clicked({
            let bar = self.downgrade();
            let tab = tab.downgrade();
            move |_| {
                let (Some(bar), Some(tab)) = (bar.upgrade(), tab.upgrade()) else {
                    return;
                };
                bar.imp().more().popdown();
                bar.imp().focus.replace(Some(tab.clone()));
                tab.set_active(true);
                bar.queue_allocate();
            }
        });
        imp.menu.get().expect("constructed More menu").append(&item);
        imp.tabs.borrow_mut().push(tab.clone());
        imp.items.borrow_mut().push(item);
    }
}
