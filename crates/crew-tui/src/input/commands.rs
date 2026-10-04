use crew_core::types::BROADCAST;

use crate::app::{App, Command, Modal, View};
use crate::slash::{self, SlashCommand};

use super::{open_questions, resume_paused};

/// Run a slash command typed into the message box, which was addressed to
/// `to`. `Err` is why nothing was run; the box then stays open to say so.
pub(super) fn run_slash(
    app: &mut App,
    name: &str,
    rest: &str,
    to: &str,
    now: i64,
) -> Result<Vec<Command>, String> {
    let command = slash::find(slash::DASHBOARD, name).ok_or_else(|| slash::unknown(name))?;
    if command.args.is_empty() && !rest.is_empty() {
        return Err(slash::no_argument(command));
    }
    match command.name {
        "exit" => return Ok(vec![Command::Quit]),
        "stop" => app.modal = Some(Modal::ConfirmStop),
        "kill" => app.modal = Some(Modal::ConfirmKill(slash_agent(app, command, rest, to)?)),
        "restart" => return Ok(vec![Command::Restart(slash_agent(app, command, rest, to)?)]),
        "restart-fresh" => {
            app.modal = Some(Modal::ConfirmRestartFresh(slash_agent(
                app, command, rest, to,
            )?));
        }
        "pause" => return Ok(vec![Command::Pause(slash_agent(app, command, rest, to)?)]),
        "unpause" => return Ok(vec![Command::Resume(slash_agent(app, command, rest, to)?)]),
        "compact" => return Ok(vec![Command::Compact(slash_agent(app, command, rest, to)?)]),
        "model" => {
            // Nobody named and nobody selected: the orchestrator.
            let id = match slash_agent(app, command, rest, to) {
                Ok(id) => id,
                Err(e) if rest.is_empty() => app.orchestrator().map(|a| a.id.clone()).ok_or(e)?,
                Err(e) => return Err(e),
            };
            return Ok(app.open_model_picker(&id, now));
        }
        "resume" => return Ok(resume_paused(app, now)),
        "answer" => open_questions(app, now),
        "tasks" => app.view = View::Tasks,
        "logs" => app.view = View::Logs,
        "mouse" => {
            let on = !app.mouse;
            let note = if on {
                "mouse on: click an agent or a message, wheel scrolls (shift-drag selects text)"
            } else {
                "mouse off: drag selects text as usual; /mouse turns it back on"
            };
            app.set_status(note, false, now);
            return Ok(vec![Command::SetMouse(on)]);
        }
        "help" => app.help = true,
        other => return Err(slash::unknown(other)),
    }
    Ok(Vec::new())
}

/// The agent a command is about: the one named, else the one the message
/// box was addressed to, else the selected one.
fn slash_agent(app: &App, command: &SlashCommand, rest: &str, to: &str) -> Result<String, String> {
    if rest.is_empty() {
        return Some(to)
            .filter(|to| *to != BROADCAST)
            .map(str::to_string)
            .or_else(|| app.selected.clone())
            .ok_or_else(|| format!("name the agent: {}", command.usage()));
    }
    if app.agents.iter().any(|a| a.id == rest) {
        return Ok(rest.to_string());
    }
    let ids: Vec<&str> = app.agents.iter().map(|a| a.id.as_str()).collect();
    Err(format!(
        "no agent named {rest} (there are: {})",
        ids.join(", ")
    ))
}
