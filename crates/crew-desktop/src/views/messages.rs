use std::rc::Rc;

use crew_client::api::*;
use gtk::prelude::*;

use super::common::{self, EntityList, Select};

#[derive(Clone)]
pub struct MessagesView {
    pub root: gtk::Box,
    list: EntityList,
    empty: gtk::ScrolledWindow,
    older: gtk::Button,
    agent: gtk::Entry,
    peer: gtk::Entry,
}

impl MessagesView {
    pub fn new(select: Select, filter: Rc<dyn Fn(MessageFilter)>, older: Rc<dyn Fn()>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Filter loaded messages"));
        search.update_property(&[gtk::accessible::Property::Label("Filter loaded messages")]);
        let agent = gtk::Entry::builder()
            .placeholder_text("Agent endpoint")
            .build();
        let peer = gtk::Entry::builder()
            .placeholder_text("Other endpoint (pair)")
            .build();
        agent.update_property(&[gtk::accessible::Property::Label(
            "Messages from or to agent",
        )]);
        peer.update_property(&[gtk::accessible::Property::Label(
            "Other agent for pair filter",
        )]);
        let kind = gtk::ComboBoxText::new();
        kind.append(Some(""), "All message types");
        for value in crew_protocol::MessageType::ALL {
            kind.append(Some(value.as_str()), value.as_str());
        }
        kind.set_active(Some(0));
        kind.update_property(&[gtk::accessible::Property::Label("Message type filter")]);
        let changed: Rc<dyn Fn()> = Rc::new({
            let agent = agent.downgrade();
            let peer = peer.downgrade();
            let kind = kind.downgrade();
            let search = search.downgrade();
            move || {
                let (Some(agent), Some(peer), Some(kind), Some(search)) = (
                    agent.upgrade(),
                    peer.upgrade(),
                    kind.upgrade(),
                    search.upgrade(),
                ) else {
                    return;
                };
                let first = agent.text().to_string();
                let second = peer.text().to_string();
                let endpoints = if first.is_empty() {
                    MessageEndpoints::All
                } else if second.is_empty() {
                    MessageEndpoints::Agent { id: first }
                } else {
                    MessageEndpoints::Pair { first, second }
                };
                filter(MessageFilter {
                    endpoints,
                    kind: kind.active_id().and_then(|id| id.parse().ok()),
                    text: search.text().to_string(),
                });
            }
        });
        search.connect_search_changed({
            let changed = changed.clone();
            move |_| changed()
        });
        agent.connect_changed({
            let changed = changed.clone();
            move |_| changed()
        });
        peer.connect_changed({
            let changed = changed.clone();
            move |_| changed()
        });
        kind.connect_changed(move |_| changed());
        root.append(&search);
        let controls = gtk::FlowBox::new();
        controls.set_selection_mode(gtk::SelectionMode::None);
        controls.set_min_children_per_line(1);
        controls.set_max_children_per_line(3);
        super::add_control(&controls, &agent);
        super::add_control(&controls, &peer);
        super::add_control(&controls, &kind);
        root.append(&controls);
        let list = EntityList::new("Messages", select);
        let empty = super::empty_state(
            "mail-unread-symbolic",
            "No messages in this view",
            "Filters apply to loaded history. Older messages may still be available.",
        );
        root.append(list.widget());
        root.append(&empty);
        let older_button = gtk::Button::with_label("Load older");
        older_button.connect_clicked(move |_| older());
        root.append(&older_button);
        Self {
            root,
            list,
            empty,
            older: older_button,
            agent,
            peer,
        }
    }
    pub fn set_pair(&self, first: &str, second: &str) {
        self.agent.set_text(first);
        self.peer.set_text(second);
    }
    pub fn update(&self, scope: Option<&ClientScope>, page: &MessagePage, live: bool) {
        self.older
            .set_sensitive(live && (page.has_older_local || page.may_have_older_remote));
        self.empty.set_visible(page.items.is_empty());
        self.list.root.set_visible(!page.items.is_empty());
        let Some(scope) = scope else {
            self.list.replace(Vec::new());
            return;
        };
        self.list.replace(
            page.items
                .iter()
                .map(|message| {
                    let body = common::row();
                    body.append(&common::row_title(
                        &format!("{} to {}", message.from, message.to),
                        None,
                    ));
                    let preview = common::text(&message.body);
                    preview.set_lines(3);
                    preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    body.append(&preview);
                    body.append(&common::muted(&format!(
                        "{} · {}",
                        message.kind,
                        common::timestamp(message.timestamp)
                    )));
                    (
                        EntityKey {
                            scope: scope.clone(),
                            entity: Entity::Message(message.id.clone()),
                        },
                        body.upcast(),
                        format!(
                            "{} to {}, {}, {}",
                            message.from, message.to, message.kind, message.body
                        ),
                    )
                })
                .collect(),
        );
    }
}
