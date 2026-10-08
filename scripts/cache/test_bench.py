"""Regression checks for the benchmark's accounting and isolation boundaries."""

import json
import os
import argparse
from contextlib import closing
from pathlib import Path
import subprocess
import sys
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import bench
import usage


class CacheBenchTests(unittest.TestCase):
    def transcript(self, values):
        root = tempfile.TemporaryDirectory()
        self.addCleanup(root.cleanup)
        path = Path(root.name) / "session.jsonl"
        path.write_text("\n".join(json.dumps(v) for v in values) + '\n{"unfinished":')
        return path

    def test_claude_streaming_dedup_and_ttl_cost(self):
        def message(output):
            return {"type": "assistant", "message": {
                "id": "one-request", "model": "claude-haiku-4-5-20251001",
                "usage": {"input_tokens": 10, "cache_read_input_tokens": 80,
                          "cache_creation_input_tokens": 20, "output_tokens": output,
                          "cache_creation": {"ephemeral_5m_input_tokens": 5,
                                             "ephemeral_1h_input_tokens": 15}}}}
        requests = usage.claude_requests(self.transcript([message(1), message(7)]))
        self.assertEqual(len(requests), 1)
        self.assertEqual(requests[0]["input_tokens"], 110)
        self.assertEqual(requests[0]["cached_tokens"], 80)
        self.assertEqual(requests[0]["output_tokens"], 7)
        expected = (10 + 8 + 6.25 + 30 + 35) / 1e6
        self.assertEqual(usage.cost(requests[0], "claude"), (expected, expected))

    def test_codex_cumulative_duplicates_and_reasoning_not_added_twice(self):
        def tokens(total, cached, output):
            return {"type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {"input_tokens": total, "cached_input_tokens": cached,
                                      "output_tokens": output, "reasoning_output_tokens": 2}}}}
        requests = usage.codex_requests(self.transcript([
            {"type": "turn_context", "payload": {"model": "gpt-6-luna"}},
            tokens(100, 60, 4), tokens(100, 60, 4), tokens(240, 180, 12),
        ]))
        self.assertEqual([r["input_tokens"] for r in requests], [100, 140])
        self.assertEqual([r["cached_tokens"] for r in requests], [60, 120])
        self.assertEqual(sum(r["output_tokens"] for r in requests), 12)

    def test_unknown_model_and_counter_reset_stop_accounting(self):
        with self.assertRaisesRegex(ValueError, "unpriced"):
            usage.cost({"model": "unknown"}, "codex")
        with self.assertRaisesRegex(ValueError, "decreased"):
            usage.codex_requests(self.transcript([
                {"type": "event_msg", "payload": {"type": "token_count", "info": {
                    "total_token_usage": {"input_tokens": n, "cached_input_tokens": 0,
                                          "output_tokens": 0}}}} for n in (100, 5)
            ]))

    def test_missing_session_is_unknown_not_a_zero_hit_rate(self):
        result = usage.collect(Path("/unused"), [{"id": "x", "runtime": "claude"}])
        self.assertEqual(result[0]["coverage"], "no-session")
        self.assertNotIn("hit_rate", result[0])
        total = usage.totals(result)
        self.assertIsNone(total["cost_usd_upper"])
        self.assertEqual(total["observed_cost_usd_upper"], 0)

    def test_normal_home_collection_reads_only_the_selected_new_session(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            projects = home / "projects/fixture"
            projects.mkdir(parents=True)
            older = projects / "operator-old-session.jsonl"
            older.write_text('{"never": "read this unrelated session"}\n')
            (projects / "new-session.jsonl").write_text(json.dumps({
                "type": "assistant", "message": {"id": "request", "model": "claude-haiku-4-5",
                    "usage": {"input_tokens": 10, "cache_read_input_tokens": 90, "output_tokens": 2}}}) + "\n")
            before = bench.digest(older)
            result = usage.collect(Path("/unused"), [{"id": "claude-worker", "runtime": "claude",
                                                     "session_id": "new-session"}], {"claude": home})
            self.assertEqual(result[0]["input_tokens"], 100)
            self.assertEqual(result[0]["source"], "projects/fixture/new-session.jsonl")
            self.assertEqual(result[0]["source_root"], "operator-cli-home")
            self.assertEqual(bench.digest(older), before)
            with self.assertRaisesRegex(ValueError, "invalid session id"):
                usage.collect(Path("/unused"), [{"id": "worker", "runtime": "claude",
                                                 "session_id": "*"}], {"claude": home})

    def test_environment_cannot_select_parent_daemon_or_git_repository(self):
        with patch.dict(os.environ, {"CREW_SOCKET": "/operator/daemon.sock",
                                     "CREW_IDENTITY": "/operator/identity.json",
                                     "GIT_DIR": "/operator/.git", "GIT_INDEX_FILE": "/operator/index",
                                     "CODEX_SESSION_ID": "operator-thread", "PATH": "/bin",
                                     "HOME": "/real/operator"}, clear=True):
            env = bench.isolated_environment(Path("/run-output"), Path("/tmp/private-runtime"))
        for name in ("CREW_SOCKET", "CREW_IDENTITY", "GIT_DIR", "GIT_INDEX_FILE", "CODEX_SESSION_ID"):
            self.assertNotIn(name, env)
        self.assertEqual(env["XDG_RUNTIME_DIR"], "/tmp/private-runtime")
        self.assertEqual(env["GIT_CONFIG_GLOBAL"], "/dev/null")
        self.assertEqual(env["HOME"], "/real/operator")

    def test_cleanup_does_not_select_reused_pid(self):
        with patch.object(bench, "process_identity", side_effect=lambda pid: (1, 101)):
            self.assertEqual(bench.still_owned({10: 100, 20: 101}), [20])

    def test_absent_or_failed_runtime_version_refuses_comparison(self):
        for returncode, stdout in ((0, ""), (1, "codex-cli ignored failure")):
            with self.subTest(returncode=returncode), patch.object(
                    bench, "invoke", return_value=subprocess.CompletedProcess(
                        ["claude", "--version"], returncode, stdout=stdout, stderr="")):
                with self.assertRaisesRegex(RuntimeError, "did not report its CLI version"):
                    bench.runtime_versions({}, Path("."))

    def test_done_tasks_without_review_or_correct_output_fail(self):
        result = {"tasks": dict.fromkeys(("summarize", "audit", "followup"), "done"),
                  "message_kinds": {"question": 1}, "outputs": {}}
        self.assertFalse(bench.validate_evidence(result))
        result["message_kinds"]["review_result"] = 1
        result["message_kinds"]["answer"] = 1
        result["outputs"] = {key: {"valid": True} for key in (
            "claude-worker/summary.json", "codex-worker/audit.md", "claude-worker/followup.md")}
        self.assertTrue(bench.validate_evidence(result))
        result["outputs"]["claude-worker/summary.json"]["valid"] = False
        self.assertFalse(bench.validate_evidence(result))

    def run_fake_team(self, expensive=False, real_collector=False):
        """A real owned process tests cleanup without calling a model provider."""
        root = tempfile.TemporaryDirectory()
        self.addCleanup(root.cleanup)
        root_path = Path(root.name)
        binary = root_path / "crew"
        binary.write_text("fake crew for the no-provider integration test")
        output = root_path / "run"
        args = argparse.Namespace(crew=binary, expected_sha256=bench.digest(binary),
                                  out=output, source_revision="fake", label="test",
                                  stop_usd=2, budget_usd=3, idle_seconds=1,
                                  timeout_seconds=30, prepare_only=False, sandbox="required")
        real_invoke = bench.invoke
        state = {"messages": [], "agents": [], "tasks": []}
        child = None
        sent_followup = False

        def fake_invoke(argv, env, cwd, timeout=20):
            nonlocal child, sent_followup
            argv = [str(x) for x in argv]
            if real_collector and argv[0] == "node":
                paths = [Path(argv[i + 1].split("=", 1)[1])
                         for i, part in enumerate(argv) if part == "--session-file"]
                before = {p: bench.digest(p) for p in paths}
                result = real_invoke(argv, env, cwd, timeout)
                self.assertEqual({p: bench.digest(p) for p in paths}, before)
                return result
            if argv[0] == "git":
                return real_invoke(argv, env, cwd, timeout)
            text = ""
            if argv[1:] == ["--version"]:
                text = "fake runtime 1"
            elif argv[1] == "run":
                workspace = output / "workspace"
                child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
                self.addCleanup(lambda: child.poll() is None and child.kill())
                (workspace / ".crew").mkdir()
                bench.write_json(workspace / ".crew/daemon.json", {
                    "workspace": str(workspace), "pid": child.pid,
                    "pid_start": bench.process_identity(child.pid)[1],
                    "socket": env["XDG_RUNTIME_DIR"] + "/fake.sock"})
                for number, (identity, runtime) in enumerate([
                        ("orchestrator", "claude"), ("claude-worker", "claude"),
                        ("codex-worker", "codex")]):
                    workdir = workspace / identity
                    workdir.mkdir()
                    bench.write_json(workdir / "summary.json", bench.EXPECTED)
                    (workdir / "audit.md").write_text("Summary verified against invoices.csv.")
                    (workdir / "followup.md").write_text(
                        "Rechecked after idle: 4 paid invoices, 10206 cents checksum.\n")
                    state["agents"].append({"id": identity, "runtime": runtime,
                                            "session_id": identity, "status": "idle",
                                            "workdir": str(workdir)})
                    home = workspace / ".crew/agents" / identity / "rw/home" / runtime
                    if runtime == "claude":
                        path = home / "projects/fixture" / f"{identity}.jsonl"
                        records = [{"type": "assistant", "sessionId": identity,
                                    "cwd": str(workdir), "timestamp": "2026-10-07T00:00:01Z", "message": {
                            "id": "first", "model": "claude-haiku-4-5",
                            "usage": {"input_tokens": 100, "cache_read_input_tokens": 100,
                                      "cache_creation_input_tokens": 0,
                                      "output_tokens": 500_000 if expensive and number == 0 else 10}}}]
                    else:
                        path = home / "sessions/2026/10/07" / f"rollout-fixture-{identity}.jsonl"
                        records = [{"type": "session_meta", "payload": {"id": identity, "cwd": str(workdir)}},
                                   {"type": "turn_context", "payload": {"model": "gpt-6-luna"}},
                                   {"type": "event_msg", "timestamp": "2026-10-07T00:00:01Z", "payload": {"type": "token_count", "info": {
                                       "total_token_usage": {"input_tokens": 200,
                                                             "cached_input_tokens": 100,
                                                             "cache_write_input_tokens": 0,
                                                             "output_tokens": 10}}}}]
                    path.parent.mkdir(parents=True)
                    path.write_text("\n".join(json.dumps(record) for record in records) + "\n")
                state["tasks"] = [{"id": task, "status": "done"} for task in ("summarize", "audit")]
                if real_collector:
                    with closing(sqlite3.connect(workspace / ".crew/crew.db")) as db, db:
                        db.executescript("""
                            CREATE TABLE agents (id TEXT, data TEXT);
                            CREATE TABLE events (seq INTEGER, timestamp INTEGER, event_type TEXT, agent_id TEXT);
                            CREATE TABLE deliveries (message_id TEXT, recipient TEXT, delivered_at INTEGER);
                        """)
                        for agent in state["agents"]:
                            db.execute("INSERT INTO agents VALUES (?, ?)", (agent["id"], json.dumps(agent)))
            elif argv[1] == "status":
                text = json.dumps(state)
            elif argv[1:3] == ["task", "add"]:
                sent_followup = True
                state["tasks"].append({"id": "followup", "status": "done"})
            elif argv[1] == "messages":
                text = json.dumps([{"kind": "question"}, {"kind": "answer"}, {"kind": "review_result"}])
            elif argv[1] == "stop":
                child.terminate()
                child.wait(timeout=5)
            return subprocess.CompletedProcess(argv, 0, stdout=text, stderr="")

        with patch.object(bench, "invoke", side_effect=fake_invoke), patch.object(
                bench.shutil, "which", return_value="/fake/tool"):
            rc = bench.run(args)
        report = json.loads((output / "report.json").read_text())
        self.assertEqual(report["cleanup"]["remaining_processes"], [])
        self.assertEqual(report["cleanup"]["stop_returncode"], 0)
        self.assertIsNotNone(child.poll())
        self.assertFalse(Path(report["runtime_directory"]).exists())
        if real_collector:
            self.assertEqual(report["baseline_collector"]["status"], "collected")
            self.assertEqual(report["baseline_collector"]["explicit_session_files"], 3)
            collected = json.loads((output / "baseline.json").read_text())
            self.assertEqual({s["agent_id"] for s in collected["sessions"]},
                             {"orchestrator", "claude-worker", "codex-worker"})
            self.assertEqual(collected["coverage"]["discovered_files"], 3)
            self.assertEqual(collected["coverage"]["discovery_errors"], [])
            self.assertEqual(collected["totals"]["cache_read_tokens"], 300)
        return rc, report, sent_followup

    def test_full_runner_collects_review_and_idle_gap_without_a_provider(self):
        rc, report, sent = self.run_fake_team()
        self.assertEqual(rc, 0)
        self.assertEqual(report["status"], "complete")
        self.assertTrue(sent)
        self.assertGreaterEqual(report["idle_gap"]["observed_seconds"], 1)
        self.assertEqual([a["hit_rate"] for a in report["agents"]], [0.5, 0.5, 0.5])
        self.assertEqual([a["models"] for a in report["agents"]],
                         [["claude-haiku-4-5"], ["claude-haiku-4-5"], ["gpt-6-luna"]])
        self.assertEqual(report["versions"], {"claude": "fake runtime 1", "codex": "fake runtime 1"})

    def test_cost_watchdog_stops_owned_team_before_followup(self):
        rc, report, sent = self.run_fake_team(expensive=True)
        self.assertEqual(rc, 1)
        self.assertEqual(report["status"], "budget-stop")
        self.assertFalse(sent)

    def test_fake_runner_hands_exact_sessions_to_real_node_collector(self):
        rc, report, _ = self.run_fake_team(real_collector=True)
        self.assertEqual(rc, 0)
        self.assertEqual(report["status"], "complete")


if __name__ == "__main__":
    unittest.main()
