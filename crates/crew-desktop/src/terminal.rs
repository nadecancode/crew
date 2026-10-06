//! VTE renderer over a scoped native attachment. No local process is spawned.
use crate::views::common;
use crew_client::api::*;
use gtk::{glib, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use vte4::prelude::*;

mod viewport {
    use super::*;
    #[derive(Default)]
    pub struct TerminalViewport {
        pub child: RefCell<Option<vte4::Terminal>>,
        pub resized: RefCell<Option<Box<dyn Fn()>>>,
        pending: Cell<bool>,
    }
    #[glib::object_subclass]
    impl ObjectSubclass for TerminalViewport {
        const NAME: &'static str = "CrewTerminalViewport";
        type Type = super::TerminalViewport;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for TerminalViewport {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
            self.resized.borrow_mut().take();
        }
    }
    impl WidgetImpl for TerminalViewport {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.child
                .borrow()
                .as_ref()
                .map_or((1, 1, -1, -1), |child| child.measure(orientation, for_size))
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.child.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
        }
        fn focus(&self, direction: gtk::DirectionType) -> bool {
            self.child
                .borrow()
                .as_ref()
                .is_some_and(|child| child.child_focus(direction))
        }
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                child.allocate(width, height, baseline, None);
            }
            if self.pending.replace(true) {
                return;
            }
            let weak = self.obj().downgrade();
            glib::idle_add_local_once(move || {
                if let Some(obj) = weak.upgrade() {
                    obj.imp().pending.set(false);
                    if let Some(resized) = obj.imp().resized.borrow().as_ref() {
                        resized();
                    }
                }
            });
        }
    }
}
glib::wrapper! {
    pub struct TerminalViewport(ObjectSubclass<viewport::TerminalViewport>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

#[derive(Clone)]
pub struct TerminalView {
    pub root: gtk::Box,
    pub terminal: vte4::Terminal,
    status: gtk::Label,
    binding: Rc<RefCell<Option<(ClientScope, AttachmentId)>>>,
    active: Rc<Cell<bool>>,
    last_size: Rc<Cell<(u16, u16)>>,
    action: Rc<dyn Fn(ActionIntent)>,
    adopted: Rc<RefCell<Option<TerminalView>>>,
}
impl TerminalView {
    pub fn new(action: Rc<dyn Fn(ActionIntent)>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let status = common::text("Not attached · read-only");
        root.append(&status);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let copy = gtk::Button::with_label("Copy selection");
        let detach = gtk::Button::with_label("Detach terminal");
        controls.append(&copy);
        controls.append(&detach);
        root.append(&controls);
        let terminal = vte4::Terminal::new();
        terminal.set_hexpand(true);
        terminal.set_vexpand(true);
        terminal.set_input_enabled(false);
        terminal.set_allow_hyperlink(false);
        terminal.set_audible_bell(false);
        terminal.set_scroll_on_output(false);
        terminal.set_scrollback_lines(2_000);
        terminal.set_text_blink_mode(vte4::TextBlinkMode::Never);
        terminal.update_property(&[gtk::accessible::Property::Label("Agent terminal")]);
        let viewport: TerminalViewport = glib::Object::builder().build();
        terminal.set_parent(&viewport);
        *viewport.imp().child.borrow_mut() = Some(terminal.clone());
        viewport.set_hexpand(true);
        viewport.set_vexpand(true);
        root.append(&viewport);
        let this = Self {
            root,
            terminal,
            status,
            action,
            binding: Rc::new(RefCell::new(None)),
            active: Rc::new(Cell::new(false)),
            last_size: Rc::new(Cell::new((0, 0))),
            adopted: Rc::new(RefCell::new(None)),
        };
        this.terminal.connect_commit({
            let active = this.active.clone();
            let binding = this.binding.clone();
            let action = this.action.clone();
            move |terminal, text, _| {
                if active.get() && terminal.has_focus() {
                    if let Some((scope, id)) = binding.borrow().as_ref() {
                        action(ActionIntent::AttachInput {
                            scope: scope.clone(),
                            attachment_id: *id,
                            data: text.as_bytes().to_vec(),
                        });
                    }
                }
            }
        });
        copy.connect_clicked({
            let terminal = this.terminal.clone();
            move |_| terminal.copy_clipboard_format(vte4::Format::Text)
        });
        detach.connect_clicked({
            let binding = this.binding.clone();
            let active = this.active.clone();
            let terminal = this.terminal.downgrade();
            let status = this.status.downgrade();
            let action = this.action.clone();
            move |_| {
                let current = binding.borrow().clone();
                active.set(false);
                if let Some(terminal) = terminal.upgrade() {
                    terminal.set_input_enabled(false);
                }
                if let Some(status) = status.upgrade() {
                    status.set_label("Detached · read-only");
                }
                if let Some((scope, attachment_id)) = current {
                    action(ActionIntent::Detach {
                        scope,
                        attachment_id,
                    });
                }
            }
        });
        *viewport.imp().resized.borrow_mut() = Some(Box::new({
            let binding = this.binding.clone();
            let active = this.active.clone();
            let terminal = this.terminal.downgrade();
            let last_size = this.last_size.clone();
            let action = this.action.clone();
            move || {
                if let Some(terminal) = terminal.upgrade() {
                    resize(&terminal, &binding, &active, &last_size, &action);
                }
            }
        }));
        this
    }
    /// Called only for the successful Attach's identified notice. Data cannot
    /// create or redirect this binding.
    pub fn bind(&self, scope: ClientScope, attachment_id: AttachmentId) {
        *self.binding.borrow_mut() = Some((scope, attachment_id));
        self.active.set(true);
        self.last_size.set((0, 0));
        self.terminal.set_input_enabled(true);
        self.status
            .set_label("Attached · input goes to this agent's terminal");
        // GTK allocation triggers resize. Before the captured panel is
        // associated, preserve the successful request's initial 80x24 size.
    }
    pub fn adopt(&self, renderer: TerminalView) {
        if self
            .adopted
            .borrow()
            .as_ref()
            .is_some_and(|old| old.binding() == renderer.binding())
        {
            return;
        }
        common::clear(&self.root);
        self.root.append(&renderer.root);
        *self.adopted.borrow_mut() = Some(renderer);
    }
    pub fn accept(&self, chunk: &TerminalChunk) -> bool {
        if let Some(renderer) = self.adopted.borrow().as_ref() {
            return renderer.accept(chunk);
        }
        if !self.active.get()
            || self.binding.borrow().as_ref() != Some(&(chunk.scope.clone(), chunk.attachment_id))
        {
            return false;
        }
        if !chunk.data.is_empty() {
            self.terminal.feed(&chunk.data);
        }
        if let Some(end) = &chunk.ended {
            self.end(&format!("Terminal ended: {end:?} · read-only"));
        }
        true
    }
    pub fn end(&self, label: &str) {
        if let Some(renderer) = self.adopted.borrow().as_ref() {
            renderer.end(label);
        }
        self.active.set(false);
        self.terminal.set_input_enabled(false);
        self.status.set_label(label);
    }
    pub fn binding(&self) -> Option<(ClientScope, AttachmentId)> {
        if let Some(renderer) = self.adopted.borrow().as_ref() {
            return renderer.binding();
        }
        self.binding.borrow().clone()
    }
    pub fn is_active(&self) -> bool {
        self.adopted
            .borrow()
            .as_ref()
            .map_or(self.active.get(), |renderer| renderer.is_active())
    }
}
fn resize(
    terminal: &vte4::Terminal,
    binding: &RefCell<Option<(ClientScope, AttachmentId)>>,
    active: &Cell<bool>,
    last_size: &Cell<(u16, u16)>,
    action: &Rc<dyn Fn(ActionIntent)>,
) {
    if !active.get() {
        return;
    }
    let rows = terminal.row_count().clamp(1, i64::from(u16::MAX)) as u16;
    let cols = terminal.column_count().clamp(1, i64::from(u16::MAX)) as u16;
    if last_size.replace((rows, cols)) == (rows, cols) {
        return;
    }
    if let Some((scope, attachment_id)) = binding.borrow().as_ref() {
        action(ActionIntent::AttachResize {
            scope: scope.clone(),
            attachment_id: *attachment_id,
            rows,
            cols,
        });
    }
}
