//! Defer termination until a foreground client has restored its terminal.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicI32 = AtomicI32::new(0);

extern "C" fn record_signal(signal: libc::c_int) {
    // Async-signal-safe: no allocation, locks, I/O or terminal operations.
    let _ = PENDING.compare_exchange(0, signal, Ordering::Relaxed, Ordering::Relaxed);
}

/// Temporarily record SIGTERM/HUP/QUIT/INT. Drop after terminal cleanup to restore
/// the original handlers and re-raise the signal, preserving the exit status.
pub struct TerminationSignals {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

impl TerminationSignals {
    pub fn install() -> io::Result<Self> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "termination signal watcher is already active",
            ));
        }
        PENDING.store(0, Ordering::Relaxed);
        let mut guard = Self {
            previous: Vec::with_capacity(4),
        };
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT, libc::SIGINT] {
            // SAFETY: a zeroed sigaction has valid integer/optional-pointer fields.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = record_signal as *const () as usize;
            // SAFETY: both pointers refer to initialized sigaction storage.
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: a null action reads the current disposition without changing it.
            if unsafe { libc::sigaction(signal, std::ptr::null(), &mut previous) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if previous.sa_sigaction == libc::SIG_IGN {
                continue;
            }
            // SAFETY: sa_mask is valid and signal is one of the supported signals.
            let rc = unsafe {
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(signal, &action, &mut previous)
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            guard.previous.push((signal, previous));
        }
        Ok(guard)
    }

    pub fn is_pending(&self) -> bool {
        PENDING.load(Ordering::Relaxed) != 0
    }
}

impl Drop for TerminationSignals {
    fn drop(&mut self) {
        for (signal, action) in &self.previous {
            // SAFETY: this restores the exact action read during installation.
            unsafe {
                libc::sigaction(*signal, action, std::ptr::null_mut());
            }
        }
        let signal = PENDING.swap(0, Ordering::Relaxed);
        if signal != 0 {
            // SAFETY: these are initialized masks; unblock this signal on the
            // cleanup thread, then restore its prior mask if the handler returns.
            let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
            let mut previous_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
            // SAFETY: signal came from our handler; terminal cleanup is complete.
            unsafe {
                libc::sigemptyset(&mut mask);
                libc::sigaddset(&mut mask, signal);
                let changed =
                    libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, &mut previous_mask) == 0;
                libc::raise(signal);
                if changed {
                    libc::pthread_sigmask(libc::SIG_SETMASK, &previous_mask, std::ptr::null_mut());
                }
            }
        }
        ACTIVE.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn termination_waits_for_cleanup_and_preserves_the_signal_status() {
        if let Some(marker) = std::env::var_os("CREW_SIGNAL_TEST_MARKER") {
            // The test must start from known dispositions, including under nohup
            // or a backgrounded shell. The ignored-signal case is installed below.
            for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT, libc::SIGINT] {
                // SAFETY: changes only this test child's standard signal actions.
                unsafe {
                    libc::signal(signal, libc::SIG_DFL);
                }
            }
            let core_limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: this changes only this test child's core-dump limit.
            assert_eq!(
                unsafe { libc::setrlimit(libc::RLIMIT_CORE, &core_limit) },
                0
            );
            if std::env::var_os("CREW_SIGNAL_TEST_IGNORE").is_some() {
                // SAFETY: valid signal and the standard ignored disposition.
                unsafe {
                    libc::signal(libc::SIGHUP, libc::SIG_IGN);
                }
                let signals = TerminationSignals::install().unwrap();
                // SAFETY: valid output storage; observe the live kernel disposition.
                let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
                assert_eq!(
                    unsafe { libc::sigaction(libc::SIGHUP, std::ptr::null(), &mut current) },
                    0
                );
                assert_eq!(current.sa_sigaction, libc::SIG_IGN);
                println!("SIGNALS_READY");
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_millis(200));
                assert!(!signals.is_pending(), "an ignored signal must stay ignored");
                drop(signals);
                std::fs::write(marker, "ignore preserved").unwrap();
                return;
            }
            let signals = TerminationSignals::install().unwrap();
            assert!(TerminationSignals::install().is_err());
            println!("SIGNALS_READY");
            std::io::stdout().flush().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !signals.is_pending() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            std::fs::write(marker, "cleanup completed").unwrap();
            drop(signals);
            panic!("the original default handler must terminate the child");
        }
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("ignored");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .current_dir(temp.path())
            .args([
                "--exact",
                "signals::tests::termination_waits_for_cleanup_and_preserves_the_signal_status",
                "--nocapture",
            ])
            .env("CREW_SIGNAL_TEST_MARKER", &marker)
            .env("CREW_SIGNAL_TEST_IGNORE", "1")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if line.contains("SIGNALS_READY") {
                break;
            }
        }
        // SAFETY: this is the test child that announced readiness.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGHUP) },
            0
        );
        assert!(child.wait().unwrap().success());
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "ignore preserved");
        // Consecutive normal lifetimes restore handlers and release the watcher.
        for _ in 0..2 {
            // SAFETY: both buffers are valid; a null action only reads disposition.
            let mut before: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut after: libc::sigaction = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &mut before) },
                0
            );
            drop(TerminationSignals::install().unwrap());
            assert_eq!(
                unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &mut after) },
                0
            );
            assert_eq!(before.sa_sigaction, after.sa_sigaction);
        }
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT, libc::SIGINT] {
            let marker = temp.path().join(signal.to_string());
            let mut child = Command::new(std::env::current_exe().unwrap())
                .current_dir(temp.path())
                .args([
                    "--exact",
                    "signals::tests::termination_waits_for_cleanup_and_preserves_the_signal_status",
                    "--nocapture",
                ])
                .env("CREW_SIGNAL_TEST_MARKER", &marker)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                assert!(output.read_line(&mut line).unwrap() > 0);
                if line.contains("SIGNALS_READY") {
                    break;
                }
            }
            // SAFETY: the child is ours and just confirmed its handler is installed.
            assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, signal) }, 0);
            assert_eq!(child.wait().unwrap().signal(), Some(signal));
            assert_eq!(
                std::fs::read_to_string(marker).unwrap(),
                "cleanup completed"
            );
        }
    }
}
