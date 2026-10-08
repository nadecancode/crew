//! A local toolbox server: a child process speaking MCP on its stdio.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::{Connection, Event};

pub(crate) struct Child {
    child: std::process::Child,
    stdin: Option<ChildStdin>,
}

impl Child {
    /// Start `command` in `cwd` with the proxy's own environment, less the
    /// member's secret names (`scrub`), plus `env` (the server's resolved
    /// variables, under their destination names).
    pub(crate) fn start(
        command: &[String],
        cwd: Option<&str>,
        env: &BTreeMap<String, String>,
        scrub: &[String],
        events: mpsc::Sender<Event>,
    ) -> Result<Self> {
        let Some((program, args)) = command.split_first() else {
            bail!("the server has no command");
        };
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The server's own log goes where the runtime keeps this one's.
            .stderr(Stdio::inherit());
        for name in scrub {
            cmd.env_remove(name);
        }
        cmd.envs(env);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd.spawn().with_context(|| format!("starting {program}"))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let stdin = child.stdin.take();
        std::thread::spawn(move || {
            let mut why = "its output ended".to_string();
            for line in BufReader::new(stdout).lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(e) => {
                        why = format!("reading its output failed: {e}");
                        break;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                // A line that is not JSON is the server's mistake; it is
                // dropped rather than taking the session down.
                if let Ok(message) = serde_json::from_str::<Value>(&line) {
                    if events.send(Event::Upstream(message)).is_err() {
                        return;
                    }
                }
            }
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
