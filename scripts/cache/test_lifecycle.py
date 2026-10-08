"""Token-free checks for the lifecycle probe's native evidence boundaries."""

import json
from pathlib import Path
import tempfile
import unittest

import lifecycle


class LifecycleTests(unittest.TestCase):
    def session(self, values):
        root = tempfile.TemporaryDirectory()
        self.addCleanup(root.cleanup)
        path = Path(root.name) / "session.jsonl"
        path.write_text("\n".join(json.dumps(v) for v in values) + "\n")
        return path

    def test_fold_window_closes_on_matching_tool_result(self):
        assistant = {"type": "assistant", "message": {"content": [{
            "type": "tool_use", "id": "tool-1", "input": {"command": "sleep 30 # cache-fold-01"}}]}}
        path = self.session([assistant])
        self.assertEqual(lifecycle.native_markers(path, lifecycle.FOLD_MARKER)["pending_tool_ids"], ["tool-1"])
        with path.open("a") as out:
            out.write(json.dumps({"type": "user", "message": {"content": [{
                "type": "tool_result", "tool_use_id": "tool-1", "content": ""}]}}) + "\n")
        self.assertEqual(lifecycle.native_markers(path, lifecycle.FOLD_MARKER)["pending_tool_ids"], [])

    def test_task_marker_is_not_injection_and_thinking_body_is_not_exported(self):
        values = [
            {"type": "user", "isMeta": True, "message": {"content": [{"type": "text", "text":
                "[crew] Task: run sleep 30 # cache-fold-01"}]}},
            {"type": "assistant", "message": {"id": "before", "content": [{"type": "thinking", "thinking": "private-before"}]}},
            {"type": "user", "isMeta": True, "message": {"content": [{"type": "text", "text":
                "[crew] Message from operator: [cache-fold-injected-01] compute checksum"}]}},
            {"type": "assistant", "message": {"id": "after", "content": [{"type": "thinking", "thinking": "private-after"}]}},
        ]
        result = lifecycle.native_markers(self.session(values), lifecycle.FOLD_MARKER, lifecycle.FOLD_MESSAGE)
        self.assertEqual(result["folded_rows"], 1)
        self.assertEqual(result["post_fold_thinking_blocks"], 1)
        self.assertNotIn("private-", json.dumps(result))

    def test_prefix_measurement_preserves_bytes_and_local_scope(self):
        before = b"fixed\nroster: a,b\nunchanged tail"
        after = b"fixed\nroster: a,b,c\nunchanged tail"
        self.assertEqual(lifecycle.prefix_delta(before, before)["common_prefix_bytes"], len(before))
        delta = lifecycle.prefix_delta(before, after)
        self.assertEqual(delta["common_prefix_bytes"], len(b"fixed\nroster: a,b"))
        self.assertTrue(delta["changed"])
        self.assertIn("not the provider", delta["scope"])

    def test_step_attribution_keeps_first_request_and_token_weighting(self):
        def request(total, read):
            return {"input_tokens": total, "cached_tokens": read, "cost_usd_lower": 0.1, "cost_usd_upper": 0.2}
        usage = [{"agent": "claude-worker", "requests": [request(100, 0), request(200, 20), request(300, 290)]}]
        row = lifecycle.aligned_usage({"claude-worker": 1}, usage)["claude-worker"]
        self.assertEqual(row["requests"], 2)
        self.assertEqual(row["uncached_input_tokens"], 190)
        self.assertEqual(row["hit_rate"], 310 / 500)
        self.assertEqual(row["first_request"]["cached_tokens"], 20)

    def test_extended_plan_requires_strictly_longer_idle_windows(self):
        runner = lifecycle.Lifecycle(Path("/unused"), lambda *args: None, long_idle=True)
        idle = [row[3] for row in runner.plan if row[0] == "idle"]
        self.assertGreater(idle[0], 300)
        self.assertGreater(idle[1], 3600)
        self.assertEqual(len([row for row in runner.plan if row[0] == "restart"]), 6)
        self.assertIn("fold", [row[0] for row in runner.plan])


if __name__ == "__main__":
    unittest.main()
