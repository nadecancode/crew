//! `crew agent attach`: put the user's terminal on an agent's PTY.
//!
//! Output streams from the daemon; keystrokes are forwarded back. `Ctrl-]`
//! detaches and leaves the agent running.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use base64::Engine;
use crew_core::client::{Client, ClientError};
use crew_core::rpc::{Call, Push};
use ratatui::crossterm::terminal;

/// `Ctrl-]`, the classic telnet escape.
const DETACH: u8 = 0x1d;

/// Undo whatever modes the agent's TUI switched on in our terminal.
const RESTORE: &[u8] =
    b"\x1b[?1049l\x1b[?25h\x1b[0m\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1004l";

struct RawMode;

impl RawMode {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let mut out = std::io::stdout();
        let _ = out.write_all(RESTORE);
        let _ = out.flush();
    }
}

pub fn run(socket: &Path, agent: &str) -> Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((120, 40));
    let (mut client, _) = Client::operator(socket, "attach")?;
    if let Err(e) = client.call_value(Call::Attach {
        agent: agent.to_string(),
        rows,
        cols,
    }) {
        bail!("cannot attach to {agent}: {e}");
    }
    let (mut reader, mut writer) = client.split();
    let done = Arc::new(AtomicBool::new(false));
    let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);

    eprintln!("[attached to {agent} — press Ctrl-] to detach]");
    let _raw = RawMode::enter()?;

    // Keystrokes and window-size changes go to the daemon.
    {
        let done = done.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 1024];
            let mut size = (cols, rows);
            loop {
                if done.load(Ordering::SeqCst) {
                    break;
                }
                let n = match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let chunk = &buf[..n];
                if let Some(at) = chunk.iter().position(|b| *b == DETACH) {
                    if at > 0 {
                        let _ = writer.send(Call::AttachInput {
                            data: encode(&chunk[..at]),
                        });
                    }
                    break;
                }
                if let Ok(now) = terminal::size() {
                    if now != size {
                        size = now;
                        let _ = writer.send(Call::AttachResize {
                            rows: now.1,
                            cols: now.0,
                        });
                    }
                }
                if writer
                    .send(Call::AttachInput {
                        data: encode(chunk),
                    })
                    .is_err()
                {
                    break;
                }
            }
            done.store(true, Ordering::SeqCst);
            // Unblock the reader, which is waiting for terminal output.
            writer.shutdown();
        });
    }

    // Terminal output comes back as pushes.
    let mut stdout = std::io::stdout();
    let mut reason = "detached";
    loop {
        if done.load(Ordering::SeqCst) {
            break;
        }
        match reader.next() {
            Ok(frame) => match frame.push {
                Some(Push::Pty { data }) => {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                        let _ = stdout.write_all(&bytes);
                        let _ = stdout.flush();
                    }
                }
                Some(Push::PtyClosed) => {
                    reason = "the agent's process ended";
                    break;
                }
                Some(Push::Shutdown) => {
                    reason = "the daemon stopped";
                    break;
                }
                _ => {}
            },
            Err(ClientError::Closed) => {
                if !done.load(Ordering::SeqCst) {
                    reason = "the daemon closed the connection";
                }
                break;
            }
            Err(_) => break,
        }
    }
    done.store(true, Ordering::SeqCst);
    drop(_raw);
    // Let the input thread notice; it is blocked in read() until a key arrives,
    // so do not join it.
    std::thread::sleep(Duration::from_millis(20));
    eprintln!("\r\n[{reason}; {agent} keeps running — `crew attach` for the dashboard]");
    // Exit directly: the input thread may still be blocked reading stdin.
    std::process::exit(0);
}
