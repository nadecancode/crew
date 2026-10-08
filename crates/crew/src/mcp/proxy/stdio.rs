//! A local toolbox server: a child process speaking MCP on its stdio.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::{Connection, Event, Redactor};

/// The largest message a local server may write, as for a remote one.
const MAX_MESSAGE: usize = 16 * 1024 * 1024;
/// The longest line of a server's log that is passed on.
const MAX_LOG_LINE: usize = 64 * 1024;

pub(crate) struct Child {
    child: std::process::Child,
    stdin: Option<ChildStdin>,
}

/// One line of at most `max` bytes, without its line break: `None` at the
/// end, `Err` for a line that is too long (read up to its end and dropped,
/// so no part of it is passed on).
fn bounded_line(reader: &mut impl BufRead, max: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let read = reader
        .by_ref()
        .take(max as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > max && line.last() != Some(&b'\n') {
        let mut rest = Vec::new();
        loop {
            rest.clear();
            let n = reader
                .by_ref()
                .take(64 * 1024)
                .read_until(b'\n', &mut rest)?;
            if n == 0 || rest.last() == Some(&b'\n') {
                break;
            }
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a line longer than {max} bytes"),
        ));
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    Ok(Some(line))
}

impl Child {
    /// Start `command` in `cwd` with the proxy's own environment, less the
    /// member's secret names (`scrub`), plus `env` (the server's resolved
    /// variables, under their destination names). Its log goes on to the
    /// proxy's own, redacted.
    pub(crate) fn start(
        command: &[String],
        cwd: Option<&str>,
        env: &BTreeMap<String, String>,
        scrub: &[String],
        redactor: Redactor,
        events: mpsc::Sender<Event>,
    ) -> Result<Self> {
        let Some((program, args)) = command.split_first() else {
            bail!("the server has no command");
        };
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in scrub {
            cmd.env_remove(name);
        }
        cmd.envs(env);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().with_context(|| format!("starting {program}"))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let stdin = child.stdin.take();
        // The server's log goes where the runtime keeps this one's, with its
        // secret values replaced like everything else it says.
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut out = std::io::stderr();
            loop {
                let line = match bounded_line(&mut reader, MAX_LOG_LINE) {
                    Ok(Some(line)) => redactor.text(&String::from_utf8_lossy(&line)),
                    Ok(None) => return,
                    Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                        "[a log line too long to pass on was dropped]".to_string()
                    }
                    Err(_) => return,
                };
                if writeln!(out, "{line}").is_err() {
                    return;
                }
            }
        });
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let why = loop {
                let line = match bounded_line(&mut reader, MAX_MESSAGE) {
                    Ok(Some(line)) => line,
                    Ok(None) => break "its output ended".to_string(),
                    Err(e) => break format!("reading its output failed: {e}"),
                };
                // A line that is not JSON is the server's mistake; it is
                // dropped rather than taking the session down.
                if let Ok(message) = serde_json::from_slice::<Value>(&line) {
                    if events.send(Event::Upstream(message)).is_err() {
                        return;
                    }
                }
            };
            let _ = events.send(Event::UpstreamClosed(why));
        });
        Ok(Child { child, stdin })
    }
}

impl Connection for Child {
    fn send(&mut self, message: &Value) -> Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            bail!("the server's input is closed");
        };
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        stdin
            .write_all(&line)
            .and_then(|()| stdin.flush())
            .context("writing to the server")
    }

    fn close(&mut self) {
        // Closing its input asks a well-behaved server to stop; it is killed
        // either way, and reaped.
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_bounded_and_an_overlong_one_is_dropped_whole() {
        let text = format!("short\r\n{}\nnext\nlast", "x".repeat(40));
        let mut reader = BufReader::with_capacity(8, text.as_bytes());
        assert_eq!(bounded_line(&mut reader, 10).unwrap().unwrap(), b"short");
        assert!(bounded_line(&mut reader, 10).is_err());
        assert_eq!(bounded_line(&mut reader, 10).unwrap().unwrap(), b"next");
        assert_eq!(bounded_line(&mut reader, 10).unwrap().unwrap(), b"last");
        assert!(bounded_line(&mut reader, 10).unwrap().is_none());
        let exact = "0123456789\n";
        let mut reader = BufReader::new(exact.as_bytes());
        assert_eq!(
            bounded_line(&mut reader, 10).unwrap().unwrap(),
            b"0123456789"
        );
    }
}
