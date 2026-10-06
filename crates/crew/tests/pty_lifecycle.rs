//! Terminal EOF against a real daemon and a scripted agent on an actual PTY.
mod common;

use base64::Engine;
use common::*;
use crew_core::client::Client;
use crew_core::rpc::{Call, Push};
use crew_core::AgentStatus;
use std::time::{Duration, Instant};

fn terminal_until_closed(client: &mut Client) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut screen = String::new();
    loop {
        assert!(Instant::now() < deadline, "terminal did not announce EOF");
        match client
            .next_frame()
            .expect("terminal frame before deadline")
            .push
        {
            Some(Push::Pty { data }) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap();
                screen.push_str(&String::from_utf8_lossy(&bytes));
            }
            Some(Push::PtyClosed) => return screen,
            _ => {}
        }
    }
}

#[test]
fn confirmed_kill_closes_the_terminal_and_late_attach_replays_the_retained_screen() {
    if test_support::isolate_operator_test(
        "confirmed_kill_closes_the_terminal_and_late_attach_replays_the_retained_screen",
    ) {
        return;
    }
    let session = Session::new();
    session.run(&session.team("pty-lifecycle", vec![agent("a", "worker", &[])], vec![]));
    session.wait("scripted worker ready", |snapshot| {
        find_agent(snapshot, "a").status != AgentStatus::Spawning
    });
    let id = crew_core::paths::session_id(&session.workspace.join(".crew"));
    let socket = session.root().join("run/crew").join(format!("{id}.sock"));
    let (mut terminal, _) = Client::operator(&socket, "pty-lifecycle").unwrap();
    terminal.set_timeout(Some(Duration::from_secs(5))).unwrap();
    terminal
        .call_value(Call::Attach {
            agent: "a".into(),
            rows: 24,
            cols: 80,
        })
        .unwrap();
    // Both operations are one-way on the existing daemon wire.
    terminal
        .send(Call::AttachResize {
            rows: 30,
            cols: 100,
        })
        .unwrap();
    terminal
        .send(Call::AttachInput {
            data: base64::engine::general_purpose::STANDARD.encode("terminal-retained-marker"),
        })
        .unwrap();
    let mut live = String::new();
    while !live.contains("terminal-retained-marker") {
        match terminal.next_frame().unwrap().push {
            Some(Push::Pty { data }) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap();
                live.push_str(&String::from_utf8_lossy(&bytes));
            }
            Some(Push::PtyClosed) => panic!("terminal ended before the kill"),
            _ => {}
        }
    }

    let (mut control, _) = Client::operator(&socket, "pty-lifecycle-control").unwrap();
    control.set_timeout(Some(Duration::from_secs(5))).unwrap();
    control
        .call_value(Call::AgentKill { id: "a".into() })
        .unwrap();
    let stopped = session.snapshot();
    assert_eq!(find_agent(&stopped, "a").status, AgentStatus::Stopped);
    assert!(find_agent(&stopped, "a").pid.is_none());
    // Agent state is evidence that the kill ran, not a substitute for EOF.
    terminal_until_closed(&mut terminal);

    let (mut late, _) = Client::operator(&socket, "pty-lifecycle-replay").unwrap();
    late.set_timeout(Some(Duration::from_secs(5))).unwrap();
    late.call_value(Call::Attach {
        agent: "a".into(),
        rows: 10,
        cols: 40,
    })
    .unwrap();
    let replay = terminal_until_closed(&mut late);
    assert!(replay.contains("terminal-retained-marker"), "{replay:?}");
    assert!(late
        .send(Call::AttachInput {
            data: String::new()
        })
        .is_ok());
    let error = late
        .next_frame()
        .unwrap()
        .error
        .expect("input to an ended process is refused");
    assert!(error.message.contains("not running"), "{error}");
}
