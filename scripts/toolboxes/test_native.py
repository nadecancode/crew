import json
from pathlib import Path
import tempfile
import unittest

from native import native_receipt


class NativeReplyEvidence(unittest.TestCase):
    def test_claude_metadata_cannot_hide_a_completed_phase_or_supply_a_reply(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "home/.claude/projects/fixture/session-id.jsonl"
            path.parent.mkdir(parents=True)
            prompt = {"type": "user", "message": {"role": "user", "content": "Toolbox acceptance probe. Reply briefly."}}
            reply = {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "Fixture response complete."}]}}
            metadata = {"type": "last-prompt", "lastPrompt": "Toolbox acceptance probe. Reply briefly."}
            def write(rows):
                path.write_text("\n".join(json.dumps(row) for row in rows) + "\n")
            write([prompt, reply, metadata])
            before = path.read_bytes()
            receipt = native_receipt(root, "claude", "session-id", "probe")
            self.assertEqual(receipt["reply_line"], 2)
            self.assertEqual(path.read_bytes(), before)
            write([reply, prompt, metadata])
            self.assertIsNone(native_receipt(root, "claude", "session-id", "probe"))
            write([prompt, {"type": "note", "text": "Fixture response complete."}])
            self.assertIsNone(native_receipt(root, "claude", "session-id", "probe"))

    def test_another_prompt_ends_the_phase_and_session_selection_stays_contained(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home = root / "home/.codex/sessions"
            home.mkdir(parents=True)
            outside = root / "outside.jsonl"
            def message(role, text):
                return {"type": "response_item", "payload": {"type": "message", "role": role,
                        "content": [{"type": "input_text" if role == "user" else "output_text", "text": text}]}}
            outside.write_text("\n".join(json.dumps(row) for row in [message("user", "Toolbox acceptance probe."),
                message("user", "Another prompt"), message("assistant", "Fixture response complete.")]))
            path = home / "rollout-date-session-id.jsonl"
            path.write_bytes(outside.read_bytes())
            self.assertIsNone(native_receipt(root, "codex", "session-id", "probe"))
            path.unlink()
            path.symlink_to(outside)
            self.assertIsNone(native_receipt(root, "codex", "session-id", "probe"))
            self.assertIsNone(native_receipt(root, "codex", "../outside", "probe"))


if __name__ == "__main__":
    unittest.main()
