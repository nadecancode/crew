use std::rc::Rc;

use crew_client::api::*;
use gtk::prelude::*;

use super::common::{self, EntityList, Select};

#[derive(Clone)]
pub struct EventsView {
    pub root: gtk::Box,
    list: EntityList,
    empty: gtk::ScrolledWindow,
    older: gtk::Button,
}

impl EventsView {
    pub fn new(select: Select, filter: Rc<dyn Fn(EventFilter)>, older: Rc<dyn Fn()>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let controls = gtk::FlowBox::new();
        controls.set_selection_mode(gtk::SelectionMode::None);
        controls.set_min_children_per_line(1);
        controls.set_max_children_per_line(3);
        let search = gtk::SearchEntry::new();
        search.set_hexpand(true);
        search.set_placeholder_text(Some("Filter loaded events"));
        search.update_property(&[gtk::accessible::Property::Label("Filter loaded events")]);
        let vcs = gtk::CheckButton::with_label("VCS only");
        let agent = gtk::Entry::builder()
            .placeholder_text("Agent filter")
            .build();
        agent.update_property(&[gtk::accessible::Property::Label("Event agent filter")]);
        let level = gtk::ComboBoxText::new();
        level.append(Some(""), "All levels");
        for value in crew_protocol::EventLevel::ALL {
            level.append(Some(value.as_str()), value.as_str());
        }
        level.set_active(Some(0));
        level.update_property(&[gtk::accessible::Property::Label("Event level filter")]);
        let changed: Rc<dyn Fn()> = Rc::new({
            let search = search.downgrade();
            let vcs = vcs.downgrade();
            let agent = agent.downgrade();
            let level = level.downgrade();
            move || {
                let (Some(search), Some(vcs), Some(agent), Some(level)) = (
                    search.upgrade(),
                    vcs.upgrade(),
                    agent.upgrade(),
                    level.upgrade(),
                ) else {
                    return;
                };
                filter(EventFilter {
                    agent_id: (!agent.text().is_empty()).then(|| agent.text().to_string()),
                    level: level.active_id().and_then(|id| id.parse().ok()),
                    text: search.text().to_string(),
                    vcs_only: vcs.is_active(),
                });
            }
        });
        search.connect_search_changed({
            let changed = changed.clone();
            move |_| changed()
        });
        vcs.connect_toggled({
            let changed = changed.clone();
            move |_| changed()
        });
        agent.connect_changed({
            let changed = changed.clone();
            move |_| changed()
        });
        level.connect_changed(move |_| changed());
        search.set_margin_start(24);
        search.set_margin_end(24);
        root.append(&search);
        super::add_control(&controls, &agent);
        super::add_control(&controls, &level);
        super::add_control(&controls, &vcs);
        controls.set_margin_start(24);
        controls.set_margin_end(24);
        controls.set_column_spacing(8);
        controls.set_row_spacing(8);
        root.append(&controls);
        let list = EntityList::new("Events", select);
        let empty = super::empty_state(
            "view-list-bullet-symbolic",
            "No events in this view",
            "Filters apply to loaded history. Older evidence may still be available.",
        );
        root.append(list.widget());
        root.append(&empty);
        let older_button = gtk::Button::with_label("Load older");
        older_button.set_tooltip_text(Some(
            "Loads more from the host, up to this client's bounded history",
        ));
        older_button.set_halign(gtk::Align::Start);
        older_button.set_margin_top(12);
        older_button.set_margin_bottom(12);
        older_button.set_margin_start(24);
        older_button.connect_clicked(move |_| older());
        root.append(&older_button);
        Self {
            root,
            list,
            empty,
            older: older_button,
        }
    }
    /// A bounded host read is under way; its progress stays on this page.
    pub fn loading_older(&self) {
        self.older.set_label("Loading older…");
        self.older.set_sensitive(false);
    }
    pub fn update(&self, scope: Option<&ClientScope>, page: &EventPage, live: bool) {
        self.older.set_label("Load older");
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
                .filter_map(|event| {
                    let seq = u64::try_from(event.seq).ok()?;
                    let body = common::row();
                    let (level, tone) = level_marker(event.level);
                    body.append(&common::row_title(
                        &event.event_type,
                        Some(common::status_chip(level, tone)),
                    ));
                    let message = common::text(&event.message);
                    message.set_lines(3);
                    message.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    body.append(&message);
                    body.append(&common::muted(&format!(
                        "{} · {} · #{}",
                        event.agent_id.as_deref().unwrap_or("Team"),
                        common::timestamp(event.timestamp),
                        event.seq
                    )));
                    Some((
                        EntityKey {
                            scope: scope.clone(),
                            entity: Entity::Event(seq),
                        },
                        body.upcast(),
                        format!(
                            "{}, {}, {}, {}",
                            event.seq, event.level, event.event_type, event.message
                        ),
                    ))
                })
                .collect(),
        );
    }
}

/// Each level reads as a word as well as a colour.
fn level_marker(level: crew_protocol::EventLevel) -> (&'static str, Tone) {
    use crew_protocol::EventLevel;
    match level {
        EventLevel::Debug => ("Debug", Tone::Neutral),
        EventLevel::Info => ("Info", Tone::Info),
        EventLevel::Warn => ("Warning", Tone::Warning),
        EventLevel::Error => ("Error", Tone::Danger),
    }
}
