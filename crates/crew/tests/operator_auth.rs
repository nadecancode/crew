//! Who may speak to the daemon as whom: raw protocol frames on the daemon's
//! socket, with no CLI in between, as an agent's own code could send them.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use common::*;
use crew_core::rpc::PROTOCOL_VERSION;
use crew_core::AgentStatus;
use serde_json::{json, Value};

/// One connection to the daemon, spoken to in raw JSON lines.
struct Raw {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Raw {
    fn connect(s: &Session) -> Self {
        let info: Value = serde_json::from_str(
            &std::fs::read_to_string(s.workspace.join(".crew/daemon.json")).unwrap(),
        )
        .unwrap();
        let stream = UnixStream::connect(info["socket"].as_str().unwrap()).unwrap();
        Self {
            writer: stream.try_clone().unwrap(),
            reader: BufReader::new(stream),
            next_id: 1,
        }
    }

    /// Send one call and return its reply frame (pushes are skipped).
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "id": id, "method": method, "params": params });
        writeln!(self.writer, "{request}").unwrap();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            let frame: Value = serde_json::from_str(&line).unwrap();
            if frame["id"] == id {
                return frame;
            }
        }
    }

    fn hello_operator(&mut self) -> Value {
        self.call(
            "hello",
            json!({ "role": "operator", "client": "test", "protocol": PROTOCOL_VERSION }),
        )
    }

    fn hello_agent(&mut self, s: &Session, agent: &str) -> Value {
        let identity: Value =
            serde_json::from_str(&std::fs::read_to_string(s.identity_path(agent)).unwrap())
                .unwrap();
        self.call(
            "hello",
            json!({
                "role": "agent",
                "agent_id": agent,
                "token": identity["token"],
                "client": "test",
                "protocol": PROTOCOL_VERSION,
            }),
        )
    }
}

fn error_code(frame: &Value) -> &str {
    frame["error"]["code"].as_str().unwrap_or("")
}

#[test]
fn second_hello_is_refused_for_operator_and_agent_and_authority_is_unchanged() {
    let s = Session::new();
    let team = s.team(
        "hello",
        vec![agent("a", "", &[]), agent("b", "", &[])],
        vec![],
    );
    s.run(&team);
    s.wait("the agents to come up", |snap| {
        snap.agents
            .iter()
            .all(|a| a.status != AgentStatus::Spawning)
    });

    // An agent cannot become the operator on its own connection...
    let mut as_agent = Raw::connect(&s);
    assert!(as_agent.hello_agent(&s, "a")["result"].is_object());
    let again = as_agent.hello_operator();
    assert_eq!(error_code(&again), "forbidden", "{again}");
    let message = again["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("already authenticated as agent 'a'"),
        "{message}"
    );
    // ...and is still only that agent afterwards.
    let kill = as_agent.call("agent_kill", json!({ "id": "b" }));
    assert_eq!(error_code(&kill), "forbidden", "{kill}");
    assert!(as_agent.call("agents_list", json!(null))["result"].is_array());
    assert!(find_agent(&s.snapshot(), "b").pid.is_some());

    // The operator's connection cannot turn into an agent's either, and
    // keeps its operator authority.
    let mut as_operator = Raw::connect(&s);
    assert!(as_operator.hello_operator()["result"].is_object());
    let again = as_operator.hello_agent(&s, "a");
    assert_eq!(error_code(&again), "forbidden", "{again}");
    let message = again["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("already authenticated as the operator"),
        "{message}"
    );
    let snapshot = as_operator.call("snapshot", json!({}));
    assert!(snapshot["result"]["agents"].is_array(), "{snapshot}");

    // A first hello that fails leaves the connection open to a valid one.
    let mut retry = Raw::connect(&s);
    let wrong = retry.call(
        "hello",
        json!({
            "role": "agent",
            "agent_id": "a",
            "token": "not-the-token",
            "client": "test",
            "protocol": PROTOCOL_VERSION,
        }),
    );
    assert_eq!(error_code(&wrong), "unauthorized", "{wrong}");
    assert!(retry.hello_agent(&s, "a")["result"].is_object());
}
