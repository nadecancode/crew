//! Default-deny network policy. A native key never becomes an operator token.

use crew_core::rpc::{Call, RpcError};
use crew_core::MessageType;
use crew_protocol::native::{Capability, ChannelPurpose};

pub(crate) fn authorize(
    call: &Call,
    purpose: ChannelPurpose,
    capabilities: &[Capability],
    attached: bool,
) -> Result<(), RpcError> {
    let denied = || RpcError::forbidden("operation is not available for this device grant");
    if matches!(call, Call::Ping) {
        return Ok(());
    }
    if matches!(call, Call::FeaturesNegotiate(_)) {
        let allowed = match purpose {
            ChannelPurpose::Workspace | ChannelPurpose::Read => {
                capabilities.contains(&Capability::Read)
            }
            ChannelPurpose::Action => capabilities.contains(&Capability::Control),
            ChannelPurpose::Index | ChannelPurpose::Terminal => false,
        };
        return allowed.then_some(()).ok_or_else(denied);
    }
    if purpose == ChannelPurpose::Index {
        return matches!(call, Call::Workspaces)
            .then_some(())
            .ok_or_else(denied);
    }
    if matches!(call, Call::Workspaces) {
        return Err(denied());
    }
    let need = match call {
        Call::EpicsList(_)
        | Call::EpicGet(_)
        | Call::TasksQuery(_)
        | Call::TaskGetScoped(_)
        | Call::AsideMembers(_)
        | Call::AsideList(_)
        | Call::AsideGet(_)
        | Call::AsideWatch(_)
        | Call::AsideUnwatch(_)
        | Call::DocsIndex(_)
        | Call::DocsRead(_)
        | Call::DocsAsset(_)
        | Call::DocsHistory(_)
        | Call::DocsDiff(_)
        | Call::DocsWatch(_)
        | Call::DocsUnwatch(_)
        | Call::UsageSummary(_)
        | Call::UsageSeries(_)
        | Call::UsageBreakdown(_)
        | Call::UsageObservations(_)
        | Call::UsageRuns(_)
        | Call::UsagePrices(_)
        | Call::UsageWatch(_)
        | Call::UsageUnwatch(_)
        | Call::Snapshot { .. }
        | Call::Subscribe
        | Call::AgentsList
        | Call::TasksList
        | Call::TaskGet { .. }
        | Call::History { .. }
        | Call::MessageGet { .. }
        | Call::Inbox { .. }
        | Call::Events { .. }
        | Call::AgentOutput { .. }
        | Call::AgentTranscript { .. }
        | Call::GitStatus { .. }
        | Call::GitDiff { .. } => Capability::Read,
        Call::AsideAsk(_)
        | Call::AsideCancel(_)
        | Call::AsideRemove(_)
        | Call::Send(_)
        | Call::Answer { .. }
        | Call::AgentPause { .. }
        | Call::AgentResume { .. }
        | Call::AgentCompact { .. }
        | Call::AgentRestart { .. }
        | Call::AgentRestartFresh { .. }
        | Call::AgentKill { .. }
        | Call::AgentSetModel { .. }
        | Call::Resume { .. }
        | Call::Shutdown { .. }
        | Call::ModelsList => Capability::Control,
        Call::AgentInput { .. }
        | Call::Attach { .. }
        | Call::AttachInput { .. }
        | Call::AttachResize { .. } => Capability::Terminal,
        _ => return Err(denied()),
    };
    if !capabilities.contains(&need) {
        return Err(denied());
    }
    let lane_ok = match purpose {
        ChannelPurpose::Index => false,
        ChannelPurpose::Workspace => need == Capability::Read,
        ChannelPurpose::Read => {
            need == Capability::Read
                && !matches!(
                    call,
                    Call::Subscribe
                        | Call::AsideWatch(_)
                        | Call::AsideUnwatch(_)
                        | Call::DocsWatch(_)
                        | Call::DocsUnwatch(_)
                        | Call::UsageWatch(_)
                        | Call::UsageUnwatch(_)
                )
        }
        ChannelPurpose::Action => {
            need == Capability::Control || matches!(call, Call::AgentInput { .. })
        }
        ChannelPurpose::Terminal => matches!(
            call,
            Call::Attach { .. } | Call::AttachInput { .. } | Call::AttachResize { .. }
        ),
    };
    if !lane_ok {
        return Err(denied());
    }
    if matches!(call, Call::Attach { .. }) && attached {
        return Err(RpcError::forbidden("terminal already attached"));
    }
    if matches!(call, Call::AttachInput { .. } | Call::AttachResize { .. }) && !attached {
        return Err(RpcError::invalid("terminal is not attached"));
    }
    match call {
        Call::TaskGetScoped(crew_core::rpc::epics::TaskGetScoped { id: None, .. })
        | Call::TaskGet { id: None }
        | Call::AgentCompact { id: None, .. }
        | Call::GitStatus { agent: None }
        | Call::GitDiff { agent: None, .. }
        | Call::Inbox { agent: None, .. } => {
            return Err(RpcError::invalid("name the resource explicitly"))
        }
        Call::Inbox { ack: true, .. } => return Err(denied()),
        Call::Shutdown { stop_agents: false } => return Err(denied()),
        Call::Send(p)
            if p.kind != MessageType::Message
                || p.id.is_none()
                || !(p.metadata.is_null()
                    || p.metadata.as_object().is_some_and(|m| m.is_empty())) =>
        {
            return Err(RpcError::invalid("invalid operator message"))
        }
        _ => {}
    }
    let range = |v: Option<usize>, max| v.is_none_or(|n| (1..=max).contains(&n));
    let bounded = match call {
        Call::Snapshot { messages, events } => range(*messages, 2000) && range(*events, 5000),
        Call::History { limit, .. } => range(*limit, 2000),
        Call::Events { limit, .. } => range(*limit, 5000),
        Call::AgentOutput { lines, .. } => range(*lines, 300),
        Call::AgentTranscript { limit, .. } => range(*limit, 400),
        Call::Inbox { limit, .. } => range(*limit, 500),
        Call::Send(p) => {
            p.body.len() <= 64 * 1024
                && p.id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty() && id.len() <= 128)
        }
        Call::Answer { body, .. } => body.len() <= 64 * 1024,
        Call::AgentInput { text, .. } => text.as_ref().is_none_or(|t| t.len() <= 64 * 1024),
        Call::AttachInput { data } => data.len() <= 4 * (64 * 1024usize).div_ceil(3),
        Call::Attach { rows, cols, .. } | Call::AttachResize { rows, cols } => {
            (1..=1000).contains(rows) && (1..=1000).contains(cols)
        }
        _ => true,
    };
    if !bounded {
        return Err(RpcError::too_large(
            "native request exceeds its resource limit",
        ));
    }
    Ok(())
}

pub(crate) fn allowed_actions(capabilities: &[Capability]) -> Vec<String> {
    let mut actions = Vec::new();
    if capabilities.contains(&Capability::Read) {
        actions.extend([
            "features_negotiate",
            "epics_list",
            "epic_get",
            "tasks_query",
            "task_get_scoped",
            "aside_members",
            "aside_list",
            "aside_get",
            "aside_watch",
            "aside_unwatch",
            "docs_index",
            "docs_read",
            "docs_asset",
            "docs_history",
            "docs_diff",
            "docs_watch",
            "docs_unwatch",
            "usage_summary",
            "usage_series",
            "usage_breakdown",
            "usage_observations",
            "usage_runs",
            "usage_prices",
            "usage_watch",
            "usage_unwatch",
            "snapshot",
            "subscribe",
            "agents_list",
            "tasks_list",
            "task_get",
            "history",
            "message_get",
            "inbox",
            "events",
            "agent_output",
            "agent_transcript",
            "git_status",
            "git_diff",
        ]);
    }
    if capabilities.contains(&Capability::Control) {
        actions.extend([
            "aside_ask",
            "aside_cancel",
            "aside_remove",
            "send",
            "answer",
            "agent_pause",
            "agent_resume",
            "agent_compact",
            "agent_restart",
            "agent_restart_fresh",
            "agent_kill",
            "agent_set_model",
            "resume",
            "shutdown",
            "models_list",
        ]);
    }
    if capabilities.contains(&Capability::Terminal) {
        actions.extend(["agent_input", "attach", "attach_input", "attach_resize"]);
    }
    actions.into_iter().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_core::rpc::SendParams;

    #[test]
    fn every_daemon_method_has_an_explicit_tier_and_lane_expectation() {
        let source = include_str!("../../../crew-core/src/rpc.rs");
        let getter = source
            .split("pub fn method(&self)")
            .nth(1)
            .unwrap()
            .split("pub struct Request")
            .next()
            .unwrap();
        let names = getter
            .lines()
            .filter_map(|line| {
                line.split("=> \"")
                    .nth(1)
                    .and_then(|tail| tail.split('"').next())
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            names.len(),
            105,
            "a new daemon method needs a deliberate gateway policy"
        );
        let reads = [
            "epics_list",
            "epic_get",
            "tasks_query",
            "task_get_scoped",
            "aside_members",
            "aside_list",
            "aside_get",
            "aside_watch",
            "aside_unwatch",
            "docs_index",
            "docs_read",
            "docs_asset",
            "docs_history",
            "docs_diff",
            "docs_watch",
            "docs_unwatch",
            "usage_summary",
            "usage_series",
            "usage_breakdown",
            "usage_observations",
            "usage_runs",
            "usage_prices",
            "usage_watch",
            "usage_unwatch",
            "snapshot",
            "subscribe",
            "agents_list",
            "tasks_list",
            "task_get",
            "history",
            "message_get",
            "inbox",
            "events",
            "agent_output",
            "agent_transcript",
            "git_status",
            "git_diff",
        ];
        let controls = [
            "aside_ask",
            "aside_cancel",
            "aside_remove",
            "send",
            "answer",
            "agent_pause",
            "agent_resume",
            "agent_compact",
            "agent_restart",
            "agent_restart_fresh",
            "agent_kill",
            "agent_set_model",
            "resume",
            "shutdown",
            "models_list",
        ];
        let terminals = ["agent_input", "attach", "attach_input", "attach_resize"];
        let denied = [
            // Epic administration stays outside every native device grant.
            "epic_create",
            "epic_edit",
            "task_create_scoped",
            "task_revise",
            "epic_complete",
            "hello",
            "load_team",
            "agent_add",
            "agent_remove",
            "agent_focus",
            "agent_spawn",
            "usage_status",
            "listen",
            "delivered",
            "credential",
            "ack",
            "status_update",
            "decline",
            "progress",
            "task_create",
            "task_edit",
            "task_assign",
            "task_complete",
            "task_fail",
            "task_retry",
            "review_request",
            "review_submit",
            "session_complete",
            "git_commit",
            "git_cherry_pick",
            "git_merge",
            "integrate",
            "hook",
            "web_open",
            "web_close",
            "web_pair",
            "web_prefs",
            "web_rescan",
            "summary",
            "web_attach",
            "web_event",
            // Device grants do not include toolbox administration or proxy credentials.
            "toolbox",
            "toolbox_check",
            "toolbox_reload",
            "toolbox_report",
            "toolbox_server",
        ];
        let assigned = reads
            .iter()
            .chain(controls.iter())
            .chain(terminals.iter())
            .chain(denied.iter())
            .copied()
            .chain(["ping", "workspaces", "features_negotiate"])
            .collect::<Vec<_>>();
        assert_eq!(assigned.len(), 105);
        assert_eq!(
            assigned
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            names
        );
        let unit = [
            "ping",
            "subscribe",
            "agents_list",
            "tasks_list",
            "models_list",
            "usage_status",
            "listen",
            "credential",
            "integrate",
            "workspaces",
            "web_rescan",
        ];
        let feature_requests: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../../crew-core/tests/fixtures/features/requests-v1.json"
        ))
        .unwrap();
        for name in names {
            let params = if unit.contains(&name) {
                serde_json::json!({})
            } else if name == "toolbox_server" {
                serde_json::json!({"name":"fixture","preflight":true})
            } else if name == "toolbox_report" {
                serde_json::json!({"name":"fixture","kind":"listing_differs","detail":"upstream changed"})
            } else {
                serde_json::json!({
                    "id":"a","agent":"a","agent_id":"a","role":"operator","protocol":6,"client":"fixture","config":{"project":"native-policy"},"spec":{"id":"a"},"status":"working","percent":10,"body":"x","to":"a","type":"message","task":"work","task_id":"t","reason":"x","artifact":"x","comments":"","approve":true,"summary":"","decisions":[],"message":"m","commit":"abc123","from_agent":"a","event":"x","ids":["m"],"code":"a","kind":"paired","count":1,"url":"http://localhost:1","ack":false,"data":"","rows":24,"cols":80,"model":"","note":null,"text":"x","submit":true,"stop_agents":true
                })
            };
            // Strict feature params cannot use the legacy catch-all object.
            let params = feature_requests
                .iter()
                .find(|r| r["method"].as_str() == Some(name))
                .map(|r| r["params"].clone())
                .unwrap_or(params);
            let request = serde_json::from_value::<crew_core::rpc::Request>(
                serde_json::json!({"id":1,"method":name,"params":params}),
            )
            .unwrap_or_else(|e| panic!("fixture {name}: {e}"));
            assert_eq!(request.call.method(), name);
            for capabilities in [
                vec![Capability::Read],
                vec![Capability::Read, Capability::Control],
                vec![Capability::Read, Capability::Terminal],
                vec![Capability::Read, Capability::Control, Capability::Terminal],
            ] {
                for purpose in [
                    ChannelPurpose::Index,
                    ChannelPurpose::Workspace,
                    ChannelPurpose::Read,
                    ChannelPurpose::Action,
                    ChannelPurpose::Terminal,
                ] {
                    let expected = if name == "ping" {
                        true
                    } else if name == "workspaces" {
                        purpose == ChannelPurpose::Index
                    } else if name == "features_negotiate" {
                        matches!(purpose, ChannelPurpose::Workspace | ChannelPurpose::Read)
                            || (purpose == ChannelPurpose::Action
                                && capabilities.contains(&Capability::Control))
                    } else if reads.contains(&name) {
                        matches!(purpose, ChannelPurpose::Workspace | ChannelPurpose::Read)
                            && (![
                                "subscribe",
                                "aside_watch",
                                "aside_unwatch",
                                "docs_watch",
                                "docs_unwatch",
                                "usage_watch",
                                "usage_unwatch",
                            ]
                            .contains(&name)
                                || purpose == ChannelPurpose::Workspace)
                    } else if controls.contains(&name) {
                        purpose == ChannelPurpose::Action
                            && capabilities.contains(&Capability::Control)
                    } else if terminals.contains(&name) {
                        capabilities.contains(&Capability::Terminal)
                            && if name == "agent_input" {
                                purpose == ChannelPurpose::Action
                            } else {
                                purpose == ChannelPurpose::Terminal
                            }
                    } else {
                        false
                    };
                    let attached = matches!(name, "attach_input" | "attach_resize");
                    assert_eq!(
                        authorize(&request.call, purpose, &capabilities, attached).is_ok(),
                        expected,
                        "{name} {purpose:?} {capabilities:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn messages_answers_and_terminal_require_their_own_grants() {
        let read = [Capability::Read];
        let control = [Capability::Read, Capability::Control];
        let terminal = [Capability::Read, Capability::Terminal];
        let send = Call::Send(SendParams {
            id: Some("m-1".into()),
            to: "a".into(),
            body: "work".into(),
            ..Default::default()
        });
        assert!(authorize(&send, ChannelPurpose::Action, &read, false).is_err());
        assert!(authorize(&send, ChannelPurpose::Action, &control, false).is_ok());
        assert!(authorize(
            &Call::AgentInput {
                id: "a".into(),
                text: Some("hi".into()),
                submit: true
            },
            ChannelPurpose::Action,
            &control,
            false
        )
        .is_err());
        assert!(authorize(
            &Call::AgentInput {
                id: "a".into(),
                text: Some("hi".into()),
                submit: true
            },
            ChannelPurpose::Action,
            &terminal,
            false
        )
        .is_ok());
        assert!(authorize(
            &Call::Summary { watch: true },
            ChannelPurpose::Workspace,
            &control,
            false
        )
        .is_err());
    }

    #[test]
    fn read_parameters_and_purpose_cannot_change_authority() {
        let all = [Capability::Read, Capability::Control, Capability::Terminal];
        assert!(authorize(
            &Call::GitStatus { agent: None },
            ChannelPurpose::Read,
            &all,
            false
        )
        .is_err());
        assert!(authorize(
            &Call::GitStatus {
                agent: Some("a".into())
            },
            ChannelPurpose::Read,
            &all,
            false
        )
        .is_ok());
        assert!(authorize(
            &Call::Inbox {
                agent: Some("a".into()),
                unread_only: true,
                ack: true,
                limit: None
            },
            ChannelPurpose::Read,
            &all,
            false
        )
        .is_err());
        assert!(authorize(
            &Call::Snapshot {
                messages: Some(2001),
                events: None
            },
            ChannelPurpose::Workspace,
            &all,
            false
        )
        .is_err());
        assert!(authorize(&Call::Workspaces, ChannelPurpose::Workspace, &all, false).is_err());
        assert!(authorize(
            &Call::AttachInput { data: "".into() },
            ChannelPurpose::Terminal,
            &all,
            false
        )
        .is_err());
        assert!(authorize(
            &Call::AttachInput { data: "".into() },
            ChannelPurpose::Terminal,
            &all,
            true
        )
        .is_ok());
    }
}
