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
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // Read progress and failures stay in this tab; the window status line
        // is for mutations only.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bar.set_margin_top(12);
        bar.set_margin_bottom(12);
        bar.set_margin_start(24);
        bar.set_margin_end(24);
        let status = common::muted("Not loaded");
        status.set_hexpand(true);
        status.set_valign(gtk::Align::Center);
        bar.append(&status);
        let refresh_button = gtk::Button::with_label("Refresh");
        refresh_button.set_valign(gtk::Align::Center);
        refresh_button.connect_clicked(move |_| refresh());
        bar.append(&refresh_button);
        root.append(&bar);
        let links = gtk::Box::new(gtk::Orientation::Vertical, 4);
        links.set_margin_start(24);
        links.set_margin_end(24);
        root.append(&links);
        let view = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(true)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(24)
            .right_margin(24)
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
        let label = status_text(state);
        if self.status.label() != label {
            self.status.set_label(&label);
        }
        if state.status == ResourceStatus::Failed {
            self.status.add_css_class("error");
        } else {
            self.status.remove_css_class("error");
        }
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
                let mut text = format!(
                    "{} to {} · {}\n{}\n\n{}",
                    m.from,
                    m.to,
                    m.kind,
                    common::timestamp(m.timestamp),
                    m.body,
                );
                if let Some(metadata) = extras(m.metadata.to_string()) {
                    text.push_str(&format!("\n\nMetadata: {metadata}"));
                }
                text
            })
        }
        ResourceValue::Event(event) => event.as_ref().map_or("Event not retained".into(), |e| {
            let mut text = format!(
                "{} · {} · {}\nSequence {} · {}\n\n{}",
                e.level,
                e.source,
                e.event_type,
                e.seq,
                common::timestamp(e.timestamp),
                e.message,
            );
            if let Some(data) = extras(e.data.to_string()) {
                text.push_str(&format!("\n\n{data}"));
            }
            text
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
                    "{} to {} · {}\n{}\n{}\n{}",
                    e.message.from,
                    e.message.to,
                    e.message.kind,
                    common::timestamp(e.message.timestamp),
                    e.message.body,
                    match (e.delivered_at, e.acked_at) {
                        (None, _) => "Not delivered yet".to_owned(),
                        (Some(at), None) => format!("Delivered {}", common::timestamp(at)),
                        (Some(at), Some(read)) => format!(
                            "Delivered {} · acknowledged {}",
                            common::timestamp(at),
                            common::timestamp(read)
                        ),
                    }
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

/// A structured value's text, or nothing when it holds nothing: `null`, an
/// empty object, list or string.
fn extras(text: String) -> Option<String> {
    (!matches!(text.as_str(), "null" | "{}" | "[]" | "\"\"")).then_some(text)
}

fn noun(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::TaskCard => "task card",
        ResourceKind::Message => "message",
        ResourceKind::Event => "event",
        ResourceKind::Transcript => "transcript",
        ResourceKind::Diff => "diff",
        ResourceKind::Output => "host screen",
        ResourceKind::Inbox => "inbox",
        ResourceKind::Models => "model list",
        ResourceKind::History => "message history",
        ResourceKind::Events => "event history",
    }
}

/// The read's state in words, with the source's own reason where it gave one.
pub fn status_text(state: &ResourceState) -> String {
    let noun = noun(state.key.kind);
    let phrase = match state.status {
        ResourceStatus::Idle => "Not loaded".to_owned(),
        ResourceStatus::Loading if state.value.is_some() => format!("Refreshing the {noun}…"),
        ResourceStatus::Loading => format!("Loading the {noun}…"),
        ResourceStatus::Ready => format!("Loaded {noun}"),
        ResourceStatus::Empty => format!("No {noun} to show"),
        ResourceStatus::Unsupported => format!("The {noun} is not available here"),
        ResourceStatus::Failed => format!("Could not load the {noun}"),
        ResourceStatus::Stale => format!("Last known {noun} · refresh when live"),
    };
    match &state.error {
        Some(error) if !error.message.is_empty() => format!("{phrase}\n{}", error.message),
        _ => phrase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_message_metadata_is_left_out() {
        let message = |metadata| {
            ResourceValue::Message(Some(crew_protocol::Message {
                id: "m-1".into(),
                from: "claude-1".into(),
                to: "operator".into(),
                kind: crew_protocol::MessageType::Message,
                body: "Done".into(),
                timestamp: 0,
                correlation_id: None,
                reply_to: None,
                metadata,
            }))
        };
        for empty in [
            serde_json::Value::Null,
            serde_json::json!({}),
            serde_json::json!([]),
        ] {
            let text = value_text(&message(empty));
            assert!(
                text.ends_with("Done") && !text.contains("Metadata"),
                "{text}"
            );
        }
        let text = value_text(&message(serde_json::json!({"task": "billing"})));
        assert!(text.ends_with("Metadata: {\"task\":\"billing\"}"), "{text}");
    }
}
