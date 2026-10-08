//! Render one dashboard frame from a fixture to stdout — no daemon needed.
//!
//! ```text
//! cargo run -p crew-tui --example preview                # dashboard, 120x40
//! cargo run -p crew-tui --example preview -- 80 24       # another size
//! cargo run -p crew-tui --example preview -- 120 40 p2p  # with a live P2P link
//! ```
//! Views: `dashboard` (default), `p2p`, `sub`, `empty`, `detail`, `toolbox` (an inspector with
//! a toolbox), `toolbox-pane` (its `T` pane), `tasks`, `logs`, `help`,
//! `compose`, `usage` (the dashboard with the usage strip), `model` (the model picker), and
//! `launcher` (the work-style chooser).

use crew_core::rpc::{OutputResult, Push};
use crew_core::types::MessageType;
use crew_tui::app::{Compose, Modal, Reply, View};
use crew_tui::{fixture, render_to_string, App};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let width: u16 = args.first().and_then(|s| s.parse().ok()).unwrap_or(120);
    let height: u16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(40);
    let view = args.get(2).map(String::as_str).unwrap_or("dashboard");
    if view == "launcher" {
        print!("{}", launcher_frame(width, height));
        return;
    }

    let now = 1_790_000_000_000;
    let mut app = App::new();
    app.apply_snapshot(fixture::snapshot(now), now);
    let mut at = now;
    if view == "usage" {
        // The usage strip: each account's windows as the runtimes report them.
        use crew_core::rpc::Quota;
        use crew_core::types::RuntimeKind;
        let quota = |runtime, window: &str, percent, hours: i64| Quota {
            runtime,
            window: window.to_string(),
            percent_used: percent,
            resets_at: Some(now + hours * 3_600_000),
            updated_at: now,
        };
        app.session.quotas = vec![
            quota(RuntimeKind::Claude, "5h", 33.0, 2),
            quota(RuntimeKind::Claude, "week", 65.0, 34),
            quota(RuntimeKind::Codex, "week", 72.0, 80),
        ];
    }

    match view {
        "p2p" => {
            app.apply_push(
                Push::Message(fixture::message(
                    99,
                    "claude-6",
                    "claude-3",
                    MessageType::Question,
                    "does search need the ledger index rebuilt?",
                    now,
                )),
                now,
            );
            at = now + 450;
        }
        "sub" => {
            // A subagent orbiting its parent, reporting back to it directly.
            app.apply_snapshot(fixture::with_subagents(now), now);
            app.apply_push(
                Push::Message(fixture::message(
                    99,
                    "claude-3-s1",
                    "claude-3",
                    MessageType::Status,
                    "integration tests are green, merging my branch is safe",
                    now,
                )),
                now,
            );
            at = now + 450;
        }
        "empty" => {
            // Only the orchestrator: it has not assembled its team yet.
            let mut snap = fixture::team(0, now);
            snap.tasks.clear();
            snap.messages.clear();
            app.apply_snapshot(snap, now);
        }
        "detail" => {
            app.open_detail("claude-1", now);
            app.apply_reply(
                Reply::Output {
                    agent: "claude-1".into(),
                    result: Ok(OutputResult {
                        text: "● Reading src/billing.rs\n● Edit src/billing.rs (+42 -7)\n\n  Running cargo check…\n> ".into(),
                        alive: true,
                    }),
                },
                now,
            );
        }
        "toolbox" | "toolbox-pane" => {
            // The T02 `release` member: its summary, then (`T`) the pane.
            let mut snapshot = fixture::snapshot(now);
            snapshot.agents.extend(fixture::toolbox_agents(now));
            app.apply_snapshot(snapshot, now);
            app.open_detail("release", now);
            if view == "toolbox-pane" {
                render_to_string(&app, width, height, now);
                let key = KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE);
                crew_tui::input::handle_key(&mut app, key, now, 10);
            }
        }
        "tasks" => app.view = View::Tasks,
        "logs" => app.view = View::Logs,
        "help" => app.help = true,
        "model" => {
            app.open_model_picker("orchestrator", at);
            app.apply_reply(Reply::Models(Ok(fixture::models())), at);
        }
        "compose" => {
            app.modal = Some(Modal::Compose(Compose {
                to: "claude-3".into(),
                text: "please rebase onto a91ec21".into(),
                cursor: 26,
                ..Compose::default()
            }));
        }
        "command" => {
            app.modal = Some(Modal::Compose(Compose {
                to: "claude-3".into(),
                text: "/st".into(),
                cursor: 3,
                ..Compose::default()
            }));
        }
        "stop" => app.modal = Some(Modal::ConfirmStop),
        "context" => {
            // How full each context window is, shown in the boxes.
            let fills = [
                (350_000, 1_000_000),
                (164_000, 200_000),
                (12_000, 258_400),
                (930_000, 1_000_000),
            ];
            for (agent, (tokens, window)) in app.agents.iter_mut().zip(fills.into_iter().cycle()) {
                agent.context = Some(crew_core::types::ContextUsage::new(tokens, window, at));
            }
        }
        "question" | "band" => {
            let question = fixture::question(at);
            if view == "question" {
                app.modal = Some(Modal::Question(crew_tui::app::QuestionBox {
                    id: question.id.clone(),
                    ..Default::default()
                }));
            }
            app.questions.push(question);
        }
        _ => {}
    }
    print!("{}", render_to_string(&app, width, height, at));
}

fn launcher_frame(width: u16, height: u16) -> String {
    use crew_tui::{launcher, Launcher, StyleChoice};
    let choice = |name: &str, origin: &str, description: &str| StyleChoice {
        name: name.into(),
        origin: origin.into(),
        description: description.into(),
        details: vec![
            ("team".into(), "staffed by the orchestrator".into()),
            ("runtime".into(), "claude".into()),
        ],
        ..StyleChoice::default()
    };
    let mut research = choice(
        "research",
        "built-in",
        "Investigate a question. Gather evidence, cross-check it, write up findings.",
    );
    research.details.push((
        "last time".into(),
        "3 member(s): scout, skeptic, writer".into(),
    ));
    research.instructions =
        "This is a research team. The deliverable is knowledge, written down: not code.\n\n\
Ground rules for every member:\n\
- Every claim in a deliverable says where it comes from: a file and line, a URL, a paper, \
or the command you ran and its output.\n\
- Look for evidence against your current answer, not only for it."
            .into();
    let mut l = Launcher::new(
        "/work/payments",
        vec![
            choice("default", "built-in", "General software work."),
            research,
            choice("experiment", "built-in", "Test hypotheses."),
            choice("field-notes", "project", "Our own style."),
        ],
        1,
        "how do the two queue designs compare?",
    );
    l.notice = None;
    launcher::render_to_string(&l, width, height)
}
