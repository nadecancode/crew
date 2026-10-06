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

/// Hears whether a terminal is attached.
type AttachedListener = Rc<dyn Fn(bool)>;

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
    detach: gtk::Button,
    /// Told whether the terminal is attached whenever that changes.
    listener: Rc<RefCell<Option<AttachedListener>>>,
}
impl TerminalView {
    pub fn new(action: Rc<dyn Fn(ActionIntent)>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        // The status line and controls keep the 12/24-pixel inset of the other
        // tabs; the terminal itself runs edge to edge.
        let status = common::text("Not attached · read-only");
        status.set_margin_top(12);
        status.set_margin_start(24);
        status.set_margin_end(24);
        root.append(&status);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        controls.set_margin_start(24);
        controls.set_margin_end(24);
        // Copy needs a selection and Detach an attachment.
        let copy = gtk::Button::builder()
            .label("Copy selection")
            .sensitive(false)
            .build();
        let detach = gtk::Button::builder()
            .label("Detach terminal")
            .sensitive(false)
            .build();
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
        // An attached terminal takes every key, Tab and Ctrl+1–5 included.
        // F6 leaves it for the controls above, as Shift+Tab would elsewhere.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let viewport = viewport.downgrade();
            move |_, key, _, modifiers| {
                use gtk::gdk::ModifierType;
                if key != gtk::gdk::Key::F6
                    || modifiers.intersects(
                        ModifierType::CONTROL_MASK
                            | ModifierType::ALT_MASK
                            | ModifierType::SUPER_MASK
                            | ModifierType::SHIFT_MASK,
                    )
                {
                    return glib::Propagation::Proceed;
                }
                if let Some(viewport) = viewport.upgrade() {
                    crate::shell::focus_past(
                        viewport.upcast_ref(),
                        gtk::DirectionType::TabBackward,
                    );
                }
                glib::Propagation::Stop
            }
        });
        viewport.add_controller(keys);
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
            detach: detach.clone(),
            listener: Rc::new(RefCell::new(None)),
        };
        this.terminal.connect_selection_changed({
            let copy = copy.downgrade();
            move |terminal| {
                if let Some(copy) = copy.upgrade() {
                    copy.set_sensitive(terminal.has_selection());
                }
            }
        });
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
            let listener = this.listener.clone();
            move |detach| {
                let current = binding.borrow().clone();
                let had_focus = detach.is_focus();
                active.set(false);
                if let Some(terminal) = terminal.upgrade() {
                    terminal.set_input_enabled(false);
                    terminal.update_property(&[gtk::accessible::Property::Description("")]);
                }
                if let Some(status) = status.upgrade() {
                    status.set_label("Detached · read-only");
                }
                // The listener hears it while Detach still has focus, so the
                // panel can take focus back to its Attach control.
                let notify = listener.borrow().clone();
                if let Some(notify) = notify {
                    notify(false);
                }
                detach.set_sensitive(false);
                // Otherwise focus moves back to the control before Detach
                // rather than being lost with it.
                if had_focus && keeps_focus(detach.upcast_ref()) {
                    crate::shell::focus_past(detach.upcast_ref(), gtk::DirectionType::TabBackward);
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
                    // VTE refreshes its accessible text only when output changes
                    // the screen, so text shown before it was laid out (the
                    // attachment's first screen) stayed hidden from screen
                    // readers until the next output. Refresh it once laid out.
                    terminal.emit_by_name::<()>("contents-changed", &[]);
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
        // What a screen reader says on reaching the terminal.
        self.terminal
            .update_property(&[gtk::accessible::Property::Description(ATTACHED)]);
        self.changed();
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
        *renderer.listener.borrow_mut() = self.listener.borrow().clone();
        renderer.changed();
        *self.adopted.borrow_mut() = Some(renderer);
    }
    /// `listener` hears whether this terminal is attached, now and whenever
    /// that changes, also through a renderer it adopts.
    pub fn connect_attached(&self, listener: impl Fn(bool) + 'static) {
        let listener: AttachedListener = Rc::new(listener);
        *self.listener.borrow_mut() = Some(listener.clone());
        if let Some(renderer) = self.adopted.borrow().as_ref() {
            *renderer.listener.borrow_mut() = Some(listener);
        }
        self.changed();
    }
    fn changed(&self) {
        let active = self.is_active();
        let had_focus = self.detach.is_focus();
        let listener = self.listener.borrow().clone();
        if let Some(listener) = listener {
            listener(active);
        }
        self.detach.set_sensitive(self.active.get());
        if had_focus && !self.detach.is_sensitive() && keeps_focus(self.detach.upcast_ref()) {
            crate::shell::focus_past(self.detach.upcast_ref(), gtk::DirectionType::TabBackward);
        }
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
        self.terminal
            .update_property(&[gtk::accessible::Property::Description("")]);
        self.changed();
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

/// Announced when an attached terminal gets focus.
pub const ATTACHED: &str = "Attached. F6 leaves the terminal.";

/// Whether focus is still on `control` or nowhere: nobody has moved it.
fn keeps_focus(control: &gtk::Widget) -> bool {
    control
        .root()
        .and_then(|root| root.focus())
        .is_none_or(|focus| focus == *control)
}
