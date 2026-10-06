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
    Action(ActionIntent),
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
#[derive(Default)]
pub struct Controls(RefCell<BTreeMap<u64, Binding>>);
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
        if (binding.intent)() != Input::Action(captured.clone()) {
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
                reason.set_label(&explanation);
            }
        }
    }
    pub fn unavailable(&self, id: u64, explanation: &str) {
        if let Some(binding) = self.0.borrow().get(&id) {
            if let Some(button) = binding.button.upgrade() {
                button.set_sensitive(false);
            }
            if let Some(reason) = binding.reason.upgrade() {
                reason.set_label(explanation);
            }
        }
    }
    pub fn remove(&self, id: u64) {
        self.0.borrow_mut().remove(&id);
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
