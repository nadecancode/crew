use crew_client::{policy::*, CommandKind, ConfirmationKind, GrantTier};
use crew_protocol::{
    AgentStatus, MAX_FRAME_BYTES, MAX_MESSAGE_BYTES, MAX_OPTION_CHARS, MAX_QUESTION_OPTIONS,
};

#[test]
fn policy_covers_the_closed_catalogue_once_and_preserves_current_timeout_fallback() {
    for kind in CommandKind::ALL {
        assert_eq!(
            METHODS
                .iter()
                .filter(|policy| policy.kind == Some(*kind))
                .count(),
            1
        );
        let expected = match kind {
            CommandKind::AgentKill
            | CommandKind::AgentRestart
            | CommandKind::AgentRestartFresh
            | CommandKind::AgentResume
            | CommandKind::AgentSetModel => 30_000,
            CommandKind::Resume | CommandKind::Shutdown => 90_000,
            CommandKind::ModelsList => 20_000,
            _ => 3_000,
        };
        assert_eq!(timeout_for(kind.method()), expected);
    }
    assert_eq!(METHODS.len(), CommandKind::ALL.len() + 1);
    for method in [
        "",
        "unknown",
        "__proto__",
        "constructor",
        "toString",
        "AGENT_KILL",
    ] {
        assert_eq!(timeout_for(method), 3_000);
    }
}

#[test]
fn descriptors_are_borrowed_authority_with_four_confirmations_and_one_replay_class() {
    let exported = web_policy();
    assert!(std::ptr::eq(exported.method_timeouts, METHODS));
    assert!(std::ptr::eq(exported.commands, COMMANDS));
    assert!(std::ptr::eq(exported.statuses, STATUSES));
    let confirmations: Vec<_> = METHODS
        .iter()
        .filter_map(|policy| match policy.confirmation {
            ConfirmationRule::Always(kind) | ConfirmationRule::RestartFlag(kind) => Some(kind),
            ConfirmationRule::None => None,
        })
        .collect();
    assert_eq!(
        confirmations,
        [
            ConfirmationKind::RestartFresh,
            ConfirmationKind::KillAgent,
            ConfirmationKind::RestartLimited,
            ConfirmationKind::StopTeam
        ]
    );
    let replays: Vec<_> = METHODS
        .iter()
        .filter(|policy| policy.replay != ReplayPolicy::Never)
        .map(|policy| policy.kind)
        .collect();
    assert_eq!(replays, [Some(CommandKind::Send)]);
    assert_eq!(
        method_policy(CommandKind::ModelsList).gateway.tier(),
        Some(GrantTier::Control)
    );
    assert_eq!(
        method_policy(CommandKind::AgentFocus).gateway,
        GatewayPermission::Denied
    );
    for kind in [
        CommandKind::AgentInput,
        CommandKind::Attach,
        CommandKind::AttachInput,
        CommandKind::AttachResize,
    ] {
        assert_eq!(
            method_policy(kind).gateway.tier(),
            Some(GrantTier::Terminal)
        );
    }
}

#[test]
fn slash_and_identifier_rules_match_actual_core_utilities_including_unknown_names() {
    for text in [
        "/kill claude-2",
        "/Stop now",
        "/unknown arg",
        "/exit\nand more",
        "/?",
        " /stop",
        "/src/main.rs slow",
        "/2fast",
        "/",
        "//stop",
        "/stop.",
        "résumé",
        "/Stop\u{a0}  a",
    ] {
        assert_eq!(
            slash_command(text),
            crew_core::util::slash_command(text),
            "{text:?}"
        );
    }
    for id in [
        "",
        "a",
        "a_1-b",
        "orchestrator",
        "A",
        "1x",
        "é",
        "a.b",
        "a:b",
        "a/",
        " a",
    ] {
        assert_eq!(is_valid_id(id), crew_core::util::is_valid_id(id), "{id:?}");
    }
    assert!(is_valid_id(&"a".repeat(32)));
    assert!(!is_valid_id(&"a".repeat(33)));
    assert!(is_valid_message_id("w-A012_a.b:c"));
    assert!(is_valid_message_id(&"a".repeat(64)));
    for id in ["", "m/abc", "é", "m abc"] {
        assert!(!is_valid_message_id(id));
    }
    assert!(!is_valid_message_id(&"a".repeat(65)));
    assert_eq!(command_policy("CLI").unwrap().name, "model");
    assert_eq!(command_policy("?").unwrap().name, "help");
    assert!(command_policy("unknown").is_none());
    assert_eq!(COMMANDS.len(), 18);
}

#[test]
fn bounds_match_protocol_and_native_configuration_rejects_zero_and_above_maximum() {
    assert_eq!(VALIDATION_LIMITS.message_bytes, MAX_MESSAGE_BYTES);
    assert_eq!(VALIDATION_LIMITS.frame_bytes, MAX_FRAME_BYTES);
    assert_eq!(VALIDATION_LIMITS.question_options, MAX_QUESTION_OPTIONS);
    assert_eq!(VALIDATION_LIMITS.option_chars, MAX_OPTION_CHARS);
    validate_limits(NATIVE_LIMITS).unwrap();
    for limits in [
        crew_client::ClientLimits {
            pending_operations: 0,
            ..NATIVE_LIMITS
        },
        crew_client::ClientLimits {
            messages: NATIVE_LIMITS.messages + 1,
            ..NATIVE_LIMITS
        },
        crew_client::ClientLimits {
            events: u32::MAX,
            ..NATIVE_LIMITS
        },
        crew_client::ClientLimits {
            page_size: 0,
            ..NATIVE_LIMITS
        },
    ] {
        assert!(validate_limits(limits).is_err());
    }
    assert!(!status_policy(AgentStatus::Done).terminal());
    for status in AgentStatus::ALL {
        assert_eq!(status_policy(*status).terminal(), status.is_terminal());
    }
}
