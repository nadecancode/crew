use crew_client::{Command, CommandKind};
use crew_protocol::rpc::SendParams;
use crew_protocol::RuntimeKind;
use serde_json::{json, Value};

// Exhaustive: adding a catalogue variant without a constructor is a compile
// error. Defaults and integer/optional boundaries are supplied independently.
pub fn construct(kind: CommandKind, boundary: bool) -> Command {
    let id = "claude-1".to_string();
    let optional = boundary.then(|| "main".to_string());
    let count = boundary.then_some(u32::MAX);
    match kind {
        CommandKind::FeaturesNegotiate => Command::FeaturesNegotiate(
            serde_json::from_value(feature_params(kind.method())).unwrap(),
        ),
        CommandKind::EpicsList => {
            Command::EpicsList(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::EpicGet => {
            Command::EpicGet(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::TasksQuery => {
            Command::TasksQuery(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::TaskGetScoped => {
            Command::TaskGetScoped(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::AsideMembers => {
            Command::AsideMembers(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::AsideAsk => {
            Command::AsideAsk(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::AsideCancel => {
            Command::AsideCancel(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::AsideRemove => {
            Command::AsideRemove(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::AsideList => {
            Command::AsideList(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::AsideGet => {
            Command::AsideGet(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::AsideWatch => {
            Command::AsideWatch(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::AsideUnwatch => {
            Command::AsideUnwatch(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::DocsIndex => {
            Command::DocsIndex(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::DocsRead => {
            Command::DocsRead(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::DocsAsset => {
            Command::DocsAsset(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::DocsHistory => {
            Command::DocsHistory(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::DocsDiff => {
            Command::DocsDiff(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::DocsWatch => {
            Command::DocsWatch(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::DocsUnwatch => {
            Command::DocsUnwatch(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::UsageSummary => {
            Command::UsageSummary(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::UsageSeries => {
            Command::UsageSeries(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::UsageBreakdown => {
            Command::UsageBreakdown(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::UsageObservations => Command::UsageObservations(
            serde_json::from_value(feature_params(kind.method())).unwrap(),
        ),
        CommandKind::UsageRuns => {
            Command::UsageRuns(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::UsagePrices => {
            Command::UsagePrices(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::UsageWatch => {
            Command::UsageWatch(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::UsageUnwatch => {
            Command::UsageUnwatch(serde_json::from_value(feature_params(kind.method())).unwrap())
        }
        CommandKind::Ping => Command::Ping,
        CommandKind::Snapshot => Command::Snapshot {
            messages: count,
            events: count,
        },
        CommandKind::Subscribe => Command::Subscribe,
        CommandKind::Workspaces => Command::Workspaces,
        CommandKind::AgentsList => Command::AgentsList,
        CommandKind::TasksList => Command::TasksList,
        CommandKind::TaskGet => Command::TaskGet { id: optional },
        CommandKind::History => Command::History {
            agent: optional,
            limit: count,
        },
        CommandKind::MessageGet => Command::MessageGet {
            id: "m-0104".into(),
        },
        CommandKind::Events => Command::Events {
            agent: optional,
            limit: count,
        },
        CommandKind::Inbox => Command::Inbox {
            agent: optional,
            limit: count,
        },
        CommandKind::AgentOutput => Command::AgentOutput {
            id,
            lines: count,
            screen: boundary,
            ansi: boundary,
        },
        CommandKind::AgentTranscript => Command::AgentTranscript { id, limit: count },
        CommandKind::GitDiff => Command::GitDiff {
            against: optional.clone(),
            agent: optional,
        },
        CommandKind::ModelsList => Command::ModelsList,
        CommandKind::Send => Command::Send(SendParams {
            to: id,
            body: "  résumé\n账目\u{0}  ".into(),
            id: boundary.then(|| "w-0102030405060708".into()),
            correlation_id: optional.clone(),
            reply_to: optional,
            metadata: if boundary {
                json!({"integer":u64::MAX, "empty":{}, "nested":[null,0]})
            } else {
                Value::Null
            },
            ..SendParams::default()
        }),
        CommandKind::Answer => Command::Answer {
            id: "m-9001".into(),
            body: "  Use the adapter  ".into(),
        },
        CommandKind::AgentPause => Command::AgentPause { id },
        CommandKind::AgentResume => Command::AgentResume { id },
        CommandKind::AgentCompact => Command::AgentCompact {
            id: optional.clone(),
            keep: optional,
        },
        CommandKind::AgentRestart => Command::AgentRestart { id },
        CommandKind::AgentRestartFresh => Command::AgentRestartFresh { id },
        CommandKind::AgentKill => Command::AgentKill { id },
        CommandKind::AgentSetModel => Command::AgentSetModel {
            id,
            model: if boundary {
                "opus".into()
            } else {
                String::new()
            },
            runtime: boundary.then_some(RuntimeKind::Claude),
            note: optional,
        },
        CommandKind::AgentFocus => Command::AgentFocus { id },
        CommandKind::AgentInput => Command::AgentInput {
            id,
            text: boundary.then(|| "\r\u{1b}[200~text\u{1b}[201~".into()),
            submit: boundary,
        },
        CommandKind::Resume => Command::Resume {
            agents: if boundary { vec![id] } else { vec![] },
            restart: boundary,
        },
        CommandKind::Shutdown => Command::Shutdown {
            stop_agents: boundary,
        },
        CommandKind::Attach => Command::Attach {
            agent: id,
            rows: if boundary { u16::MAX } else { 0 },
            cols: if boundary { u16::MAX } else { 0 },
        },
        CommandKind::AttachInput => Command::AttachInput {
            data: if boundary {
                "ABuA/w==".into()
            } else {
                String::new()
            },
        },
        CommandKind::AttachResize => Command::AttachResize {
            rows: if boundary { u16::MAX } else { 0 },
            cols: if boundary { u16::MAX } else { 0 },
        },
    }
}

fn feature_params(method: &str) -> Value {
    let fixtures: Vec<Value> = serde_json::from_str(include_str!(
        "../../../crew-core/tests/fixtures/features/requests-v1.json"
    ))
    .unwrap();
    fixtures
        .into_iter()
        .find(|v| v["method"] == method)
        .unwrap()["params"]
        .clone()
}
