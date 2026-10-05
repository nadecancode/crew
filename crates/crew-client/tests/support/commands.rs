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
