"""Loopback-only synthetic provider for native CLI acceptance, never a cache meter.

Responses event schema:
https://developers.openai.com/api/reference/resources/responses/streaming-events
Anthropic framing follows the repository's pi_tool_order.rs and probe-delivery.mjs.
No outbound HTTP, real credentials, or request headers are read or retained.
"""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import hashlib
import json
import threading
import time


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def tool_definitions(body):
    tools = list(body.get("tools", []))
    for item in body.get("input", []):
        if isinstance(item, dict) and item.get("type") == "additional_tools":
            tools.extend(item.get("tools", []))
    return tools


def code_mode(body):
    return any(tool.get("type") == "namespace" and tool.get("name") == "functions"
               for tool in tool_definitions(body))


def native_probe_catalog(body):
    """The latest catalog actually returned by native functions.exec."""
    catalogs = []
    def visit(value):
        if isinstance(value, dict):
            if isinstance(value.get("fixture_catalog"), list):
                catalogs.append(value["fixture_catalog"])
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)
        elif isinstance(value, str):
            at = value.find('{"fixture_catalog":')
            if at >= 0:
                try:
                    visit(json.JSONDecoder().raw_decode(value[at:])[0])
                except ValueError:
                    pass
    for item in body.get("input", []):
        if isinstance(item, dict) and item.get("type") == "custom_tool_call_output" and str(item.get("call_id", "")).startswith("call_fixture_"):
            visit(item.get("output"))
    return catalogs[-1] if catalogs else None


def probe_name(body):
    for tool in tool_definitions(body):
        name = tool.get("name", "")
        if "probe" in name and "alpha" in name:
            return name
    return None


class Provider:
    def __init__(self, root):
        self.root = root
        self.phase = "startup"
        self.action = "reply"
        self.requests = []
        self.calls = []
        self.lock = threading.Lock()
        parent = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def do_GET(self):
                self.answer_json({"object": "list", "data": []})

            def answer_json(self, value, status=200):
                data = canonical(value)
                self.send_response(status)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_POST(self):
                size = int(self.headers.get("content-length", "0"))
                if size > 32 * 1024 * 1024:
                    self.answer_json({"error": "fixture request too large"}, 413)
                    return
                try:
                    body = json.loads(self.rfile.read(size))
                except (ValueError, OSError):
                    self.answer_json({"error": "invalid fixture request"}, 400)
                    return
                path = self.path.split("?")[0]
                if path.endswith("/count_tokens"):
                    self.answer_json({"input_tokens": 1000})
                    return
                if not (path.endswith("/messages") or path.endswith("/responses")):
                    self.answer_json({})
                    return
                with parent.lock:
                    if len(parent.requests) >= 60:
                        self.answer_json({"error": "fixture request cap"}, 429)
                        return
                    sequence = len(parent.requests) + 1
                    phase = parent.phase
                    action = parent.action
                    script = None
                    if code_mode(body) and action in ("inspect", "call"):
                        script = 'const catalog = ALL_TOOLS.filter(t => t.name.includes("probe")); text({fixture_catalog:catalog});'
                        if action == "call":
                            script += ' const selected = catalog.find(t => t.name.endsWith("__alpha")); if (!selected) throw new Error("fixture probe tool missing"); text(await tools[selected.name]({}));'
                        parent.action = "reply"
                    requested = "native-code-mode-probe" if script and action == "call" else probe_name(body) if action == "call" else None
                    if requested:
                        parent.action = "reply"
                        parent.calls.append({"phase": phase, "name": requested, "request": sequence})
                    entry = {"sequence": sequence, "phase": phase, "path": path,
                             "model": body.get("model"), "tools": tool_definitions(body),
                             "tool_sha256": hashlib.sha256(canonical(tool_definitions(body))).hexdigest(),
                             "input_sha256": hashlib.sha256(canonical(body.get("input", body.get("messages")))).hexdigest(),
                             "native_probe_catalog": native_probe_catalog(body),
                             "requested_tool": requested, "at": time.time()}
                    parent.requests.append(entry)
                    # Native prompts stay in this ignored, synthetic-home evidence tree.
                    (parent.root / f"request-{sequence:03}.json").write_bytes(canonical(body))
                events = (anthropic(body.get("model"), sequence, requested) if path.endswith("/messages")
                          else responses(body.get("model"), sequence, requested, script))
                wire = b"".join(("event: " + e["type"] + "\ndata: " + json.dumps(e) + "\n\n").encode() for e in events)
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(wire)))
                self.send_header("connection", "close")
                self.end_headers()
                try:
                    self.wfile.write(wire)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def set_phase(self, phase, action="inspect"):
        with self.lock:
            self.phase, self.action = phase, action
            return len(self.requests)

    def observed(self):
        with self.lock:
            return list(self.requests)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


def anthropic(model, sequence, tool):
    usage = {"input_tokens": 100, "output_tokens": 2, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
    block = {"type": "tool_use", "id": f"toolu_fixture_{sequence}", "name": tool, "input": {}} if tool else {"type": "text", "text": ""}
    delta = {"type": "input_json_delta", "partial_json": "{}"} if tool else {"type": "text_delta", "text": "Fixture response complete."}
    return [
        {"type": "message_start", "message": {"id": f"msg_fixture_{sequence}", "type": "message", "role": "assistant", "model": model,
          "content": [], "stop_reason": None, "stop_sequence": None, "usage": usage}},
        {"type": "content_block_start", "index": 0, "content_block": block},
        {"type": "content_block_delta", "index": 0, "delta": delta},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "tool_use" if tool else "end_turn", "stop_sequence": None}, "usage": {"output_tokens": 2}},
        {"type": "message_stop"},
    ]


def responses(model, sequence, tool, script=None):
    response = {"id": f"resp_fixture_{sequence}", "object": "response", "created_at": int(time.time()),
                "model": model, "status": "in_progress", "error": None, "incomplete_details": None, "output": []}
    events = [{"type": "response.created", "response": dict(response)}]
    identity = f"fc_fixture_{sequence}" if tool else f"msg_fixture_{sequence}"
    if script:
        item = {"id": identity, "type": "custom_tool_call", "name": "exec", "namespace": "functions",
                "call_id": f"call_fixture_{sequence}", "input": "", "status": "in_progress"}
        events.extend([
            {"type": "response.output_item.added", "output_index": 0, "item": dict(item)},
            {"type": "response.custom_tool_call_input.delta", "output_index": 0, "item_id": identity, "delta": script},
            {"type": "response.custom_tool_call_input.done", "output_index": 0, "item_id": identity, "input": script},
        ])
        item.update(input=script, status="completed")
    elif tool:
        item = {"id": identity, "type": "function_call", "name": tool, "call_id": f"call_fixture_{sequence}", "arguments": "", "status": "in_progress"}
        events.extend([
            {"type": "response.output_item.added", "output_index": 0, "item": dict(item)},
            {"type": "response.function_call_arguments.delta", "output_index": 0, "item_id": identity, "delta": "{}"},
            {"type": "response.function_call_arguments.done", "output_index": 0, "item_id": identity, "arguments": "{}"},
        ])
        item.update(arguments="{}", status="completed")
    else:
        item = {"id": identity, "type": "message", "role": "assistant", "status": "in_progress", "content": []}
        part = {"type": "output_text", "text": "", "annotations": [], "logprobs": []}
        events.extend([
            {"type": "response.output_item.added", "output_index": 0, "item": dict(item)},
            {"type": "response.content_part.added", "output_index": 0, "content_index": 0, "item_id": identity, "part": dict(part)},
            {"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "item_id": identity, "delta": "Fixture response complete.", "logprobs": []},
            {"type": "response.output_text.done", "output_index": 0, "content_index": 0, "item_id": identity, "text": "Fixture response complete.", "logprobs": []},
        ])
        part["text"] = "Fixture response complete."
        events.append({"type": "response.content_part.done", "output_index": 0, "content_index": 0, "item_id": identity, "part": part})
        item.update(status="completed", content=[part])
    events.append({"type": "response.output_item.done", "output_index": 0, "item": item})
    response.update(status="completed", output=[item], usage={"input_tokens": 100, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 2, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 102})
    events.append({"type": "response.completed", "response": response})
    return [dict(event, sequence_number=index) for index, event in enumerate(events)]
