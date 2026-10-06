//! Slash commands: what a line starting with `/` means in the launcher's
//! objective box and in the dashboard's message box.
//!
//! Such a line is never an objective or a message. It runs the command it
//! names, or — when there is no such command — stays in the box with a note,
//! so `/exit` typed out of habit cannot start a team or land in an agent's
//! conversation.

pub use crew_client::policy::slash_command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// The argument, as shown in the usage: `[agent]`, `<name>`, or nothing.
    pub args: &'static str,
    pub about: &'static str,
}

impl SlashCommand {
    pub fn usage(&self) -> String {
        if self.args.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.args)
        }
    }
}

const fn command(
    name: &'static str,
    aliases: &'static [&'static str],
    args: &'static str,
    about: &'static str,
) -> SlashCommand {
    SlashCommand {
        name,
        aliases,
        args,
        about,
    }
}

// Membership/order belongs to the terminal UI. Descriptions, arguments and
// aliases come from the live shared policy descriptors.
macro_rules! shared_command {
    ($index:expr) => {{
        let descriptor = &crew_client::policy::COMMANDS[$index];
        SlashCommand {
            name: descriptor.name,
            aliases: descriptor.aliases,
            args: descriptor.args,
            about: descriptor.about,
        }
    }};
}

/// Commands of the dashboard's message box.
pub static DASHBOARD: &[SlashCommand] = &[
    shared_command!(0),
    shared_command!(1),
    shared_command!(2),
    shared_command!(3),
    shared_command!(4),
    shared_command!(5),
    shared_command!(6),
    shared_command!(7),
    shared_command!(8),
    shared_command!(9),
    shared_command!(10),
    shared_command!(11),
    shared_command!(12),
    shared_command!(13),
    shared_command!(14),
];

/// Commands of the launcher's objective box.
pub const LAUNCHER: &[SlashCommand] = &[
    command(
        "exit",
        &["quit", "q", "cancel", "stop"],
        "",
        "leave without starting a team",
    ),
    command("style", &[], "<name>", "pick a work style by name"),
    command("help", &["?"], "", "list these commands"),
];

/// The command called `name` (or one of its aliases), ignoring case.
pub fn find(commands: &'static [SlashCommand], name: &str) -> Option<&'static SlashCommand> {
    let name = name.to_ascii_lowercase();
    commands
        .iter()
        .find(|c| c.name == name || c.aliases.contains(&name.as_str()))
}

/// The partly typed command name, while `text` is still just `/` and a word.
fn typing(text: &str) -> Option<&str> {
    let word = text.strip_prefix('/')?;
    (!word.contains(char::is_whitespace)).then_some(word)
}

/// Commands whose name starts with what was typed.
fn candidates(commands: &'static [SlashCommand], word: &str) -> Vec<&'static SlashCommand> {
    let word = word.to_ascii_lowercase();
    commands
        .iter()
        .filter(|c| c.name.starts_with(&word))
        .collect()
}

/// What Tab turns a partly typed command into: the first command it could
/// become, with a space when an argument may follow.
pub fn complete(commands: &'static [SlashCommand], text: &str) -> Option<String> {
    let word = typing(text)?;
    let command = find(commands, word)
        .filter(|c| c.name.eq_ignore_ascii_case(word))
        .or_else(|| candidates(commands, word).first().copied())?;
    Some(if command.args.is_empty() {
        format!("/{}", command.name)
    } else {
        format!("/{} ", command.name)
    })
}

/// The line shown under the input while a command is being typed; `None`
/// when `text` is not a command.
pub fn hint(commands: &'static [SlashCommand], text: &str) -> Option<String> {
    if !text.starts_with('/') {
        return None;
    }
    let describe = |c: &SlashCommand| format!("{}  {}", c.usage(), c.about);
    let Some(word) = typing(text) else {
        // An argument is being typed: describe the command it goes to.
        let (name, _) = slash_command(text)?;
        return Some(match find(commands, name) {
            Some(command) => describe(command),
            None => unknown(name),
        });
    };
    if let Some(command) = find(commands, word) {
        return Some(describe(command));
    }
    Some(match candidates(commands, word).as_slice() {
        [] => unknown(word),
        [only] => format!("{} · tab completes", describe(only)),
        many => many
            .iter()
            .map(|c| c.usage())
            .collect::<Vec<_>>()
            .join(" · "),
    })
}

/// Why a line that names no command was not sent.
pub fn unknown(name: &str) -> String {
    format!("no command /{name} — /help lists them (start with a space to send it as text)")
}

/// Why a command was not run with the argument it got.
pub fn no_argument(command: &SlashCommand) -> String {
    format!("/{} takes no argument", command.name)
}

/// The usages of `commands`, packed into rows of at most `width` columns.
pub fn usage_rows(commands: &[SlashCommand], width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for command in commands {
        let usage = command.usage();
        match rows.last_mut() {
            Some(row) if row.chars().count() + 3 + usage.chars().count() <= width => {
                row.push_str(" · ");
                row.push_str(&usage);
            }
            _ => rows.push(usage),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_membership_and_order_remain_the_terminal_contract() {
        assert_eq!(
            DASHBOARD
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            [
                "exit",
                "stop",
                "kill",
                "restart",
                "restart-fresh",
                "pause",
                "unpause",
                "compact",
                "model",
                "resume",
                "answer",
                "tasks",
                "logs",
                "mouse",
                "help"
            ]
        );
    }

    #[test]
    fn commands_are_found_by_name_or_alias() {
        assert_eq!(find(DASHBOARD, "exit").map(|c| c.name), Some("exit"));
        assert_eq!(find(DASHBOARD, "QUIT").map(|c| c.name), Some("exit"));
        assert_eq!(find(DASHBOARD, "terminate").map(|c| c.name), Some("stop"));
        assert_eq!(find(DASHBOARD, "?").map(|c| c.name), Some("help"));
        assert!(find(DASHBOARD, "ex").is_none(), "a prefix is not a command");
        assert!(find(DASHBOARD, "clear").is_none());
        // In the launcher there is nothing to stop yet; it just leaves.
        assert_eq!(find(LAUNCHER, "stop").map(|c| c.name), Some("exit"));
        // Names and aliases are unique within a table.
        for table in [DASHBOARD, LAUNCHER] {
            let mut names: Vec<&str> = table
                .iter()
                .flat_map(|c| std::iter::once(c.name).chain(c.aliases.iter().copied()))
                .collect();
            let total = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), total);
        }
    }

    #[test]
    fn tab_completes_and_the_hint_follows_the_typing() {
        assert_eq!(complete(DASHBOARD, "/st").as_deref(), Some("/stop"));
        assert_eq!(complete(DASHBOARD, "/k").as_deref(), Some("/kill "));
        assert_eq!(complete(DASHBOARD, "/re").as_deref(), Some("/restart "));
        assert_eq!(complete(DASHBOARD, "/resume").as_deref(), Some("/resume"));
        assert_eq!(complete(DASHBOARD, "/").as_deref(), Some("/exit"));
        assert_eq!(complete(DASHBOARD, "/zz"), None);
        assert_eq!(
            complete(DASHBOARD, "/kill cl"),
            None,
            "arguments are not completed"
        );
        assert_eq!(complete(DASHBOARD, "hello"), None);

        assert_eq!(hint(DASHBOARD, "hello"), None);
        assert_eq!(hint(DASHBOARD, " /exit"), None);
        let all = hint(DASHBOARD, "/").unwrap();
        assert!(all.starts_with("/exit · /stop · /kill [agent]"), "{all}");
        assert_eq!(
            hint(DASHBOARD, "/re").as_deref(),
            Some("/restart [agent] · /restart-fresh [agent] · /resume")
        );
        assert_eq!(
            hint(DASHBOARD, "/sto").as_deref(),
            Some("/stop  stop the team: every agent and the daemon (asks first) · tab completes")
        );
        assert_eq!(
            hint(DASHBOARD, "/quit").as_deref(),
            Some("/exit  leave the dashboard — the team keeps running")
        );
        assert_eq!(
            hint(DASHBOARD, "/kill claude-2").as_deref(),
            Some("/kill [agent]  stop one agent's process (asks first)")
        );
        assert!(hint(DASHBOARD, "/clear")
            .unwrap()
            .starts_with("no command /clear"));
        assert!(hint(DASHBOARD, "/clear now")
            .unwrap()
            .starts_with("no command /clear"));
    }

    #[test]
    fn usages_pack_into_rows() {
        let rows = usage_rows(DASHBOARD, 48);
        assert!(rows.len() > 1);
        assert!(rows.iter().all(|r| r.chars().count() <= 48), "{rows:?}");
        assert_eq!(rows[0], "/exit · /stop · /kill [agent] · /restart [agent]");
        let joined = rows.join(" · ");
        for command in DASHBOARD {
            assert!(joined.contains(&command.usage()), "{joined}");
        }
    }
}
