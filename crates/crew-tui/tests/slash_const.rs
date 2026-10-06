//! Compile the existing public menu from a downstream const context. This
//! consumer is also compiled by the declared Rust 1.82 compatibility gate.

use crew_tui::slash::{SlashCommand, DASHBOARD};

const fn menu() -> &'static [SlashCommand] {
    DASHBOARD
}

const FIRST_NAME: &str = menu()[0].name;

#[test]
fn dashboard_remains_available_to_a_downstream_const_consumer() {
    assert_eq!(FIRST_NAME, "exit");
    assert_eq!(menu().len(), 15);
}
