//! The TUI's public DASHBOARD remains usable from a Rust 1.82 const consumer.
//! This adapter projects the existing four fields without another catalogue.

use crew_client::policy::{web_policy, COMMANDS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SlashCommand {
    name: &'static str,
    aliases: &'static [&'static str],
    args: &'static str,
    about: &'static str,
}

const fn command(index: usize) -> SlashCommand {
    let descriptor = &COMMANDS[index];
    SlashCommand {
        name: descriptor.name,
        aliases: descriptor.aliases,
        args: descriptor.args,
        about: descriptor.about,
    }
}

const DASHBOARD: &[SlashCommand] = &[
    command(0),
    command(1),
    command(2),
    command(3),
    command(4),
    command(5),
    command(6),
    command(7),
    command(8),
    command(9),
    command(10),
    command(11),
    command(12),
    command(13),
    command(14),
];

#[test]
fn const_dashboard_projects_the_same_live_descriptor_fields_in_order() {
    assert_eq!(DASHBOARD.len(), 15);
    let policy = web_policy();
    for (index, command) in DASHBOARD.iter().enumerate() {
        let live = &policy.commands[index];
        assert_eq!(
            (command.name, command.aliases, command.args, command.about),
            (live.name, live.aliases, live.args, live.about)
        );
    }
    assert_eq!(DASHBOARD[0].name, "exit");
    assert_eq!(DASHBOARD[14].name, "help");
    assert_eq!(policy.commands.len(), 18);
}
