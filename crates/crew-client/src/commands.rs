//! Protocol 6 encoding for the closed application command catalogue.
//!
//! Hosts add framing and correlate envelopes. Successful payloads are decoded
//! against the originating command, rather than inferred from their shape.

use crew_protocol::{Agent, MAX_FRAME_BYTES};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::{ClientError, Command, CommandResult};

#[derive(Serialize)]
struct WithParams<'a, T> {
    id: u64,
    method: &'a str,
    params: T,
}

fn encode<T: Serialize>(id: u64, method: &str, params: T) -> Result<String, ClientError> {
    serde_json::to_string(&WithParams { id, method, params }).map_err(encode_error)
}

fn encode_error(error: serde_json::Error) -> ClientError {
    ClientError::InvalidInput {
        field: "command".into(),
        reason: error.to_string(),
    }
}

macro_rules! params {
    ($id:expr, $method:expr, $($field:ident: $ty:ty = $value:expr),+ $(,)?) => {{
        #[derive(Serialize)]
        struct Params<'a> { $($field: $ty),+ }
        encode($id, $method, Params { $($field: $value),+ })
    }};
}

impl Command {
    /// JSON record without a trailing newline. Parameter order matches the
    /// host Request serializer, including null optional fields and defaults.
    pub fn encode(&self, request_id: u64) -> Result<String, ClientError> {
        let method = self.kind().method();
        let bytes = match self {
            Self::Ping
            | Self::Subscribe
            | Self::Workspaces
            | Self::AgentsList
            | Self::TasksList
            | Self::ModelsList => {
                #[derive(Serialize)]
                struct Empty<'a> {
                    id: u64,
                    method: &'a str,
                }
                serde_json::to_string(&Empty {
                    id: request_id,
                    method,
                })
                .map_err(encode_error)
            }
            Self::Snapshot { messages, events } => params!(request_id, method,
                messages: &'a Option<u32> = messages, events: &'a Option<u32> = events),
            Self::TaskGet { id } => params!(request_id, method, id: &'a Option<String> = id),
            Self::History { agent, limit } | Self::Events { agent, limit } => {
                params!(request_id, method,
                agent: &'a Option<String> = agent, limit: &'a Option<u32> = limit)
            }
            Self::Inbox { agent, limit } => params!(request_id, method,
                agent: &'a Option<String> = agent, unread_only: &'a bool = &false,
                ack: &'a bool = &false, limit: &'a Option<u32> = limit),
            Self::MessageGet { id }
            | Self::AgentPause { id }
            | Self::AgentResume { id }
            | Self::AgentRestart { id }
            | Self::AgentRestartFresh { id }
            | Self::AgentKill { id }
            | Self::AgentFocus { id } => params!(request_id, method,
                id: &'a str = id),
            Self::AgentOutput {
                id,
                lines,
                screen,
                ansi,
            } => params!(request_id, method,
                id: &'a str = id, lines: &'a Option<u32> = lines,
                screen: &'a bool = screen, ansi: &'a bool = ansi),
            Self::AgentTranscript { id, limit } => params!(request_id, method,
                id: &'a str = id, limit: &'a Option<u32> = limit),
            Self::GitDiff { against, agent } => params!(request_id, method,
                against: &'a Option<String> = against, agent: &'a Option<String> = agent),
            Self::Send(params) => encode(request_id, method, params),
            Self::Answer { id, body } => params!(request_id, method,
                id: &'a str = id, body: &'a str = body),
            Self::AgentCompact { id, keep } => params!(request_id, method,
                id: &'a Option<String> = id, keep: &'a Option<String> = keep),
            Self::AgentSetModel {
                id,
                model,
                runtime,
                note,
            } => params!(request_id, method,
                id: &'a str = id, model: &'a str = model,
                runtime: &'a Option<crew_protocol::RuntimeKind> = runtime, note: &'a Option<String> = note),
            Self::AgentInput { id, text, submit } => params!(request_id, method,
                id: &'a str = id, text: &'a Option<String> = text, submit: &'a bool = submit),
            Self::Resume { agents, restart } => params!(request_id, method,
                agents: &'a [String] = agents, restart: &'a bool = restart),
            Self::Shutdown { stop_agents } => params!(request_id, method,
                stop_agents: &'a bool = stop_agents),
            Self::Attach { agent, rows, cols } => params!(request_id, method,
                agent: &'a str = agent, rows: &'a u16 = rows, cols: &'a u16 = cols),
            Self::AttachInput { data } => params!(request_id, method, data: &'a str = data),
            Self::AttachResize { rows, cols } => params!(request_id, method,
                rows: &'a u16 = rows, cols: &'a u16 = cols),
        }?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(ClientError::TooLarge {
                actual: bytes.len() as u64,
                limit: MAX_FRAME_BYTES as u64,
            });
        }
        Ok(bytes)
    }

    /// Decode a successful result payload. RpcError, request correlation and
    /// unknown push handling belong to the driver, not this method.
    pub fn decode_result(&self, value: Value) -> Result<CommandResult, ClientError> {
        let method = self.kind().method();
        macro_rules! payload {
            ($variant:ident) => {
                decode(value, method).map(CommandResult::$variant)
            };
        }
        match self {
            Self::Ping => true_ack(value, method, "pong").map(|()| CommandResult::Pong),
            Self::Subscribe => {
                true_ack(value, method, "subscribed").map(|()| CommandResult::Subscribed)
            }
            Self::Shutdown { .. } => {
                true_ack(value, method, "stopping").map(|()| CommandResult::Stopping)
            }
            Self::AgentFocus { .. } | Self::AgentInput { .. } => {
                true_ack(value, method, "ok").map(|()| CommandResult::Ok)
            }
            Self::Attach { agent, .. } => {
                #[derive(Deserialize)]
                struct Attached {
                    attached: String,
                }
                let result: Attached = decode(value, method)?;
                if &result.attached != agent {
                    return Err(invalid(method, "attachment acknowledged a different agent"));
                }
                Ok(CommandResult::Attached {
                    agent: result.attached,
                })
            }
            Self::AttachInput { .. } | Self::AttachResize { .. } => Err(invalid(
                method,
                "successful replies are suppressed for this method",
            )),
            Self::Snapshot { .. } => payload!(Snapshot),
            Self::Workspaces => payload!(Workspaces),
            Self::AgentsList => payload!(Agents),
            Self::TasksList => payload!(Tasks),
            Self::TaskGet { .. } => payload!(TaskCard),
            Self::History { .. } => payload!(Messages),
            Self::MessageGet { .. } => payload!(Message),
            Self::Events { .. } => payload!(Events),
            Self::Inbox { .. } => payload!(Inbox),
            Self::AgentOutput { .. } => payload!(Output),
            Self::AgentTranscript { .. } => payload!(Transcript),
            Self::GitDiff { .. } => payload!(Diff),
            Self::ModelsList => payload!(Models),
            Self::Send(_) | Self::Answer { .. } => payload!(Sent),
            Self::AgentPause { .. }
            | Self::AgentResume { .. }
            | Self::AgentRestart { .. }
            | Self::AgentRestartFresh { .. }
            | Self::AgentKill { .. }
            | Self::AgentSetModel { .. } => payload!(Agent),
            Self::AgentCompact { .. } => {
                let agent: Agent = decode(value, method)?;
                Ok(CommandResult::Agent(Some(Box::new(agent))))
            }
            Self::Resume { .. } => {
                #[derive(Deserialize)]
                struct Resumed {
                    resumed: Vec<String>,
                }
                let result: Resumed = decode(value, method)?;
                Ok(CommandResult::Resumed {
                    agents: result.resumed,
                })
            }
        }
    }
}

fn decode<T: DeserializeOwned>(value: Value, method: &str) -> Result<T, ClientError> {
    serde_json::from_value(value).map_err(|error| invalid(method, &error.to_string()))
}

fn true_ack(value: Value, method: &str, field: &str) -> Result<(), ClientError> {
    if value.as_object().and_then(|object| object.get(field)) == Some(&Value::Bool(true)) {
        Ok(())
    } else {
        Err(invalid(method, &format!("expected {field}: true")))
    }
}

fn invalid(method: &str, message: &str) -> ClientError {
    ClientError::DecodeFailure {
        message: format!("{method}: {message}"),
    }
}
