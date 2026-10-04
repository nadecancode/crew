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

use anyhow::{Context, Result};
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
    /// The dashboard was closed; agents and the daemon keep running.
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
    let mut terminal = enter_terminal().context("preparing the terminal")?;
    net::spawn_input(tx.clone(), stop.clone());
    net::spawn_subscriber(socket.to_path_buf(), tx.clone(), stop.clone());
    let requests = net::spawn_requester(socket.to_path_buf(), tx);

    let theme = Theme::detect();
    let mut app = App::new();
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
    let result = event_loop(&mut terminal, &mut app, &theme, &rx, &requests);

    stop.store(true, Ordering::Relaxed);
    leave_terminal();
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
) -> Result<()> {
    loop {
        let now = now_ms();
        app.tick(now);
        for command in app.due_commands(now) {
            let _ = requests.send(command);
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
                let page = page_rows(app, if area.height == 0 { 24 } else { area.height });
                handle(app, event, requests, page, area);
                // Drain whatever else is queued so a burst costs one redraw.
                while let Ok(event) = rx.try_recv() {
                    handle(app, event, requests, page, area);
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

fn page_rows(app: &App, height: u16) -> usize {
    match app.view {
        View::Logs => views::logs::page_rows(height),
        _ => usize::from(height.saturating_sub(4)).max(1),
    }
}

fn handle(app: &mut App, event: UiEvent, requests: &net::Requests, page: usize, area: Rect) {
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
                    let _ = requests.send(command);
                }
            }
        }
    };
    match event {
        UiEvent::Input(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => {
            let commands = input::handle_key(app, key, now, page);
            run(app, commands);
        }
        UiEvent::Input(TermEvent::Key(_)) => {}
        UiEvent::Input(TermEvent::Mouse(mouse)) => {
            let commands = input::handle_mouse(app, mouse, area, now, page);
            run(app, commands);
        }
        UiEvent::Input(TermEvent::Paste(text)) => {
            let commands = input::handle_paste(app, &text, now);
            run(app, commands);
        }
        UiEvent::Input(_) => app.mark_dirty(),
        UiEvent::Snapshot(snapshot) => app.apply_snapshot(*snapshot, now),
        UiEvent::Push(push) => app.apply_push(*push, now),
        UiEvent::Disconnected => app.disconnected(),
        UiEvent::Reply(reply) => app.apply_reply(*reply, now),
    }
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
