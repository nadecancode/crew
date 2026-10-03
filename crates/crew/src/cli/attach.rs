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
    // What the agent prints may draw on our terminal but not drive it.
    let mut filter = TerminalFilter::default();
    let mut reason = "detached";
    loop {
        if done.load(Ordering::SeqCst) {
            break;
        }
        match reader.next() {
            Ok(frame) => match frame.push {
                Some(Push::Pty { data }) => {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                        let _ = stdout.write_all(&filter.feed(&bytes));
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

/// Longest CSI sequence kept; a longer one is abandoned.
const MAX_CSI: usize = 64;
/// Longest window title kept.
const MAX_TITLE: usize = 4096;
/// Most bytes an unterminated string (OSC, DCS, APC, PM, SOS) is followed
/// before it is abandoned. Nothing past the title limit is stored.
const MAX_STRING: usize = 64 * 1024;

/// What the agent's terminal output may do to the operator's terminal: it may
/// draw, but it may not write the clipboard (OSC 52), set hyperlinks or other
/// OSC state, run DCS/APC/PM/SOS strings, or ask the terminal questions whose
/// answers would be typed back into the agent (device attributes, cursor
/// position, window reports, mode queries). Window titles, colours (SGR),
/// cursor movement, erasing and the private modes TUIs switch are kept.
///
/// The filter is a state machine, so a sequence split across chunks is handled
/// as if it had arrived whole, and what it holds back or counts is bounded.
#[derive(Default)]
pub struct TerminalFilter {
    state: State,
    /// Continuation bytes still owed by a UTF-8 sequence: while it is open,
    /// bytes in 0x80..=0x9f are text, not 8-bit C1 controls.
    utf8_pending: u8,
}

#[derive(Default)]
enum State {
    #[default]
    Ground,
    /// After ESC.
    Esc,
    /// ESC, intermediates (0x20..=0x2f), then a final byte.
    EscSeq(Vec<u8>),
    /// A CSI sequence: everything between the introducer and the final byte.
    Csi(Vec<u8>),
    /// An OSC string. `title` collects what a title would keep (until it is
    /// too long: `overflow`); `seen` counts every byte of the string.
    Osc {
        title: Vec<u8>,
        seen: usize,
        esc: bool,
        overflow: bool,
    },
    /// DCS, APC, PM or SOS: dropped to the terminator.
    Str { seen: usize, esc: bool },
}

impl TerminalFilter {
    /// The part of `data` that is safe to show, given everything fed before.
    pub fn feed(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        for &b in data {
            self.step(b, &mut out);
        }
        out
    }

    /// Bytes currently held while a sequence is open (what tests bound).
    #[cfg(test)]
    pub fn buffered(&self) -> usize {
        match &self.state {
            State::EscSeq(b) | State::Csi(b) => b.len(),
            State::Osc { title, .. } => title.len(),
            _ => 0,
        }
    }

    /// Whether `b` is an 8-bit C1 control (and not part of a UTF-8 character).
    fn is_c1(&self, b: u8) -> bool {
        self.utf8_pending == 0 && (0x80..=0x9f).contains(&b)
    }

    fn track_utf8(&mut self, b: u8) {
        if self.utf8_pending > 0 && (0x80..=0xbf).contains(&b) {
            self.utf8_pending -= 1;
        } else {
            self.utf8_pending = match b {
                0xc2..=0xdf => 1,
                0xe0..=0xef => 2,
                0xf0..=0xf4 => 3,
                _ => 0,
            };
        }
    }

    fn step(&mut self, b: u8, out: &mut Vec<u8>) {
        // `is_c1` is asked before the byte is tracked: a UTF-8 continuation
        // byte is text in every state.
        let c1 = self.is_c1(b);
        self.track_utf8(b);
        let state = std::mem::take(&mut self.state);
        match state {
            State::Ground => self.ground(b, c1, out),
            State::Esc => self.after_esc(b, out),
            State::EscSeq(mut seq) => match b {
                0x20..=0x2f if seq.len() < 8 => {
                    seq.push(b);
                    self.state = State::EscSeq(seq);
                }
                0x30..=0x7e => {
                    out.push(0x1b);
                    out.extend_from_slice(&seq);
                    out.push(b);
                }
                _ => self.reprocess(b, out),
            },
            State::Csi(mut seq) => match b {
                0x20..=0x3f if seq.len() < MAX_CSI => {
                    seq.push(b);
                    self.state = State::Csi(seq);
                }
                0x40..=0x7e => {
                    if keep_csi(&seq, b) {
                        out.extend_from_slice(b"\x1b[");
                        out.extend_from_slice(&seq);
                        out.push(b);
                    }
                }
                // Too long, or a control byte: the sequence is abandoned.
                _ => self.reprocess(b, out),
            },
            State::Osc {
                mut title,
                mut seen,
                esc,
                mut overflow,
            } => {
                seen += 1;
                if esc {
                    if b == b'\\' {
                        if !overflow {
                            finish_title(&title, out);
                        }
                    } else {
                        self.reprocess_after_esc(b, out);
                    }
                } else if b == 0x07 || (c1 && b == 0x9c) {
                    if !overflow {
                        finish_title(&title, out);
                    }
                } else if b == 0x1b {
                    self.state = State::Osc {
                        title,
                        seen,
                        esc: true,
                        overflow,
                    };
                } else if matches!(b, 0x18 | 0x1a) || seen > MAX_STRING {
                    // Cancelled, or abandoned at the limit: nothing is shown.
                } else {
                    if !overflow {
                        if title.len() < MAX_TITLE {
                            title.push(b);
                        } else {
                            // Too long for a title: drop it, store nothing more.
                            overflow = true;
                            title.clear();
                        }
                    }
                    self.state = State::Osc {
                        title,
                        seen,
                        esc: false,
                        overflow,
                    };
                }
            }
            State::Str { mut seen, esc } => {
                seen += 1;
                if esc {
                    if b != b'\\' {
                        self.reprocess_after_esc(b, out);
                    }
                } else if b == 0x1b {
                    self.state = State::Str { seen, esc: true };
                } else if (c1 && b == 0x9c) || matches!(b, 0x18 | 0x1a) {
                } else if seen <= MAX_STRING {
                    self.state = State::Str { seen, esc: false };
                }
            }
        }
    }

    /// `b` ended whatever was open without belonging to it: handle it afresh.
    fn reprocess(&mut self, b: u8, out: &mut Vec<u8>) {
        self.state = State::Ground;
        match b {
            0x18 | 0x1a => {}
            _ => self.step_from_ground(b, out),
        }
    }

    /// An ESC was seen inside a string and `b` is not `\`: the string is over
    /// and `ESC b` starts a new sequence.
    fn reprocess_after_esc(&mut self, b: u8, out: &mut Vec<u8>) {
        self.state = State::Esc;
        self.after_esc(b, out);
    }

    fn step_from_ground(&mut self, b: u8, out: &mut Vec<u8>) {
        let c1 = self.utf8_pending == 0 && (0x80..=0x9f).contains(&b);
        self.ground(b, c1, out);
    }

    fn ground(&mut self, b: u8, c1: bool, out: &mut Vec<u8>) {
        match b {
            0x1b => self.state = State::Esc,
            // ENQ makes some terminals answer with a string.
            0x05 => {}
            0x9b if c1 => self.state = State::Csi(Vec::new()),
            0x9d if c1 => self.state = osc(),
            0x90 | 0x98 | 0x9e | 0x9f if c1 => {
                self.state = State::Str {
                    seen: 0,
                    esc: false,
                }
            }
            _ if c1 => {}
            _ => out.push(b),
        }
    }

    fn after_esc(&mut self, b: u8, out: &mut Vec<u8>) {
        match b {
            b'[' => self.state = State::Csi(Vec::new()),
            b']' => self.state = osc(),
            b'P' | b'_' | b'^' | b'X' => {
                self.state = State::Str {
                    seen: 0,
                    esc: false,
                }
            }
            0x1b => self.state = State::Esc,
            0x20..=0x2f => self.state = State::EscSeq(vec![b]),
            // DECID answers like a device-attributes query; ST on its own is
            // nothing; RIS would wipe the operator's terminal.
            b'Z' | b'\\' | b'c' => {}
            0x30..=0x7e => {
                out.push(0x1b);
                out.push(b);
            }
            _ => self.reprocess(b, out),
        }
    }
}

fn osc() -> State {
    State::Osc {
        title: Vec::new(),
        seen: 0,
        esc: false,
        overflow: false,
    }
}

/// Emit a window title (OSC 0, 1, 2) with every control character removed;
/// any other OSC is dropped.
fn finish_title(raw: &[u8], out: &mut Vec<u8>) {
    let Some(at) = raw.iter().position(|b| *b == b';') else {
        return;
    };
    if !matches!(&raw[..at], b"0" | b"1" | b"2") {
        return;
    }
    let text: String = String::from_utf8_lossy(&raw[at + 1..])
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    out.extend_from_slice(b"\x1b]");
    out.extend_from_slice(&raw[..at]);
    out.push(b';');
    out.extend_from_slice(text.as_bytes());
    out.push(0x07);
}

/// Whether a complete CSI sequence is drawing (kept) rather than a query or a
/// report request. `params` is everything between `ESC [` and `final_byte`.
fn keep_csi(params: &[u8], final_byte: u8) -> bool {
    let private = params.first().is_some_and(|b| (0x3c..=0x3f).contains(b));
    let intermediates: Vec<u8> = params
        .iter()
        .copied()
        .filter(|b| (0x20..=0x2f).contains(b))
        .collect();
    if private {
        // The modes TUIs switch (`?1049h`, `?25l`, `?2004h`); not the
        // keyboard-protocol, key-modifier or other private reports.
        return matches!(final_byte, b'h' | b'l') && intermediates.is_empty();
    }
    match final_byte {
        // Cursor movement and position, erase, insert/delete, scroll,
        // SGR, save/restore, modes, scroll region.
        b'm'
        | b'A'..=b'H'
        | b'f'
        | b'J'
        | b'K'
        | b'L'
        | b'M'
        | b'P'
        | b'S'
        | b'T'
        | b'X'
        | b'@'
        | b'd'
        | b'e'
        | b'`'
        | b'a'
        | b'g'
        | b'I'
        | b'Z'
        | b'b'
        | b's'
        | b'u'
        | b'h'
        | b'l'
        | b'r' => intermediates.is_empty(),
        // DECSCUSR, the cursor style.
        b'q' => intermediates == b" ",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the filter shows for `input` split into two chunks at `at`.
    fn split_at(input: &[u8], at: usize) -> Vec<u8> {
        let mut filter = TerminalFilter::default();
        let mut out = filter.feed(&input[..at]);
        out.extend(filter.feed(&input[at..]));
        out
    }

    /// `input` gives `expected` whole and split at every byte boundary, and
    /// fed one byte at a time.
    fn assert_shows(input: &[u8], expected: &[u8]) {
        let show = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        assert_eq!(
            show(&TerminalFilter::default().feed(input)),
            show(expected),
            "whole"
        );
        for at in 0..=input.len() {
            assert_eq!(
                show(&split_at(input, at)),
                show(expected),
                "split at {at} of {}",
                show(input)
            );
        }
        let mut filter = TerminalFilter::default();
        let bytewise: Vec<u8> = input.iter().flat_map(|b| filter.feed(&[*b])).collect();
        assert_eq!(show(&bytewise), show(expected), "byte by byte");
    }

    #[test]
    fn osc_52_dcs_and_device_attribute_queries_are_dropped_at_every_split() {
        for hostile in [
            &b"\x1b]52;c;ZXZpbA==\x07"[..],
            b"\x1b]52;c;ZXZpbA==\x1b\\",
            b"\x1b]52;c;?\x07",
            b"\x1b]8;;http://evil\x07",
            b"\x1b]0;\x1b]52;c;x\x07",
            b"\x1bP$qm\x1b\\",
            b"\x1bP+q544e\x1b\\",
            b"\x1b_Gi=1;payload\x1b\\",
            b"\x1b^private\x1b\\",
            b"\x1bXstring\x1b\\",
            b"\x1b[c",
            b"\x1b[0c",
            b"\x1b[>c",
            b"\x1b[=c",
            b"\x1b[6n",
            b"\x1b[?6n",
            b"\x1b[18t",
            b"\x1b[?1$p",
            b"\x1bZ",
            b"\x05",
        ] {
            let mut input = b"ab".to_vec();
            input.extend_from_slice(hostile);
            input.extend_from_slice(b"cd");
            assert_shows(&input, b"abcd");
        }
    }

    #[test]
    fn eight_bit_c1_forms_are_dropped() {
        for hostile in [
            &b"\x9d52;c;ZXZpbA==\x9c"[..],
            b"\x9d52;c;ZXZpbA==\x07",
            b"\x9b6n",
            b"\x9bc",
            b"\x90$qm\x9c",
            b"\x9fAPC\x9c",
            b"\x9ePM\x9c",
            b"\x98SOS\x9c",
            b"\x85",
        ] {
            let mut input = b"ab".to_vec();
            input.extend_from_slice(hostile);
            input.extend_from_slice(b"cd");
            assert_shows(&input, b"abcd");
        }
    }

    #[test]
    fn utf8_text_whose_continuation_bytes_look_like_c1_is_kept() {
        // U+00DB is C3 9B, U+2605 is E2 98 85: bytes in the C1 range.
        let text = "Û ★ ✓ héllo".as_bytes();
        assert_shows(text, text);
    }

    #[test]
    fn sgr_cursor_and_ordinary_sequences_are_kept() {
        let drawing = b"\x1b[1;31mred\x1b[0m\x1b[2;3H\x1b[K\x1b[?25l\x1b[?1049h\x1b[2J\
\x1b7\x1b8\x1b(B\x1b[38;2;1;2;3mx\x1b[3 q\r\n\t\x07\x1b[5A\x1b[?2004h";
        assert_shows(drawing, drawing);
    }

    #[test]
    fn titles_are_kept_without_control_characters() {
        assert_shows(b"a\x1b]0;my title\x07b", b"a\x1b]0;my title\x07b");
        assert_shows(b"a\x1b]2;win\x1b\\b", b"a\x1b]2;win\x07b");
        // An escape inside the string ends it (the title is lost) and starts a
        // new sequence, as a terminal would treat it.
        assert_shows(b"\x1b]0;ti\x1b[2Jtle\x07", b"\x1b[2Jtle\x07");
        assert_shows(b"\x1b]2;bad\x08\x7ftitle\x07", b"\x1b]2;badtitle\x07");
    }

    #[test]
    fn an_oversized_unterminated_string_is_dropped_and_memory_stays_bounded() {
        let mut filter = TerminalFilter::default();
        let mut shown = filter.feed(b"before\x1b]52;c;");
        let chunk = [b'A'; 4096];
        for _ in 0..1024 {
            shown.extend(filter.feed(&chunk));
            assert!(filter.buffered() <= 8192, "{}", filter.buffered());
        }
        let text = String::from_utf8_lossy(&shown).into_owned();
        assert!(text.starts_with("before"), "{text}");
        assert!(!text.contains("52;c"), "{text}");
        // Past the cap the string is abandoned; what leaks afterwards is plain
        // text, never an escape sequence.
        assert!(!shown.contains(&0x1b));
        // And ordinary output works again afterwards.
        let mut filter = TerminalFilter::default();
        filter.feed(b"\x1bP");
        for _ in 0..1024 {
            filter.feed(&chunk);
        }
        assert_eq!(filter.feed(b"\x1b\\ok"), b"ok");
    }
}
