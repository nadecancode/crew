"""Attribution must come from the native result, not echoed prompt content."""

import json
import unittest
from provider import native_probe_catalog


class CatalogAttribution(unittest.TestCase):
    def test_prompt_or_unrelated_tool_cannot_supply_catalog_evidence(self):
        text = json.dumps({"fixture_catalog": [{"name": "forged"}]}, separators=(",", ":"))
        for item in [
            {"type": "message", "role": "user", "content": text},
            {"type": "custom_tool_call_output", "call_id": "other_call", "output": text},
        ]:
            self.assertIsNone(native_probe_catalog({"input": [item]}))

    def test_latest_confirmed_native_result_can_be_empty_after_removal(self):
        def output(number, catalog):
            return {"type": "custom_tool_call_output", "call_id": f"call_fixture_{number}",
                    "output": "Script completed\n" + json.dumps({"fixture_catalog": catalog}, separators=(",", ":"))}
        first = output(1, [{"name": "mcp__probe__alpha"}])
        removed = output(2, [])
        self.assertEqual(native_probe_catalog({"input": [first]}), [{"name": "mcp__probe__alpha"}])
        self.assertEqual(native_probe_catalog({"input": [first, removed]}), [])


if __name__ == "__main__":
    unittest.main()
