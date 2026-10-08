"""The native metadata artifact fingerprints boundaries without exporting text."""

import json
from pathlib import Path
import tempfile
import unittest

from prompt_metadata import read_completed_report, surfaces


class PromptMetadataTests(unittest.TestCase):
    def test_native_metadata_excludes_bodies_and_account_identity(self):
        result = surfaces([
            {"type": "session_meta", "payload": {"creator_account_id": "private-account",
                "base_instructions": {"text": "private instructions"},
                "dynamic_tools": [{"name": "example", "description": "private tool description"}]}},
            {"type": "response_item", "payload": {"type": "message", "role": "developer",
                "content": [{"type": "input_text", "text": "private developer message"}]}},
            {"type": "response_item", "payload": {"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": "private user message"}]}},
        ])
        self.assertEqual(len(result), 3)
        self.assertNotIn("private", json.dumps(result))
        self.assertEqual(result[1]["tool_count"], 1)

    def test_content_boundaries_affect_fingerprint(self):
        def message(parts):
            return {"type": "response_item", "payload": {"type": "message", "role": "developer",
                "content": [{"type": "input_text", "text": part} for part in parts]}}
        a = surfaces([message(["ab", "c"])])[0]["sha256"]
        b = surfaces([message(["a", "bc"])])[0]["sha256"]
        self.assertNotEqual(a, b)

    def test_unfinished_run_cannot_be_published_as_completed_metadata(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "report.json"
            path.write_text(json.dumps({"status": "error", "agents": []}))
            with self.assertRaisesRegex(ValueError, "completed"):
                read_completed_report(path)


if __name__ == "__main__":
    unittest.main()
