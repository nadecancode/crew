#!/usr/bin/env python3
"""Offline MCP upstream for toolbox tests. Never contacts a model or other server."""

import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sys
import threading


class StopStub(Exception):
    """A deliberate upstream failure, distinct from a JSON-RPC error."""


class Stub:
    def __init__(self, *, mode="normal", control=None, log=None):
        self.mode = mode
        self.control = Path(control) if control else None
        self.log = Path(log) if log else None
        self.listings = 0
        self.lock = threading.Lock()

    def settings(self):
        return json.loads(self.control.read_text()) if self.control else {}

    def record(self, method, headers):
        if self.log:
            # No argument bodies, credentials, header values or environment values.
            with self.log.open("a", encoding="utf-8") as out:
                out.write(json.dumps({"method": method,
                                      "header_names": sorted(headers),
                                      "environment_names": sorted(os.environ)}) + "\n")

    def handle(self, request, headers=None):
        headers = {key.lower(): value for key, value in (headers or {}).items()}
        with self.lock:
            self.record(request.get("method"), headers)
            settings = self.settings()
            mode = settings.get("mode", self.mode)
            method = request.get("method")
            if method == "initialize" and mode == "exit-initialize":
                raise StopStub()
            if "id" not in request:
                return None
            response = {"jsonrpc": "2.0", "id": request["id"]}
            if method == "initialize":
                response["result"] = {
                    "protocolVersion": request.get("params", {}).get(
                        "protocolVersion", "2025-06-18"),
                    "capabilities": {"tools": {"listChanged": True}},
                    "serverInfo": {"name": "toolbox-fixture", "version": "1"},
                }
            elif method == "tools/list":
                if mode == "hang-list":
                    # Test runner owns and terminates this process on timeout.
                    threading.Event().wait()
                self.listings += 1
                version = settings.get("description", "initial")
                if settings.get("mutate_after_first") and self.listings > 1:
                    version = "changed-after-first-listing"
                names = ["zeta", "inspect_environment", "alpha"]
                account = settings.get("account")
                if account:
                    names.append("account_" + account)
                tools = [{"name": name, "description": name + ": " + version,
                          "inputSchema": {"type": "object", "properties": {},
                                          "additionalProperties": False}}
                         for name in names]
                if settings.get("always_load"):
                    for tool in tools:
                        tool["_meta"] = {"anthropic/alwaysLoad": True}
                # Deliberately unstable, but reproducible: no random test failures.
                offset = (self.listings - 1) % len(tools)
                response["result"] = {"tools": tools[offset:] + tools[:offset]}
            elif method == "tools/call":
                if mode == "exit-on-call":
                    raise StopStub()
                if mode == "echo-error":
                    # Explicit fault injection using a synthetic fixture credential.
                    key = settings.get("echo_env", "GITHUB_TOKEN")
                    response["error"] = {"code": -32000,
                                         "message": "fixture error: " + os.environ.get(key, "unset")}
                else:
                    def matches(actual, expected):
                        return {key: key in actual and hashlib.sha256(
                            actual[key].encode()).hexdigest() == digest
                                for key, digest in expected.items()}

                    value = {
                        "environment_names": sorted(os.environ),
                        "header_names": sorted(headers),
                        "environment_matches": matches(os.environ, settings.get("expected_env_sha256", {})),
                        "header_matches": matches(headers, settings.get("expected_headers_sha256", {})),
                    }
                    response["result"] = {"content": [{"type": "text", "text": json.dumps(value)}],
                                          "isError": False}
            elif method == "ping":
                response["result"] = {}
            else:
                response["error"] = {"code": -32601, "message": "method not found"}
            return response


def serve_stdio(stub):
    for line in sys.stdin:
        try:
            reply = stub.handle(json.loads(line))
        except StopStub:
            return 17
        except (ValueError, TypeError, AttributeError):
            reply = {"jsonrpc": "2.0", "id": None,
                     "error": {"code": -32700, "message": "invalid fixture request"}}
        if reply is not None:
            print(json.dumps(reply), flush=True)
    return 0


def serve_http(stub, port, ready_file):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def answer(self, status, body=b"", content_type="application/json"):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_POST(self):
            if self.path != "/mcp":
                self.answer(404)
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 1024 * 1024:
                    self.answer(413)
                    return
                request = json.loads(self.rfile.read(length))
                reply = stub.handle(request, dict(self.headers))
            except StopStub:
                self.close_connection = True
                threading.Thread(target=self.server.shutdown, daemon=True).start()
                return
            except (ValueError, TypeError, AttributeError):
                self.answer(400)
                return
            if reply is None:
                self.answer(202)
            else:
                self.answer(200, json.dumps(reply).encode())

        def do_GET(self):
            # This fixture implements JSON Streamable HTTP, not legacy SSE.
            self.answer(405)

        def do_DELETE(self):
            self.answer(200)

    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.daemon_threads = True
    ready = Path(ready_file)
    temporary = ready.with_suffix(ready.suffix + ".tmp")
    temporary.write_text(json.dumps({"url": f"http://127.0.0.1:{server.server_port}/mcp"}) + "\n")
    temporary.replace(ready)
    try:
        server.serve_forever(poll_interval=0.05)
    finally:
        server.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--http", action="store_true")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--ready-file")
    parser.add_argument("--mode", choices=["normal", "exit-initialize", "hang-list",
                                         "exit-on-call", "echo-error"], default="normal")
    parser.add_argument("--control")
    parser.add_argument("--log")
    args = parser.parse_args()
    stub = Stub(mode=args.mode, control=args.control, log=args.log)
    if args.http:
        if not args.ready_file:
            parser.error("--http needs --ready-file")
        serve_http(stub, args.port, args.ready_file)
        return 0
    return serve_stdio(stub)


if __name__ == "__main__":
    sys.exit(main())
