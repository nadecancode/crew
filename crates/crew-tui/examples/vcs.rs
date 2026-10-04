//! Play every version control animation from fixtures — no daemon needed.
//!
//! ```text
//! cargo run -p crew-tui --example vcs              # live in the terminal, q quits
//! cargo run -p crew-tui --example vcs -- --loop    # again and again
//! cargo run -p crew-tui --example vcs -- --dump    # plain-text frames, no terminal
//! cargo run -p crew-tui --example vcs -- --dump 80 24   # another size
//! ```
//! The script is `fixture::vcs_script`: a workspace, a commit, reads, a
//! cherry-pick, a merge that runs and conflicts, a failed read, an integration.

use std::io::stdout;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crew_core::rpc::Push;
use crew_core::vcs::VcsEvent;
use crew_tui::theme::Theme;
use crew_tui::{fixture, render_to_string, views, App};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event as Term, KeyCode, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

fn clock() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn push(app: &mut App, seq: i64, at: i64, text: &str, event: &VcsEvent) {
    app.apply_push(
        Push::Event(fixture::vcs_event(seq, at, text, event.clone())),
        at,
    );
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dump = args.iter().any(|a| a == "--dump");
    let again = args.iter().any(|a| a == "--loop");
    let sizes: Vec<u16> = args.iter().filter_map(|a| a.parse().ok()).collect();
    let width = sizes.first().copied().unwrap_or(120);
    let height = sizes.get(1).copied().unwrap_or(40);
    let script = fixture::vcs_script();

    if dump {
        let now = 1_790_000_000_000;
        let mut app = App::new();
        app.apply_snapshot(fixture::with_subagents(now), now);
        for (i, (offset, text, event)) in script.iter().enumerate() {
            let at = now + offset;
            push(&mut app, 100 + i as i64, at, text, event);
            // A moment into the pulse and sweep, then once it is all lit.
            for after in [450, 1500] {
                println!(
                    "── {} {:?} {} @ +{}ms ──",
                    event.event_type(),
                    event.outcome,
                    event.agent,
                    offset + after
                );
                print!("{}", render_to_string(&app, width, height, at + after));
            }
        }
        return Ok(());
    }

    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let result = play(&script, again);
    execute!(stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}

/// Run the script against the wall clock, drawing the way the dashboard does:
/// on the app's own schedule.
fn play(script: &[(i64, String, VcsEvent)], again: bool) -> std::io::Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let theme = Theme::truecolor();
    loop {
        let start = clock();
        let mut app = App::new();
        app.apply_snapshot(fixture::with_subagents(start), start);
        let mut next = 0;
        let end = script.last().map_or(0, |(at, ..)| *at) + 5000;
        loop {
            let now = clock();
            while next < script.len() && now - start >= script[next].0 {
                let (_, text, event) = &script[next];
                push(&mut app, 100 + next as i64, now, text, event);
                next += 1;
            }
            app.tick(now);
            if app.take_dirty() {
                terminal.draw(|frame| views::render(frame, &app, &theme, now))?;
            }
            if now - start > end {
                break;
            }
            let wait = app.next_wake(now).min(Duration::from_millis(100));
            if event::poll(wait)? {
                if let Term::Key(key) = event::read()? {
                    let quit = key.code == KeyCode::Char('q')
                        || key.code == KeyCode::Esc
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL));
                    if quit {
                        return Ok(());
                    }
                }
            }
        }
        if !again {
            return Ok(());
        }
    }
}
