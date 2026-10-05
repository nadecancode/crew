use std::cell::{Cell, OnceCell};

use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::AdaptiveBox)]
    pub struct AdaptiveBox {
        #[property(get)]
        compact: Cell<bool>,
        pending: Cell<bool>,
        child: OnceCell<gtk::Box>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AdaptiveBox {
        const NAME: &'static str = "CrewAdaptiveBox";
        type Type = super::AdaptiveBox;
        type ParentType = gtk::Widget;
    }

    #[glib::derived_properties]
    impl ObjectImpl for AdaptiveBox {
        fn constructed(&self) {
            self.parent_constructed();
            let child = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            child.set_parent(&*self.obj());
            self.child.set(child).expect("one native layout child");
        }

        fn dispose(&self) {
            self.child().unparent();
        }
    }

    impl WidgetImpl for AdaptiveBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.child().request_mode()
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.child().measure(orientation, for_size)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().snapshot_child(self.child(), snapshot);
        }

        fn focus(&self, direction: gtk::DirectionType) -> bool {
            self.child().child_focus(direction)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.child().allocate(width, height, baseline, None);
            // GtkBox's layout manager bypasses Widget::size_allocate. This
            // small container delegates real layout to a native Box while
            // observing allocation without scheduling continuous frame ticks.
            let compact = width < self.limit();
            if compact == self.compact.get() || self.pending.replace(true) {
                return;
            }
            // Visibility changes run after allocation, not inside GTK layout.
            let weak = self.obj().downgrade();
            glib::idle_add_local_once(move || {
                if let Some(obj) = weak.upgrade() {
                    obj.imp().pending.set(false);
                    let compact = obj.width() < obj.imp().limit();
                    if obj.imp().compact.replace(compact) != compact {
                        obj.notify("compact");
                    }
                }
            });
        }
    }

    impl AdaptiveBox {
        pub fn child(&self) -> &gtk::Box {
            self.child.get().expect("constructed native layout")
        }

        fn limit(&self) -> i32 {
            // Pane collapse follows native text scaling, not a web breakpoint.
            let metrics = self.obj().pango_context().metrics(None, None);
            (metrics.height() / gtk::pango::SCALE * 56).max(720)
        }
    }
}

glib::wrapper! {
    pub struct AdaptiveBox(ObjectSubclass<imp::AdaptiveBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AdaptiveBox {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("accessible-role", gtk::AccessibleRole::Group)
            .build()
    }

    pub fn append(&self, child: &impl IsA<gtk::Widget>) {
        self.imp().child().append(child);
    }
}
