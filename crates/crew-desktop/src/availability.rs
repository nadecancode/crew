//! GTK bindings for the immutable shared availability query. These records
//! hold presentation callbacks and widgets; they do not decide permission.
use crew_client::api::*;
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

#[derive(Clone, PartialEq)]
pub enum Input {
    Action(Box<ActionIntent>),
    Local,
    Invalid(String),
}
pub type Intent = Rc<dyn Fn() -> Input>;
struct Binding {
    button: glib::WeakRef<gtk::Button>,
    reason: glib::WeakRef<gtk::Label>,
    intent: Intent,
    pending: Rc<Cell<bool>>,
}
/// Bindings by control, and the controls whose reason waits for input.
#[derive(Default)]
pub struct Controls(
    RefCell<BTreeMap<u64, Binding>>,
    RefCell<BTreeMap<u64, Rc<Cell<bool>>>>,
);
impl Controls {
    pub fn bind(
        &self,
        id: u64,
        button: &gtk::Button,
        reason: &gtk::Label,
        intent: Intent,
        pending: Rc<Cell<bool>>,
    ) {
        button.set_sensitive(false);
        self.0.borrow_mut().insert(
            id,
            Binding {
                button: button.downgrade(),
                reason: reason.downgrade(),
                intent,
                pending,
            },
        );
    }
    /// Show no reason line for this control until `touched` is set: before
    /// any input, a disabled control with an empty field is not an error.
    pub fn wait_for_input(&self, id: u64, touched: Rc<Cell<bool>>) {
        self.1.borrow_mut().insert(id, touched);
    }
    fn quiet(&self, id: u64) -> bool {
        self.1
            .borrow()
            .get(&id)
            .is_some_and(|touched| !touched.get())
    }
    pub fn ids(&self) -> Vec<u64> {
        self.0.borrow().keys().copied().collect()
    }
    pub fn intent(&self, id: u64) -> Option<Input> {
        let intent = self.0.borrow().get(&id)?.intent.clone();
        Some(intent())
    }
    pub fn apply(
        &self,
        id: u64,
        captured: &ActionIntent,
        result: Result<ActionAvailability, ClientError>,
    ) {
        let bindings = self.0.borrow();
        let Some(binding) = bindings.get(&id) else {
            return;
        };
        if (binding.intent)() != Input::Action(Box::new(captured.clone())) {
            return;
        }
        let Some(button) = binding.button.upgrade() else {
            return;
        };
        let (available, explanation) = match result {
            Ok(value) => (
                value.available,
                value.reason.map_or(String::new(), |reason| reason.message),
            ),
            Err(error) => (false, error.to_string()),
        };
        button.set_sensitive(available && !binding.pending.get());
        button.set_tooltip_text((!explanation.is_empty()).then_some(explanation.as_str()));
        button.update_property(&[gtk::accessible::Property::Description(&explanation)]);
        if let Some(reason) = binding.reason.upgrade() {
            if !binding.pending.get() {
                show(&reason, if self.quiet(id) { "" } else { &explanation });
            }
        }
    }
    pub fn unavailable(&self, id: u64, explanation: &str) {
        if let Some(binding) = self.0.borrow().get(&id) {
            if let Some(button) = binding.button.upgrade() {
                button.set_sensitive(false);
            }
            if let Some(reason) = binding.reason.upgrade() {
                show(&reason, if self.quiet(id) { "" } else { explanation });
            }
        }
    }
    pub fn remove(&self, id: u64) {
        self.0.borrow_mut().remove(&id);
        self.1.borrow_mut().remove(&id);
    }
    pub fn local(&self, id: u64) {
        if let Some(binding) = self.0.borrow().get(&id) {
            if let Some(button) = binding.button.upgrade() {
                button.set_sensitive(!binding.pending.get());
            }
            if let Some(reason) = binding.reason.upgrade() {
                if !binding.pending.get() {
                    reason.set_label("");
                }
            }
        }
    }
}

/// An availability reason reads as a note, not an error: only a failed
/// submit is styled as one.
fn show(reason: &gtk::Label, text: &str) {
    // A failed submit stays visible until there is something else to say.
    if text.is_empty() && reason.has_css_class("error") {
        return;
    }
    reason.remove_css_class("error");
    reason.add_css_class("crew-muted");
    reason.set_label(text);
}
