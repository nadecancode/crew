use super::common;
use crew_client::api::*;
use gtk::prelude::*;
use std::{cell::RefCell, rc::Rc};

/// Persistent text buffer. Neither a read completion nor a push moves the
/// evidence caret or scroll when its qualified resource has not changed.
#[derive(Clone)]
pub struct EvidenceView {
    pub root: gtk::Box,
    status: gtk::Label,
    view: gtk::TextView,
    scroll: gtk::ScrolledWindow,
    links: gtk::Box,
    link_signature: Rc<RefCell<String>>,
}
impl EvidenceView {
    pub fn new(refresh: impl Fn() + 'static) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let status = common::text("Not loaded");
        root.append(&status);
        let refresh_button = gtk::Button::with_label("Refresh evidence");
        refresh_button.connect_clicked(move |_| refresh());
        root.append(&refresh_button);
        let links = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.append(&links);
        let view = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(true)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(12)
            .right_margin(12)
            .top_margin(12)
            .bottom_margin(12)
            .build();
        view.update_property(&[gtk::accessible::Property::Label("Complete evidence text")]);
        let scroll = common::scroll(&view);
        root.append(&scroll);
        Self {
            root,
            status,
            view,
            scroll,
            links,
            link_signature: Rc::new(RefCell::new(String::new())),
        }
    }
    pub fn set_links(&self, signature: String, links: Vec<(String, Rc<dyn Fn()>)>) {
        if *self.link_signature.borrow() == signature {
            return;
        }
        common::clear(&self.links);
        for (name, action) in links {
            let button = gtk::Button::with_label(&name);
            button.connect_clicked(move |_| action());
            self.links.append(&button);
        }
        *self.link_signature.borrow_mut() = signature;
    }
    pub fn update(&self, state: &ResourceState) {
        let label = format!(
            "{:?} · {:?}{}",
            state.key.kind,
            state.status,
            state
                .error
                .as_ref()
                .map_or(String::new(), |e| format!(" · {}", e.message))
        );
        self.status.set_label(&label);
        let text = state.value.as_ref().map_or_else(String::new, value_text);
        if common::buffer_text(&self.view.buffer()) != text {
            let offset = self.scroll.vadjustment().value();
            self.view.buffer().set_text(&text);
            self.scroll.vadjustment().set_value(offset);
        }
    }
}

pub fn value_text(value: &ResourceValue) -> String {
    match value {
        ResourceValue::Transcript(transcript) => {
            let mut text = if !transcript.available {
                "Transcript unavailable for this runtime/session.\n".into()
            } else if transcript.entries.is_empty() {
                "No transcript entries returned.\n".into()
            } else {
                String::new()
            };
            if transcript.truncated {
                text.push_str("Older entries omitted by the bounded read.\n\n");
            }
            for entry in &transcript.entries {
                text.push_str(&format!(
                    "{}{}\n{}\n\n",
                    entry.kind,
                    entry
                        .at
                        .map_or(String::new(), |at| format!(" · {}", common::timestamp(at))),
                    entry.text
                ));
            }
            text
        }
        ResourceValue::Diff(diff) => {
            if diff.text.is_empty() && diff.ok {
                "No changes returned.".into()
            } else {
                diff.text.clone()
            }
        }
        ResourceValue::Output(output) => format!(
            "{}\n\n{}",
            if output.alive {
                "Process running"
            } else {
                "Process ended · read-only"
            },
            output.text
        ),
        ResourceValue::Message(message) => {
            message.as_ref().map_or("Message unavailable".into(), |m| {
                format!(
                    "{} to {} · {}\n{}\n\n{}\n\nMetadata: {}",
                    m.from,
                    m.to,
                    m.kind,
                    common::timestamp(m.timestamp),
                    m.body,
                    m.metadata
                )
            })
        }
        ResourceValue::Event(event) => event.as_ref().map_or("Event not retained".into(), |e| {
            format!(
                "{} · {} · {}\nSequence {} · {}\n\n{}\n\n{}",
                e.level,
                e.source,
                e.event_type,
                e.seq,
                common::timestamp(e.timestamp),
                e.message,
                e.data
            )
        }),
        ResourceValue::TaskCard(card) => format!(
            "{} · {}\n{}\n\n{}\n\nResult: {}\nNote: {}\nDependencies:\n{}\nArtifacts:\n{}",
            card.task.id,
            card.task.title,
            card.task.status,
            card.task.description,
            card.task.result.as_deref().unwrap_or("None"),
            card.task.note.as_deref().unwrap_or("None"),
            card.dependencies
                .iter()
                .map(|t| format!("{} · {} · {}", t.id, t.status, t.title))
                .collect::<Vec<_>>()
                .join("\n"),
            card.task
                .artifacts
                .iter()
                .map(|a| format!("{}: {}\n{}", a.kind, a.value, a.description))
                .collect::<Vec<_>>()
                .join("\n\n")
        ),
        ResourceValue::Inbox(inbox) => inbox
            .messages
            .iter()
            .map(|e| {
                format!(
                    "{} to {} · {}\n{}\n{}\nDelivered: {:?}, acknowledged: {:?}",
                    e.message.from,
                    e.message.to,
                    e.message.kind,
                    common::timestamp(e.message.timestamp),
                    e.message.body,
                    e.delivered_at,
                    e.acked_at
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        ResourceValue::Models(models) => models
            .models
            .iter()
            .map(|m| {
                format!(
                    "{} · {} · {}\n{}{}",
                    m.name,
                    m.runtime,
                    m.price,
                    m.summary,
                    m.unavailable
                        .as_ref()
                        .map_or(String::new(), |r| format!("\nUnavailable: {r}"))
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        ResourceValue::History(items) => items
            .iter()
            .map(|m| {
                format!(
                    "{} · {} to {}\n{}",
                    common::timestamp(m.timestamp),
                    m.from,
                    m.to,
                    m.body
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        ResourceValue::Events(items) => items
            .iter()
            .map(|e| {
                format!(
                    "{} · {} · {}\n{}",
                    common::timestamp(e.timestamp),
                    e.source,
                    e.event_type,
                    e.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}
