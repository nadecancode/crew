//! Platform navigation identities, independent of workspace/domain records.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Destination {
    #[default]
    Team,
    Tasks,
    Messages,
    Events,
    Usage,
}

impl Destination {
    pub const ALL: [Self; 5] = [
        Self::Team,
        Self::Tasks,
        Self::Messages,
        Self::Events,
        Self::Usage,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Team => "team",
            Self::Tasks => "tasks",
            Self::Messages => "messages",
            Self::Events => "events",
            Self::Usage => "usage",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Team => "Team",
            Self::Tasks => "Tasks",
            Self::Messages => "Messages",
            Self::Events => "Events",
            Self::Usage => "Usage",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Team => "system-run-symbolic",
            Self::Tasks => "view-list-symbolic",
            Self::Messages => "mail-unread-symbolic",
            Self::Events => "view-list-bullet-symbolic",
            Self::Usage => "dialog-information-symbolic",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|item| *item == self)
            .expect("known destination")
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.id() == id)
    }
}
