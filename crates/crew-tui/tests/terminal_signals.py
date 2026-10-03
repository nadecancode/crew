"""Run with `just test-terminal`: isolated signal/shutdown pseudo-terminal checks.

Run `just test` or `just build` once first to populate the offline Cargo cache.
"""

import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import resource
import select
import shutil
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import time
import threading
import unittest


class FakeDaemon:
    def __init__(self, path):
        self.path = path
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(path))
        self.listener.listen()
        self.listener.settimeout(0.05)
        self.done = threading.Event()
        self.subscribed = threading.Event()
        self.lock = threading.Lock()
        self.peers = []
        self.subscribers = []
        self.thread = threading.Thread(target=self.accept, daemon=True)
        self.thread.start()

    def accept(self):
        while not self.done.is_set():
            try:
                peer, _ = self.listener.accept()
            except (socket.timeout, OSError):
                continue
            with self.lock:
                self.peers.append(peer)
            threading.Thread(target=self.serve, args=(peer,), daemon=True).start()

    def serve(self, peer):
        try:
            for line in peer.makefile("rb"):
                request = json.loads(line)
                method = request["method"]
                result = None
                if method == "hello":
                    result = {"role": "operator", "version": "test", "protocol": request["params"]["protocol"], "pid": 1}
                elif method == "subscribe":
                    with self.lock:
                        self.subscribers.append(peer)
                    self.subscribed.set()
                elif method == "snapshot":
                    result = {"session": {"workspace": "/probe", "state_dir": "", "daemon_started_at": 0, "daemon_pid": 1, "version": "test"}, "agents": [], "tasks": [], "messages": [], "events": [], "server_time": 0}
                with self.lock:
                    peer.sendall((json.dumps({"id": request["id"], "result": result}) + "\n").encode())
                if method == "shutdown":
                    self.shutdown()
        except (OSError, ValueError):
            pass

    def shutdown(self):
        with self.lock:
            for peer in self.subscribers:
                peer.sendall(b'{"push":{"kind":"shutdown"}}\n')

    def close(self):
        self.done.set()
        self.thread.join(timeout=1)
        self.listener.close()
        for peer in self.peers:
            try:
                peer.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            peer.close()
        self.path.unlink(missing_ok=True)


class TerminalSignals(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory(prefix="crew-signal-probe-")
        root = Path(cls.scratch.name)
        crate = Path(os.environ.get("CREW_TUI_CRATE", Path(__file__).resolve().parents[1]))
        (root / "src").mkdir()
        (root / "Cargo.toml").write_text(
            '[package]\nname = "crew-terminal-signal-probe"\nversion = "0.0.0"\nedition = "2021"\n'
            f'[dependencies]\ncrew-tui = {{ path = "{crate}" }}\n[workspace]\n'
        )
        shutil.copyfile(crate.parents[1] / "Cargo.lock", root / "Cargo.lock")
        (root / "src/main.rs").write_text('''
use crew_tui::{run, run_launcher, Launcher, StyleChoice};
fn main() {
    if let Some(socket) = std::env::args().nth(1) {
        let exit = run(std::path::Path::new(&socket), Default::default(), None).unwrap();
        println!("EXIT={exit:?}");
        return;
    }
    let style = StyleChoice { name: "test".into(), ..StyleChoice::default() };
    run_launcher(Launcher::new("/probe", vec![style], 0, "")).unwrap();
}
''')
        subprocess.run(["cargo", "build", "--offline", "--manifest-path", str(root / "Cargo.toml")], check=True)
        target = Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
        cls.binary = target / "debug/crew-terminal-signal-probe"
        cls.root = root

    @classmethod
    def tearDownClass(cls):
        cls.scratch.cleanup()

    def run_terminal(self, termination, dashboard=False, shutdown=False, own_stop=False):
        daemon = FakeDaemon(self.root / "daemon.sock") if dashboard else None
        master, slave = pty.openpty()
        before = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))

        def attach():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))

        child = subprocess.Popen(
            [str(self.binary), *([str(daemon.path)] if daemon else [])], stdin=slave, stdout=slave, stderr=slave,
            cwd=self.root, env={**os.environ, "TERM": "xterm-256color"}, preexec_fn=attach,
        )
        output = bytearray()

        def drain():
            if select.select([master], [], [], 0.05)[0]:
                try:
                    output.extend(os.read(master, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise

        try:
            deadline = time.monotonic() + 8
            while b"crew" not in output or b"\x1b[?1049h" not in output:
                self.assertLess(time.monotonic(), deadline, "launcher did not draw")
                self.assertIsNone(child.poll(), output.decode(errors="replace"))
                drain()
            if daemon:
                self.assertTrue(daemon.subscribed.wait(timeout=3))
            if shutdown:
                if own_stop:
                    os.write(master, b"Sy")
                else:
                    daemon.shutdown()
            elif termination is None:
                os.write(master, b"q" if dashboard else b"\x1b")
            else:
                os.kill(child.pid, termination)
            deadline = time.monotonic() + 5
            while child.poll() is None:
                self.assertLess(time.monotonic(), deadline, "launcher did not terminate")
                drain()
            drain()
            after = termios.tcgetattr(slave)
            expected = 0 if termination is None else -termination
            self.assertEqual(child.returncode, expected)
            mask = termios.ICANON | termios.ECHO
            self.assertEqual(after[3] & mask, before[3] & mask, "raw mode survived exit")
            self.assertEqual(after, before, "terminal attributes were not restored")
            self.assertIn(b"\x1b[?1049l", output, "alternate screen survived exit")
            self.assertIn(b"\x1b[?2004l", output, "bracketed paste survived exit")
            self.assertIn(b"\x1b[?1000l", output, "mouse capture survived exit")
            if shutdown:
                expected_exit = b"EXIT=Stopped" if own_stop else b"EXIT=Detached"
                self.assertIn(expected_exit, output)
                if not own_stop:
                    self.assertGreater(output.rfind(b"the crew daemon stopped"), output.rfind(b"\x1b[?1049l"))
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(master)
            os.close(slave)
            if daemon:
                daemon.close()

    def test_normal_exit_restores_terminal(self):
        self.run_terminal(None)

    def test_termination_signals_restore_terminal(self):
        for dashboard in [False, True]:
            for signum in [signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT, signal.SIGINT]:
                with self.subTest(signal=signum, dashboard=dashboard):
                    self.run_terminal(signum, dashboard=dashboard)

    def test_dashboard_shutdown_distinguishes_external_from_own_stop(self):
        for own_stop in [False, True]:
            with self.subTest(own_stop=own_stop):
                self.run_terminal(None, dashboard=True, shutdown=True, own_stop=own_stop)


if __name__ == "__main__":
    unittest.main()
