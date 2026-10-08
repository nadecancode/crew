//! Operator commands: everything the human runs.

use std::path::{Path, PathBuf};

use anyhow::Result;
use crew_core::client::Client;
use crew_core::paths::{self, DaemonInfo};

mod agents;
mod catalog;
mod comms;
mod native;
mod plain;
mod session;
mod team;
mod toolbox;
mod web;
#[cfg(test)]
pub(super) use agents::restart_call;
pub use agents::{agent, task};
pub use catalog::{config, models, style};
pub use comms::{answer, integrate, logs, messages, questions, send};
pub use native::{native, NativeCommand};
pub use plain::{quick, refuse_command};
pub use session::{attach, frame, resume, sessions, start, status, stop};
pub use team::run_team;
pub use toolbox::{toolbox, toolbox_check, toolbox_reload};
pub use web::{web, web_list, web_remove, web_stop, WebJoin};

/// Which session a command addresses.
pub struct Target {
    pub dir: Option<PathBuf>,
    pub project: Option<String>,
}

impl Target {
    fn discover(&self) -> Result<DaemonInfo> {
        paths::discover(self.dir.as_deref(), self.project.as_deref())
    }

    fn connect(&self, client: &str) -> Result<(Client, DaemonInfo)> {
        let info = self.discover()?;
        let (c, _) = Client::operator(Path::new(&info.socket), client)?;
        Ok((c, info))
    }
}
