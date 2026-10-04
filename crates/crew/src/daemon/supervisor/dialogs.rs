//! Startup dialogs on an agent's screen (workspace trust, notices): answered or reported.

use crew_core::AgentStatus;

use crate::agents::{runtime_for, Signal};
use crate::daemon::Daemon;
use crate::persistence::NewEvent;

impl Daemon {
    /// Check a starting session for a dialog, and resume start-up once a
    /// dialog that was blocking it is gone.
    pub(crate) async fn check_startup_dialog(&self, id: &str) {
        let (ready, was_blocked) = {
            let mut model = self.model();
            let was_blocked = model
                .agent(id)
                .is_some_and(|a| a.block_kind.as_deref() == Some("dialog"));
            (model.rt(id).ready, was_blocked)
        };
        if ready && !was_blocked {
            return;
        }
        let blocked = self.dialog_blocks(id).await;
        if was_blocked && !blocked {
            // The dialog was answered; pick up where start-up left off.
            let (has_prompt, turn_active) = {
                let mut model = self.model();
                let rt = model.rt(id);
                (rt.pending_prompt.is_some(), rt.turn_active)
            };
            if has_prompt {
                self.on_signal(id, Signal::Ready).await;
            } else if turn_active {
                self.update_agent(id, |a| {
                    if a.status == AgentStatus::Spawning {
                        a.status = AgentStatus::Working;
                        a.activity.clear();
                    }
                });
            }
        }
    }

    /// Dismiss a menu on `screen` that needs no answer (Codex's "extra
    /// thought" notice), so it holds no keys and blocks nothing. Returns
    /// whether there was one.
    pub(crate) async fn dismiss_notice(&self, id: &str, screen: &str) -> bool {
        let Some(agent) = self.agent(id) else {
            return false;
        };
        let Some(notice) = runtime_for(agent.runtime)
            .detect_dialog(screen)
            .filter(|d| d.kind == "notice")
        else {
            return false;
        };
        let Ok(procs) = self.procs() else {
            return false;
        };
        if procs.send_keys(id, &notice.accept_keys).await.is_ok() {
            self.emit(
                NewEvent::new(
                    "daemon",
                    "agent.notice_dismissed",
                    format!("{id}: dismissed — {}", notice.description),
                )
                .agent(id),
            );
        }
        true
    }

    /// Look for a blocking dialog on the agent's screen. Returns true when the
    /// session must not receive input yet.
    pub(super) async fn dialog_blocks(&self, id: &str) -> bool {
        // One dialog handler per agent at a time: two of them answering the
        // same dialog would press its keys twice and pick the wrong option.
        let lock = self.delivery_lock(&format!("dialog:{id}"));
        let _guard = lock.lock().await;
        let Some(agent) = self.agent(id) else {
            return true;
        };
        let Ok(procs) = self.procs() else { return true };
        let runtime = runtime_for(agent.runtime);
        let auto_trust = self
            .model()
            .local
            .get(id)
            .map(|l| l.settings.auto_trust)
            .unwrap_or(false);
        for attempt in 0..4 {
            let screen = procs.visible(id, 80).await.unwrap_or_default();
            let Some(dialog) = runtime.detect_dialog(&screen) else {
                let was_reported = std::mem::take(&mut self.model().rt(id).dialog_reported);
                if was_reported {
                    self.update_agent(id, |a| {
                        if a.block_kind.as_deref() == Some("dialog") {
                            a.block_kind = None;
                            a.status = AgentStatus::Spawning;
                            a.activity = "starting".into();
                        }
                    });
                }
                return false;
            };
            match dialog.accept_keys.as_slice() {
                keys if dialog.kind == "notice" && !keys.is_empty() && attempt < 3 => {
                    if attempt == 0 {
                        self.emit(
                            NewEvent::new(
                                "daemon",
                                "agent.notice_dismissed",
                                format!("{id}: dismissed — {}", dialog.description),
                            )
                            .agent(id),
                        );
                    }
                    for key in keys {
                        let _ = procs.send_keys(id, &[key]).await;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                }
                keys if auto_trust && dialog.kind == "trust" && !keys.is_empty() && attempt < 3 => {
                    if attempt == 0 {
                        self.emit(
                            NewEvent::new(
                                "daemon",
                                "agent.dialog_accepted",
                                format!(
                                    "{id}: accepted the workspace-trust dialog (auto_trust is on)"
                                ),
                            )
                            .agent(id),
                        );
                    }
                    for key in keys {
                        let _ = procs.send_keys(id, &[key]).await;
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                }
                _ => {
                    let already = std::mem::replace(&mut self.model().rt(id).dialog_reported, true);
                    if !already {
                        self.update_agent(id, |a| {
                            a.status = AgentStatus::Blocked;
                            a.block_kind = Some("dialog".into());
                            a.activity = dialog.description.clone();
                        });
                        self.emit(
                            NewEvent::new(
                                "daemon",
                                "agent.dialog",
                                format!(
                                    "{id}: {}. Answer it in the agent's terminal, or set \
defaults.auto_trust: true in the team file.",
                                    dialog.description
                                ),
                            )
                            .agent(id)
                            .warn(),
                        );
                    }
                    return true;
                }
            }
        }
        true
    }
}
