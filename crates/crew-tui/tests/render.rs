//! Rendering tests against ratatui's `TestBackend`: what the dashboard puts on
//! screen for a given state, without a daemon or a real terminal.

use crew_core::rpc::{OutputResult, Push};
use crew_core::types::{AgentStatus, MessageType, TaskStatus};
use crew_tui::app::{Compose, Modal, Reply, View};
use crew_tui::theme::Theme;
use crew_tui::{fixture, render_to_string, views, App};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::Terminal;

const NOW: i64 = 1_790_000_000_000;

fn press(app: &mut App, code: KeyCode) {
    crew_tui::input::handle_key(app, KeyEvent::new(code, KeyModifiers::NONE), NOW, 10);
}

#[test]
fn message_end_then_up_moves_from_the_rendered_last_page() {
    let mut app = app();
    let body = (1..=60)
        .map(|i| format!("Line {i}: {}", "wide text ".repeat(12)))
        .collect::<Vec<_>>()
        .join("\n");
    app.messages.push_back(fixture::message(
        999,
        "claude-1",
        "claude-2",
        MessageType::Message,
        &body,
        NOW,
    ));
    app.modal = Some(Modal::Message(crew_tui::app::MessageView {
        id: "m-0999".into(),
        scroll: 0,
    }));
    render_to_string(&app, 100, 30, NOW);
    press(&mut app, KeyCode::End);
    let bottom = render_to_string(&app, 100, 30, NOW);
    press(&mut app, KeyCode::Up);
    assert_ne!(render_to_string(&app, 100, 30, NOW), bottom);
    // Resize while at the bottom: the next key must still move immediately.
    press(&mut app, KeyCode::End);
    let narrow_bottom = render_to_string(&app, 60, 30, NOW);
    press(&mut app, KeyCode::Up);
    assert_ne!(render_to_string(&app, 60, 30, NOW), narrow_bottom);
}

#[test]
fn question_page_up_moves_after_paging_to_the_bottom() {
    let mut app = app();
    let mut question = fixture::question(NOW);
    question.body = (1..=60)
        .map(|i| format!("Question line {i}: {}", "wide text ".repeat(8)))
        .collect::<Vec<_>>()
        .join("\n");
    let id = question.id.clone();
    app.questions.push(question);
    app.modal = Some(Modal::Question(crew_tui::app::QuestionBox {
        id,
        ..Default::default()
    }));
    render_to_string(&app, 100, 30, NOW);
    for _ in 0..200 {
        press(&mut app, KeyCode::PageDown);
    }
    let bottom = render_to_string(&app, 100, 30, NOW);
    press(&mut app, KeyCode::PageUp);
    assert_ne!(render_to_string(&app, 100, 30, NOW), bottom);
}

fn app() -> App {
    let mut app = App::new();
    app.apply_snapshot(fixture::snapshot(NOW), NOW);
    app
}

#[test]
fn parked_work_never_replaces_the_executing_task_in_the_dashboard() {
    use crew_tui::widgets::agent_box::{work_line, Decor};

    for status in [TaskStatus::Working, TaskStatus::Blocked] {
        let mut app = app();
        let mut executing = fixture::task("executing", "claude-1", status, &[], NOW);
        executing.started_at = Some(NOW - 30_000);
        let mut parked = fixture::task("parked", "claude-1", TaskStatus::Waiting, &[], NOW);
        parked.started_at = Some(NOW - 10_000);
        let mut newer = fixture::task("newer", "claude-1", TaskStatus::Waiting, &[], NOW);
        newer.started_at = Some(NOW - 5_000);
        app.tasks = vec![parked, executing, newer];
        let agent = app.agents.iter_mut().find(|a| a.id == "claude-1").unwrap();
        agent.task = Some("implement executing".into());
        agent.status = if status == TaskStatus::Working {
            AgentStatus::Working
        } else {
            AgentStatus::Blocked
        };
        let work = app.work_of("claude-1", NOW).unwrap();
        assert_eq!(work.title, "implement executing");
        assert_eq!(work.elapsed, Some(30_000));
        assert!(!work.done);
        let agent = app.agent("claude-1").unwrap();
        let decor = Decor {
            work: Some(work),
            ..Decor::default()
        };
        assert_eq!(work_line(agent, &decor).0, "implement executing");
        let screen = render_to_string(&app, 100, 6, NOW);
        let row = screen.lines().find(|l| l.contains("claude-1")).unwrap();
        assert!(row.contains("implement executing"), "{screen}");
        assert!(!row.contains("implement newer"), "{screen}");

        app.tasks[1].status = TaskStatus::Done;
        app.tasks[1].completed_at = Some(NOW);
        app.tasks[0].status = TaskStatus::Working;
        let resumed = app.work_of("claude-1", NOW).unwrap();
        assert_eq!(resumed.title, "implement parked");
        assert_eq!(resumed.elapsed, Some(10_000));
        assert_eq!(app.tasks[2].status, TaskStatus::Waiting);
    }
}

#[test]
fn parked_cards_remain_separate_and_inspectable_in_the_task_view() {
    let mut app = app();
    app.tasks = vec![
        fixture::task("executing", "claude-1", TaskStatus::Working, &[], NOW),
        fixture::task("parked-a", "claude-1", TaskStatus::Waiting, &[], NOW),
        fixture::task("parked-b", "claude-1", TaskStatus::Waiting, &[], NOW),
    ];
    app.view = View::Tasks;
    let screen = render_to_string(&app, 120, 30, NOW);
    for (id, status) in [
        ("executing", "working"),
        ("parked-a", "waiting"),
        ("parked-b", "waiting"),
    ] {
        let row = screen.lines().find(|l| l.contains(id)).unwrap();
        assert!(row.contains("claude-1") && row.contains(status), "{screen}");
    }
    assert_eq!(app.tasks_for("claude-1").count(), 3);
}

#[test]
fn waiting_projection_uses_the_same_stable_selection_as_default_task_operations() {
    let mut app = app();
    let mut first = fixture::task("first", "claude-1", TaskStatus::Waiting, &[], NOW);
    first.started_at = Some(NOW - 5_000);
    let mut second = fixture::task("second", "claude-1", TaskStatus::Waiting, &[], NOW);
    second.started_at = first.started_at;
    let mut review = fixture::task("review", "claude-1", TaskStatus::Review, &[], NOW);
    review.started_at = Some(NOW - 1_000);
    app.tasks = vec![first, second, review];
    assert_eq!(
        app.work_of("claude-1", NOW).unwrap().title,
        "implement first"
    );

    app.tasks[0].started_at = None;
    assert_eq!(
        app.work_of("claude-1", NOW).unwrap().title,
        "implement second"
    );
    app.tasks[1].started_at = None;
    let waiting = app.work_of("claude-1", NOW).unwrap();
    assert_eq!(waiting.title, "implement first");
    assert_eq!(waiting.elapsed, None);

    // An unstarted blocked card is not executing; a working card is,
    // even when its timestamp has not been reported in this snapshot.
    app.tasks[0].status = TaskStatus::Blocked;
    assert_eq!(
        app.work_of("claude-1", NOW).unwrap().title,
        "implement second"
    );
    app.tasks[1].status = TaskStatus::Working;
    let working = app.work_of("claude-1", NOW).unwrap();
    assert_eq!(working.title, "implement second");
    assert_eq!(working.elapsed, None);
}

#[test]
fn provider_errors_override_stale_activity_and_stay_visible_without_event_history() {
    use crew_tui::widgets::agent_box::{doing, status_tail, work_line, Decor, Tone};
    for kind in ["provider_error", "refused"] {
        let mut snap = fixture::snapshot(NOW);
        let agent = snap.agents.iter_mut().find(|a| a.id == "claude-1").unwrap();
        agent.status = AgentStatus::Blocked;
        agent.block_kind = Some(kind.into());
        agent.activity = "unresponsive old activity".into();
        agent.error = Some("DENIED provider message\u{1b}]52;c;bad\u{7}".into());
        let decor = Decor::default();
        let (what, _, tone) = doing(agent, &decor);
        assert!(what.starts_with("DENIED provider message"), "{what}");
        assert_eq!(tone, Tone::Bad);
        assert!(
            !status_tail(agent).contains('%'),
            "stale progress must not hide the error"
        );
        assert_eq!(work_line(agent, &decor).0, agent.task.as_deref().unwrap());
        snap.events.clear();
        let mut a = App::new();
        a.apply_snapshot(snap, NOW);
        for (width, height) in [(42, 24), (120, 40)] {
            let screen = render_to_string(&a, width, height, NOW);
            assert!(screen.contains("DEN"), "{screen}");
            assert!(!screen.contains("unresponsive"), "{screen}");
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| views::render(frame, &a, &Theme::truecolor(), NOW))
                .unwrap();
            assert!(terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .all(|cell| !cell.symbol().chars().any(char::is_control)));
        }
        a.open_detail("claude-1", NOW);
        let screen = render_to_string(&a, 120, 40, NOW);
        assert!(screen.contains("DENIED provider message"), "{screen}");
        assert!(!screen.contains("unresponsive old activity"), "{screen}");
    }
}

/// Geometry queries use the dashboard's area below the persistent warning.
fn dashboard_area(app: &App, width: u16, height: u16) -> ratatui::layout::Rect {
    let header = u16::from(height > 0 && app.session.sandbox != "required");
    ratatui::layout::Rect::new(0, header, width, height - header)
}

#[test]
fn socket_fallback_warning_stays_above_views_and_modals_and_preserves_hits() {
    use ratatui::layout::Rect;
    let mut wire = serde_json::to_value(fixture::snapshot(NOW)).unwrap();
    wire["session"]["socket_warning"] =
        serde_json::json!("daemon socket is in /tmp\u{1b}]52;c;bad\u{7}");
    wire["events"] = serde_json::json!([]);
    let mut a = App::new();
    a.apply_snapshot(serde_json::from_value(wire).unwrap(), NOW);
    for mode in ["required", "off"] {
        a.session.sandbox = mode.into();
        for view in [View::Dashboard, View::Detail, View::Tasks, View::Logs] {
            a.view = view;
            for help in [false, true] {
                a.help = help;
                for modal in [
                    None,
                    Some(Modal::Compose(crew_tui::app::Compose {
                        to: "claude-1".into(),
                        text: "hello".into(),
                        cursor: 5,
                        notice: None,
                    })),
                ] {
                    a.modal = modal;
                    let screen = render_to_string(&a, 42, 24, NOW);
                    let warning_row = usize::from(mode != "required");
                    assert!(
                        screen
                            .lines()
                            .nth(warning_row)
                            .unwrap()
                            .contains("SOCKET IN /tmp"),
                        "{screen}"
                    );
                    assert!(
                        !screen.contains("52;c;bad"),
                        "raw warning text must not be drawn"
                    );
                }
            }
        }
        a.help = false;
        a.modal = None;
        for view in [View::Dashboard, View::Detail, View::Tasks, View::Logs] {
            a.view = view;
            for width in [0, 1, 9, 10, 42] {
                for height in [0, 1, 2, 3, 24] {
                    let text = render_to_string(&a, width, height, NOW);
                    assert!(!text.chars().any(|c| c.is_control() && c != '\n'));
                    if width > 0 && height > 0 {
                        assert!(
                            text.starts_with(if width < 10 {
                                "!"
                            } else if mode == "required" {
                                "SOCKET"
                            } else {
                                "NO"
                            }),
                            "{text}"
                        );
                    }
                }
            }
        }
        a.view = View::Dashboard;
        a.modal = None;
        a.help = false;
        let rows = 1 + u16::from(mode != "required");
        let screen = Rect::new(0, 0, 120, 40);
        render_to_string(&a, 120, 40, NOW);
        for row in 0..rows {
            assert_eq!(views::hit(&a, screen, 10, row), None);
        }
        let rects = views::dashboard::agent_rects(&a, Rect::new(0, rows, 120, 40 - rows));
        assert!(!rects.is_empty());
        for (id, rect) in rects {
            assert_eq!(
                views::hit(&a, screen, rect.x + 1, rect.y + 1),
                Some(views::dashboard::Hit::Agent(id))
            );
        }
    }
}

#[test]
fn sandbox_warning_is_persistent_and_only_the_snapshot_can_confirm_it() {
    use crew_tui::app::{MessageView, ModelPicker, QuestionBox};
    for width in [42, 120] {
        let mut a = app();
        a.session.sandbox = "required".into();
        let screen = render_to_string(&a, width, 24, NOW);
        assert!(!screen.contains("NO SANDBOX") && !screen.contains("SANDBOX UNCONFIRMED"));
        let question = fixture::question(NOW);
        let question_id = question.id.clone();
        a.questions.push(question);
        for (mode, label) in [
            ("off", "NO SANDBOX"),
            ("", "SANDBOX UNCONFIRMED"),
            ("future", "SANDBOX UNCONFIRMED"),
            ("\u{1b}]52;c;bad\u{7}", "SANDBOX UNCONFIRMED"),
        ] {
            a.session.sandbox = mode.into();
            for view in [View::Dashboard, View::Detail, View::Tasks, View::Logs] {
                a.view = view;
                let overlays = [
                    None,
                    Some(Modal::Compose(Compose::default())),
                    Some(Modal::Type(Compose::default())),
                    Some(Modal::ConfirmKill("claude-1".into())),
                    Some(Modal::ConfirmRestartPaused(1)),
                    Some(Modal::ConfirmStop),
                    Some(Modal::Message(MessageView {
                        id: a.messages[0].id.clone(),
                        ..Default::default()
                    })),
                    Some(Modal::Question(QuestionBox {
                        id: question_id.clone(),
                        ..Default::default()
                    })),
                    Some(Modal::Model(ModelPicker {
                        agent: "claude-1".into(),
                        runtime: crew_core::RuntimeKind::Claude,
                        row: 0,
                        moved: false,
                        notice: None,
                    })),
                ];
                for overlay in overlays {
                    a.modal = overlay;
                    for help in [false, true] {
                        a.help = help;
                        let screen = render_to_string(&a, width, 24, NOW);
                        assert!(
                            screen.lines().next().unwrap().contains(label),
                            "{mode:?}/{view:?}: {screen}"
                        );
                        assert!(!screen.contains('\u{1b}'));
                    }
                }
            }
        }
    }
    for width in 0..=42 {
        for height in 0..=3 {
            let a = app();
            let screen = render_to_string(&a, width, height, NOW);
            if width > 0 && height > 0 {
                assert!(
                    !screen.lines().next().unwrap().trim().is_empty(),
                    "{width}x{height}"
                );
            }
        }
    }
}

#[test]
fn sandbox_header_keeps_mouse_hits_on_the_rows_that_are_drawn() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut a = app();
    let target = a
        .agents
        .iter_mut()
        .find(|agent| agent.id == "claude-1")
        .unwrap();
    target.id = "tap".into();
    target.name = "tap".into();
    let area = ratatui::layout::Rect::new(0, 0, 42, 24);
    let screen = render_to_string(&a, 42, 24, NOW);
    assert!(
        views::hit(&a, area, 1, 0).is_none(),
        "the warning is not a question or agent"
    );
    let name = a
        .agents
        .iter()
        .find(|agent| agent.id == "tap")
        .unwrap()
        .name
        .clone();
    let (row, line) = screen
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(&name))
        .unwrap();
    let at = line.find(&name).unwrap();
    let column = crew_tui::widgets::text::width(&line[..at]) as u16;
    assert!(
        matches!(views::hit(&a, area, column, row as u16), Some(views::dashboard::Hit::Agent(id)) if id=="tap")
    );
    crew_tui::input::handle_mouse(
        &mut a,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: row as u16,
            modifiers: KeyModifiers::NONE,
        },
        area,
        NOW,
        10,
    );
    assert_eq!(a.view, View::Detail);
    assert_eq!(a.detail.agent, "tap");
}

#[test]
fn sandbox_header_and_modal_scroll_limits_share_the_reduced_area() {
    use crew_tui::app::MessageView;
    let mut offsets = Vec::new();
    for mode in ["required", "off"] {
        let mut a = app();
        a.session.sandbox = mode.into();
        let body = (0..80)
            .map(|n| format!("Body line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        a.messages.push_back(fixture::message(
            999,
            "claude-1",
            "claude-2",
            MessageType::Message,
            &body,
            NOW,
        ));
        a.modal = Some(Modal::Message(MessageView {
            id: "m-0999".into(),
            scroll: 0,
        }));
        render_to_string(&a, 80, 24, NOW);
        press(&mut a, KeyCode::End);
        let bottom = render_to_string(&a, 80, 24, NOW);
        assert!(bottom.contains("Body line 79"));
        let Some(Modal::Message(view)) = &a.modal else {
            unreachable!()
        };
        offsets.push(view.scroll);
        press(&mut a, KeyCode::Up);
        assert_ne!(render_to_string(&a, 80, 24, NOW), bottom);
    }
    assert_eq!(
        offsets[1],
        offsets[0] + 1,
        "the warning uses one visible row"
    );
}

#[test]
fn inspector_text_never_renders_terminal_controls() {
    let poison = "\u{1b}]52;c;Y29tbWFuZA==\u{7}\u{9b}2J\tvisible";
    let mut snapshot = fixture::snapshot(NOW);
    let agent = snapshot
        .agents
        .iter_mut()
        .find(|a| a.id == "claude-1")
        .unwrap();
    agent.name = poison.into();
    agent.role = poison.into();
    agent.task = Some(poison.into());
    agent.model = Some(poison.into());
    agent.running_model = Some(poison.into());
    agent.model_name = Some(poison.into());
    agent.effort = Some(poison.into());
    agent.workdir = poison.into();
    agent.session_id = Some(poison.into());
    agent.backend_ref = Some(poison.into());
    agent.branch = Some(poison.into());
    let mut task = fixture::task("salty", "claude-1", TaskStatus::Done, &[], NOW);
    task.title = poison.into();
    task.result = Some(poison.into());
    task.artifacts[0].kind = poison.into();
    task.artifacts[0].value = poison.into();
    task.artifacts[0].description = poison.into();
    snapshot.tasks = vec![task];
    for message in &mut snapshot.messages {
        message.body = poison.into();
    }
    for event in &mut snapshot.events {
        event.message = poison.into();
    }
    let mut app = App::new();
    app.apply_snapshot(snapshot, NOW);
    app.detail.agent = "claude-1".into();
    for view in [View::Detail, View::Tasks, View::Logs] {
        app.view = view;
        let mut terminal = Terminal::new(TestBackend::new(180, 80)).unwrap();
        terminal
            .draw(|frame| views::render(frame, &app, &Theme::truecolor(), NOW))
            .unwrap();
        for cell in &terminal.backend().buffer().content {
            assert!(
                !cell.symbol().chars().any(char::is_control),
                "{view:?} rendered a terminal control: {:?}",
                cell.symbol()
            );
        }
    }
}

#[test]
fn dashboard_handles_control_bearing_model_labels() {
    let mut app = app();
    let poison = "\u{1b}]52;c;Y29tbWFuZA==\u{7}\u{9b}2J\tvisible";
    let agent = app.agents.iter_mut().find(|a| a.id == "claude-1").unwrap();
    agent.model_name = Some(poison.into());
    agent.effort = Some(poison.into());
    let mut terminal = Terminal::new(TestBackend::new(180, 80)).unwrap();
    terminal
        .draw(|frame| views::render(frame, &app, &Theme::truecolor(), NOW))
        .unwrap();
    assert!(terminal
        .backend()
        .buffer()
        .content
        .iter()
        .all(|cell| !cell.symbol().chars().any(char::is_control)));
}

#[test]
fn dashboard_handles_unicode_presentation_labels() {
    for label in ["☺\u{fe0f}".repeat(12), "👩\u{200d}👩\u{200d}👦".repeat(6)] {
        let mut app = app();
        app.agents
            .iter_mut()
            .find(|a| a.id == "claude-1")
            .unwrap()
            .model_name = Some(label);
        for width in [42, 48, 60, 80, 120, 180, 220] {
            for height in [24, 40] {
                render_to_string(&app, width, height, NOW);
            }
        }
    }
}

/// How many member boxes are on screen: each leads with its runtime's glyph.
/// (The hub writes its CLI's glyph too, before its model's proper name.)
fn member_marks(screen: &str) -> usize {
    screen
        .match_indices(['✱', '❂'])
        .filter(|(at, mark)| {
            !screen[at + mark.len()..]
                .trim_start_matches(' ')
                .starts_with(char::is_uppercase)
        })
        .count()
}

fn p2p(app: &mut App, from: &str, to: &str) {
    app.apply_push(
        Push::Message(fixture::message(
            900,
            from,
            to,
            MessageType::Message,
            "I changed Transaction::commit(), please update the persistence layer",
            NOW,
        )),
        NOW,
    );
}

#[test]
fn dashboard_shows_every_agent_and_the_bus() {
    let screen = render_to_string(&app(), 120, 40, NOW);
    for (id, role) in [
        ("claude-1", "billing"),
        ("claude-2", "queue"),
        ("claude-3", "ledger"),
        ("claude-4", "notify"),
        ("claude-5", "db"),
        ("claude-6", "search"),
    ] {
        // claude-2 is the fixture's Codex agent.
        let mark = if id == "claude-2" { '❂' } else { '✱' };
        assert!(
            screen.contains(&format!("{mark} {id}")),
            "missing box for {id}\n{screen}"
        );
        assert!(screen.contains(role), "missing role {role}\n{screen}");
    }
    assert!(screen.contains("◆ orchestrator"), "{screen}");
    assert!(screen.contains("status · 5/6"), "{screen}");
    assert!(
        screen.contains("51%") && screen.contains("100%"),
        "{screen}"
    );
    assert!(screen.contains("message bus ──"), "{screen}");
    assert!(screen.contains("claude-3 → claude-5"), "{screen}");
    assert!(screen.contains("orchestrator ⇉ claude-4"), "{screen}");
    assert!(screen.contains("orchestrator ⇶ *"), "{screen}");
    assert!(screen.contains("your trait bound broke"), "{screen}");
    assert!(screen.contains("checkout-rewrite"), "{screen}");
    // Nothing is talking right now.
    assert!(!screen.contains("p2p channel OPEN"), "{screen}");
    assert!(!screen.contains("private channel"), "{screen}");
}

#[test]
fn a_message_in_classical_chinese_keeps_the_dashboard_in_shape() {
    let mut app = app();
    app.apply_push(
        Push::Message(fixture::message(
            901,
            "claude-3",
            "claude-5",
            MessageType::Message,
            "`parse_header` 遇空行即崩。已補檢於 `src/parse.rs:88`，`cargo test` 皆過，見 a1b2c3d。".repeat(3).as_str(),
            NOW,
        )),
        NOW,
    );
    for (cols, rows) in [(120, 40), (80, 24), (60, 20)] {
        let screen = render_to_string(&app, cols, rows, NOW);
        for line in screen.lines() {
            // One character per cell (a wide character's second cell prints as a space).
            assert!(
                line.chars().count() <= cols as usize,
                "{cols}x{rows}: {line}"
            );
        }
    }
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.replace(' ', "").contains("遇空行即崩"), "{screen}");
}

#[test]
fn a_temporary_member_is_marked_on_its_box() {
    let mut app = app();
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(!screen.contains("temp"), "{screen}");
    let mut helper = app.agent("claude-6").unwrap().clone();
    helper.temporary = true;
    app.apply_push(Push::Agent(Box::new(helper)), NOW);
    let screen = render_to_string(&app, 120, 40, NOW);
    let line = screen.lines().find(|l| l.contains("✱ claude-6")).unwrap();
    assert!(line.contains("temp"), "{screen}");
}

#[test]
fn p2p_message_opens_the_private_channel_banner() {
    let mut app = app();
    p2p(&mut app, "claude-3", "claude-5");
    let during = render_to_string(&app, 120, 40, NOW + 400);
    assert!(
        during.contains("p2p channel OPEN · agents talking directly"),
        "{during}"
    );
    assert!(
        during.contains("private channel · orchestrator not in loop"),
        "{during}"
    );
    assert!(
        during.contains('●'),
        "the pulse should be in flight\n{during}"
    );

    // Pulse has landed but the link is still highlighted.
    let after_pulse = render_to_string(&app, 120, 40, NOW + 1500);
    assert!(after_pulse.contains("p2p channel OPEN"), "{after_pulse}");
    assert!(!after_pulse.contains('●'), "{after_pulse}");

    // Once the link expires the banner goes away.
    app.tick(NOW + 3100);
    let later = render_to_string(&app, 120, 40, NOW + 3100);
    assert!(!later.contains("p2p channel OPEN"), "{later}");
    assert!(!later.contains("private channel"), "{later}");
}

#[test]
fn orchestrator_traffic_is_not_reported_as_p2p() {
    let mut app = app();
    app.apply_push(
        Push::Message(fixture::message(
            901,
            "orchestrator",
            "claude-4",
            MessageType::Task,
            "implement notifications",
            NOW,
        )),
        NOW,
    );
    let screen = render_to_string(&app, 120, 40, NOW + 400);
    assert!(
        screen.contains('●'),
        "orchestrator traffic still animates\n{screen}"
    );
    assert!(!screen.contains("p2p channel OPEN"), "{screen}");
    assert!(screen.contains("implement notifications"), "{screen}");
}

#[test]
fn every_team_size_renders_all_boxes() {
    for (w, h) in [(120u16, 40u16), (80, 24)] {
        for n in 2..=10usize {
            let mut app = App::new();
            app.apply_snapshot(fixture::team(n, NOW), NOW);
            let screen = render_to_string(&app, w, h, NOW);
            assert!(screen.contains("◆ orchestrator"), "n={n} {w}x{h}\n{screen}");
            assert_eq!(
                member_marks(&screen),
                n,
                "expected {n} agent boxes at {w}x{h}\n{screen}"
            );
            assert_eq!(
                screen.matches('┌').count(),
                n + 1,
                "expected the radial layout (not the list) at {w}x{h} for n={n}\n{screen}"
            );
        }
    }
}

#[test]
fn tiny_terminals_never_panic() {
    let sizes = [
        (1u16, 1u16),
        (2, 2),
        (5, 3),
        (10, 4),
        (20, 8),
        (30, 12),
        (40, 10),
        (60, 15),
        (79, 23),
    ];
    for (w, h) in sizes {
        for view in [View::Dashboard, View::Detail, View::Tasks, View::Logs] {
            let mut app = app();
            p2p(&mut app, "claude-3", "claude-5");
            if view == View::Detail {
                app.open_detail("claude-3", NOW);
            }
            app.view = view;
            let _ = render_to_string(&app, w, h, NOW + 300);
            app.help = true;
            let _ = render_to_string(&app, w, h, NOW + 300);
            app.help = false;
            app.modal = Some(Modal::Compose(Compose {
                to: "claude-3".into(),
                text: "x".repeat(300),
                cursor: 300,
                ..Compose::default()
            }));
            let _ = render_to_string(&app, w, h, NOW + 300);
            app.modal = Some(Modal::ConfirmKill("claude-3".into()));
            app.connected = false;
            let _ = render_to_string(&app, w, h, NOW + 300);
        }
    }
    // An app that never received a snapshot renders too.
    let _ = render_to_string(&App::new(), 20, 8, NOW);
    let _ = render_to_string(&App::new(), 120, 40, NOW);
}

#[test]
fn small_terminal_falls_back_to_a_compact_list() {
    let screen = render_to_string(&app(), 30, 12, NOW);
    assert!(!screen.contains('┌'), "{screen}");
    for id in ["orchestrator", "claude-1", "claude-6"] {
        assert!(screen.contains(id), "missing {id}\n{screen}");
    }
    assert!(
        screen.contains("▸"),
        "the selection marker should show\n{screen}"
    );
}

#[test]
fn selected_box_is_highlighted() {
    let mut app = app();
    app.selected = Some("claude-2".into());
    let theme = Theme::truecolor();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| views::render(frame, &app, &theme, NOW))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let corners: Vec<(u16, u16)> = (0..40u16)
        .flat_map(|y| (0..120u16).map(move |x| (x, y)))
        .filter(|&(x, y)| buffer[(x, y)].symbol() == "┌")
        .collect();
    let bold: Vec<&(u16, u16)> = corners
        .iter()
        .filter(|&&(x, y)| buffer[(x, y)].modifier.contains(Modifier::BOLD))
        .collect();
    assert_eq!(corners.len(), 7);
    assert_eq!(
        bold.len(),
        1,
        "exactly one box carries the selection border"
    );
    let &(x, y) = bold[0];
    let title: String = (x..x + 16)
        .map(|cx| buffer[(cx, y)].symbol().to_string())
        .collect();
    assert!(title.contains("claude-2"), "{title}");
}

#[test]
fn status_changes_show_up() {
    let mut app = app();
    let mut failed = app.agent("claude-4").unwrap().clone();
    failed.status = AgentStatus::Failed;
    failed.progress = None;
    app.apply_push(Push::Agent(Box::new(failed)), NOW);
    let mut waiting = app.agent("claude-2").unwrap().clone();
    waiting.status = AgentStatus::Waiting;
    waiting.progress = None;
    app.apply_push(Push::Agent(Box::new(waiting)), NOW);
    let mut paused = app.agent("claude-6").unwrap().clone();
    paused.status = AgentStatus::Working;
    paused.progress = None;
    paused.paused = true;
    app.apply_push(Push::Agent(Box::new(paused)), NOW);
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("✗ failed"), "{screen}");
    assert!(screen.contains("○ waiting"), "{screen}");
    assert!(screen.contains("‖ paused"), "{screen}");

    app.session.completed_at = Some(NOW);
    app.session.summary = Some("checkout rewritten; all six branches merged".into());
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("✓ all tasks done"), "{screen}");
}

#[test]
fn task_graph_view_lists_layers_and_dependencies() {
    let mut app = app();
    app.view = View::Tasks;
    let mut blocked = app
        .tasks
        .iter()
        .find(|t| t.id == "billing")
        .unwrap()
        .clone();
    blocked.status = TaskStatus::Blocked;
    blocked.note = Some("waiting on a schema decision".into());
    app.apply_push(Push::Task(Box::new(blocked)), NOW);
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("task graph"), "{screen}");
    assert!(screen.contains("layer 0"), "{screen}");
    assert!(screen.contains("layer 2"), "{screen}");
    assert!(screen.contains("← ledger, queue"), "{screen}");
    assert!(screen.contains("review: claude-2"), "{screen}");
    assert!(screen.contains("waiting on a schema decision"), "{screen}");
    let layer0 = screen.find("layer 0").unwrap();
    let layer1 = screen.find("layer 1").unwrap();
    let db = screen.find(" db ").unwrap();
    let ledger = screen.find(" ledger ").unwrap();
    assert!(layer0 < db && db < layer1 && layer1 < ledger, "{screen}");
}

#[test]
fn detail_view_shows_the_agent_and_its_terminal() {
    let mut app = app();
    app.open_detail("claude-3", NOW);
    app.apply_reply(
        Reply::Output {
            agent: "claude-3".into(),
            result: Ok(OutputResult {
                text: "● Edit src/ledger.rs\n\u{1b}[31mcargo test … ok\n\n".into(),
                alive: true,
            }),
        },
        NOW,
    );
    let screen = render_to_string(&app, 140, 44, NOW + 1000);
    for needle in [
        "claude-3 · ledger",
        "agents/claude-3",
        "/work/demo/.crew/worktrees/claude-3",
        "herdr · w2:p4",
        "0d73aa0a-bdaf-4916-8421-d1917fc3bf2a",
        "220k",
        "terminal · live",
        "● Edit src/ledger.rs",
        "cargo test … ok",
        "← db",
        "⬡ commit a91ec21",
        "→ claude-5",
        "vcs.cherry_pick",
    ] {
        assert!(screen.contains(needle), "missing {needle:?}\n{screen}");
    }
    assert!(
        !screen.contains('\u{1b}'),
        "escape bytes must not reach the screen"
    );

    // The narrow layout still shows identity and terminal.
    let narrow = render_to_string(&app, 70, 24, NOW + 1000);
    assert!(
        narrow.contains("claude-3") && narrow.contains("terminal · live"),
        "{narrow}"
    );
}

#[test]
fn logs_view_follows_and_filters() {
    let mut app = app();
    app.view = View::Logs;
    let screen = render_to_string(&app, 120, 30, NOW);
    assert!(screen.contains("event log"), "{screen}");
    assert!(screen.contains("following"), "{screen}");
    assert!(screen.contains("agent.crashed"), "{screen}");
    assert!(screen.contains("team checkout-rewrite loaded"), "{screen}");

    app.logs.filter_agent = Some("claude-3".into());
    let screen = render_to_string(&app, 120, 30, NOW);
    assert!(screen.contains("event log · claude-3"), "{screen}");
    assert!(screen.contains("vcs.cherry_pick"), "{screen}");
    assert!(!screen.contains("agent.crashed"), "{screen}");
}

#[test]
fn overlays_render_on_top() {
    let mut app = app();
    app.help = true;
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("crew · keys"), "{screen}");
    assert!(screen.contains("agents keep running"), "{screen}");
    assert!(
        screen.contains("stop the team: agents and daemon (asks first)"),
        "{screen}"
    );
    assert!(
        screen.contains("commands") && screen.contains("/exit · /stop · /kill [agent]"),
        "{screen}"
    );
    assert!(
        screen.contains("/unpause [agent]") && screen.contains("/help"),
        "{screen}"
    );
    assert!(screen.contains("/compact [agent]"), "{screen}");
    assert!(
        screen.contains("open an agent or a bus message; wheel scrolls"),
        "{screen}"
    );
    assert!(screen.contains("a subagent, tethered"), "{screen}");

    app.help = false;
    app.modal = Some(Modal::Compose(Compose {
        to: "claude-5".into(),
        text: "need a migration".into(),
        cursor: 16,
        ..Compose::default()
    }));
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("message → claude-5"), "{screen}");
    assert!(screen.contains("› need a migration"), "{screen}");

    app.modal = Some(Modal::ConfirmKill("claude-5".into()));
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("Stop claude-5's process?"), "{screen}");

    app.modal = Some(Modal::ConfirmStop);
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("stop the team"), "{screen}");
    assert!(
        screen.contains("Stop every agent and the crew daemon?"),
        "{screen}"
    );
    assert!(
        screen.contains("y stop · any other key cancels"),
        "{screen}"
    );
}

#[test]
fn the_message_box_turns_into_a_command_line_on_a_slash() {
    let mut app = app();
    let compose = |text: &str, notice: Option<&str>| {
        Modal::Compose(Compose {
            to: "claude-5".into(),
            text: text.into(),
            cursor: text.chars().count(),
            notice: notice.map(str::to_string),
        })
    };
    app.modal = Some(compose("", None));
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("message → claude-5"), "{screen}");
    assert!(
        screen.contains("shift+enter new line") && screen.contains("/ commands"),
        "{screen}"
    );

    // The commands are listed as soon as the slash is typed, then narrowed.
    app.modal = Some(compose("/", None));
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains(" command "), "{screen}");
    assert!(!screen.contains("message → claude-5"), "{screen}");
    assert!(screen.contains("/exit · /stop · /kill [agent]"), "{screen}");
    app.modal = Some(compose("/sto", None));
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("› /sto"), "{screen}");
    assert!(
        screen.contains("/stop  stop the team: every agent and the daemon"),
        "{screen}"
    );
    app.modal = Some(compose(
        "/clear",
        Some("no command /clear — /help lists them"),
    ));
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen.contains("no command /clear — /help lists them"),
        "{screen}"
    );

    // While a stop is under way the footer says so.
    app.modal = None;
    app.stopping = true;
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("stopping the team"), "{screen}");
}

#[test]
fn disconnect_shows_a_banner_and_keeps_the_last_state() {
    let mut app = app();
    app.connected = false;
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen.contains("daemon disconnected — reconnecting"),
        "{screen}"
    );
    assert!(screen.contains("✱ claude-3"), "{screen}");
}

#[test]
fn run_refuses_to_start_without_a_daemon() {
    // Fails before the terminal is touched, so it is safe under the test harness.
    let err = crew_tui::run(
        std::path::Path::new("/nonexistent/crew-daemon.sock"),
        Default::default(),
        None,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("cannot attach the dashboard"),
        "{err:#}"
    );
}

fn sub_app() -> App {
    let mut app = App::new();
    app.apply_snapshot(fixture::with_subagents(NOW), NOW);
    app
}

#[test]
fn subagents_orbit_their_parent() {
    let screen = render_to_string(&sub_app(), 140, 44, NOW);
    assert!(screen.contains("↳ claude-3-s1"), "{screen}");
    assert!(screen.contains("tests"), "{screen}");
    assert!(screen.contains("40%"), "{screen}");
    // The ring still holds exactly the six top-level workers.
    assert_eq!(member_marks(&screen), 6, "{screen}");
    assert_eq!(screen.matches('┌').count(), 8, "{screen}");
    // The satellite sits next to its parent, not across the screen.
    let row_of = |needle: &str| screen.lines().position(|l| l.contains(needle)).unwrap() as i32;
    let col_of = |needle: &str| {
        let line = screen.lines().find(|l| l.contains(needle)).unwrap();
        line[..line.find(needle).unwrap()].chars().count() as i32
    };
    assert!(
        (row_of("↳ claude-3-s1") - row_of("✱ claude-3 ")).abs() <= 9,
        "{screen}"
    );
    assert!(
        (col_of("↳ claude-3-s1") - col_of("✱ claude-3 ")).abs() <= 44,
        "{screen}"
    );
    // The report from the subagent to its parent is ordinary P2P traffic.
    assert!(screen.contains("claude-3-s1 → claude-3"), "{screen}");
}

#[test]
fn subagent_and_parent_talk_over_their_tether() {
    let mut app = sub_app();
    p2p(&mut app, "claude-3-s1", "claude-3");
    let screen = render_to_string(&app, 140, 44, NOW + 400);
    assert!(screen.contains("p2p channel OPEN"), "{screen}");
    assert!(
        screen.contains('●'),
        "the pulse travels the tether\n{screen}"
    );
}

#[test]
fn in_session_subagents_show_as_a_badge() {
    let screen = render_to_string(&sub_app(), 120, 40, NOW);
    assert!(screen.contains("⑂2"), "{screen}");
    // The badge shares the title row; the status row is intact.
    let title = screen.lines().find(|l| l.contains("✱ claude-1")).unwrap();
    assert!(title.contains("⑂2"), "{title}");
    assert!(
        screen.contains("billing") && screen.contains("51%"),
        "{screen}"
    );

    // At the narrowest boxes the status row still survives.
    let mut crowded = App::new();
    let mut snap = fixture::team(10, NOW);
    snap.agents[0].subagents = 3;
    crowded.apply_snapshot(snap, NOW);
    let screen = render_to_string(&crowded, 80, 24, NOW);
    assert!(screen.contains("⑂3"), "{screen}");
    assert_eq!(member_marks(&screen), 10, "{screen}");
}

#[test]
fn small_terminals_badge_the_parent_instead_of_drawing_satellites() {
    let mut app = sub_app();
    let second = fixture::subagent(
        "claude-3-s2",
        "claude-3",
        "docs",
        AgentStatus::Working,
        None,
        NOW,
    );
    app.apply_push(Push::Agent(Box::new(second)), NOW);
    let screen = render_to_string(&app, 80, 24, NOW);
    assert!(!screen.contains("↳ claude-3-s1"), "{screen}");
    let title = screen.lines().find(|l| l.contains("✱ claude-3")).unwrap();
    assert!(title.contains("+2"), "{title}\n{screen}");
    assert_eq!(screen.matches('┌').count(), 7, "{screen}");
    // The compact list still names them.
    let list = render_to_string(&app, 30, 14, NOW);
    assert!(
        list.contains("↳claude-3-s1") || list.contains("↳ claude-3-s1"),
        "{list}"
    );
}

#[test]
fn an_empty_roster_explains_itself() {
    let mut app = App::new();
    let mut snap = fixture::team(0, NOW);
    snap.tasks.clear();
    snap.messages.clear();
    app.apply_snapshot(snap, NOW);
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("◆ orchestrator"), "{screen}");
    assert!(
        screen.contains("the orchestrator is assembling the team…"),
        "{screen}"
    );
    assert_eq!(screen.matches('┌').count(), 1, "{screen}");
    assert_eq!(member_marks(&screen), 0, "{screen}");

    // No orchestrator agent and nobody else either.
    let mut bare = App::new();
    let mut snap = fixture::team(0, NOW);
    snap.agents.clear();
    snap.tasks.clear();
    bare.apply_snapshot(snap, NOW);
    let screen = render_to_string(&bare, 120, 40, NOW);
    assert!(screen.contains("no agents on the team yet"), "{screen}");
    // Members arriving one at a time never garble the ring.
    for id in ["backend", "db"] {
        app.apply_push(
            Push::Agent(Box::new(fixture::agent(
                id,
                id,
                AgentStatus::Spawning,
                None,
                NOW,
            ))),
            NOW,
        );
        let screen = render_to_string(&app, 120, 40, NOW);
        assert!(screen.contains(&format!("✱ {id}")), "{screen}");
        assert!(!screen.contains("assembling"), "{screen}");
    }
}

#[test]
fn detail_and_task_views_name_parents_and_subagents() {
    let mut app = sub_app();
    app.open_detail("claude-3-s1", NOW);
    let screen = render_to_string(&app, 140, 44, NOW);
    assert!(screen.contains("↳ claude-3-s1"), "{screen}");
    assert!(screen.contains("parent    claude-3"), "{screen}");
    assert!(screen.contains("delivery  socket"), "{screen}");

    app.open_detail("claude-3", NOW);
    let screen = render_to_string(&app, 140, 44, NOW);
    assert!(
        screen.contains("subagents claude-3-s1 (working)"),
        "{screen}"
    );

    app.open_detail("claude-1", NOW);
    let screen = render_to_string(&app, 140, 44, NOW);
    assert!(screen.contains("in-session subagents: 2"), "{screen}");

    app.view = View::Tasks;
    let screen = render_to_string(&app, 140, 44, NOW);
    assert!(screen.contains("claude-3-s1 (sub of claude-3)"), "{screen}");

    app.view = View::Dashboard;
    app.help = true;
    let screen = render_to_string(&app, 140, 44, NOW);
    assert!(screen.contains("a subagent, tethered"), "{screen}");
}

#[test]
fn tiny_terminals_never_panic_with_subagents() {
    for (w, h) in [
        (1u16, 1u16),
        (5, 3),
        (20, 8),
        (40, 10),
        (60, 15),
        (79, 23),
        (100, 23),
        (101, 30),
    ] {
        for view in [View::Dashboard, View::Detail, View::Tasks, View::Logs] {
            let mut app = sub_app();
            for i in 2..=4 {
                let extra = fixture::subagent(
                    &format!("claude-3-s{i}"),
                    "claude-3",
                    "extra",
                    AgentStatus::Working,
                    None,
                    NOW,
                );
                app.apply_push(Push::Agent(Box::new(extra)), NOW);
            }
            p2p(&mut app, "claude-3-s1", "claude-3");
            p2p(&mut app, "claude-3-s2", "claude-5");
            app.selected = Some("claude-3-s4".into());
            if view == View::Detail {
                app.open_detail("claude-3-s1", NOW);
            }
            app.view = view;
            let _ = render_to_string(&app, w, h, NOW + 300);
        }
    }
}

#[test]
fn a_question_for_the_operator_is_announced_and_shown_in_full() {
    use crew_tui::app::QuestionBox;
    let mut app = app();
    let plain = render_to_string(&app, 120, 40, NOW);
    assert!(!plain.contains("question from"), "{plain}");

    // The band at the top says who asks what, and how to answer.
    let question = fixture::question(NOW);
    app.questions.push(question.clone());
    let screen = render_to_string(&app, 120, 40, NOW);
    let band = screen.lines().nth(1).unwrap();
    assert!(
        band.contains("? question from orchestrator (for claude-2) · 3m05s ago: The legacy export"),
        "{band}"
    );
    assert!(band.trim_end().ends_with("a answer"), "{band}");
    assert!(
        screen.contains("claude-1") && screen.contains("message bus"),
        "{screen}"
    );
    let mut second = question.clone();
    second.id = "m-second".into();
    second.metadata = serde_json::Value::Null;
    app.questions.push(second);
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen
            .lines()
            .nth(1)
            .unwrap()
            .contains("? 2 questions for you · the first from orchestrator"),
        "{screen}"
    );
    // It is there in every view, above the view's own header.
    for view in [View::Detail, View::Tasks, View::Logs] {
        app.view = view;
        let screen = render_to_string(&app, 120, 40, NOW);
        assert!(
            screen
                .lines()
                .nth(1)
                .unwrap()
                .contains("? 2 questions for you"),
            "{view:?}:\n{screen}"
        );
        assert_eq!(screen.lines().count(), 40, "{view:?}");
    }
    app.view = View::Dashboard;

    // The box has the whole question, who wants to know, the suggested
    // answers and a line for the operator's own.
    app.modal = Some(Modal::Question(QuestionBox {
        id: question.id.clone(),
        ..QuestionBox::default()
    }));
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen.contains("question 1 of 2 · orchestrator, for claude-2 · 3m05s ago"),
        "{screen}"
    );
    for part in [
        "The legacy export writes amounts as integer cents",
        "breaks the two finance dashboards until they",
        "Which do you want?",
        "▸ 1. Convert in the adapter",
        "  2. Change the export and update the dashboards",
        "  3. › type your own answer",
        "↑↓ choose · enter send · tab next question",
    ] {
        assert!(screen.contains(part), "missing {part:?}:\n{screen}");
    }
    let mut typed = QuestionBox {
        id: question.id.clone(),
        choice: 2,
        ..QuestionBox::default()
    };
    typed.input.text = "convert, but log every mismatch".into();
    typed.input.cursor = 7;
    typed.input.notice = Some("pick an answer with ↑↓, or type your own".into());
    app.modal = Some(Modal::Question(typed));
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen.contains("▸ 3. › convert, but log every mismatch"),
        "{screen}"
    );
    assert!(
        screen.contains("pick an answer with ↑↓, or type your own"),
        "{screen}"
    );

    // A long question on a short screen scrolls instead of being cut off.
    let mut long = question.clone();
    long.id = "m-long".into();
    long.body = (1..=40)
        .map(|i| format!("Line {i} of the question."))
        .collect::<Vec<_>>()
        .join("\n");
    app.questions = vec![long.clone()];
    app.modal = Some(Modal::Question(QuestionBox {
        id: long.id.clone(),
        ..QuestionBox::default()
    }));
    let screen = render_to_string(&app, 100, 24, NOW);
    assert!(
        screen.contains("Line 1 of the question.") && screen.contains("more line(s) · pgdn"),
        "{screen}"
    );
    assert!(
        screen.contains("▸ 1. Convert in the adapter") && screen.contains("esc later"),
        "{screen}"
    );
    app.modal = Some(Modal::Question(QuestionBox {
        id: long.id,
        scroll: 500,
        ..QuestionBox::default()
    }));
    let screen = render_to_string(&app, 100, 24, NOW);
    assert!(
        screen.contains("Line 40 of the question.") && !screen.contains("more line(s)"),
        "{screen}"
    );

    // Answered meanwhile, tiny screens: nothing breaks.
    app.questions.clear();
    assert!(render_to_string(&app, 100, 24, NOW).contains("This question has been answered."));
    app.questions = vec![question.clone()];
    app.modal = Some(Modal::Question(QuestionBox {
        id: question.id,
        ..QuestionBox::default()
    }));
    for (w, h) in [
        (1, 1),
        (10, 3),
        (24, 7),
        (30, 8),
        (40, 10),
        (60, 14),
        (300, 80),
    ] {
        let screen = render_to_string(&app, w, h, NOW);
        assert!(
            screen
                .lines()
                .all(|l| crew_tui::widgets::text::width(l) <= usize::from(w)),
            "{w}x{h}"
        );
    }
}

#[test]
fn the_inspector_shows_how_full_the_context_window_is() {
    use crew_core::types::ContextUsage;
    let mut app = app();
    app.open_detail("claude-3", NOW);
    let screen = render_to_string(&app, 140, 44, NOW);
    assert!(
        !screen.contains("% full"),
        "nothing is shown before the runtime reports it:\n{screen}"
    );

    let agent = app.agents.iter_mut().find(|a| a.id == "claude-3").unwrap();
    agent.context = Some(ContextUsage::new(164_000, 200_000, NOW));
    agent.compact_pending = true;
    agent.compactions = 2;
    let screen = render_to_string(&app, 140, 44, NOW);
    assert!(
        screen.contains("context   82% full · 164k of 200k"),
        "{screen}"
    );
    assert!(
        screen.contains("compact   queued · done 2× so far"),
        "{screen}"
    );

    // Right after a compaction there is no figure yet, only the count.
    let agent = app.agents.iter_mut().find(|a| a.id == "claude-3").unwrap();
    agent.context = None;
    agent.compact_pending = false;
    let screen = render_to_string(&app, 140, 44, NOW);
    assert!(
        screen.contains("compact   done 2×") && !screen.contains("% full"),
        "{screen}"
    );
    assert_eq!(ContextUsage::new(5, 0, NOW).percent, 0);
    assert_eq!(ContextUsage::new(300_000, 200_000, NOW).percent, 100);
}

#[test]
fn a_bus_message_opens_in_full() {
    use crew_tui::app::MessageView;
    let mut app = app();
    let mut long = app.messages.back().unwrap().clone();
    long.id = "m-long".into();
    long.body = (1..=60)
        .map(|i| format!("Paragraph {i} of the report, which the bus log cuts short."))
        .collect::<Vec<_>>()
        .join("\n");
    app.messages.push_back(long.clone());
    app.modal = Some(Modal::Message(MessageView {
        id: long.id.clone(),
        scroll: 0,
    }));
    let screen = render_to_string(&app, 120, 30, NOW);
    assert!(
        screen.contains(&format!("{} → {} · {}", long.from, long.to, long.kind)),
        "{screen}"
    );
    assert!(
        screen.contains("Paragraph 1 of the report") && !screen.contains("Paragraph 60 of"),
        "{screen}"
    );
    assert!(
        screen.contains("more line(s)") && screen.contains("esc or right-click close"),
        "{screen}"
    );
    // Scrolled past the end, it stops at the last line.
    app.modal = Some(Modal::Message(MessageView {
        id: long.id,
        scroll: 999,
    }));
    let screen = render_to_string(&app, 120, 30, NOW);
    assert!(
        screen.contains("Paragraph 60 of the report") && !screen.contains("more line(s)"),
        "{screen}"
    );
    // A message that left the log, and screens of every size.
    app.modal = Some(Modal::Message(MessageView {
        id: "m-gone".into(),
        scroll: 0,
    }));
    assert!(render_to_string(&app, 120, 30, NOW).contains("no longer in the log"));
    for (w, h) in [(1, 1), (20, 4), (24, 6), (40, 8), (300, 80)] {
        for id in ["m-long", "m-gone"] {
            app.modal = Some(Modal::Message(MessageView {
                id: id.into(),
                scroll: 3,
            }));
            let screen = render_to_string(&app, w, h, NOW);
            assert!(
                screen
                    .lines()
                    .all(|l| crew_tui::widgets::text::width(l) <= usize::from(w)),
                "{w}x{h}"
            );
        }
    }
}

#[test]
fn each_box_names_its_model_and_effort_on_its_bottom_border() {
    use crew_core::types::{model_display_name, short_model};
    use crew_tui::widgets::agent_box::running_label;
    // The bottom border of an agent's box, as drawn: on a 120-column screen
    // the boxes are 30 wide, on a 72-column one 22.
    let border_at = |app: &App, id: &str, w: u16| -> String {
        let area = dashboard_area(app, w, 40);
        let screen = render_to_string(app, w, 40, NOW);
        let rects = crew_tui::views::dashboard::agent_rects(app, area);
        let (_, rect) = rects.iter().find(|(agent, _)| agent == id).unwrap();
        screen
            .lines()
            .nth(usize::from(rect.bottom()) - 1)
            .unwrap()
            .chars()
            .skip(usize::from(rect.x))
            .take(usize::from(rect.width))
            .collect()
    };
    let border_of = |app: &App, id: &str| border_at(app, id, 120);
    let narrow_border_of = |app: &App, id: &str| border_at(app, id, 72);

    // The model by its proper name and the effort, in the border itself.
    let mut app = app();
    let screen = render_to_string(&app, 120, 40, NOW);
    assert_eq!(screen.matches(" Sonnet 5.5 · high ").count(), 7, "{screen}");
    assert_eq!(
        border_of(&app, "claude-1"),
        "└──── Sonnet 5.5 · high ─────┘"
    );
    assert_eq!(narrow_border_of(&app, "claude-1"), "└ Sonnet 5.5 · high ─┘");
    // A border that carries a label is a plain line whatever the progress
    // (half done, done): no stray pieces of the bar beside the name.
    assert_eq!(
        border_of(&app, "claude-2"),
        "└──── Sonnet 5.5 · high ─────┘"
    );
    assert!(
        !screen
            .lines()
            .any(|l| l.contains("Sonnet 5.5") && l.contains('━')),
        "{screen}"
    );
    // The inspector spells it out.
    app.open_detail("claude-3", NOW);
    assert!(render_to_string(&app, 140, 44, NOW).contains("runtime   claude · Sonnet 5.5 · high"));
    app.view = View::Dashboard;

    // It sits in the middle of the border, the line running on at both ends.
    let set = |app: &mut App,
               id: &str,
               model: Option<&str>,
               name: Option<&str>,
               running: Option<&str>,
               effort: Option<&str>| {
        let agent = app.agents.iter_mut().find(|a| a.id == id).unwrap();
        agent.model = model.map(str::to_string);
        agent.model_name = name.map(str::to_string);
        agent.running_model = running.map(str::to_string);
        agent.effort = effort.map(str::to_string);
    };
    set(
        &mut app,
        "claude-1",
        Some("opus"),
        Some("Opus 5.5"),
        None,
        None,
    );
    let border = border_of(&app, "claude-1");
    assert_eq!(border.chars().count(), 30);
    assert!(border.contains(" Opus 5.5 "), "{border}");
    let (left, right) = border.split_once(" Opus 5.5 ").unwrap();
    assert!(
        left.chars().count().abs_diff(right.chars().count()) <= 1,
        "{border}"
    );
    assert!(left.chars().count() >= 5, "{border}");

    // Without a name from the daemon it is worked out: from what the
    // session reports, else from the name in the team file.
    set(
        &mut app,
        "claude-2",
        Some("opus"),
        None,
        Some("claude-opus-5-5"),
        Some("xhigh"),
    );
    assert!(border_of(&app, "claude-2").contains(" Opus 5.5 · xhigh "));
    set(&mut app, "claude-2", Some("fable"), None, None, None);
    assert!(border_of(&app, "claude-2").contains(" Fable "));
    // A long name gives way a step at a time in a narrow box: a short
    // level, no spaces round the dot, the model alone.
    set(
        &mut app,
        "claude-2",
        None,
        Some("Sonnet 5.5"),
        None,
        Some("medium"),
    );
    assert!(
        border_of(&app, "claude-2").contains(" Sonnet 5.5 · medium "),
        "{}",
        border_of(&app, "claude-2")
    );
    assert!(
        narrow_border_of(&app, "claude-2").contains(" Sonnet 5.5 · med "),
        "{}",
        narrow_border_of(&app, "claude-2")
    );
    set(
        &mut app,
        "claude-2",
        None,
        Some("GPT 6 Astra"),
        None,
        Some("xhigh"),
    );
    assert_eq!(
        narrow_border_of(&app, "claude-2")
            .chars()
            .filter(|c| !"└┘━─".contains(*c))
            .collect::<String>(),
        " GPT 6 Astra·xhigh "
    );
    // An agent whose runtime has not said yet has a plain border.
    set(&mut app, "claude-3", None, None, None, None);
    // Its border is the progress bar, as before.
    assert!(border_of(&app, "claude-3")
        .chars()
        .all(|c| "└┘━─".contains(c)));
    assert!(border_of(&app, "claude-3").contains('━'));

    let agent = app.agents.iter().find(|a| a.id == "claude-2").unwrap();
    assert_eq!(
        running_label(agent, 30).as_deref(),
        Some("GPT 6 Astra · xhigh")
    );
    assert_eq!(
        running_label(agent, 17).as_deref(),
        Some("GPT 6 Astra·xhigh")
    );
    assert_eq!(running_label(agent, 12).as_deref(), Some("GPT 6 Astra"));
    assert_eq!(running_label(agent, 7).as_deref(), Some("GPT 6 …"));
    assert_eq!(running_label(agent, 3), None);
    let silent = app.agents.iter().find(|a| a.id == "claude-3").unwrap();
    assert_eq!(running_label(silent, 30), None);

    for (id, name) in [
        ("claude-opus-5-5", "Opus 5.5"),
        ("claude-fable-5-1", "Fable 5.1"),
        ("claude-haiku-4-5-20251001", "Haiku 4.5"),
        ("claude-sonnet-5-5[1m]", "Sonnet 5.5"),
        ("gpt-6-astra", "GPT 6 Astra"),
        ("gpt-6.1-sol", "GPT 6.1 Sol"),
        ("gpt-6-luna", "GPT 6 Luna"),
        ("opus", "Opus"),
        ("house-model", "House Model"),
        ("", ""),
    ] {
        assert_eq!(model_display_name(id), name, "{id}");
    }
    assert_eq!(short_model("claude-haiku-4-5-20251001"), "haiku-4-5");

    // Nothing spills at any size.
    for (w, h) in [(40, 12), (60, 20), (80, 24), (100, 30), (200, 60)] {
        let screen = render_to_string(&app, w, h, NOW);
        assert!(
            screen
                .lines()
                .all(|l| crew_tui::widgets::text::width(l) <= usize::from(w)),
            "{w}x{h}"
        );
    }
}

#[test]
fn the_boxes_show_how_full_each_context_window_is() {
    use crew_core::types::ContextUsage;
    use crew_tui::views::dashboard::agent_rects;
    use crew_tui::widgets::agent_box::window_size;

    // Nobody reports a context: the boxes have the task and what the agent
    // is doing, nothing more.
    let mut app = app();
    let area = dashboard_area(&app, 120, 40);
    assert!(agent_rects(&app, area)
        .iter()
        .filter(|(id, _)| id != "orchestrator")
        .all(|(_, r)| r.height == 4));
    assert!(!render_to_string(&app, 120, 40, NOW).contains("context —"));

    // Once one does, every box gets a row for it: the window, a bar, the share.
    let set = |app: &mut App, id: &str, tokens: u64, window: u64| {
        app.agents.iter_mut().find(|a| a.id == id).unwrap().context =
            Some(ContextUsage::new(tokens, window, NOW));
    };
    set(&mut app, "claude-1", 350_000, 1_000_000);
    set(&mut app, "claude-2", 164_000, 200_000);
    set(&mut app, "claude-4", 930_000, 1_000_000);
    set(&mut app, "orchestrator", 12_000, 258_400);
    let screen = render_to_string(&app, 120, 40, NOW);
    let rects = agent_rects(&app, area);
    for (id, rect) in &rects {
        assert_eq!(rect.height, 5, "{id}");
    }
    let row_of = |id: &str| -> String {
        let (_, rect) = rects.iter().find(|(agent, _)| agent == id).unwrap();
        screen
            .lines()
            .nth(usize::from(rect.bottom()) - 2)
            .unwrap()
            .chars()
            .skip(usize::from(rect.x))
            .take(usize::from(rect.width))
            .collect()
    };
    assert_eq!(row_of("claude-1"), "│ 1M ██████▎             35% │");
    assert_eq!(row_of("claude-2"), "│ 200k █████████████▏    82% │");
    assert_eq!(row_of("claude-4"), "│ 1M ████████████████▊   93% │");
    assert_eq!(row_of("orchestrator"), "│ 258k ▌              4% │");
    // An agent that has not reported yet says so.
    assert_eq!(row_of("claude-3"), "│ context —                  │");
    // The other rows are where they were: the task and what the agent is
    // doing above, the model below.
    assert!(
        screen.contains("│ implement billing   ◓  51% │"),
        "{screen}"
    );
    assert!(
        screen.contains("│ ▸ Edit src/lib.rs   11m40s │"),
        "{screen}"
    );
    assert_eq!(screen.matches(" Sonnet 5.5 · high ").count(), 7, "{screen}");
    // A click still lands on the taller boxes, bottom row included: that
    // row says what the agent runs on, and a click there is for choosing it.
    let (_, rect) = rects.iter().find(|(id, _)| id == "claude-2").unwrap();
    assert_eq!(
        crew_tui::views::hit(
            &app,
            ratatui::layout::Rect::new(0, 0, 120, 40),
            rect.x + 3,
            rect.bottom() - 2
        ),
        Some(crew_tui::views::dashboard::Hit::Agent("claude-2".into()))
    );
    assert_eq!(
        crew_tui::views::hit(
            &app,
            ratatui::layout::Rect::new(0, 0, 120, 40),
            rect.x + 3,
            rect.bottom() - 1
        ),
        Some(crew_tui::views::dashboard::Hit::Model("claude-2".into()))
    );

    // Where the taller boxes do not fit, the shorter ones are drawn.
    let short = dashboard_area(&app, 120, 24);
    let tall_fits = agent_rects(&app, short).iter().any(|(_, r)| r.height == 5);
    let screen = render_to_string(&app, 120, 24, NOW);
    assert_eq!(screen.contains("│ 1M "), tall_fits, "{screen}");
    for (w, h) in [(40, 12), (60, 18), (80, 24), (100, 30), (160, 50)] {
        let screen = render_to_string(&app, w, h, NOW);
        assert!(
            screen
                .lines()
                .all(|l| crew_tui::widgets::text::width(l) <= usize::from(w)),
            "{w}x{h}"
        );
    }

    assert_eq!(window_size(1_000_000), "1M");
    assert_eq!(window_size(1_500_000), "1.5M");
    assert_eq!(window_size(258_400), "258k");
    assert_eq!(window_size(200_000), "200k");
    assert_eq!(window_size(900), "900");
}

#[test]
fn each_runtime_has_its_own_glyph_and_colour() {
    let theme = Theme::truecolor();
    // The colour of the cell `skip` columns into the first `what` on screen.
    let color_of = |app: &App, size: (u16, u16), what: &str, skip: u16| {
        let mut terminal = Terminal::new(TestBackend::new(size.0, size.1)).unwrap();
        terminal
            .draw(|frame| views::render(frame, app, &theme, NOW))
            .unwrap();
        let buf = terminal.backend().buffer();
        let cells: Vec<String> = what.chars().map(String::from).collect();
        let area = buf.area;
        for y in area.y..area.bottom() {
            for x in area.x..=area.right() - cells.len() as u16 {
                if cells
                    .iter()
                    .enumerate()
                    .all(|(i, c)| buf[(x + i as u16, y)].symbol() == c)
                {
                    return buf[(x + skip, y)].fg;
                }
            }
        }
        panic!(
            "{what} is not on screen\n{}",
            render_to_string(app, size.0, size.1, NOW)
        );
    };

    let mut app = app();
    app.selected = None;
    p2p(&mut app, "claude-2", "claude-1");
    let big = (120, 40);
    // Claude Code and Codex agents differ in glyph and in colour, name included.
    assert_eq!(color_of(&app, big, "✱ claude-1", 0), theme.accent);
    assert_eq!(color_of(&app, big, "✱ claude-1", 2), theme.accent);
    assert_eq!(color_of(&app, big, "❂ claude-2", 0), theme.codex);
    assert_eq!(color_of(&app, big, "❂ claude-2", 2), theme.codex);
    assert_ne!(theme.codex, theme.accent);
    // The orchestrator keeps its own mark.
    assert_eq!(color_of(&app, big, "◆ orchestrator", 0), theme.orch);
    // On the bus a name wears its agent's colour.
    assert_eq!(color_of(&app, big, "claude-2 → claude-1", 0), theme.codex);
    assert_eq!(color_of(&app, big, "claude-2 → claude-1", 11), theme.accent);
    assert_eq!(color_of(&app, big, "orchestrator ⇉", 0), theme.orch);

    // A selected box is named in white; its glyph still tells the runtime.
    app.selected = Some("claude-2".to_string());
    assert_eq!(color_of(&app, big, "❂ claude-2", 0), theme.codex);
    assert_eq!(color_of(&app, big, "❂ claude-2", 2), theme.text);
    app.selected = None;

    // A subagent is tethered in its runtime's colour, and a shell agent has
    // a mark of its own.
    let mut snap = fixture::with_subagents(NOW);
    for agent in &mut snap.agents {
        match agent.id.as_str() {
            "claude-3" => agent.runtime = crew_core::types::RuntimeKind::Shell,
            "claude-3-s1" => agent.runtime = crew_core::types::RuntimeKind::Codex,
            _ => {}
        }
    }
    let mut mixed = App::new();
    mixed.apply_snapshot(snap, NOW);
    mixed.selected = None;
    assert_eq!(color_of(&mixed, big, "$ claude-3", 0), theme.shell);
    assert_eq!(color_of(&mixed, big, "$ claude-3", 2), theme.shell);
    assert_eq!(color_of(&mixed, big, "↳ claude-3-s1", 0), theme.codex);

    // The one-row-per-agent list of a small terminal uses the same marks.
    let small = (60, 16);
    let screen = render_to_string(&app, small.0, small.1, NOW);
    assert!(!screen.contains('┌'), "{screen}");
    assert_eq!(color_of(&app, small, "✱ claude-1", 0), theme.accent);
    assert_eq!(color_of(&app, small, "❂ claude-2", 2), theme.codex);

    // The inspector's heading too.
    app.selected = Some("claude-2".to_string());
    app.detail.agent = "claude-2".to_string();
    app.view = View::Detail;
    assert_eq!(color_of(&app, big, "❂ claude-2", 0), theme.codex);
    // And the help overlay says what the marks mean.
    app.view = View::Dashboard;
    app.help = true;
    let screen = render_to_string(&app, 120, 44, NOW);
    assert!(
        screen.contains("an agent on Claude Code · Codex · pi · a shell"),
        "{screen}"
    );
    // A pi member has its own mark and colour too.
    let pi = app.agents.iter_mut().find(|a| a.id == "claude-3").unwrap();
    pi.runtime = crew_core::types::RuntimeKind::Pi;
    app.help = false;
    let screen = render_to_string(&app, 120, 44, NOW);
    assert!(screen.contains("π claude-3"), "{screen}");
    assert_eq!(color_of(&app, (120, 44), "π claude-3", 0), theme.pi);
    assert_ne!(theme.pi, theme.codex);
}

#[test]
fn the_inspector_scrolled_up_shows_the_session_transcript() {
    use crew_core::rpc::TranscriptResult;
    use crew_core::types::{TranscriptEntry, TranscriptKind};

    let mut app = app();
    app.open_detail("claude-2", NOW);
    app.apply_reply(
        Reply::Output {
            agent: "claude-2".into(),
            result: Ok(OutputResult {
                text: "› Ask Codex to do anything\n  GPT-6-Astra max".into(),
                alive: true,
            }),
        },
        NOW,
    );
    // At the bottom the pane is the live terminal, and says what is above.
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen.contains("terminal · live · 0s ago · ↑ transcript"),
        "{screen}"
    );
    assert!(screen.contains("› Ask Codex to do anything"), "{screen}");

    // Scrolled up before the transcript has arrived, it says it is reading.
    app.detail.scroll = 1;
    app.detail.transcript_pending = true;
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("reading the transcript…"), "{screen}");
    assert!(screen.contains("› Ask Codex to do anything"), "{screen}");

    let entry = |kind, text: &str| TranscriptEntry {
        kind,
        at: Some(NOW - 60_000),
        text: text.to_string(),
    };
    app.apply_reply(
        Reply::Transcript {
            agent: "claude-2".into(),
            result: Ok(TranscriptResult {
                entries: vec![
                    entry(TranscriptKind::Prompt, "[crew] Message from orchestrator (type task, id m-1):\n\nReview the queue design."),
                    entry(TranscriptKind::Reply, "I'll read the design note first."),
                    entry(TranscriptKind::Tool, "exec  cat docs/queue.md"),
                    entry(TranscriptKind::Tool, "crew.agent_send  {\"to\":\"claude-1\"}"),
                    entry(TranscriptKind::Reply, "The retry path loses messages when the broker restarts."),
                ],
                available: true,
                truncated: false,
            }),
        },
        NOW,
    );
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen.contains("transcript · read 0s ago · ↑1 of 11 · ↓ live terminal"),
        "{screen}"
    );
    assert!(!screen.contains("› Ask Codex to do anything"), "{screen}");
    let at = |what: &str| {
        screen
            .lines()
            .position(|l| l.contains(what))
            .unwrap_or_else(|| panic!("{what}\n{screen}"))
    };
    let rows = [
        at("── prompt · "),
        at("[crew] Message from orchestrator (type task, id m-1):"),
        at("Review the queue design."),
        at("I'll read the design note first."),
        at("▸ exec  cat docs/queue.md"),
        at("▸ crew.agent_send  {\"to\":\"claude-1\"}"),
        at("The retry path loses messages when the broker restarts."),
    ];
    assert!(rows.windows(2).all(|w| w[0] < w[1]), "{rows:?}\n{screen}");
    // The two tool calls sit together; the reply after them stands apart.
    assert_eq!(rows[5], rows[4] + 1);
    assert_eq!(rows[6], rows[5] + 2);

    // Further up, the end of the transcript scrolls out of the pane.
    app.detail.scroll = 400;
    let screen = render_to_string(&app, 120, 14, NOW);
    assert!(screen.contains("── prompt · "), "{screen}");
    assert!(
        !screen.contains("The retry path loses messages"),
        "{screen}"
    );
    // And at the bottom again it is the terminal.
    app.detail.scroll = 0;
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen.contains("› Ask Codex to do anything") && !screen.contains("── prompt"),
        "{screen}"
    );

    // An agent without a session file keeps scrolling its terminal.
    let mut snap = fixture::snapshot(NOW);
    snap.agents
        .iter_mut()
        .find(|a| a.id == "claude-2")
        .unwrap()
        .runtime = crew_core::types::RuntimeKind::Shell;
    let mut shell = App::new();
    shell.apply_snapshot(snap, NOW);
    shell.open_detail("claude-2", NOW);
    shell.apply_reply(
        Reply::Output {
            agent: "claude-2".into(),
            result: Ok(OutputResult {
                text: (0..60).map(|i| format!("line {i}\n")).collect(),
                alive: true,
            }),
        },
        NOW,
    );
    let screen = render_to_string(&shell, 120, 40, NOW);
    assert!(
        screen.contains("terminal · live · 0s ago ─") && !screen.contains("↑ transcript"),
        "{screen}"
    );
    shell.detail.scroll = 50;
    shell.apply_reply(
        Reply::Transcript {
            agent: "claude-2".into(),
            result: Ok(TranscriptResult::default()),
        },
        NOW,
    );
    let screen = render_to_string(&shell, 120, 40, NOW);
    assert!(
        screen.contains("line 0") && !screen.contains("line 59"),
        "{screen}"
    );
}

#[test]
fn a_team_that_waits_for_the_operator_shows_it_on_the_overview() {
    let theme = Theme::truecolor();
    let mut app = app();
    app.selected = None;
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        !screen.contains("answer needed") && !screen.contains("answers needed"),
        "{screen}"
    );

    // One question, passed on for claude-3: the hub says the team waits, and
    // so does the box of the member who wants to know.
    let mut question = fixture::question(NOW);
    question.metadata = serde_json::json!({ "asked_by": "claude-3" });
    app.questions = vec![question.clone()];
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("│ ? your answer needed"), "{screen}");
    let member = screen
        .lines()
        .position(|l| l.contains("✱ claude-3"))
        .unwrap();
    assert!(
        screen.lines().nth(member + 1).unwrap().contains(" ? "),
        "{screen}"
    );
    let other = screen
        .lines()
        .position(|l| l.contains("✱ claude-1"))
        .unwrap();
    assert!(
        !screen.lines().nth(other + 1).unwrap().contains(" ? "),
        "{screen}"
    );

    // In the colour of things that need attention, border included.
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| views::render(frame, &app, &theme, NOW))
        .unwrap();
    let buf = terminal.backend().buffer();
    let find = |what: &str| {
        let cells: Vec<String> = what.chars().map(String::from).collect();
        for y in 0..40u16 {
            for x in 0..=120 - cells.len() as u16 {
                if cells
                    .iter()
                    .enumerate()
                    .all(|(i, c)| buf[(x + i as u16, y)].symbol() == c)
                {
                    return (x, y);
                }
            }
        }
        panic!("{what} is not on screen");
    };
    let (x, y) = find("? your answer needed");
    assert_eq!(buf[(x, y)].fg, theme.warn);
    assert_eq!(buf[(x + 2, y)].fg, theme.warn);
    assert_eq!(buf[(x - 2, y)].fg, theme.warn, "the hub's border");

    // Several, after the work is done: that is what the footer says too.
    let mut second = fixture::question(NOW);
    second.id = "m-9002".into();
    app.questions.push(second);
    app.session.completed_at = Some(NOW - 5_000);
    app.session.summary = Some("Everything is built.".into());
    let screen = render_to_string(&app, 160, 40, NOW);
    assert!(screen.contains("│ ? 2 answers needed"), "{screen}");
    assert!(!screen.contains("✓ all tasks done   "), "{screen}");
    assert!(
        screen.contains("✓ all tasks done · ? 2 answers are needed"),
        "{screen}"
    );
    assert!(!screen.contains("Everything is built."), "{screen}");
    // Answered, the team is simply done.
    app.questions.clear();
    let screen = render_to_string(&app, 160, 40, NOW);
    assert!(
        screen.contains("│ ✓ all tasks done") && screen.contains("· Everything is built."),
        "{screen}"
    );

    // The small-terminal list marks the same agents.
    app.questions = vec![question];
    let screen = render_to_string(&app, 60, 16, NOW);
    let hub = screen
        .lines()
        .find(|l| l.contains("◆ orchestrator"))
        .unwrap();
    let asker = screen.lines().find(|l| l.contains("✱ claude-3")).unwrap();
    let quiet = screen.lines().find(|l| l.contains("✱ claude-1")).unwrap();
    assert!(
        hub.contains(" ? ") && asker.contains(" ? ") && !quiet.contains(" ? "),
        "{screen}"
    );
}

#[test]
fn the_live_terminal_keeps_its_colours_and_the_type_box_is_drawn() {
    let theme = Theme::truecolor();
    let mut app = app();
    app.open_detail("claude-1", NOW);
    app.apply_reply(
        Reply::Output {
            agent: "claude-1".into(),
            result: Ok(OutputResult {
                text: "plain\n\x1b[31mred\x1b[0m then \x1b[1;38;2;10;200;10mbold green\x1b[m\n\x1b]0;title\x07❯ \x1b[2Kcursor".into(),
                alive: true,
            }),
        },
        NOW,
    );
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("red then bold green"), "{screen}");
    assert!(
        screen.contains("❯ cursor") && !screen.contains("title"),
        "{screen}"
    );

    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| views::render(frame, &app, &theme, NOW))
        .unwrap();
    let buf = terminal.backend().buffer();
    let find = |what: &str| {
        let cells: Vec<String> = what.chars().map(String::from).collect();
        for y in 0..40u16 {
            for x in 0..=120 - cells.len() as u16 {
                if cells
                    .iter()
                    .enumerate()
                    .all(|(i, c)| buf[(x + i as u16, y)].symbol() == c)
                {
                    return (x, y);
                }
            }
        }
        panic!(
            "{what} is not on screen\n{}",
            render_to_string(&app, 120, 40, NOW)
        );
    };
    let (x, y) = find("red then bold green");
    assert_eq!(buf[(x, y)].fg, ratatui::style::Color::Red);
    assert_eq!(
        buf[(x + 4, y)].fg,
        theme.text,
        "a reset goes back to the pane's colour"
    );
    let green = &buf[(x + 9, y)];
    assert_eq!(green.fg, ratatui::style::Color::Rgb(10, 200, 10));
    assert!(green.modifier.contains(Modifier::BOLD));

    // The box that types into the terminal.
    app.modal = Some(Modal::Type(Compose {
        to: "claude-1".into(),
        text: "hello there".into(),
        cursor: 11,
        ..Compose::default()
    }));
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("type into claude-1's terminal"), "{screen}");
    assert!(screen.contains("❯ hello there"), "{screen}");
    assert!(
        screen.contains("enter alone presses ⏎ · esc back"),
        "{screen}"
    );
}

#[test]
fn the_terminal_pane_takes_the_inspector_with_f_and_shows_when_it_is_typed_into() {
    let theme = Theme::truecolor();
    let mut app = app();
    app.open_detail("claude-1", NOW);
    app.apply_reply(
        Reply::Output {
            agent: "claude-1".into(),
            result: Ok(OutputResult {
                text: "❯ ".into(),
                alive: true,
            }),
        },
        NOW,
    );
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen.contains("messages") && screen.contains("errors") && screen.contains("tasks"),
        "{screen}"
    );
    assert!(screen.contains("↑ transcript · click to type"), "{screen}");
    // Drawing records where the pane is, and a click there is into it.
    let pane = app.detail.terminal_rect.get();
    assert!(pane.width > 40 && pane.height > 10, "{pane:?}");
    assert_eq!(
        views::hit(
            &app,
            ratatui::layout::Rect::new(0, 0, 120, 40),
            pane.x + 2,
            pane.y + 2
        ),
        Some(views::dashboard::Hit::Terminal)
    );
    assert_eq!(
        views::hit(&app, ratatui::layout::Rect::new(0, 0, 120, 40), 2, 5),
        None
    );

    app.detail.full = true;
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        !screen.contains("messages") && !screen.contains("errors") && !screen.contains("tasks ─"),
        "{screen}"
    );
    assert!(screen.contains("terminal · live"), "{screen}");
    let wide = app.detail.terminal_rect.get();
    assert!(wide.width >= 118 && wide.height >= 36, "{wide:?}");

    // Typed into, the pane says so, in the colour of things that are live.
    app.detail.typing = true;
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(
        screen.contains("typing: keys go to the agent · ctrl-] stops"),
        "{screen}"
    );
    assert!(
        screen.contains("ctrl-] stop typing (right-click too)"),
        "{screen}"
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| views::render(frame, &app, &theme, NOW))
        .unwrap();
    let buf = terminal.backend().buffer();
    assert_eq!(
        buf[(wide.x, wide.y + 1)].fg,
        theme.accent,
        "the pane's border"
    );
}

#[test]
fn a_session_off_its_model_is_marked_on_its_box_and_explained_in_the_inspector() {
    use crew_core::types::ModelFallback;
    let theme = Theme::truecolor();
    let mut app = app();
    let area = dashboard_area(&app, 120, 40);
    let agent = app.agents.iter_mut().find(|a| a.id == "claude-1").unwrap();
    agent.fallback = Some(ModelFallback {
        from: "Opus 5.5".into(),
        note: "Switched to Sonnet 5.5 for this session · Opus 5.5 requires usage credits · /model to change".into(),
        since: NOW - 60_000,
        policy: false,
    });
    // The label on the border is marked and in the warning colour; the
    // other boxes are as before.
    let screen = render_to_string(&app, 120, 40, NOW);
    assert_eq!(
        screen.matches(" ↓ Sonnet 5.5 · high ").count(),
        1,
        "{screen}"
    );
    // Six boxes and the hub, the marked one included.
    assert_eq!(screen.matches(" Sonnet 5.5 · high ").count(), 7, "{screen}");
    let rects = crew_tui::views::dashboard::agent_rects(&app, area);
    let (_, rect) = rects.iter().find(|(id, _)| id == "claude-1").unwrap();
    let row = screen.lines().nth(usize::from(rect.bottom()) - 1).unwrap();
    let at = row.find("↓ Sonnet").unwrap();
    let x = row[..at].chars().count() as u16;
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| views::render(frame, &app, &theme, NOW))
        .unwrap();
    let cell = &terminal.backend().buffer()[(x, rect.bottom() - 1)];
    assert_eq!(cell.fg, theme.warn, "{row}");
    assert!(cell.modifier.contains(Modifier::BOLD));

    // The inspector says what it runs on instead of what, and why.
    app.open_detail("claude-1", NOW);
    let screen = render_to_string(&app, 160, 44, NOW);
    assert!(
        screen.contains("runtime   claude · Sonnet 5.5 · high"),
        "{screen}"
    );
    assert!(
        screen.contains("fallback  ↓ on Sonnet 5.5 instead of Opus 5.5 — Swi"),
        "{screen}"
    );
}

#[test]
fn each_box_says_which_task_its_member_is_on_and_what_it_is_doing_now() {
    use crew_tui::widgets::agent_box::{doing, Decor, Tone};
    let mut app = app();
    let area = dashboard_area(&app, 120, 40);
    // The rows of a box, inside its borders.
    let rows_of = |app: &App, id: &str| -> Vec<String> {
        let screen = render_to_string(app, 120, 40, NOW);
        let rects = crew_tui::views::dashboard::agent_rects(app, area);
        let (_, rect) = rects.iter().find(|(agent, _)| agent == id).unwrap();
        screen
            .lines()
            .skip(usize::from(rect.y) + 1)
            .take(usize::from(rect.height) - 2)
            .map(|l| {
                l.chars()
                    .skip(usize::from(rect.x) + 2)
                    .take(usize::from(rect.width) - 4)
                    .collect::<String>()
            })
            .collect()
    };
    // The task it is on (not its role), and the tool it is running with how
    // long it has been on the task.
    assert_eq!(
        rows_of(&app, "claude-1"),
        ["implement billing   ◓  51%", "▸ Edit src/lib.rs   11m40s"]
    );
    // Every report from the session shows at once.
    let agent = app.agents.iter_mut().find(|a| a.id == "claude-1").unwrap();
    agent.activity = "$ cargo test --workspace".into();
    assert_eq!(rows_of(&app, "claude-1")[1], "▸ $ cargo test --w… 11m40s");
    // A finished member says what it finished, and when.
    assert_eq!(
        rows_of(&app, "claude-2"),
        ["implement queue     ✓ 100%", "✓ finished       1m00s ago"]
    );

    // Other states say what they are.
    let set = |app: &mut App, id: &str, f: &dyn Fn(&mut crew_core::types::Agent)| {
        f(app.agents.iter_mut().find(|a| a.id == id).unwrap());
    };
    set(&mut app, "claude-3", &|a| {
        a.status = AgentStatus::Blocked;
        a.activity = "usage limit reached · resets 3:19 PM".into();
    });
    assert_eq!(rows_of(&app, "claude-3")[1], "usage limit reached · res…");
    set(&mut app, "claude-4", &|a| {
        a.status = AgentStatus::Failed;
        a.error = Some("failed to start: no such file".into());
    });
    assert_eq!(rows_of(&app, "claude-4")[1], "failed to start: no such …");
    // A member without a task shows its role, and how long it has been idle.
    app.tasks
        .retain(|t| t.assigned_agent.as_deref() != Some("claude-5"));
    set(&mut app, "claude-5", &|a| {
        a.status = AgentStatus::Idle;
        a.task = None;
        a.progress = None;
        a.last_activity_at = NOW - 300_000;
    });
    assert_eq!(
        rows_of(&app, "claude-5"),
        ["db                  · idle", "idle                 5m00s"]
    );

    // The tones: live while working, a warning when blocked, an error when
    // failed, quiet otherwise.
    let tone = |id: &str| {
        let agent = app.agents.iter().find(|a| a.id == id).unwrap();
        let decor = Decor {
            work: app.work_of(id, NOW),
            ..Decor::default()
        };
        doing(agent, &decor).2
    };
    assert_eq!(
        (
            tone("claude-1"),
            tone("claude-2"),
            tone("claude-3"),
            tone("claude-4")
        ),
        (Tone::Live, Tone::Quiet, Tone::Warn, Tone::Bad)
    );

    // Where only one row per member fits, the list carries both: the task,
    // then what a working member is doing.
    let compact = render_to_string(&app, 100, 6, NOW);
    let row = compact.lines().find(|l| l.contains("claude-1")).unwrap();
    assert!(
        row.contains("implement billing · ▸ $ cargo test --workspace"),
        "{compact}"
    );
}

#[test]
fn the_hub_says_its_cli_and_the_picker_chooses_the_orchestrators_model_and_cli() {
    use crew_core::RuntimeKind;
    use crew_tui::app::Command;
    let mut app = app();
    // The hub says which CLI the orchestrator runs on, as a member's box does.
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("─ ✱ Sonnet 5.5 · high ─"), "{screen}");

    assert_eq!(
        app.open_model_picker("orchestrator", NOW),
        vec![Command::FetchModels]
    );
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(screen.contains("orchestrator · model and CLI"), "{screen}");
    assert!(
        screen.contains("asking the daemon what can be started here…"),
        "{screen}"
    );
    app.apply_reply(Reply::Models(Ok(fixture::models())), NOW);
    let screen = render_to_string(&app, 120, 40, NOW);
    // A tab per CLI, the orchestrator's own marked; its models, with the
    // one it runs on highlighted and marked.
    assert!(screen.contains(" 1 ✱ Claude Code ● "), "{screen}");
    assert!(screen.contains(" 2 ❂ Codex "), "{screen}");
    assert!(screen.contains(" 3 π pi "), "{screen}");
    let row = |screen: &str, text: &str| {
        screen
            .lines()
            .find(|l| l.contains(text))
            .unwrap_or_default()
            .to_string()
    };
    let current = row(&screen, "Sonnet 5.5  ");
    assert!(current.contains("▸● Sonnet 5.5"), "{current}");
    assert!(
        current.contains("balanced")
            && current.contains("$2 / $10 per MTok")
            && current.contains("now"),
        "{current}"
    );
    assert!(
        row(&screen, "Fable 5.1").contains("   Fable 5.1"),
        "{screen}"
    );
    assert!(
        screen.contains("Sonnet 5.5, as its vendor describes it."),
        "{screen}"
    );
    assert!(
        screen.contains("same CLI: the session is restarted on the model, resumed where it was"),
        "{screen}"
    );
    assert!(
        screen.contains("←→ CLI · ↑↓ model · enter switch · esc cancel"),
        "{screen}"
    );

    // Another CLI says what moving there means; a model that cannot start
    // there says why.
    if let Some(Modal::Model(picker)) = &mut app.modal {
        picker.runtime = RuntimeKind::Codex;
        picker.row = 1;
    }
    let screen = render_to_string(&app, 120, 40, NOW);
    assert!(row(&screen, "GPT 6 Luna").contains("not here"), "{screen}");
    assert!(
        screen.contains("Cannot be started here: not in the model list of this Codex account"),
        "{screen}"
    );
    assert!(
        screen.contains("another CLI: a fresh Codex session there, handed where the work stands"),
        "{screen}"
    );
    // It fits a small terminal.
    let screen = render_to_string(&app, 80, 24, NOW);
    assert!(
        screen.contains("model and CLI")
            && screen.contains("GPT 6 Astra")
            && screen.contains("esc cancel"),
        "{screen}"
    );
}

// ---- version control animations ------------------------------------------------

mod vcs {
    use super::*;
    use crew_core::vcs::{VcsAction, VcsEvent, VcsOutcome};
    use crew_tui::anim::{GLANCE_MS, MARK_MS, MARK_SWEEP_MS, PULSE_MS};
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    fn push(app: &mut App, seq: i64, event: VcsEvent) {
        app.apply_push(Push::Event(fixture::vcs_event(seq, NOW, "vcs", event)), NOW);
    }

    fn event(action: VcsAction, outcome: VcsOutcome, op: &str, agent: &str) -> VcsEvent {
        fixture::vcs(action, outcome, op, agent)
    }

    fn commit(agent: &str) -> VcsEvent {
        VcsEvent {
            commit: Some("1a2b3c4d5e6f7a8b".into()),
            ..event(VcsAction::Commit, VcsOutcome::Ok, "v1", agent)
        }
    }

    fn conflict() -> VcsEvent {
        VcsEvent {
            peer: Some("claude-5".into()),
            files: vec!["src/ledger.rs".into(), "db/schema.sql".into()],
            ..event(VcsAction::Merge, VcsOutcome::Conflict, "v2", "claude-3")
        }
    }

    fn frame(app: &App, width: u16, height: u16, now: i64) -> Buffer {
        let theme = Theme::truecolor();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| views::render(frame, app, &theme, now))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// The cell where `needle` starts, if it is on screen.
    fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
        let first = needle.chars().next()?.to_string();
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            if let Some(at) = row.find(needle) {
                let x = row[..at].chars().count() as u16;
                assert_eq!(buf[(x, y)].symbol(), first);
                return Some((x, y));
            }
        }
        None
    }

    fn fg(buf: &Buffer, at: (u16, u16)) -> Color {
        buf[at].fg
    }

    fn bold(buf: &Buffer, at: (u16, u16)) -> bool {
        buf[at].modifier.contains(Modifier::BOLD)
    }

    #[test]
    fn a_commit_labels_the_box_and_sweeps_its_border() {
        let mut app = app();
        push(&mut app, 900, commit("claude-3"));
        let theme = Theme::truecolor();
        let before = frame(&app, 120, 40, NOW - 1);
        let during = frame(&app, 120, 40, NOW + 300);
        let at = find(&during, "◆ 1a2b3c4d").expect("label on the bottom border");
        assert_eq!(fg(&during, at), theme.ok);
        assert!(bold(&during, at));
        assert!(find(&before, "◆ 1a2b3c4d").is_none());
        // The bottom border is the label's: the model name gives way.
        let row: String = (0..120)
            .map(|x| during[(x, at.1)].symbol().to_string())
            .collect();
        assert!(row.contains("└ ◆ 1a2b3c4d ─"), "{row}");
        // A bright segment on the border: some ok-coloured, bold border cells
        // beside the label.
        let lit = |buf: &Buffer| {
            (0..40u16)
                .flat_map(|y| (0..120u16).map(move |x| (x, y)))
                .filter(|&p| {
                    matches!(buf[p].symbol(), "─" | "│" | "┌" | "┐" | "└" | "┘")
                        && fg(buf, p) == theme.ok
                        && bold(buf, p)
                })
                .count()
        };
        assert!(lit(&during) >= 4, "{}", lit(&during));
        // The sweep over, only the label stays lit.
        let after = frame(&app, 120, 40, NOW + MARK_SWEEP_MS + 100);
        assert!(find(&after, "◆ 1a2b3c4d").is_some());
        assert_eq!(lit(&after), 0);
        // Gone after its time: the border shows the model again.
        let gone = frame(&app, 120, 40, NOW + MARK_MS + 10);
        assert!(find(&gone, "◆ 1a2b3c4d").is_none());
        assert_eq!(lit(&gone), 0);
    }

    #[test]
    fn a_conflict_looks_different_from_a_merge() {
        let theme = Theme::truecolor();
        let mut ok = app();
        push(
            &mut ok,
            900,
            VcsEvent {
                commit: Some("5e6f7a8b9c0d".into()),
                ..VcsEvent {
                    peer: Some("claude-5".into()),
                    ..event(VcsAction::Merge, VcsOutcome::Ok, "v2", "claude-3")
                }
            },
        );
        let mut bad = app();
        push(&mut bad, 900, conflict());
        // The pulse travels first: diamonds on the way, no label yet.
        let mid = frame(&bad, 120, 40, NOW + PULSE_MS / 2);
        assert!(find(&mid, "◆").is_some() || find(&mid, "◇").is_some());
        assert!(find(&mid, "✗ 2 files").is_none());
        let on_arrival = NOW + PULSE_MS + 50;
        let good = frame(&ok, 120, 40, on_arrival);
        let wrong = frame(&bad, 120, 40, on_arrival);
        let g = find(&good, "◆ 5e6f7a8b").expect("merge label");
        let w = find(&wrong, "✗ 2 files").expect("conflict label");
        assert_eq!(fg(&good, g), theme.ok);
        assert_eq!(fg(&wrong, w), theme.err);
        assert_ne!(theme.ok, theme.err);
        // The message bus is not the private channel.
        let screen = render_to_string(&bad, 120, 40, NOW + 100);
        assert!(!screen.contains("p2p channel OPEN"), "{screen}");
    }

    #[test]
    fn a_read_glances_quietly_and_briefly() {
        let theme = Theme::truecolor();
        let mut app = app();
        push(
            &mut app,
            900,
            event(VcsAction::Status, VcsOutcome::Ok, "v1", "claude-2"),
        );
        let buf = frame(&app, 120, 40, NOW + 100);
        let at = find(&buf, "· status").expect("glance label");
        assert_eq!(fg(&buf, at), theme.muted);
        assert!(!bold(&buf, at));
        let later = frame(&app, 120, 40, NOW + GLANCE_MS + 10);
        assert!(find(&later, "· status").is_none());
        // A failed read is the one with a colour.
        let mut failed = self::app();
        push(
            &mut failed,
            900,
            event(VcsAction::Diff, VcsOutcome::Error, "v1", "claude-2"),
        );
        let buf = frame(&failed, 120, 40, NOW + 100);
        let at = find(&buf, "! diff").expect("failed glance");
        assert_eq!(fg(&buf, at), theme.warn);
    }

    #[test]
    fn a_running_merge_spins_and_lights_its_link_until_it_ends() {
        let theme = Theme::truecolor();
        let mut app = app();
        push(
            &mut app,
            900,
            VcsEvent {
                peer: Some("claude-5".into()),
                ..event(VcsAction::Merge, VcsOutcome::Started, "v3", "claude-3")
            },
        );
        let buf = frame(&app, 120, 40, NOW + 5000);
        let at = find(&buf, "merge claude-5").expect("running label");
        // The spinner leads it, in a cell of its own.
        let lead = buf[(at.0 - 2, at.1)].symbol().to_string();
        assert!(["◐", "◓", "◑", "◒"].contains(&lead.as_str()), "{lead:?}");
        // The link is lit but no pulse travels.
        let screen = render_to_string(&app, 120, 40, NOW + 5000);
        assert!(!screen.contains('◇'), "{screen}");
        let held_dots = (0..40u16)
            .flat_map(|y| (0..120u16).map(move |x| (x, y)))
            .filter(|&p| buf[p].symbol() == "·" && fg(&buf, p) == theme.muted && !bold(&buf, p))
            .count();
        let quiet = frame(&self::app(), 120, 40, NOW + 5000);
        let quiet_dots = (0..40u16)
            .flat_map(|y| (0..120u16).map(move |x| (x, y)))
            .filter(|&p| {
                quiet[p].symbol() == "·" && fg(&quiet, p) == theme.muted && !bold(&quiet, p)
            })
            .count();
        assert!(held_dots > quiet_dots, "{held_dots} vs {quiet_dots}");
        // The end: the label goes, the same line pulses.
        push(
            &mut app,
            901,
            VcsEvent {
                peer: Some("claude-5".into()),
                commit: Some("5e6f7a8b9c0d".into()),
                ..event(VcsAction::Merge, VcsOutcome::Ok, "v3", "claude-3")
            },
        );
        let done = render_to_string(&app, 120, 40, NOW + PULSE_MS + 100);
        assert!(!done.contains("merge claude-5"), "{done}");
        assert!(done.contains("◆ 5e6f7a8b"), "{done}");
    }

    #[test]
    fn the_hub_shows_an_integration_running_and_its_tally() {
        let theme = Theme::truecolor();
        let mut app = app();
        push(
            &mut app,
            900,
            event(
                VcsAction::Integrate,
                VcsOutcome::Started,
                "v4",
                "integration",
            ),
        );
        let buf = frame(&app, 120, 40, NOW + 1000);
        assert!(find(&buf, "integrating").is_some());
        push(
            &mut app,
            901,
            VcsEvent {
                summary: Some("2 merged, 1 conflict".into()),
                branch: Some("agents/integration".into()),
                ..event(
                    VcsAction::Integrate,
                    VcsOutcome::Conflict,
                    "v4",
                    "integration",
                )
            },
        );
        let buf = frame(&app, 120, 40, NOW + 1100);
        assert!(find(&buf, "integrating").is_none());
        let at = find(&buf, "2 merged, 1 conflict").expect("tally on the hub");
        assert_eq!(fg(&buf, at), theme.err);
    }

    #[test]
    fn a_workspace_for_a_subagent_runs_from_its_parent() {
        let mut app = App::new();
        app.apply_snapshot(fixture::with_subagents(NOW), NOW);
        push(
            &mut app,
            900,
            VcsEvent {
                peer: Some("claude-3".into()),
                ..event(
                    VcsAction::Workspace,
                    VcsOutcome::Started,
                    "v5",
                    "claude-3-s1",
                )
            },
        );
        let buf = frame(&app, 120, 40, NOW + 500);
        assert!(find(&buf, "workspace claude-3-s1").is_some());
        push(
            &mut app,
            901,
            VcsEvent {
                peer: Some("claude-3".into()),
                branch: Some("agents/claude-3-s1".into()),
                commit: Some("1a2b3c4d5e6f".into()),
                ..event(VcsAction::Workspace, VcsOutcome::Ok, "v5", "claude-3-s1")
            },
        );
        let screen = render_to_string(&app, 120, 40, NOW + PULSE_MS + 100);
        assert!(!screen.contains("workspace claude-3-s1"), "{screen}");
        // The mark is on the member's own box, which is narrow: cut to fit.
        assert!(screen.contains("+ agents"), "{screen}");
    }

    #[test]
    fn marks_for_agents_without_a_box_draw_nothing_and_do_not_panic() {
        let mut app = app();
        push(&mut app, 900, commit("ghost"));
        push(
            &mut app,
            901,
            VcsEvent {
                peer: Some("ghost".into()),
                ..event(VcsAction::Merge, VcsOutcome::Started, "v6", "also-gone")
            },
        );
        for (w, h) in [(120, 40), (70, 24), (40, 10), (8, 4)] {
            render_to_string(&app, w, h, NOW + 100);
        }
    }

    #[test]
    fn the_compact_layout_replaces_the_activity_with_the_label() {
        let mut app = app();
        push(&mut app, 900, commit("claude-3"));
        let screen = render_to_string(&app, 100, 6, NOW + 100);
        assert!(screen.contains("◆ 1a2b3c4d"), "{screen}");
        let mut running = self::app();
        push(
            &mut running,
            900,
            VcsEvent {
                peer: Some("claude-5".into()),
                ..event(VcsAction::Merge, VcsOutcome::Started, "v7", "claude-3")
            },
        );
        let screen = render_to_string(&running, 100, 6, NOW + 100);
        assert!(screen.contains("merge claude-5"), "{screen}");
    }

    #[test]
    fn the_log_view_shows_the_vcs_messages() {
        let mut app = app();
        push(&mut app, 900, commit("claude-3"));
        app.view = View::Logs;
        let screen = render_to_string(&app, 120, 30, NOW);
        assert!(screen.contains("vcs.commit"), "{screen}");
    }

    #[test]
    fn the_compact_layout_shows_the_hub_marks_too() {
        let mut app = app();
        push(
            &mut app,
            900,
            event(
                VcsAction::Integrate,
                VcsOutcome::Started,
                "v8",
                "integration",
            ),
        );
        let screen = render_to_string(&app, 100, 6, NOW + 100);
        assert!(screen.contains("integrating"), "{screen}");
        push(
            &mut app,
            901,
            VcsEvent {
                summary: Some("2 merged, 1 conflict".into()),
                ..event(
                    VcsAction::Integrate,
                    VcsOutcome::Conflict,
                    "v8",
                    "integration",
                )
            },
        );
        let screen = render_to_string(&app, 100, 6, NOW + 200);
        assert!(!screen.contains("integrating"), "{screen}");
        assert!(screen.contains("2 merged, 1 conflict"), "{screen}");
        let mut ws = self::app();
        push(
            &mut ws,
            900,
            event(VcsAction::Workspace, VcsOutcome::Started, "v9", "claude-7"),
        );
        let screen = render_to_string(&ws, 100, 6, NOW + 100);
        assert!(screen.contains("workspace claude-7"), "{screen}");
        // Selected hub: still on the first row.
        ws.selected = Some("orchestrator".into());
        let screen = render_to_string(&ws, 100, 6, NOW + 100);
        assert!(screen.contains("workspace claude-7"), "{screen}");
    }
}

#[test]
fn detail_view_shows_the_effective_toolbox() {
    // notes/toolboxes.md §Display, T15: the summary and a pending change next
    // to the runtime; `T` shows the groups, sources and definition facts in
    // the terminal pane, wrapped and scrolled like the diff, at the sizes
    // terminals have.
    for (width, height) in [(80, 24), (120, 30), (160, 70)] {
        let mut app = app();
        let mut snapshot = fixture::snapshot(NOW);
        snapshot.agents.extend(fixture::toolbox_agents(NOW));
        app.apply_snapshot(snapshot, NOW);
        app.open_detail("release", NOW);
        let size = format!("{width}x{height}");
        let screen = render_to_string(&app, width, height, NOW + 1000);
        for needle in [
            "toolbox   b56e01d7c4a9 · 4 items",
            "          2 not available",
            "Toolbox changed: restart to apply (R restart)",
            "T shows its items, sources and digest",
        ] {
            assert!(
                screen.contains(needle),
                "{size}: missing {needle:?}\n{screen}"
            );
        }
        press(&mut app, KeyCode::Char('T'));
        let first = render_to_string(&app, width, height, NOW + 1000);
        assert!(
            first.contains("MCP servers"),
            "{size}: opens at the top\n{first}"
        );
        // The pane is read-only: a click in it types into nothing, and the
        // keys after it still scroll the pane instead of reaching the agent.
        {
            use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
            let pane = app.detail.terminal_rect.get();
            let (column, row) = (pane.x + pane.width / 2, pane.y + pane.height / 2);
            let area = ratatui::layout::Rect::new(0, 0, width, height);
            assert!(views::hit(&app, area, column, row).is_none());
            let click = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            };
            crew_tui::input::handle_mouse(&mut app, click, area, NOW, 10);
            assert!(!app.detail.typing && app.detail.show_toolbox, "{size}");
            let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
            assert!(crew_tui::input::handle_key(&mut app, down, NOW, 10).is_empty());
            let moved = render_to_string(&app, width, height, NOW + 1000);
            assert!(
                moved.contains("toolbox ·")
                    && (moved != first || !first.contains("toolbox · rows")),
                "{size}\n{moved}"
            );
            let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
            assert!(crew_tui::input::handle_key(&mut app, up, NOW, 10).is_empty());
            assert!(crew_tui::input::handle_paste(&mut app, "pasted", NOW).is_empty());
            // T after the click still closes the pane; nothing reaches the agent.
            let toggle = KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE);
            assert!(crew_tui::input::handle_key(&mut app, toggle, NOW, 10).is_empty());
            assert!(!app.detail.show_toolbox && !app.detail.typing, "{size}");
            render_to_string(&app, width, height, NOW + 1000);
            assert!(crew_tui::input::handle_key(&mut app, toggle, NOW, 10).is_empty());
            assert_eq!(
                render_to_string(&app, width, height, NOW + 1000),
                first,
                "{size}"
            );
        }
        // Every row is reachable a page at a time, and nothing is cut off.
        let mut seen = first.clone();
        let mut last = first.clone();
        for _ in 0..20 {
            press(&mut app, KeyCode::PageDown);
            let page = render_to_string(&app, width, height, NOW + 1000);
            if page == last {
                break;
            }
            seen.push_str(&page);
            last = page;
        }
        for needle in [
            "docs",
            "https://docs.example.com/mcp",
            "Bundle github · Needs $SHOP_GITHUB_TOKEN",
            "github-mcp-server",
            "repos,issues,pull_requests",
            "Member · Not available on Codex",
            "Claude Code plugin format",
            "Sources",
            "Disabled (ambient, docs-local)",
            "Cannot be excluded on Codex",
            "without the sandbox",
            "Definitions",
            "definition digest: unavailable",
            "tools: unknown",
            "definition bytes: unknown",
        ] {
            assert!(
                seen.contains(needle),
                "{size}: unreachable {needle:?}\n{seen}"
            );
        }
        assert!(
            last.contains("definition bytes: unknown"),
            "{size}: the last page ends with the definitions\n{last}"
        );
        assert!(
            !seen.contains("Subagents\n") && !seen.contains(" Hooks"),
            "{size}"
        );
        // Home goes back to the top; T gives the terminal back.
        press(&mut app, KeyCode::Home);
        assert_eq!(
            render_to_string(&app, width, height, NOW + 1000),
            first,
            "{size}"
        );
        press(&mut app, KeyCode::Char('T'));
        let screen = render_to_string(&app, width, height, NOW + 1000);
        assert!(
            screen.contains("terminal ·") && !screen.contains("Definitions"),
            "{size}\n{screen}"
        );
    }
    // A member without a toolbox says so.
    let mut app = app();
    app.apply_snapshot(fixture::snapshot(NOW), NOW);
    let id = app.agents[0].id.clone();
    app.open_detail(&id, NOW);
    render_to_string(&app, 120, 30, NOW + 1000);
    press(&mut app, KeyCode::Char('T'));
    let screen = render_to_string(&app, 120, 30, NOW + 1000);
    assert!(screen.contains("this member has no toolbox"), "{screen}");
}
