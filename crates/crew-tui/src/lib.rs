//! Terminal dashboard for crew.
//!
//! [`run`] attaches a full-screen view to a running daemon: agents arranged
//! around the orchestrator, messages animating along the links between them,
//! the message bus log, and inspectors for agents, tasks and events. It is a
//! pure presentation layer — every box corresponds to a real agent reported
//! by the daemon, and leaving the dashboard never stops anything.

pub mod anim;
pub mod app;
#[doc(hidden)]
pub mod fixture;
pub mod input;
pub mod launcher;
pub mod layout;
mod net;
mod shared;
pub mod slash;
pub mod theme;
pub mod transcript;
pub mod views;
pub mod widgets;

use std::io::{self, Stdout};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Once};
use std::time::Instant;

use anyhow::{Context, Result};
use crew_core::signals::TerminationSignals;
use crew_core::util::now_ms;
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::crossterm::{cursor, execute};
use ratatui::layout::Rect;
use ratatui::Terminal;

pub use app::App;
use app::{Command, View};
pub use launcher::{run_launcher, Launch, Launcher, StyleChoice};
use net::UiEvent;
use theme::Theme;

/// How the dashboard was left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The dashboard was closed.
    Detached,
    /// The team was stopped from the dashboard: agents and daemon are gone.
    Stopped,
}

/// Run the dashboard against the daemon listening on `socket` until the user
/// quits. Agents and the daemon are left running unless the user stopped the
/// team from the dashboard, which the result says.
///
/// `model_names` gives the proper names of known models by every name they
/// go by, for agents the daemon sent without one. `notice` is something the
/// user should know on arrival; it stays in the footer for a while.
pub fn run(
    socket: &Path,
    model_names: std::collections::HashMap<String, String>,
    notice: Option<String>,
) -> Result<Exit> {
    net::probe(socket).context("cannot attach the dashboard")?;

    let (tx, rx) = mpsc::channel::<UiEvent>();
    let stop = Arc::new(AtomicBool::new(false));
    let signals = TerminationSignals::install().context("watching termination signals")?;
    let mut terminal = enter_terminal().context("preparing the terminal")?;
    let input = match net::spawn_input(tx.clone(), stop.clone()) {
        Ok(input) => input,
        Err(error) => {
            leave_terminal();
            return Err(error).context("starting terminal input");
        }
    };
    let mut app = App::new();
    let subscriber = net::spawn_subscriber(socket.to_path_buf(), tx.clone(), stop.clone());
    let mut requests =
        net::spawn_scoped_requester(socket.to_path_buf(), tx, Some(app.shared.dispatch_guard()));
    requests.subscriber = Some(subscriber);

    let theme = Theme::detect();
    app.model_names = model_names;
    if let Some(notice) = notice {
        app.set_notice(notice, now_ms());
    }
    // CREW_BELL=0: a question that opens by itself does not ring the bell.
    if std::env::var("CREW_BELL").is_ok_and(|v| matches!(v.as_str(), "0" | "off" | "false" | "no"))
    {
        app.bell = false;
    }
    // CREW_MOUSE=0 leaves the mouse to the terminal (its own text selection).
    if std::env::var("CREW_MOUSE").is_ok_and(|v| matches!(v.as_str(), "0" | "off" | "false" | "no"))
    {
        app.mouse = false;
        set_mouse(false);
    }
    let result = event_loop(&mut terminal, &mut app, &theme, &rx, &requests, &signals);

    stop.store(true, Ordering::Relaxed);
    // Restore the terminal before waiting for host cleanup. A token read or
    // Unix dial still in progress has not yet published a close handle.
    leave_terminal();
    requests.shutdown();
    let _ = input.join();
    if result.is_ok() && app.daemon_stopped && !app.stopped {
        use std::io::Write as _;
        let _ = writeln!(io::stderr(), "the crew daemon stopped");
    }
    result.map(|()| {
        if app.stopped {
            Exit::Stopped
        } else {
            Exit::Detached
        }
    })
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    theme: &Theme,
    rx: &Receiver<UiEvent>,
    requests: &net::Requests,
    signals: &TerminationSignals,
) -> Result<()> {
    let started = Instant::now();
    loop {
        if signals.is_pending() {
            return Ok(());
        }
        let now = now_ms();
        app.shared.set_clock(crew_client::ClockSample {
            wall_ms: now,
            monotonic_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        });
        app.tick(now);
        for command in app.due_commands(now) {
            send_shared(app, command, requests, now);
        }
        if app.take_dirty() {
            terminal.draw(|frame| views::render(frame, app, theme, now))?;
        }
        // A question just opened by itself: the bell, for a terminal that
        // is not in front (most terminals and multiplexers flag the window).
        if app.take_ring() {
            use std::io::Write as _;
            let mut out = io::stdout();
            let _ = out.write_all(b"\x07").and_then(|()| out.flush());
        }
        // Sleep until something happens or the next animation frame is due.
        match rx.recv_timeout(app.next_wake(now)) {
            Ok(event) => {
                let size = terminal.size().unwrap_or_default();
                let area = Rect::new(0, 0, size.width, size.height);
                let page = page_rows(app, area);
                app.shared.set_clock(crew_client::ClockSample {
                    wall_ms: now_ms(),
                    monotonic_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                });
                handle(app, event, requests, page, area)?;
                if app.should_quit || signals.is_pending() {
                    return Ok(());
                }
                // Drain whatever else is queued so a burst costs one redraw.
                while let Ok(event) = rx.try_recv() {
                    app.shared.set_clock(crew_client::ClockSample {
                        wall_ms: now_ms(),
                        monotonic_ms: started.elapsed().as_millis().min(u128::from(u64::MAX))
                            as u64,
                    });
                    let page = page_rows(app, area);
                    handle(app, event, requests, page, area)?;
                    if app.should_quit || signals.is_pending() {
                        return Ok(());
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

fn page_rows(app: &App, area: Rect) -> usize {
    let area = if area.height == 0 {
        Rect::new(0, 0, 80, 24)
    } else {
        area
    };
    let height = views::view_area(app, area).height;
    match app.view {
        View::Logs => views::logs::page_rows(height),
        _ => usize::from(height.saturating_sub(4)).max(1),
    }
}

fn handle(
    app: &mut App,
    event: UiEvent,
    requests: &net::Requests,
    page: usize,
    area: Rect,
) -> Result<()> {
    let now = now_ms();
    let run = |app: &mut App, commands: Vec<Command>| {
        for command in commands {
            match command {
                Command::Quit => app.should_quit = true,
                // The terminal's business, not the daemon's.
                Command::SetMouse(on) => {
                    app.mouse = on;
                    set_mouse(on);
                }
                command => {
                    send_shared(app, command, requests, now);
                }
            }
        }
    };
    match event {
        UiEvent::ScopedSnapshot { epoch, snapshot } => {
            if app.shared.accept_epoch(epoch, true) {
                app.apply_snapshot(*snapshot, now);
            }
        }
        UiEvent::ScopedPush { epoch, push } => {
            if app.shared.accept_epoch(epoch, false) {
                app.apply_push(*push, now);
            }
        }
        UiEvent::ScopedDisconnected { epoch } => {
            if app.shared.accept_epoch(epoch, false) {
                app.disconnected_at(now);
            }
        }
        UiEvent::PlanKeys {
            scope,
            id,
            text,
            reply,
        } => {
            if app.shared.read(|core| core.scope() == Some(&scope)) {
                let mut planned = app.prepare(Command::Keys { id, text }, now).into_iter();
                if let Err(error) = reply.send(planned.next()) {
                    if let Some(planned) = error.0 {
                        undelivered(app, planned, "closed", "terminal input worker closed", now);
                    }
                }
                for request in planned {
                    send_planned(app, request, requests, now);
                }
            } else {
                app.set_status(
                    "queued terminal input discarded: connection or target changed",
                    true,
                    now,
                );
                let _ = reply.send(None);
            }
        }
        UiEvent::DiscardedInput => app.set_status(
            "queued terminal input discarded: connection or target changed",
            true,
            now,
        ),
        UiEvent::Input(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => {
            let commands = input::handle_key(app, key, now, page);
            run(app, commands);
            app.capture_confirmation(now);
        }
        UiEvent::Input(TermEvent::Key(_)) => {}
        UiEvent::Input(TermEvent::Mouse(mouse)) => {
            let commands = input::handle_mouse(app, mouse, area, now, page);
            run(app, commands);
            app.capture_confirmation(now);
        }
        UiEvent::Input(TermEvent::Paste(text)) => {
            let commands = input::handle_paste(app, &text, now);
            run(app, commands);
            app.capture_confirmation(now);
        }
        UiEvent::Input(_) => app.mark_dirty(),
        UiEvent::Reply(reply) => {
            let before = app.shared.read(crew_client::ClientCore::revision);
            let reconnect = matches!(
                reply.as_ref(),
                app::Reply::Shared {
                    outcome: crew_client::OperationOutcome::OutcomeUnknown { .. }
                        | crew_client::OperationOutcome::NotSent { .. },
                    ..
                }
            );
            app.apply_reply(*reply, now);
            if reconnect && app.shared.read(crew_client::ClientCore::revision) != before {
                app.disconnected_at(now);
                requests.restart_subscriber();
            }
        }
        UiEvent::Fatal(error) => return Err(error.into()),
    }
    for planned in app.shared.take_requests() {
        send_planned(app, planned, requests, now);
    }
    Ok(())
}

fn send_shared(app: &mut App, command: Command, requests: &net::Requests, now: i64) {
    if let Command::Keys { id, text } = &command {
        if let Some(scope) = app.shared.read(|core| core.scope().cloned()) {
            if let Err(error) = requests.send(Command::RawKeys {
                scope,
                id: id.clone(),
                text: text.clone(),
            }) {
                app.set_status(format!("keys not delivered: {error}"), true, now);
            }
        } else {
            app.set_status("keys not delivered: no authenticated snapshot", true, now);
        }
        return;
    }
    for planned in app.prepare(command, now) {
        send_planned(app, planned, requests, now);
    }
}

fn send_planned(app: &mut App, planned: shared::Planned, requests: &net::Requests, now: i64) {
    let captured = planned.clone();
    if let Err(error) = requests.send(Command::Planned(Box::new(planned))) {
        undelivered(
            app,
            captured,
            match error {
                net::QueueError::Full => "queue_full",
                net::QueueError::Closed => "closed",
            },
            &error.to_string(),
            now,
        );
    }
}

fn undelivered(app: &mut App, planned: shared::Planned, code: &str, message: &str, now: i64) {
    app.apply_reply(
        app::Reply::Shared {
            scope: planned.scope,
            operation_id: planned.operation_id,
            outcome: crew_client::OperationOutcome::NotSent {
                reason: crew_client::ClientReason {
                    details: None,
                    code: code.into(),
                    message: message.into(),
                },
            },
        },
        now,
    );
}

fn enter_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    // A panic must not leave the user's terminal in raw mode on the alternate
    // screen; restore it before the default hook prints the message.
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            leave_terminal();
            previous(info);
        }));
    });
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(e) = execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        cursor::Hide
    ) {
        leave_terminal();
        return Err(e);
    }
    // A terminal that speaks the kitty keyboard protocol tells Shift+Enter
    // (a new line in a box) from Enter; a plain one sends the same byte for
    // both. The query needs raw mode, which is on by now.
    if supports_keyboard_enhancement().unwrap_or(false)
        && execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
    {
        KEYS_ENHANCED.store(true, Ordering::Relaxed);
    }
    Terminal::new(CrosstermBackend::new(stdout)).inspect_err(|_| leave_terminal())
}

/// Whether the terminal was switched to the kitty keyboard protocol, which
/// has to be switched back on the way out.
static KEYS_ENHANCED: AtomicBool = AtomicBool::new(false);

fn leave_terminal() {
    if KEYS_ENHANCED.swap(false, Ordering::Relaxed) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
}

/// Ask the terminal to report mouse clicks and the wheel, or to stop: with
/// reporting off, dragging selects text the terminal's usual way.
fn set_mouse(on: bool) {
    let _ = if on {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        execute!(io::stdout(), DisableMouseCapture)
    };
}

/// Render one frame of `app` at the given size as plain text. Used by the
/// rendering tests and the `preview` example.
#[doc(hidden)]
pub fn render_to_string(app: &App, width: u16, height: u16, now: i64) -> String {
    let mut terminal =
        Terminal::new(TestBackend::new(width, height)).expect("test backend is infallible");
    let theme = Theme::truecolor();
    terminal
        .draw(|frame| views::render(frame, app, &theme, now))
        .expect("test backend is infallible");
    widgets::canvas::buffer_text(terminal.backend().buffer())
}

#[cfg(test)]
#[path = "../../crew-core/src/test_support.rs"]
mod test_support;

#[cfg(test)]
mod paging_tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn event_numbers(screen: &str) -> Vec<usize> {
        screen
            .lines()
            .filter_map(|line| {
                let at = line.find("EVENT-")? + "EVENT-".len();
                line[at..at + 4].parse().ok()
            })
            .collect()
    }

    #[test]
    fn warning_and_questions_do_not_hide_oldest_logs_or_skip_a_page_line() {
        for height in [12, 24] {
            for mode in ["off", "required", ""] {
                for (questions, socket_warning) in
                    [(false, false), (true, false), (false, true), (true, true)]
                {
                    let mut app = App::new();
                    app.apply_snapshot(fixture::snapshot(1_000), 1_000);
                    app.view = View::Logs;
                    app.session.sandbox = mode.into();
                    app.session.socket_warning =
                        if socket_warning { "fallback" } else { "" }.into();
                    app.events = (0..200)
                        .map(|n| {
                            fixture::event(
                                n,
                                crew_core::EventLevel::Info,
                                "test",
                                "test.event",
                                None,
                                &format!("EVENT-{n:04}"),
                                1_000,
                            )
                        })
                        .collect();
                    if questions {
                        app.questions.push(fixture::question(1_000));
                    }
                    let page = page_rows(&app, Rect::new(0, 0, 100, height));
                    input::handle_key(
                        &mut app,
                        KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
                        1_000,
                        page,
                    );
                    let first = event_numbers(&render_to_string(&app, 100, height, 1_000));
                    assert_eq!(
                        first.first(),
                        Some(&0),
                        "{mode}/{height}/question={questions}"
                    );
                    let last = *first.last().unwrap();
                    input::handle_key(
                        &mut app,
                        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                        1_000,
                        page,
                    );
                    let next = event_numbers(&render_to_string(&app, 100, height, 1_000));
                    assert_eq!(next.first(), Some(&(last + 1)), "pages must be contiguous");
                }
            }
        }
    }

    #[test]
    fn logs_detail_and_tasks_page_from_the_space_below_both_bands() {
        for view in [View::Logs, View::Detail, View::Tasks] {
            let mut app = App::new();
            app.apply_snapshot(fixture::snapshot(1_000), 1_000);
            app.view = view;
            app.session.sandbox = "required".into();
            let full = page_rows(&app, Rect::new(0, 0, 100, 24));
            app.session.sandbox = "off".into();
            assert_eq!(
                page_rows(&app, Rect::new(0, 0, 100, 24)) + 1,
                full,
                "{view:?} warning row"
            );
            app.questions.push(fixture::question(1_000));
            assert_eq!(
                page_rows(&app, Rect::new(0, 0, 100, 24)) + 2,
                full,
                "{view:?} question band"
            );
            app.session.socket_warning = "fallback".into();
            assert_eq!(
                page_rows(&app, Rect::new(0, 0, 100, 24)) + 3,
                full,
                "{view:?} socket warning below sandbox and question rows"
            );
        }
    }
}
