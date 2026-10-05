//! Byte freshness and compatibility for the only generated browser module.

use crew_client::policy::{web_policy, CommandPolicy, WebCommandKind};
use serde_json::{json, Value};

#[path = "../examples/generate_web_policy.rs"]
mod generator;

const COMMITTED: &str = include_str!("../../../web/src/generated/client-policy.ts");

#[test]
fn committed_browser_policy_is_byte_equal_to_live_runtime_descriptor_emission() {
    let emitted = generator::render(web_policy()).unwrap();
    assert_eq!(emitted, COMMITTED, "regenerate with cargo run --locked -p crew-client --example generate_web_policy > web/src/generated/client-policy.ts");
    assert_eq!(generator::render(web_policy()).unwrap(), emitted);
    assert!(
        !emitted.contains("RETRY")
            && !emitted.contains("MAX_MESSAGE")
            && !emitted.contains("STATUSES")
    );
}

#[test]
fn descriptor_changes_make_the_committed_module_stale_without_an_exporter_table_change() {
    let baseline = web_policy();
    let mut methods = baseline.method_timeouts.to_vec();
    methods
        .iter_mut()
        .find(|method| method.kind == Some(crew_client::CommandKind::AgentKill))
        .unwrap()
        .timeout_ms += 1;
    let changed = crew_client::policy::WebPolicy {
        method_timeouts: Box::leak(methods.into_boxed_slice()),
        ..baseline
    };
    assert_ne!(generator::render(changed).unwrap(), COMMITTED);
    let mut commands = baseline.commands.to_vec();
    commands[0].about = "updated runtime help";
    let changed = crew_client::policy::WebPolicy {
        commands: Box::leak(commands.into_boxed_slice()),
        ..baseline
    };
    assert_ne!(generator::render(changed).unwrap(), COMMITTED);
}

#[test]
fn command_descriptors_preserve_existing_wire_fixture_and_browser_only_metadata() {
    let policy = web_policy();
    let baseline: Vec<Value> =
        serde_json::from_str(include_str!("../../crew/tests/fixtures/web/commands.json")).unwrap();
    for (command, old) in policy.commands.iter().take(baseline.len()).zip(&baseline) {
        assert_eq!(
            json!({"name":command.name,"about":command.about,"args":command.args,"aliases":command.aliases}),
            *old
        );
    }
    assert_eq!(policy.commands.len(), baseline.len() + 3);
    assert_eq!(
        policy.commands[15],
        CommandPolicy {
            name: "usage",
            about: "show usage: context windows, tokens, quotas",
            args: "",
            aliases: &[],
            web: Some(WebCommandKind::New)
        }
    );
    assert_eq!(
        policy.commands[16],
        CommandPolicy {
            name: "graph",
            about: "show the agent graph; reset forgets dragged positions",
            args: "[reset]",
            aliases: &[],
            web: Some(WebCommandKind::New)
        }
    );
    assert_eq!(
        policy.commands[17],
        CommandPolicy {
            name: "keys",
            about: "turn single-key shortcuts on or off",
            args: "on|off",
            aliases: &[],
            web: Some(WebCommandKind::New)
        }
    );
    assert_eq!(policy.commands[0].web, Some(WebCommandKind::Declined));
    assert_eq!(policy.commands[13].web, Some(WebCommandKind::Declined));
}

#[test]
fn emission_rejects_duplicate_names_missing_fallback_and_inexact_javascript_numbers() {
    let baseline = web_policy();
    let mut commands = baseline.commands.to_vec();
    commands[1].aliases = &["EXIT"];
    assert!(generator::render(crew_client::policy::WebPolicy {
        commands: Box::leak(commands.into_boxed_slice()),
        ..baseline
    })
    .is_err());
    let mut methods = baseline.method_timeouts.to_vec();
    methods.push(methods[0]);
    assert!(generator::render(crew_client::policy::WebPolicy {
        method_timeouts: Box::leak(methods.into_boxed_slice()),
        ..baseline
    })
    .is_err());
    let methods = baseline
        .method_timeouts
        .iter()
        .copied()
        .filter(|method| method.kind.is_some())
        .collect::<Vec<_>>();
    assert!(generator::render(crew_client::policy::WebPolicy {
        method_timeouts: Box::leak(methods.into_boxed_slice()),
        ..baseline
    })
    .is_err());
    for value in [0, 1 << 53, u64::MAX] {
        let mut methods = baseline.method_timeouts.to_vec();
        methods[0].timeout_ms = value;
        assert!(generator::render(crew_client::policy::WebPolicy {
            method_timeouts: Box::leak(methods.into_boxed_slice()),
            ..baseline
        })
        .is_err());
    }
}

#[test]
fn descriptor_text_is_json_quoted_data_and_sparse_slots_preserve_annotation() {
    let baseline = web_policy();
    let mut commands = baseline.commands.to_vec();
    commands[0].about = "\"; throw new Error('not executable');\n账目\\";
    let changed = crew_client::policy::WebPolicy {
        commands: Box::leak(commands.into_boxed_slice()),
        ..baseline
    };
    let emitted = generator::render(changed).unwrap();
    assert!(emitted.contains(&serde_json::to_string(changed.commands[0].about).unwrap()));
    assert!(emitted.contains("undefined, [\"quit\", \"q\"], \"declined\"]"));
    assert!(emitted.contains("undefined, undefined, \"new\"]"));
    assert!(emitted.contains("] as CommandTuple[]).map("));
}
