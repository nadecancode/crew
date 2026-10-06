use crate::views::common;
use crew_client::api::*;
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Clone)]
pub struct Form {
    pub dialog: gtk::Dialog,
    pub buffer: gtk::TextBuffer,
    pub error: gtk::Label,
    pub submit: gtk::Button,
    opener: glib::WeakRef<gtk::Widget>,
    pub pending: Rc<Cell<bool>>,
    pub receipt: Rc<Cell<bool>>,
    closing: Rc<Cell<bool>>,
    editor: gtk::TextView,
    extra_editors: Rc<RefCell<Vec<gtk::Widget>>>,
    caption: gtk::Label,
}
impl Form {
    pub fn new(
        parent: &gtk::ApplicationWindow,
        title: &str,
        description: &str,
        draft: &str,
        submit_label: &str,
        send: Rc<dyn Fn(String)>,
        cancel: Rc<dyn Fn(String)>,
    ) -> Self {
        let dialog = gtk::Dialog::builder()
            .transient_for(parent)
            .modal(true)
            .title(title)
            .default_width(540)
            .default_height(380)
            .build();
        let opener = glib::WeakRef::new();
        opener.set(gtk::prelude::GtkWindowExt::focus(parent).as_ref());
        let body = common::column();
        let caption = common::text(description);
        body.append(&caption);
        let view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .vexpand(true)
            .left_margin(12)
            .right_margin(12)
            .top_margin(12)
            .bottom_margin(12)
            .build();
        view.update_property(&[gtk::accessible::Property::Label("Message or answer text")]);
        view.buffer().set_text(draft);
        body.append(&common::scroll(&view));
        let error = common::text("");
        error.add_css_class("error");
        body.append(&error);
        dialog.content_area().append(&body);
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        let submit = dialog
            .add_button(submit_label, gtk::ResponseType::Accept)
            .downcast::<gtk::Button>()
            .expect("dialog button");
        dialog.set_default_response(gtk::ResponseType::Cancel);
        let form = Self {
            dialog,
            buffer: view.buffer(),
            error,
            submit,
            opener,
            pending: Rc::new(Cell::new(false)),
            receipt: Rc::new(Cell::new(false)),
            closing: Rc::new(Cell::new(false)),
            editor: view.clone(),
            extra_editors: Rc::new(RefCell::new(Vec::new())),
            caption,
        };
        form.dialog.connect_response({
            let buffer = form.buffer.clone();
            let closing = form.closing.clone();
            let receipt = form.receipt.clone();
            move |dialog, response| {
                if closing.get() {
                    return;
                }
                if response == gtk::ResponseType::Accept {
                    send(common::buffer_text(&buffer));
                } else {
                    if receipt.get() {
                        cancel(common::buffer_text(&buffer));
                        dialog.set_visible(false);
                        return;
                    }
                    closing.set(true);
                    cancel(common::buffer_text(&buffer));
                    dialog.close();
                }
            }
        });
        escape(&form.dialog);
        form.dialog.present();
        view.grab_focus();
        form
    }
    pub fn pending(&self) {
        self.pending.set(true);
        self.editor.set_editable(false);
        for editor in self.extra_editors.borrow().iter() {
            editor.set_sensitive(false);
        }
        self.submit.set_sensitive(false);
        self.error.set_label("Sending…");
    }
    pub fn failed(&self, message: &str) {
        self.pending.set(false);
        self.editor.set_editable(true);
        for editor in self.extra_editors.borrow().iter() {
            editor.set_sensitive(true);
        }
        self.submit.set_sensitive(false);
        self.error.set_label(message);
    }
    pub fn close(&self) {
        self.closing.set(true);
        self.dialog.close();
        if let Some(opener) = self.opener.upgrade().filter(|w| w.is_mapped()) {
            opener.grab_focus();
        }
    }
    pub fn retain_receipt(&self, explanation: &str) {
        self.receipt.set(true);
        self.editor.set_editable(false);
        self.submit.set_sensitive(false);
        for editor in self.extra_editors.borrow().iter() {
            editor.set_sensitive(false);
        }
        if let Some(button) = self
            .dialog
            .widget_for_response(gtk::ResponseType::Cancel)
            .and_downcast::<gtk::Button>()
        {
            button.set_label("Close");
        }
        self.dialog.set_modal(false);
        self.error.set_label(explanation);
    }
    pub fn purge(&self) {
        self.buffer.set_text("");
        self.caption.set_label("Access unavailable");
        self.error.set_label("");
        for widget in self.extra_editors.borrow().iter() {
            if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
                entry.set_text("");
            }
        }
        self.pending.set(false);
        self.receipt.set(false);
        self.close();
    }
    pub fn add_editor(&self, widget: &impl IsA<gtk::Widget>) {
        self.extra_editors
            .borrow_mut()
            .push(widget.clone().upcast());
    }
}

pub fn confirmation(
    parent: &gtk::ApplicationWindow,
    kind: ConfirmationKind,
    targets: &[String],
    confirm: Rc<dyn Fn()>,
    cancel: Rc<dyn Fn()>,
) -> gtk::Dialog {
    let names = targets.join(", ");
    let (title, description, button) = match kind {
        ConfirmationKind::KillAgent => ("Kill agent", format!("Stop {names}'s process? Its task stays open."), "Kill agent"),
        ConfirmationKind::RestartFresh => ("Restart fresh", format!("Restart {names} without resuming the old conversation?"), "Restart fresh"),
        ConfirmationKind::RestartLimited => ("Restart paused agents", format!("Restart {} agents ({names}) using the current login? Their sessions restart instead of simply continuing.", targets.len()), "Restart agents"),
        ConfirmationKind::StopTeam => ("Stop team", "Stop every agent and the daemon? Committed work remains; unfinished tasks are not completed by stopping the team.".into(), "Stop team"),
    };
    let dialog = gtk::Dialog::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .default_width(480)
        .build();
    let body = common::column();
    body.append(&common::heading(title));
    body.append(&common::text(&description));
    dialog.content_area().append(&body);
    let cancel_button = dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    let destructive = dialog.add_button(button, gtk::ResponseType::Accept);
    destructive.add_css_class("destructive-action");
    dialog.set_default_response(gtk::ResponseType::Cancel);
    let opener = glib::WeakRef::<gtk::Widget>::new();
    opener.set(gtk::prelude::GtkWindowExt::focus(parent).as_ref());
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            confirm();
        } else {
            cancel();
        }
        dialog.close();
        if let Some(opener) = opener.upgrade().filter(|w| w.is_mapped()) {
            opener.grab_focus();
        }
    });
    escape(&dialog);
    dialog.present();
    cancel_button.grab_focus();
    dialog
}

pub fn escape(dialog: &gtk::Dialog) {
    let keys = gtk::EventControllerKey::new();
    let weak = dialog.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            if let Some(dialog) = weak.upgrade() {
                dialog.response(gtk::ResponseType::Cancel);
            }
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    dialog.add_controller(keys);
}
