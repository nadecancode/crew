"""Offline checks of fixture behavior and evidence helpers, never product acceptance."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import harness as h
from mcp_stub import Stub


class Fixtures(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="crew-toolboxes-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "fixture"
        self.manifest = h.prepare(self.root)

    def rpc(self, *, mode="normal", control=None, extra=None):
        argv = [sys.executable, str(h.HERE / "mcp_stub.py"), "--mode", mode]
        if control:
            argv.extend(["--control", str(control)])
        return h.RpcProcess(argv, env=h.isolated_environment(self.root, extra), cwd=self.root)

    def control(self, value):
        path = self.root / "stub-control.json"
        h.write_json(path, value)
        return path

    def test_preparation_refuses_existing_output_and_starts_with_no_passes(self):
        with self.assertRaises(FileExistsError):
            h.prepare(self.root)
        self.assertEqual(set(self.manifest["case_results"].values()), {"not_run"})
        self.assertEqual(list(self.manifest["case_results"]), [f"T{i:02d}" for i in range(1, 22)])
        self.assertEqual(self.manifest["python"], sys.executable)

    def test_preparation_creates_missing_parents_but_still_refuses_existing_output(self):
        nested = Path(self.temp.name) / "missing" / "parents" / "fixture"
        manifest = h.prepare(nested)
        self.assertEqual(manifest["root"], str(nested))
        self.assertTrue((nested / "manifest.json").is_file())
        self.assertEqual(nested.stat().st_mode & 0o777, 0o700)
        with self.assertRaises(FileExistsError):
            h.prepare(nested)

    def test_team_templates_are_fully_materialized_and_cover_all_kinds(self):
        full = (self.root / "teams/full.yaml").read_text()
        for token in ["@ROOT@", "@PYTHON@", "@HTTP_URL@"]:
            self.assertNotIn(token, full)
        value = json.loads(full)
        kinds = {"mcp", "skills", "plugins", "subagents", "hooks"}
        present = set().union(*(set(bundle) for bundle in value["toolboxes"].values()))
        self.assertTrue(kinds.issubset(present))
        self.assertIn("reset", value["orchestrator"]["toolbox"])
        self.assertIn("toolbox", value["defaults"])
        self.assertNotIn('"toolbox":', (self.root / "teams/legacy.yaml").read_text())
        h.assert_no_secrets({"full": full})

    def test_child_environment_drops_parent_daemon_tokens_and_git_settings(self):
        with patch.dict(os.environ, {"CREW_IDENTITY": "/real/identity", "CREW_SOCKET": "/real/socket",
                                     "OPENAI_API_KEY": "real", "GIT_DIR": "/real/git",
                                     "HTTPS_PROXY": "http://real-proxy", "DOCS_TOKEN": "real"}):
            env = h.isolated_environment(self.root)
        for name in ["CREW_IDENTITY", "CREW_SOCKET", "OPENAI_API_KEY", "GIT_DIR", "HTTPS_PROXY", "DOCS_TOKEN"]:
            self.assertNotIn(name, env)
        self.assertEqual(env["HOME"], str(self.root / "operator-home"))
        h.assert_scrubbed(env)

    def test_operator_sentinels_are_enumerated_and_operator_files_stay_unchanged(self):
        self.assertEqual({item["kind"] for item in self.manifest["sentinels"]},
                         {"mcp", "skills", "plugins", "subagents", "hooks", "memory", "settings"})
        self.assertEqual(h.tree_snapshot(self.root / "operator-home"), self.manifest["operator_snapshot"])
        for name in h.SECRET_VALUES:
            path = self.root / "secret-input" / name
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertTrue((self.root / "member/rw/home/codex/crew-toolbox/earlier-session.txt").is_file())

    def test_adversarial_file_items_and_same_content_sources(self):
        faults = self.root / "faults"
        self.assertEqual(h.tree_snapshot(faults / "same-content-a"), h.tree_snapshot(faults / "same-content-b"))
        self.assertGreater((faults / "oversize/too-large.bin").stat().st_size, 16 * 1024 * 1024)
        self.assertFalse((faults / "missing-skill/SKILL.md").exists())
        self.assertEqual((faults / "readonly-skill/references/nested.txt").stat().st_mode & 0o777, 0o444)
        snapshot = h.tree_snapshot(faults / "directory-link")
        self.assertEqual(snapshot["escape"], {"link": "../../secret-input"})
        self.assertFalse(any(key.startswith("escape/") for key in snapshot))
        with self.assertRaises(ValueError):
            h.tree_snapshot(faults / "directory-link/escape")

    def test_secret_and_environment_checks_fail_without_printing_values(self):
        value = h.SECRET_VALUES["DOCS_TOKEN"]
        with self.assertRaises(AssertionError) as error:
            h.assert_no_secrets({"A/mcp.json": value})
        self.assertIn("DOCS_TOKEN", str(error.exception))
        self.assertNotIn(value, str(error.exception))
        with self.assertRaisesRegex(AssertionError, "DAEMON_TOKEN"):
            h.assert_scrubbed(["PATH", "DAEMON_TOKEN"])

    def test_stdio_initialization_notifications_and_string_ids(self):
        with self.rpc() as client:
            reply = client.request("initialize", {"protocolVersion": "2025-06-18"})
            self.assertEqual(reply["id"], "fixture-1")
            self.assertTrue(reply["result"]["capabilities"]["tools"]["listChanged"])
            client.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            self.assertEqual(client.request("ping")["result"], {})
        self.assertIsNotNone(client.process.poll())
        self.assertEqual(Stub().handle({"id": 3, "method": "ping"})["id"], 3)

    def test_catalog_is_an_adversarial_negative_control_for_pinning(self):
        control = self.control({"mutate_after_first": True, "always_load": True, "account": "release"})
        with self.rpc(control=control) as client:
            first = client.request("tools/list")["result"]
            second = client.request("tools/list")["result"]
        self.assertNotEqual(first, second)
        self.assertIn("account_release", [tool["name"] for tool in first["tools"]])
        self.assertTrue(first["tools"][0]["_meta"]["anthropic/alwaysLoad"])
        with self.assertRaisesRegex(AssertionError, "sorted"):
            h.assert_pinned_tools([first])
        first["tools"].sort(key=lambda tool: tool["name"])
        h.assert_pinned_tools([first, first])
        with self.assertRaisesRegex(AssertionError, "changed"):
            h.assert_pinned_tools([first, second])

    def test_account_specific_catalogs_stay_distinct(self):
        control = self.control({"account": "first"})
        with self.rpc(control=control) as client:
            first = client.request("tools/list")["result"]
            h.write_json(control, {"account": "second"})
            second = client.request("tools/list")["result"]
        self.assertNotEqual({t["name"] for t in first["tools"]}, {t["name"] for t in second["tools"]})

    def test_token_delivery_reports_names_and_positive_negative_digest_checks_only(self):
        token = h.SECRET_VALUES["SHOP_GITHUB_TOKEN"]
        control = self.control({"expected_env_sha256": {
            "GITHUB_TOKEN": hashlib.sha256(token.encode()).hexdigest(),
            "WRONG_TOKEN": hashlib.sha256(token.encode()).hexdigest()}})
        with self.rpc(control=control, extra={"GITHUB_TOKEN": token, "WRONG_TOKEN": "wrong"}) as client:
            reply = client.request("tools/call", {"name": "inspect_environment"})
        data = json.loads(reply["result"]["content"][0]["text"])
        self.assertEqual(data["environment_matches"], {"GITHUB_TOKEN": True, "WRONG_TOKEN": False})
        h.assert_scrubbed(data["environment_names"])
        h.assert_no_secrets({"response": json.dumps(reply)})

    def test_explicit_error_echo_exercises_redaction_without_claiming_stub_redacts(self):
        token = h.SECRET_VALUES["SHOP_GITHUB_TOKEN"]
        with self.rpc(mode="echo-error", extra={"GITHUB_TOKEN": token}) as client:
            reply = client.request("tools/call", {"name": "alpha"})
        self.assertIn(token, reply["error"]["message"])

    def test_readiness_failure_modes_and_cleanup(self):
        with self.rpc(mode="exit-initialize") as client:
            with self.assertRaises(EOFError):
                client.request("initialize")
        with self.rpc(mode="hang-list") as client:
            client.request("initialize")
            with self.assertRaises(TimeoutError):
                client.request("tools/list", timeout=0.08)
        self.assertIsNotNone(client.process.poll())
        with self.rpc(mode="exit-on-call") as client:
            client.request("initialize")
            client.request("tools/list")
            with self.assertRaises(EOFError):
                client.request("tools/call", {"name": "alpha"})

    def test_loopback_http_headers_are_verified_without_exposing_values(self):
        token = "Bearer " + h.SECRET_VALUES["DOCS_TOKEN"]
        control = self.control({"expected_headers_sha256": {
            "authorization": hashlib.sha256(token.encode()).hexdigest()}})
        with h.http_stub(self.root, control=control) as url:
            reply = h.http_request(url, "initialize")
            self.assertEqual(reply["result"]["serverInfo"]["name"], "toolbox-fixture")
            reply = h.http_request(url, "tools/call", headers={"Authorization": token})
            data = json.loads(reply["result"]["content"][0]["text"])
            self.assertEqual(data["header_matches"], {"authorization": True})
            h.assert_no_secrets({"HTTP response": json.dumps(reply)})
        for url in ["https://example.com/mcp", "http://example.com/mcp", "file:///etc/passwd"]:
            with self.assertRaises(ValueError):
                h.http_request(url, "initialize")

    def test_fake_runtime_captures_only_and_does_not_fake_native_discovery(self):
        env = h.isolated_environment(self.root)
        for runtime in ["claude", "codex", "pi"]:
            result = subprocess.run([str(self.root / "bin" / runtime), "--version"], env=env,
                                    cwd=self.root, text=True, capture_output=True, timeout=3, check=True)
            self.assertIn("not a real runtime", result.stdout)
        result = subprocess.run([str(self.root / "bin/codex"), "mcp", "list", "--json"],
                                env=env, cwd=self.root, text=True, capture_output=True, timeout=3, check=True)
        self.assertEqual(result.stdout, "")
        records = [json.loads(line) for line in (self.root / "evidence/runtime.jsonl").read_text().splitlines()]
        self.assertEqual(records[-1]["argv"], ["mcp", "list", "--json"])
        self.assertNotIn("environment", records[-1])

    def test_ledger_requires_evidence_and_separates_live_provider_case(self):
        ledger = h.CaseLedger()
        with self.assertRaises(ValueError):
            ledger.record("T01", "pass", [])
        with self.assertRaises(ValueError):
            ledger.record("T22", "pass", ["test.log"])
        ledger.record("T16", "deferred", ["No provider calls authorized by this fixture task."])
        ledger.save(self.root / "evidence/cases.json")
        result = json.loads((self.root / "evidence/cases.json").read_text())
        self.assertEqual(result["cases"]["T01"]["status"], "not_run")
        self.assertEqual(result["cases"]["T16"]["status"], "deferred")
        self.assertEqual(next(case for case in h.load_cases() if case["id"] == "T16")["mode"],
                         "live-provider-required")

    def test_explicit_codex_discovery_response_and_extra_root_acknowledgement(self):
        log = self.root / "evidence/codex.jsonl"
        argv = [sys.executable, str(h.HERE / "runtime_stub.py"), "--fixture-runtime", "codex",
                "--fixture-log", str(log)]
        env = h.isolated_environment(self.root)
        listed = subprocess.run(argv + ["--fixture-mcp-catalog", str(self.root / "codex-mcp-list.json"),
                                       "mcp", "list", "--json"],
                                env=env, cwd=self.root, capture_output=True, text=True, timeout=3, check=True)
        self.assertEqual(json.loads(listed.stdout)[0]["name"], "operator-sentinel")
        with h.RpcProcess(argv + ["--fixture-app-server"], env=env, cwd=self.root) as client:
            client.request("initialize")
            client.send({"jsonrpc": "2.0", "method": "initialized"})
            reply = client.request("skills/extraRoots/set", {"extraRoots": [str(self.root / "items/skills")]})
            self.assertEqual(reply["result"], {})
            self.assertIn("error", client.request("skills/extraRoots/set", {"extraRoots": ["relative"]}))
            self.assertIn("error", client.request("skills/list"))
        with h.RpcProcess(argv + ["--fixture-app-server", "--fixture-reject-roots"],
                          env=env, cwd=self.root) as client:
            self.assertIn("error", client.request("skills/extraRoots/set", {"extraRoots": ["/valid"]}))


if __name__ == "__main__":
    unittest.main()
