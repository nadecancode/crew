//! Key handling. Pure: mutates the app state and returns the daemon requests
//! the key press calls for.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
#[cfg(test)]
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
#[cfg(test)]
use ratatui::layout::Rect;

use crate::app::{App, Command, Modal, View};
#[cfg(test)]
use crate::app::{Compose, MessageView, ModelPicker, QuestionBox};
#[cfg(test)]
use crate::views::{self, logs};

mod commands;
mod dialogs;
mod mouse;
mod navigation;
mod terminal;

use commands::run_slash;
use dialogs::{insert, modal_key, scroll_message};
pub use mouse::handle_mouse;
#[cfg(test)]
use mouse::WHEEL_ROWS;
use navigation::{
    dashboard_key, detail_key, logs_key, open_questions, question_box, resume_paused, tasks_key,
};
#[cfg(test)]
use navigation::{open_command, open_compose};
pub use terminal::key_bytes;
use terminal::typing_key;

/// Handle a key press. `page` is the number of rows a PgUp/PgDn should move.
pub fn handle_key(app: &mut App, key: KeyEvent, now: i64, page: usize) -> Vec<Command> {
    app.mark_dirty();
    // Typing into an agent's terminal: every key is the agent's, Ctrl+C
    // included; Ctrl+] is the one that comes back.
    if app.view == View::Detail && app.detail.typing && app.modal.is_none() && !app.help {
        return typing_key(app, key, now);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return vec![Command::Quit];
    }
    if app.modal.is_some() {
        return modal_key(app, key, now);
    }
    if app.help {
        app.help = false;
        return Vec::new();
    }
    match app.view {
        View::Dashboard => dashboard_key(app, key, now),
        View::Detail => detail_key(app, key, now, page),
        View::Tasks => tasks_key(app, key, now, page),
        View::Logs => logs_key(app, key, now, page),
    }
}

/// Bracketed paste: into a box, or straight into the terminal being
/// typed into.
pub fn handle_paste(app: &mut App, text: &str, now: i64) -> Vec<Command> {
    if app.view == View::Detail && app.detail.typing && app.modal.is_none() {
        // A pasted ESC could close the wrapper early and turn the tail into keys.
        let text = text.replace('\u{1b}', "");
        app.mark_dirty();
        app.detail.output_at = None;
        app.detail.key_at = now;
        return vec![Command::Keys {
            id: app.detail.agent.clone(),
            text: format!("\x1b[200~{text}\x1b[201~"),
        }];
    }
    // Pasted into the answer box, text is the operator's own answer.
    if let Some(Modal::Question(answer)) = &mut app.modal {
        answer.choice = usize::MAX;
    }
    let input = match &mut app.modal {
        Some(Modal::Compose(compose) | Modal::Type(compose)) => Some(compose),
        Some(Modal::Question(answer)) => Some(&mut answer.input),
        _ => None,
    };
    if let Some(compose) = input {
        for ch in text.replace("\r\n", "\n").chars() {
            let ch = if ch == '\r' { '\n' } else { ch };
            if ch == '\n' || !ch.is_control() {
                insert(compose, ch);
            }
        }
        compose.notice = None;
        app.mark_dirty();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_paste_cannot_inject_bracketed_paste_markers() {
        let mut a = app();
        a.view = View::Detail;
        a.detail.agent = "claude-1".into();
        a.detail.typing = true;
        let commands = handle_paste(
            &mut a,
            "日本\ntext\u{1b}[201~\ncommand\u{1b}[200~",
            1_000_000,
        );
        let [Command::Keys { text, .. }] = commands.as_slice() else {
            panic!("expected terminal paste")
        };
        assert_eq!(
            text.matches('\u{1b}').count(),
            2,
            "only the outer markers may contain ESC"
        );
        assert_eq!(text.matches("\u{1b}[201~").count(), 1);
        assert!(text.starts_with("\u{1b}[200~日本\ntext"));
        assert!(text.ends_with("command[200~\u{1b}[201~"));
    }
    use crate::fixture;

    fn app() -> App {
        let mut app = App::new();
        app.apply_snapshot(fixture::snapshot(1_000_000), 1_000_000);
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Vec<Command> {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE), 1_000_000, 10)
    }

    fn ctrl(app: &mut App, ch: char) -> Vec<Command> {
        handle_key(
            app,
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL),
            1_000_000,
            10,
        )
    }

    fn type_text(app: &mut App, text: &str) {
        for ch in text.chars() {
            press(app, KeyCode::Char(ch));
        }
    }

    #[test]
    fn quit_never_asks_the_daemon_for_anything_else() {
        let mut a = app();
        assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Command::Quit]);
        assert_eq!(ctrl(&mut a, 'c'), vec![Command::Quit]);
        for view in [View::Detail, View::Tasks, View::Logs] {
            a.view = view;
            assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Command::Quit]);
        }
    }

    #[test]
    fn tab_cycles_and_enter_inspects() {
        let mut a = app();
        press(&mut a, KeyCode::Tab);
        press(&mut a, KeyCode::Tab);
        assert_eq!(a.selected.as_deref(), Some("claude-3"));
        press(&mut a, KeyCode::BackTab);
        assert_eq!(a.selected.as_deref(), Some("claude-2"));
        let cmds = press(&mut a, KeyCode::Enter);
        assert_eq!(a.view, View::Detail);
        assert_eq!(a.detail.agent, "claude-2");
        assert!(cmds.contains(&Command::FetchOutput {
            id: "claude-2".into()
        }));
        assert!(cmds.contains(&Command::FetchInbox {
            id: "claude-2".into()
        }));
        // Tab inside the inspector moves to the next agent and refetches.
        let cmds = press(&mut a, KeyCode::Tab);
        assert_eq!(a.detail.agent, "claude-3");
        assert!(cmds.contains(&Command::FetchOutput {
            id: "claude-3".into()
        }));
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.view, View::Dashboard);
        press(&mut a, KeyCode::Char('o'));
        assert_eq!(a.selected.as_deref(), Some("orchestrator"));
    }

    #[test]
    fn the_inspector_types_into_the_agents_terminal() {
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.view, View::Detail);
        press(&mut a, KeyCode::Char('i'));
        assert!(
            matches!(&a.modal, Some(Modal::Type(c)) if c.to == "claude-1" && c.text.is_empty())
        );
        // Shortcuts are text here; the line goes in as one prompt.
        type_text(&mut a, "run the tests, k?");
        a.detail.output_at = Some(5);
        let cmds = press(&mut a, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Command::Type {
                id: "claude-1".into(),
                text: "run the tests, k?".into()
            }]
        );
        assert!(a.modal.is_none());
        // The screen is read again at once.
        assert!(a.detail.output_at.is_none());

        // Nothing typed: Enter alone goes to the terminal.
        press(&mut a, KeyCode::Char('i'));
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Type {
                id: "claude-1".into(),
                text: String::new()
            }]
        );
        // Esc sends nothing; a paste is text.
        press(&mut a, KeyCode::Char('i'));
        type_text(&mut a, "abc");
        handle_paste(&mut a, "def", 1_000_000);
        assert!(matches!(&a.modal, Some(Modal::Type(c)) if c.text == "abcdef"));
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.modal.is_none());
        assert_eq!(a.view, View::Detail);

        // Shift+Enter is a new line, as are Alt+Enter and Ctrl+J where the
        // terminal cannot report the shift; plain Enter still sends.
        let with = |a: &mut App, code: KeyCode, modifiers: KeyModifiers| {
            handle_key(a, KeyEvent::new(code, modifiers), 1_000_000, 10)
        };
        press(&mut a, KeyCode::Char('i'));
        type_text(&mut a, "one");
        assert!(with(&mut a, KeyCode::Enter, KeyModifiers::SHIFT).is_empty());
        type_text(&mut a, "two");
        assert!(with(&mut a, KeyCode::Enter, KeyModifiers::ALT).is_empty());
        type_text(&mut a, "three");
        assert!(ctrl(&mut a, 'j').is_empty());
        assert!(matches!(&a.modal, Some(Modal::Type(c)) if c.text == "one\ntwo\nthree\n"));
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Type {
                id: "claude-1".into(),
                text: "one\ntwo\nthree\n".into()
            }]
        );
        // The same in a message, and in an answer of the operator's own.
        press(&mut a, KeyCode::Char('m'));
        type_text(&mut a, "a");
        with(&mut a, KeyCode::Enter, KeyModifiers::SHIFT);
        type_text(&mut a, "b");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Send {
                to: "claude-1".into(),
                body: "a\nb".into()
            }]
        );
        a.questions = vec![fixture::question(1_000_000)];
        a.view = View::Dashboard;
        press(&mut a, KeyCode::Char('a'));
        type_text(&mut a, "x");
        with(&mut a, KeyCode::Enter, KeyModifiers::SHIFT);
        type_text(&mut a, "y");
        assert!(matches!(&a.modal, Some(Modal::Question(q)) if q.input.text == "x\ny"));
        let sent = press(&mut a, KeyCode::Enter);
        assert!(
            matches!(&sent[..], [Command::Answer { body, .. }] if body == "x\ny"),
            "{sent:?}"
        );
    }

    #[test]
    fn a_click_into_the_terminal_pane_types_straight_into_it() {
        use ratatui::layout::Rect;

        let mut a = app();
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.view, View::Detail);
        // Where the pane was drawn is what a click is measured against.
        a.detail.terminal_rect.set(Rect::new(50, 2, 60, 20));
        assert!(left_click(&mut a, 20, 10).is_empty() && !a.detail.typing);
        a.detail.scroll = 4;
        assert!(left_click(&mut a, 60, 10).is_empty());
        assert!(a.detail.typing && a.detail.scroll == 0);

        // Every key is the agent's now, in the terminal's own encoding; the
        // screen is read again at once each time.
        let keys = |a: &mut App, code: KeyCode, modifiers: KeyModifiers| {
            a.detail.output_at = Some(1);
            let cmds = handle_key(a, KeyEvent::new(code, modifiers), 1_000_000, 10);
            assert!(a.detail.output_at.is_none(), "{cmds:?}");
            match cmds.as_slice() {
                [Command::Keys { id, text }] if id == "claude-1" => text.clone(),
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(keys(&mut a, KeyCode::Char('q'), KeyModifiers::NONE), "q");
        assert_eq!(
            keys(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL),
            "\x03"
        );
        assert_eq!(keys(&mut a, KeyCode::Esc, KeyModifiers::NONE), "\x1b");
        assert_eq!(keys(&mut a, KeyCode::Enter, KeyModifiers::NONE), "\r");
        assert_eq!(keys(&mut a, KeyCode::Enter, KeyModifiers::SHIFT), "\x1b\r");
        assert_eq!(keys(&mut a, KeyCode::Up, KeyModifiers::NONE), "\x1b[A");
        assert_eq!(
            keys(&mut a, KeyCode::Left, KeyModifiers::SHIFT),
            "\x1b[1;2D"
        );
        assert_eq!(
            keys(&mut a, KeyCode::BackTab, KeyModifiers::SHIFT),
            "\x1b[Z"
        );
        assert_eq!(keys(&mut a, KeyCode::Backspace, KeyModifiers::NONE), "\x7f");
        assert_eq!(keys(&mut a, KeyCode::Delete, KeyModifiers::NONE), "\x1b[3~");
        assert_eq!(
            keys(&mut a, KeyCode::PageDown, KeyModifiers::NONE),
            "\x1b[6~"
        );
        assert_eq!(keys(&mut a, KeyCode::F(1), KeyModifiers::NONE), "\x1bOP");
        assert_eq!(keys(&mut a, KeyCode::F(5), KeyModifiers::NONE), "\x1b[15~");
        assert_eq!(keys(&mut a, KeyCode::Char('x'), KeyModifiers::ALT), "\x1bx");
        assert_eq!(keys(&mut a, KeyCode::Char('É'), KeyModifiers::SHIFT), "É");
        assert!(handle_key(
            &mut a,
            KeyEvent::new(KeyCode::CapsLock, KeyModifiers::NONE),
            1_000_000,
            10
        )
        .is_empty());
        assert!(a.view == View::Detail && a.modal.is_none() && a.detail.typing);
        // A paste arrives as one, bracketed.
        assert_eq!(
            handle_paste(&mut a, "two\nlines", 1_000_000),
            vec![Command::Keys {
                id: "claude-1".into(),
                text: "\x1b[200~two\nlines\x1b[201~".into()
            }]
        );
        // Right after a key the terminal is read as often as it answers;
        // later on, often.
        let fetches = |a: &mut App, at: i64| {
            a.detail.pending = false;
            a.detail.output_at = Some(at - 1);
            a.detail.requested_at = at - 1;
            a.due_commands(at)
                .iter()
                .any(|c| matches!(c, Command::FetchOutput { .. }))
        };
        assert!(fetches(&mut a, 1_000_000 + 1));
        assert!(!fetches(&mut a, 1_000_000 + crate::app::KEY_BURST_MS + 10));
        a.detail.pending = false;
        a.detail.output_at = Some(1_000_000 + crate::app::KEY_BURST_MS);
        a.detail.requested_at = 1_000_000 + crate::app::KEY_BURST_MS;
        let later = 1_000_000 + crate::app::KEY_BURST_MS + crate::app::TYPING_REFRESH_MS;
        assert!(a
            .due_commands(later)
            .iter()
            .any(|c| matches!(c, Command::FetchOutput { .. })));

        // Ctrl+] is the way out; so is a right-click, or a click elsewhere.
        assert!(ctrl(&mut a, ']').is_empty());
        assert!(!a.detail.typing);
        assert_eq!(press(&mut a, KeyCode::Char('q')), vec![Command::Quit]);
        left_click(&mut a, 60, 10);
        assert!(a.detail.typing);
        mouse(&mut a, MouseEventKind::Down(MouseButton::Right), 60, 10);
        assert!(!a.detail.typing && a.view == View::Detail);
        left_click(&mut a, 60, 10);
        assert!(left_click(&mut a, 5, 5).is_empty());
        assert!(!a.detail.typing);
        // Tab is the agent's too while typing; another agent's inspector
        // starts without it.
        left_click(&mut a, 60, 10);
        assert!(
            matches!(press(&mut a, KeyCode::Tab).as_slice(), [Command::Keys { text, .. }] if text == "\t")
        );
        assert_eq!(a.detail.agent, "claude-1");
        a.open_detail("claude-2", 1_000_000);
        assert!(!a.detail.typing);

        // f fills the inspector with the terminal, and again not.
        assert!(!a.detail.full);
        press(&mut a, KeyCode::Char('f'));
        assert!(a.detail.full);
        press(&mut a, KeyCode::Char('f'));
        assert!(!a.detail.full);
    }

    #[test]
    fn compose_sends_as_typed_and_esc_cancels() {
        let mut a = app();
        press(&mut a, KeyCode::Char('m'));
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.to == "claude-1"));
        // Keys that are shortcuts elsewhere are plain text here.
        type_text(&mut a, "quick mkt");
        press(&mut a, KeyCode::Backspace);
        press(&mut a, KeyCode::Left);
        press(&mut a, KeyCode::Left);
        type_text(&mut a, "!");
        let cmds = press(&mut a, KeyCode::Enter);
        assert_eq!(
            cmds,
            vec![Command::Send {
                to: "claude-1".into(),
                body: "quick !mk".into()
            }]
        );
        assert!(a.modal.is_none());

        press(&mut a, KeyCode::Char('M'));
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.to == "*"));
        type_text(&mut a, "hello");
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.modal.is_none());

        // Empty messages are not sent.
        press(&mut a, KeyCode::Char('m'));
        type_text(&mut a, "   ");
        assert!(press(&mut a, KeyCode::Enter).is_empty());
    }

    fn compose(app: &App) -> &Compose {
        match &app.modal {
            Some(Modal::Compose(compose)) => compose,
            other => panic!("expected the message box, found {other:?}"),
        }
    }

    /// Type `line` into the message box of the selected agent and press Enter.
    fn submit(app: &mut App, line: &str) -> Vec<Command> {
        press(app, KeyCode::Char('m'));
        type_text(app, line);
        press(app, KeyCode::Enter)
    }

    #[test]
    fn a_slash_line_is_a_command_and_is_never_sent() {
        // /exit leaves the dashboard; no agent gets to read it.
        let mut a = app();
        assert_eq!(submit(&mut a, "/exit"), vec![Command::Quit]);
        for alias in ["/quit", "/q", "/EXIT"] {
            assert_eq!(submit(&mut app(), alias), vec![Command::Quit], "{alias}");
        }

        // A command that does not exist is not sent either: the box stays
        // open and says so.
        let mut a = app();
        assert!(submit(&mut a, "/clear").is_empty());
        assert_eq!(compose(&a).text, "/clear");
        assert!(compose(&a)
            .notice
            .as_deref()
            .unwrap()
            .contains("no command /clear"));
        // Editing clears the note; an argument where none belongs is refused.
        press(&mut a, KeyCode::Backspace);
        assert!(compose(&a).notice.is_none());
        ctrl(&mut a, 'u');
        type_text(&mut a, "/exit now");
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert_eq!(
            compose(&a).notice.as_deref(),
            Some("/exit takes no argument")
        );
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.modal.is_none());

        // Text that merely contains or resembles a command is a message.
        for text in [" /exit", "/src/main.rs is slow", "please /exit", "//exit"] {
            let mut a = app();
            assert_eq!(
                submit(&mut a, text),
                vec![Command::Send {
                    to: "claude-1".into(),
                    body: text.trim().into()
                }],
                "{text:?}"
            );
        }
    }

    #[test]
    fn the_team_is_stopped_from_the_dashboard_after_a_confirmation() {
        let mut a = app();
        assert!(submit(&mut a, "/stop").is_empty());
        assert_eq!(a.modal, Some(Modal::ConfirmStop));
        assert!(press(&mut a, KeyCode::Char('n')).is_empty());
        assert!(a.modal.is_none() && !a.stopping);

        for open in ["/terminate", "/shutdown"] {
            let mut a = app();
            submit(&mut a, open);
            assert_eq!(a.modal, Some(Modal::ConfirmStop), "{open}");
        }
        // The key does the same from the dashboard and the inspector.
        for view in [View::Dashboard, View::Detail] {
            let mut a = app();
            a.view = view;
            assert!(press(&mut a, KeyCode::Char('S')).is_empty());
            assert_eq!(a.modal, Some(Modal::ConfirmStop));
            assert_eq!(press(&mut a, KeyCode::Char('y')), vec![Command::StopTeam]);
            assert!(a.stopping && a.modal.is_none());
        }
    }

    #[test]
    fn fresh_restart_requires_a_no_resume_confirmation_and_targets_one_agent() {
        let mut a = app();
        assert!(submit(&mut a, "/restart-fresh claude-2").is_empty());
        assert_eq!(
            format!("{:?}", a.modal),
            "Some(ConfirmRestartFresh(\"claude-2\"))"
        );
        for width in [42, 120] {
            let screen = crate::render_to_string(&a, width, 24, 1_000_000);
            assert!(screen.contains("no resume"), "{screen}");
            assert!(screen.contains("old conversation"), "{screen}");
        }
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.modal.is_none());
        assert!(submit(&mut a, "/restart-fresh").is_empty());
        assert_eq!(
            format!("{:?}", press(&mut a, KeyCode::Char('y'))),
            "[RestartFresh(\"claude-1\")]"
        );
        assert!(a.modal.is_none());
        // Ordinary restart still resumes immediately.
        assert_eq!(
            submit(&mut a, "/restart claude-2"),
            vec![Command::Restart("claude-2".into())]
        );
    }

    #[test]
    fn slash_commands_act_on_agents_and_views() {
        // Without a name, the agent is the one the box was addressed to.
        let mut a = app();
        assert!(submit(&mut a, "/kill").is_empty());
        assert_eq!(a.modal, Some(Modal::ConfirmKill("claude-1".into())));
        assert_eq!(
            press(&mut a, KeyCode::Char('y')),
            vec![Command::Kill("claude-1".into())]
        );
        assert!(submit(&mut a, "/kill claude-3").is_empty());
        assert_eq!(a.modal, Some(Modal::ConfirmKill("claude-3".into())));
        press(&mut a, KeyCode::Esc);

        assert_eq!(
            submit(&mut a, "/restart claude-2"),
            vec![Command::Restart("claude-2".into())]
        );
        assert_eq!(
            submit(&mut a, "/pause"),
            vec![Command::Pause("claude-1".into())]
        );
        assert_eq!(
            submit(&mut a, "/compact"),
            vec![Command::Compact("claude-1".into())]
        );
        assert_eq!(
            submit(&mut a, "/compact claude-3"),
            vec![Command::Compact("claude-3".into())]
        );
        assert_eq!(
            submit(&mut a, "/unpause claude-2"),
            vec![Command::Resume("claude-2".into())]
        );

        // An agent that does not exist is named in the note, with the real ones.
        assert!(submit(&mut a, "/kill ghost").is_empty());
        let notice = compose(&a).notice.clone().unwrap();
        assert!(
            notice.contains("no agent named ghost") && notice.contains("claude-2"),
            "{notice}"
        );
        press(&mut a, KeyCode::Esc);

        // A broadcast box has no agent of its own: the selection stands in.
        press(&mut a, KeyCode::Char('M'));
        type_text(&mut a, "/restart");
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Restart("claude-1".into())]
        );

        assert!(
            submit(&mut a, "/resume").is_empty(),
            "nobody is at a usage limit"
        );
        assert!(a.status.as_ref().unwrap().is_error);
        a.agents[0].limit = Some(crew_core::types::UsageLimit::default());
        assert_eq!(
            submit(&mut a, "/resume"),
            vec![Command::ResumeLimited { restart: false }]
        );

        assert!(submit(&mut a, "/tasks").is_empty());
        assert_eq!(a.view, View::Tasks);
        // The other views open the command line with the slash key too.
        assert!(press(&mut a, KeyCode::Char('/')).is_empty());
        assert_eq!((compose(&a).text.as_str(), compose(&a).cursor), ("/", 1));
        type_text(&mut a, "logs");
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.view, View::Logs);
        press(&mut a, KeyCode::Char('/'));
        type_text(&mut a, "?");
        press(&mut a, KeyCode::Enter);
        assert!(a.help && a.modal.is_none());
    }

    fn answer_box(app: &App) -> &QuestionBox {
        match &app.modal {
            Some(Modal::Question(answer)) => answer,
            other => panic!("expected the answer box, found {other:?}"),
        }
    }

    fn with_questions() -> (App, String, String) {
        let mut a = app();
        let first = fixture::question(1_000_000);
        let mut second = first.clone();
        second.id = "m-second".into();
        second.body = "Ship without the migration?".into();
        second.metadata = serde_json::Value::Null;
        a.questions = vec![first.clone(), second.clone()];
        (a, first.id, second.id)
    }

    #[test]
    fn a_question_is_answered_by_picking_an_option_or_in_the_operators_words() {
        // Nothing to answer: say so instead of opening an empty box.
        let mut a = app();
        assert!(press(&mut a, KeyCode::Char('a')).is_empty());
        assert!(a.modal.is_none());
        assert!(a
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("no question is waiting"));

        // The oldest question opens with its first suggested answer selected.
        let (mut a, first, second) = with_questions();
        press(&mut a, KeyCode::Char('a'));
        assert_eq!(
            (answer_box(&a).id.as_str(), answer_box(&a).choice),
            (first.as_str(), 0)
        );
        press(&mut a, KeyCode::Down);
        assert_eq!(answer_box(&a).choice, 1);
        press(&mut a, KeyCode::Char('1'));
        assert_eq!(answer_box(&a).choice, 0, "a digit picks a row");
        press(&mut a, KeyCode::Char('2'));
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Answer {
                id: first.clone(),
                body: "Change the export and update the dashboards".into()
            }]
        );
        // The answered one is out of the way, and the next is up.
        assert!(a.answering.contains(&first));
        assert_eq!(a.open_questions().len(), 1);
        assert_eq!(answer_box(&a).id, second);

        // No suggestions: the box takes the operator's own words.
        assert!(
            press(&mut a, KeyCode::Enter).is_empty(),
            "an empty answer is not sent"
        );
        assert!(answer_box(&a)
            .input
            .notice
            .as_deref()
            .unwrap()
            .contains("type your own"));
        type_text(&mut a, "/exit");
        assert!(
            press(&mut a, KeyCode::Enter).is_empty(),
            "a command is not an answer"
        );
        assert!(answer_box(&a)
            .input
            .notice
            .as_deref()
            .unwrap()
            .contains("a command, not an answer"));
        ctrl(&mut a, 'u');
        assert!(answer_box(&a).input.notice.is_none());
        type_text(&mut a, "yes, ship it q");
        press(&mut a, KeyCode::Backspace);
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::Answer {
                id: second,
                body: "yes, ship it".into()
            }]
        );
        assert!(a.modal.is_none(), "nothing is left to answer");
    }

    #[test]
    fn the_answer_box_moves_between_questions_and_keeps_typing_apart_from_keys() {
        let (mut a, first, second) = with_questions();
        assert!(submit(&mut a, "/answer").is_empty());
        assert_eq!(answer_box(&a).id, first);
        // Typing on a suggested answer starts the operator's own, also for
        // letters that are dashboard keys elsewhere.
        type_text(&mut a, "q");
        assert_eq!(
            (answer_box(&a).choice, answer_box(&a).input.text.as_str()),
            (2, "q")
        );
        type_text(&mut a, "2");
        assert_eq!(
            answer_box(&a).input.text,
            "q2",
            "on the typing row a digit is text"
        );
        press(&mut a, KeyCode::Up);
        assert_eq!(answer_box(&a).choice, 1);
        press(&mut a, KeyCode::Up);
        press(&mut a, KeyCode::Up);
        assert_eq!(answer_box(&a).choice, 0);
        // Enough wrapped rows to exercise scrolling rather than an invisible offset.
        a.questions
            .iter_mut()
            .find(|q| q.id == first)
            .unwrap()
            .body
            .push_str(&"\nQuestion line".repeat(60));
        press(&mut a, KeyCode::PageDown);
        assert_eq!(answer_box(&a).scroll, 5);
        press(&mut a, KeyCode::PageUp);
        assert_eq!(answer_box(&a).scroll, 0);

        // Tab walks the questions; each starts afresh.
        press(&mut a, KeyCode::Tab);
        assert_eq!(
            (
                answer_box(&a).id.as_str(),
                answer_box(&a).input.text.as_str()
            ),
            (second.as_str(), "")
        );
        press(&mut a, KeyCode::Tab);
        assert_eq!(answer_box(&a).id, first);
        press(&mut a, KeyCode::BackTab);
        assert_eq!(answer_box(&a).id, second);
        handle_paste(&mut a, "pasted\nanswer", 1_000_000);
        assert_eq!(answer_box(&a).input.text, "pasted\nanswer");

        // Answered from another terminal meanwhile: the next key moves on.
        a.questions.retain(|q| q.id != second);
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert_eq!(answer_box(&a).id, first);
        a.questions.clear();
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert!(a.modal.is_none());

        // Esc leaves the question open for later; the key works in every view.
        let (mut a, first, _) = with_questions();
        for view in [View::Detail, View::Tasks, View::Logs] {
            a.view = view;
            press(&mut a, KeyCode::Char('a'));
            assert_eq!(answer_box(&a).id, first, "{view:?}");
            assert!(press(&mut a, KeyCode::Esc).is_empty());
            assert!(a.modal.is_none());
        }
        assert_eq!(a.open_questions().len(), 2);
    }

    const SCREEN: Rect = Rect::new(0, 0, 120, 40);

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) -> Vec<Command> {
        let event = MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(app, event, SCREEN, 1_000_000, 10)
    }

    fn left_click(app: &mut App, x: u16, y: u16) -> Vec<Command> {
        mouse(app, MouseEventKind::Down(MouseButton::Left), x, y)
    }

    #[test]
    fn a_click_on_an_agent_opens_its_inspector() {
        let boxes = views::dashboard::agent_rects(&app(), views::content_area(&app(), SCREEN));
        // Six workers and the orchestrator, each with a box of its own.
        assert_eq!(boxes.len(), 7, "{boxes:?}");
        for (id, rect) in &boxes {
            let mut a = app();
            let commands = left_click(&mut a, rect.x + rect.width / 2, rect.y + rect.height / 2);
            assert_eq!(
                (a.view, a.detail.agent.as_str()),
                (View::Detail, id.as_str())
            );
            assert_eq!(a.selected.as_deref(), Some(id.as_str()));
            assert!(
                commands.contains(&Command::FetchOutput { id: id.clone() }),
                "{id}"
            );
            // The corners of the box count; the cell outside it does not.
            let mut a = app();
            left_click(&mut a, rect.x, rect.y);
            assert_eq!(a.detail.agent, *id);
            let mut a = app();
            left_click(&mut a, rect.right() - 1, rect.y);
            assert_eq!(a.detail.agent, *id);
            // The bottom border, where the box says what it runs on, opens
            // the model picker (a shell runs no model: the inspector).
            let mut a = app();
            let commands = left_click(&mut a, rect.right() - 1, rect.bottom() - 1);
            if a.agent(id).unwrap().runtime == crew_core::RuntimeKind::Shell {
                assert_eq!(a.detail.agent, *id);
            } else {
                assert!(
                    matches!(&a.modal, Some(Modal::Model(p)) if p.agent == *id),
                    "{id}: {:?}",
                    a.modal
                );
                assert_eq!(commands, vec![Command::FetchModels]);
                assert_eq!(a.view, View::Dashboard);
            }
        }
        // The boxes are where the screen shows them.
        let screen = crate::render_to_string(&app(), SCREEN.width, SCREEN.height, 1_000_000);
        let rows: Vec<&str> = screen.lines().collect();
        for (id, rect) in &boxes {
            let top: String = rows[usize::from(rect.y)]
                .chars()
                .skip(usize::from(rect.x))
                .take(usize::from(rect.width))
                .collect();
            assert!(
                top.contains(id.as_str()),
                "{id} is not drawn at {rect:?}: {top:?}"
            );
        }

        // Empty space opens nothing, and neither does a click under a box.
        let mut a = app();
        assert!(left_click(&mut a, 0, 0).is_empty());
        assert_eq!(a.view, View::Dashboard);
        a.modal = Some(Modal::ConfirmStop);
        let (_, rect) = &boxes[0];
        assert!(left_click(&mut a, rect.x + 1, rect.y + 1).is_empty());
        assert_eq!(
            (a.view, a.modal.clone()),
            (View::Dashboard, Some(Modal::ConfirmStop))
        );

        // On a screen too small for boxes the agents are rows of a list.
        let small = Rect::new(0, 0, 50, 12);
        let rows = views::dashboard::agent_rects(&app(), views::content_area(&app(), small));
        assert_eq!(rows.len(), 7);
        assert!(rows.iter().all(|(_, r)| r.height == 1));
        let (id, row) = &rows[3];
        let mut a = app();
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut a, event, small, 1_000_000, 10);
        assert_eq!(a.detail.agent, *id);
    }

    #[test]
    fn a_click_on_a_bus_row_opens_the_message_in_full() {
        let mut a = app();
        let screen = crate::render_to_string(&a, SCREEN.width, SCREEN.height, 1_000_000);
        // The messages are listed under the heading, the newest last.
        let heading = screen
            .lines()
            .position(|l| l.contains("message bus"))
            .unwrap() as u16;
        let newest = a.messages.back().unwrap().clone();
        let y = heading + a.messages.len() as u16;
        assert!(left_click(&mut a, 30, y).is_empty());
        assert_eq!(
            a.modal,
            Some(Modal::Message(MessageView {
                id: newest.id.clone(),
                scroll: 0
            }))
        );
        // The one above it is the message before.
        let mut b = app();
        left_click(&mut b, 30, y - 1);
        let before = &b.messages[b.messages.len() - 2];
        assert!(matches!(&b.modal, Some(Modal::Message(view)) if view.id == before.id));
        // The heading of the bus is not a message, nor is a row left empty.
        let mut c = app();
        left_click(&mut c, 5, heading);
        assert!(c.modal.is_none());
        left_click(&mut c, 5, y + 1);
        assert!(c.modal.is_none());

        // It scrolls with the keys and the wheel, and closes on esc, on any
        // other key and on a right click.
        a.messages
            .iter_mut()
            .find(|m| m.id == newest.id)
            .unwrap()
            .body
            .push_str(&"\nMessage line".repeat(60));
        press(&mut a, KeyCode::Down);
        assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == 1));
        mouse(&mut a, MouseEventKind::ScrollUp, 10, 10);
        assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == 0));
        press(&mut a, KeyCode::Esc);
        assert!(a.modal.is_none());
        left_click(&mut a, 30, y);
        mouse(&mut a, MouseEventKind::Down(MouseButton::Right), 0, 0);
        assert!(a.modal.is_none());
    }

    #[test]
    fn scrolling_short_bodies_keeps_zero_offset_and_long_bodies_work_before_draw() {
        let mut a = app();
        let id = a.messages.back().unwrap().id.clone();
        a.messages.back_mut().unwrap().body = "short".into();
        a.modal = Some(Modal::Message(MessageView {
            id: id.clone(),
            scroll: 0,
        }));
        for code in [KeyCode::Down, KeyCode::PageDown, KeyCode::End] {
            press(&mut a, code);
            assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == 0));
        }
        mouse(&mut a, MouseEventKind::ScrollDown, 10, 10);
        assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == 0));
        a.messages.back_mut().unwrap().body = "long line\n".repeat(60);
        // No draw has happened: the fallback screen size still allows motion.
        press(&mut a, KeyCode::End);
        let before = match &a.modal {
            Some(Modal::Message(view)) => view.scroll,
            _ => unreachable!(),
        };
        assert!(before > 0);
        press(&mut a, KeyCode::Up);
        assert!(matches!(&a.modal, Some(Modal::Message(view)) if view.scroll == before - 1));

        let (mut a, first, _) = with_questions();
        a.questions.iter_mut().find(|q| q.id == first).unwrap().body = "short".into();
        a.modal = Some(Modal::Question(question_box(first.clone())));
        press(&mut a, KeyCode::PageDown);
        mouse(&mut a, MouseEventKind::ScrollDown, 10, 10);
        assert_eq!(answer_box(&a).scroll, 0);
        a.questions.iter_mut().find(|q| q.id == first).unwrap().body = "long line\n".repeat(60);
        press(&mut a, KeyCode::PageDown);
        assert_eq!(answer_box(&a).scroll, 5);
    }

    #[test]
    fn the_wheel_scrolls_and_the_right_button_goes_back() {
        // The band that announces a question opens the answer box.
        let (mut a, first, _) = with_questions();
        a.questions
            .iter_mut()
            .find(|q| q.id == first)
            .unwrap()
            .body
            .push_str(&"\nQuestion line".repeat(60));
        let question_row = views::content_area(&a, SCREEN).y;
        left_click(&mut a, 40, question_row);
        assert_eq!(answer_box(&a).id, first);
        mouse(&mut a, MouseEventKind::ScrollDown, 40, 10);
        assert_eq!(answer_box(&a).scroll, 5);
        mouse(&mut a, MouseEventKind::Down(MouseButton::Right), 40, 10);
        assert!(a.modal.is_none());

        // In the inspector the wheel moves the terminal output.
        let mut a = app();
        press(&mut a, KeyCode::Enter);
        a.detail.output = (0..100).map(|i| format!("line {i}\n")).collect();
        mouse(&mut a, MouseEventKind::ScrollUp, 60, 20);
        assert_eq!(a.detail.scroll, WHEEL_ROWS);
        mouse(&mut a, MouseEventKind::ScrollDown, 60, 20);
        assert_eq!(a.detail.scroll, 0);
        mouse(&mut a, MouseEventKind::Down(MouseButton::Right), 60, 20);
        assert_eq!(a.view, View::Dashboard);

        // The help overlay closes on a click; pointer movement is nothing,
        // not even a reason to redraw.
        press(&mut a, KeyCode::Char('?'));
        left_click(&mut a, 5, 5);
        assert!(!a.help);
        let _ = a.take_dirty();
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
        ] {
            assert!(mouse(&mut a, kind, 60, 20).is_empty());
        }
        assert!(!a.take_dirty());

        // /mouse hands the pointer back to the terminal, and takes it again.
        assert_eq!(submit(&mut a, "/mouse"), vec![Command::SetMouse(false)]);
        assert!(a.status.as_ref().unwrap().text.contains("mouse off"));
        a.mouse = false;
        let boxes = views::dashboard::agent_rects(&a, views::content_area(&a, SCREEN));
        assert!(
            left_click(&mut a, boxes[0].1.x + 2, boxes[0].1.y + 1).is_empty(),
            "off means off"
        );
        assert_eq!(a.view, View::Dashboard);
        assert_eq!(submit(&mut a, "/mouse"), vec![Command::SetMouse(true)]);
    }

    #[test]
    fn tab_completes_a_command_instead_of_changing_the_recipient() {
        let mut a = app();
        press(&mut a, KeyCode::Char('/'));
        type_text(&mut a, "st");
        press(&mut a, KeyCode::Tab);
        assert_eq!(
            (compose(&a).text.as_str(), compose(&a).cursor),
            ("/stop", 5)
        );
        assert_eq!(compose(&a).to, "claude-1");
        ctrl(&mut a, 'u');
        type_text(&mut a, "/k");
        press(&mut a, KeyCode::Tab);
        assert_eq!(compose(&a).text, "/kill ");
        type_text(&mut a, "claude-2");
        press(&mut a, KeyCode::Enter);
        assert_eq!(a.modal, Some(Modal::ConfirmKill("claude-2".into())));
    }

    #[test]
    fn compose_editing_and_recipient_cycling() {
        let mut a = app();
        press(&mut a, KeyCode::Char('m'));
        type_text(&mut a, "héllo wörld again");
        ctrl(&mut a, 'w');
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.text == "héllo wörld "));
        press(&mut a, KeyCode::Home);
        press(&mut a, KeyCode::Delete);
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.text == "éllo wörld "));
        press(&mut a, KeyCode::End);
        ctrl(&mut a, 'u');
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.text.is_empty() && c.cursor == 0));
        handle_paste(&mut a, "two\r\nlines\u{7}", 1_000_000);
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.text == "two\nlines"));

        press(&mut a, KeyCode::Tab);
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.to == "claude-2"));
        press(&mut a, KeyCode::BackTab);
        press(&mut a, KeyCode::BackTab);
        assert!(matches!(&a.modal, Some(Modal::Compose(c)) if c.to == "*"));
    }

    #[test]
    fn kill_requires_confirmation() {
        let mut a = app();
        assert!(press(&mut a, KeyCode::Char('k')).is_empty());
        assert_eq!(a.modal, Some(Modal::ConfirmKill("claude-1".into())));
        assert!(press(&mut a, KeyCode::Char('n')).is_empty());
        assert!(a.modal.is_none());
        press(&mut a, KeyCode::Char('k'));
        assert_eq!(
            press(&mut a, KeyCode::Char('y')),
            vec![Command::Kill("claude-1".into())]
        );
    }

    #[test]
    fn pause_toggles_and_restart_targets_the_selection() {
        let mut a = app();
        assert_eq!(
            press(&mut a, KeyCode::Char('p')),
            vec![Command::Pause("claude-1".into())]
        );
        a.agents[0].paused = true;
        assert_eq!(
            press(&mut a, KeyCode::Char('p')),
            vec![Command::Resume("claude-1".into())]
        );
        assert_eq!(
            press(&mut a, KeyCode::Char('r')),
            vec![Command::Restart("claude-1".into())]
        );
        // In the inspector `r` refreshes and `R` restarts.
        press(&mut a, KeyCode::Enter);
        let cmds = press(&mut a, KeyCode::Char('r'));
        assert!(cmds.contains(&Command::FetchOutput {
            id: "claude-1".into()
        }));
        assert_eq!(
            press(&mut a, KeyCode::Char('R')),
            vec![Command::Restart("claude-1".into())]
        );
        assert_eq!(
            press(&mut a, KeyCode::Char('d')),
            vec![Command::FetchDiff {
                id: "claude-1".into()
            }]
        );
        assert!(a.detail.show_diff);
    }

    #[test]
    fn herdr_focus_only_with_the_herdr_backend() {
        let mut a = app();
        assert_eq!(
            press(&mut a, KeyCode::Char('g')),
            vec![Command::Focus("claude-1".into())]
        );
        a.session.backend = "pty".into();
        assert!(press(&mut a, KeyCode::Char('g')).is_empty());
        assert!(a.status.as_ref().unwrap().is_error);
    }

    #[test]
    fn views_switch_and_logs_scroll_and_filter() {
        let mut a = app();
        press(&mut a, KeyCode::Char('t'));
        assert_eq!(a.view, View::Tasks);
        press(&mut a, KeyCode::Char('l'));
        assert_eq!(a.view, View::Logs);
        for i in 0..40 {
            let mut e = a.events.back().unwrap().clone();
            e.seq += 1;
            e.agent_id = Some(if i % 2 == 0 { "claude-1" } else { "claude-2" }.into());
            a.apply_push(crew_core::rpc::Push::Event(e), 0);
        }
        press(&mut a, KeyCode::PageUp);
        assert_eq!(a.logs.scroll, 10);
        press(&mut a, KeyCode::Home);
        assert_eq!(a.logs.scroll, a.events.len() - 10);
        press(&mut a, KeyCode::End);
        assert_eq!(a.logs.scroll, 0);
        press(&mut a, KeyCode::Char('f'));
        assert_eq!(a.logs.filter_agent.as_deref(), Some("claude-1"));
        assert!(logs::visible_events(&a)
            .iter()
            .all(|e| e.agent_id.as_deref() == Some("claude-1")));
        press(&mut a, KeyCode::Char('f'));
        assert_eq!(a.logs.filter_agent, None);
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.view, View::Dashboard);
    }

    #[test]
    fn help_opens_and_any_key_closes_it() {
        let mut a = app();
        press(&mut a, KeyCode::Char('?'));
        assert!(a.help);
        assert!(press(&mut a, KeyCode::Char('q')).is_empty());
        assert!(!a.help);
    }

    // ---- the model picker ---------------------------------------------------

    fn picker(a: &App) -> ModelPicker {
        match &a.modal {
            Some(Modal::Model(picker)) => picker.clone(),
            other => panic!("the model picker is not open: {other:?}"),
        }
    }

    fn listed(a: &mut App) {
        a.apply_reply(crate::app::Reply::Models(Ok(fixture::models())), 1_000_000);
    }

    #[test]
    fn the_operator_chooses_the_orchestrators_model_in_a_picker() {
        use crew_core::types::ORCHESTRATOR_ID;
        use crew_core::RuntimeKind;
        let mut a = app();
        a.selected = Some("claude-1".to_string());
        // O is the orchestrator's whatever is selected, and asks for the models.
        assert_eq!(
            press(&mut a, KeyCode::Char('O')),
            vec![Command::FetchModels]
        );
        assert_eq!(
            (picker(&a).agent.as_str(), picker(&a).runtime),
            (ORCHESTRATOR_ID, RuntimeKind::Claude)
        );
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        assert_eq!(
            picker(&a).notice.as_deref(),
            Some("the models are still being listed")
        );
        // The listing comes in: the highlight is on what it runs on.
        listed(&mut a);
        assert!(!a.models_pending);
        assert_eq!(picker(&a).row, 2);
        assert_eq!(a.current_row(ORCHESTRATOR_ID, RuntimeKind::Claude), Some(2));
        // Its own model again changes nothing.
        let mut same = a.clone();
        assert!(press(&mut same, KeyCode::Enter).is_empty());
        assert!(same.modal.is_none());
        assert!(same
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("orchestrator already runs on Sonnet 5.5"));
        // Another model of its CLI: the session is resumed on it.
        press(&mut a, KeyCode::Up);
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::SetModel {
                id: ORCHESTRATOR_ID.to_string(),
                model: "opus".to_string(),
                runtime: RuntimeKind::Claude,
                label: "restarting orchestrator on Opus 5.5, resuming its session".to_string(),
            }]
        );
        assert!(a.modal.is_none());
        assert!(a
            .status
            .as_ref()
            .unwrap()
            .text
            .starts_with("restarting orchestrator on Opus 5.5"));
        // A listing that comes in after the highlight was moved leaves it.
        press(&mut a, KeyCode::Char('O'));
        press(&mut a, KeyCode::Up);
        listed(&mut a);
        assert_eq!(picker(&a).row, 1);
        // Esc leaves it as it was.
        assert!(press(&mut a, KeyCode::Esc).is_empty());
        assert!(a.modal.is_none());
    }

    #[test]
    fn another_cli_is_a_tab_away_and_a_model_that_cannot_start_is_not_chosen() {
        use crew_core::types::ORCHESTRATOR_ID;
        use crew_core::RuntimeKind;
        let mut a = app();
        press(&mut a, KeyCode::Char('O'));
        listed(&mut a);
        // Codex: nothing is current there; the first model that starts.
        press(&mut a, KeyCode::Right);
        assert_eq!(
            (picker(&a).runtime, picker(&a).row),
            (RuntimeKind::Codex, 0)
        );
        press(&mut a, KeyCode::Down);
        press(&mut a, KeyCode::Down);
        assert_eq!(picker(&a).row, 1, "the last row is the last");
        assert!(press(&mut a, KeyCode::Enter).is_empty());
        let notice = picker(&a).notice.unwrap();
        assert!(
            notice.contains("GPT 6 Luna cannot be started here: not in the model list"),
            "{notice}"
        );
        press(&mut a, KeyCode::Up);
        assert_eq!(
            press(&mut a, KeyCode::Enter),
            vec![Command::SetModel {
                id: ORCHESTRATOR_ID.to_string(),
                model: "gpt-6-astra".to_string(),
                runtime: RuntimeKind::Codex,
                label: "moving orchestrator to Codex on GPT 6 Astra: a fresh session there, handed the work".to_string(),
            }]
        );
        // pi by its number, and its own default first.
        press(&mut a, KeyCode::Char('O'));
        press(&mut a, KeyCode::Char('3'));
        assert_eq!((picker(&a).runtime, picker(&a).row), (RuntimeKind::Pi, 0));
        let commands = press(&mut a, KeyCode::Enter);
        assert!(
            matches!(&commands[..], [Command::SetModel { model, runtime: RuntimeKind::Pi, .. }] if model.is_empty()),
            "{commands:?}"
        );
        // The tabs wrap around.
        press(&mut a, KeyCode::Char('O'));
        press(&mut a, KeyCode::Left);
        assert_eq!(picker(&a).runtime, RuntimeKind::Pi);
        press(&mut a, KeyCode::Tab);
        assert_eq!(
            (picker(&a).runtime, picker(&a).row),
            (RuntimeKind::Claude, 2)
        );
    }

    #[test]
    fn s_is_for_the_selected_or_inspected_agent_and_a_shell_has_no_model() {
        use crew_core::RuntimeKind;
        let mut a = app();
        a.selected = Some("claude-2".to_string());
        assert_eq!(
            press(&mut a, KeyCode::Char('s')),
            vec![Command::FetchModels]
        );
        assert_eq!(
            (picker(&a).agent.as_str(), picker(&a).runtime),
            ("claude-2", RuntimeKind::Codex)
        );
        press(&mut a, KeyCode::Esc);
        a.open_detail("claude-3", 1_000_000);
        press(&mut a, KeyCode::Char('s'));
        assert_eq!(picker(&a).agent, "claude-3");
        press(&mut a, KeyCode::Esc);
        // A command opens it too.
        a.view = View::Dashboard;
        open_command(&mut a);
        type_text(&mut a, "model claude-4");
        assert_eq!(press(&mut a, KeyCode::Enter), vec![Command::FetchModels]);
        assert_eq!(picker(&a).agent, "claude-4");
        press(&mut a, KeyCode::Esc);
        // A shell runs no model.
        a.agents
            .iter_mut()
            .find(|x| x.id == "claude-5")
            .unwrap()
            .runtime = RuntimeKind::Shell;
        a.selected = Some("claude-5".to_string());
        assert!(press(&mut a, KeyCode::Char('s')).is_empty());
        assert!(a.modal.is_none());
        assert!(a
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("claude-5 is a shell agent"));
    }

    #[test]
    fn a_question_from_the_team_opens_its_answer_box_by_itself_once() {
        use crew_core::rpc::Push;
        let t0 = 1_000_000;
        let mut a = app();
        let question = fixture::question(t0);
        a.apply_push(Push::Message(question.clone()), t0);
        // The band and the status line say so; the next tick opens the box,
        // and the bell is due once.
        assert!(a
            .status
            .as_ref()
            .unwrap()
            .text
            .contains("orchestrator, for claude-2 has a question for you"));
        a.tick(t0);
        assert_eq!(answer_box(&a).id, question.id);
        assert_eq!(answer_box(&a).popped_at, t0);
        assert!(a.take_ring());
        assert!(!a.take_ring(), "one bell per question");

        // An Enter right after it opened was meant for something else.
        let enter = |a: &mut App, at: i64| {
            handle_key(a, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), at, 10)
        };
        assert!(enter(&mut a, t0 + 300).is_empty());
        assert_eq!(answer_box(&a).id, question.id, "still open, nothing sent");
        // Put off with esc, it stays in the band and does not open again.
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            t0 + 400,
            10,
        );
        a.tick(t0 + 500);
        assert!(a.modal.is_none());
        assert_eq!(a.open_questions().len(), 1);

        // While the operator types a message, a new question waits; it opens
        // once the message box is closed.
        let mut second = question.clone();
        second.id = "m-later".into();
        open_compose(&mut a, None);
        a.apply_push(Push::Message(second.clone()), t0 + 600);
        a.tick(t0 + 600);
        assert!(matches!(a.modal, Some(Modal::Compose(_))));
        handle_key(
            &mut a,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            t0 + 700,
            10,
        );
        a.tick(t0 + 800);
        assert_eq!(answer_box(&a).id, second.id);
        // Past the moment, Enter sends the selected answer.
        assert_eq!(
            enter(&mut a, t0 + 800 + crate::app::POPUP_GRACE_MS),
            vec![Command::Answer {
                id: second.id.clone(),
                body: "Convert in the adapter".into(),
            }]
        );
        // Opened with `a`, the box takes Enter at once.
        a.modal = None;
        press(&mut a, KeyCode::Char('a'));
        assert_eq!(answer_box(&a).popped_at, 0);

        // Typing into an agent's terminal holds a question back too.
        let mut b = app();
        b.open_detail("claude-1", t0);
        b.detail.typing = true;
        b.apply_push(Push::Message(question.clone()), t0);
        b.tick(t0);
        assert!(b.modal.is_none());
        b.detail.typing = false;
        b.tick(t0 + 100);
        assert_eq!(answer_box(&b).id, question.id);
    }
}
