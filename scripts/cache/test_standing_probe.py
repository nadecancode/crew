import unittest
import sys
sys.dont_write_bytecode = True
from standing_probe import text_blocks, positions, lcp


class RenderingEvidence(unittest.TestCase):
    def test_only_native_text_counts_not_type_or_cache_metadata(self):
        body = {"system": [{"type": "text", "text": "system"}], "messages": [
            {"role": "user", "content": [{"type": "text", "text": "body", "cache_control": {"type": "ephemeral"}}]}]}
        self.assertEqual(list(text_blocks(body, "claude")), [("/system/0/text", "system", "system"),
            ("/messages/0/content/0/text", "user", "body")])

    def test_offsets_are_utf8_bytes_and_native_message_order_stays_explicit(self):
        body = {"input": [{"role": "developer", "content": [{"type": "input_text", "text": 'λ🦀You are "x", one agent'}]},
                          {"role": "user", "content": [{"type": "input_text", "text": "/fixture/x"}]}]}
        rows = positions(body, "codex", {"id": "x", "workdir": "/fixture/x"})
        self.assertEqual(rows[0]["offsets_utf8"]["member_identity"], 6)
        self.assertEqual(rows[1]["offsets_utf8"]["workdir"], 0)
        self.assertEqual(lcp("λx".encode(), "λy".encode()), 2)


if __name__ == "__main__":
    unittest.main()
