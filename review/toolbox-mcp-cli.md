| Response and event size limits | `MAX_BODY` and `MAX_EVENT` 16 MiB, an event counting every byte read for it (data, names, comments, line breaks); status and header lines 8 KiB; 100 header lines at most, parsed or not; a local server's messages 16 MiB. || Connect and read timeouts | Resolving, connecting, the TLS handshake and sending share one 30 s deadline (`CONNECT_WITHIN`), enforced by a watchdog that shuts the socket down, so no blocked read or write outlives it. The answer head and the SSE endpoint have their own deadlines; a body may pause `ANSWER_IDLE` (300 s) between reads, since a tool may run long. Ending a session (`DELETE`) is bounded by 5 s. A preflight's `within` bounds all of it. |# Toolbox MCP proxy and CLI (slice 3)

Task: `toolbox-mcp-cli`. Base: main `df4575cf`. Branch: `agents/scheduler`.
Specification: `notes/toolboxes.md`, slice 3 of Implementation split.

This note is a review aid. It records what the slice does, where the
security-relevant code is, and the evidence the orchestrator asked for when
it approved TLS for remote servers.

## What the slice adds

- `crew toolbox mcp <name> --identity <file>` (hidden; it is what runtimes
  launch). One process per server. It fetches the server's launch over the
  member's identity (`Call::ToolboxServer`) and then:
  - runs a local server as its child, with the resolved environment added
    to a scrubbed one, or bridges a remote server over streamable HTTP or
    the older SSE transport;
  - answers `initialize`, `tools/list` and `ping` from the pinned snapshot,
    sorted by tool name, with `listChanged` never advertised and upstream
    `notifications/*/list_changed` dropped;
  - replaces exact copies of resolved values, in strings and object keys,
    with `[redacted $NAME]` in everything that leaves it: messages to the
    runtime, the preflight listing (and so the pinned snapshot), the pinned
    view when served, failure reasons, refusals, reports and the local
    server's stderr (best effort, as the specification says);
  - reports a server that fails, or whose live listing differs from the
    pin, with `Call::ToolboxReport`, and keeps the pinned view in place.
- `--preflight`: the daemon's readiness check. It prints the live listing as
  JSON on success, or the redacted reason. The daemon pins the snapshot
  (`agent_dir/toolbox/mcp/<name>.json`), since the session cannot write
  there under the sandbox.
- `crew toolbox [agent] [--runtime R] [--json]`, `crew toolbox check <file>`
  and `crew toolbox reload <file>`.
- `toolbox` on `agent_add` and `agent_spawn`: bundle names only, checked
  against the team's toolboxes. A subagent gets a copy of its parent's
  active toolbox by default, or the named bundles when each item equals the
  parent's. `agents_list` shows each member's toolbox fingerprint and how
  many items are not active.
- The orchestrator's tool list is chosen from the launch identity
  (`Identity.orchestrator`), never by asking the daemon (T21).

## Secrets

Resolved values come from `Layers::toolbox_references` (graph, `c8db97f9`)
in the daemon and reach only the proxy process, in the `toolbox_server`
answer. They are not in any file, command line, roster entry, event or
prompt that crew writes. `ToolboxServerLaunch` and `Upstream` have
hand-written `Debug` implementations that print names only; `Redaction`
has no `Debug` at all. A child
server receives values only under destination names; every reference and
source name for the member's servers is removed from its environment first.

A local server's stderr is piped through the proxy, one bounded line at a
time, and redacted; an overlong line is dropped whole rather than split.

## Remote servers and TLS

The orchestrator approved rustls with bundled web roots on these conditions.
Each is listed with where it is met.

| Condition | Where |
| --- | --- |
| Verification always on; no insecure flag or environment override | `mcp/proxy/http.rs`, `tls()`: one `ClientConfig` with the default verifier. No code path skips it. |
| `SSL_CERT_FILE` / `SSL_CERT_DIR` only add roots | `tls()` adds their certificates to `webpki_roots::TLS_SERVER_ROOTS`. |
| Exact pins | `rustls =0.23.45` (std, ring), `rustls-pemfile =2.2.0`, `webpki-roots =1.0.9`, all in `Cargo.lock`. |
| Builds on Rust 1.82 | `scripts/check-msrv` passed (below). |
| `cargo tree -d` | One new duplicate (below). |
| Response and event size limits | `MAX_BODY` and `MAX_EVENT` 16 MiB; status and header lines 8 KiB; 100 headers at most. |
| Connect and read timeouts | `CONNECT_WITHIN` 30 s; `ANSWER_IDLE` 300 s between reads; `ENDPOINT_WITHIN` 30 s for the SSE endpoint. |
| No redirects | A 3xx answer fails the request: "the server answered HTTP 307, a redirect, which crew does not follow". |
| Chunked encoding | `Chunked` reader, unit-tested. |
| No proxy support | `HTTP_PROXY` / `HTTPS_PROXY` are not read. Documented in the module header. A server reachable only through a proxy cannot be used. |

Two more rules keep a server's headers, which carry its credentials, on
that server:

- The SSE transport's `endpoint` event must name the server's own origin
  (scheme, host and port). Otherwise the connection fails before anything
  is sent there.
- A URL path with spaces or control characters is refused, so a named
  endpoint cannot write into the request head. Header values with line
  breaks were already refused.

`tests/toolbox_proxy.rs::an_https_server_is_reached_only_through_a_verified_chain`
serves the fake server over TLS with a certificate from a test CA
(`tests/fixtures/toolbox-tls`, CA key deleted). It checks that preflight
fails without the CA and passes with it in `SSL_CERT_FILE`.

### Default release binary size

`cargo build --locked --release -p crew --bin crew`, rustc 1.93.1, the
workspace release profile (thin LTO, stripped). Each tree was a `git
archive` of the commit; all three shared one target directory, with source
mtimes refreshed so every workspace crate was rebuilt.

| Commit | Bytes | Change |
| --- | --- | --- |
| `df4575cf` (main, before the slice) | 18,306,192 | |
| `4d0c6231` (slice before TLS) | 19,458,992 | +1,152,800 from main |
| `561d0e64` (TLS) | 21,053,328 | +1,594,336 from `4d0c6231`; +2,747,136 from main |

The commits after `561d0e64` change no dependency.

### Duplicate dependencies

`cargo tree -d -p crew -e normal --locked --depth 0`, compared with main:
one new pair, `getrandom v0.2.17` (from `ring v0.17.14`) beside the
existing `getrandom v0.3.4`. The other eleven entries are unchanged from
main.

### Rust 1.82 gate

`env -u CREW_IDENTITY -u CREW_SOCKET -u CREW_LAUNCH ./scripts/check-msrv`
inside `nix-shell`, 2026-10-08 05:36:55–05:37:19 UTC: `rustc 1.82.0
(f6e511eec 2024-10-15)`, `cargo 1.82.0 (8f40fc59f 2024-08-21)`, locked
build passed, help parsed. Cargo downloaded the pinned TLS crates into its
cache for this toolchain, at the locked versions.

## Tests

- `tests/toolbox_proxy.rs`: T14 (sorted pinned view whatever the server
  does), T20 (preflight reason; a server that stops mid-session), secrets
  under destination names and redacted on the way back, streamable HTTP
  (JSON and event-stream answers), SSE, a refused header, verified TLS;
  a server that echoes its secret into instructions, descriptions, schema
  keys, stderr and an initialize error (nothing leaves unredacted); a
  server that accepts and stalls, over https and http, streamable and SSE
  (a 0.5 s preflight gives up in time, and a session serves the pinned
  view meanwhile); broken answers (JSON cut short, an event stream with no
  answer, a non-JSON SSE message), each answered with an error.
- `mcp/mod.rs::the_tool_list_does_not_depend_on_the_daemon_answering` (T21).
- `tests/team.rs::the_orchestrator_adds_members_with_bundles_by_name_only` (T11,
  part): over the orchestrator's own identity, item definitions and
  exclusions are refused, an unknown bundle is refused with the team's
  bundles listed, and a chosen bundle is saved to the roster.
- `tests/team.rs::toolbox_check_shows_planned_items_and_stops_on_a_required_one`
  (T20, first clause): `crew toolbox check` against a running daemon shows
  Planned or Needs, never Active, exits non-zero naming a required item
  that would not load, and passes when the same item is optional.
- `tests/toolbox_proxy.rs::always_loaded_tools_are_pinned_and_sorted_too`
  (T21, alwaysLoad part): tools marked `anthropic/alwaysLoad` keep the
  marker and are served pinned and sorted across sessions.
- `mcp/tools.rs::toolbox_arguments_are_bundle_names_only` (T11, part): both
  schemas take strings only, and a `toolbox` argument that is not names is
  refused rather than dropped.
- `daemon/toolbox_rpc.rs` unit tests (T11, subagents): a default subagent
  gets a copy of the parent's active toolbox (same definitions, content and
  servers, pinned from the parent's snapshots at its own launch); a
  selection gives only those bundles; after a reload widens a bundle while
  the parent runs the old one, a default subagent still gets the old items
  and asking for the bundle is refused naming the first new item; a bundle
  whose definition changed is refused; a parent without a toolbox has
  nothing to give.
- `mcp/proxy/http.rs` unit tests: URL parsing and joining, same-origin
  endpoints, event reading and its byte bound, header line count, chunked
  bodies, redirect wording; `mcp/proxy/stdio.rs` bounded lines.

## Gates

On `bd6431d5`: `cargo fmt --all -- --check` clean;
`cargo clippy --locked --workspace --all-targets -- -D warnings` clean;
`cargo test --locked --workspace` passed (67 test binaries, no failures),
run inside `nix-shell` with `CREW_IDENTITY`, `CREW_SOCKET` and
`CREW_LAUNCH` removed.

## Integration with toolbox-launch

graph's integration merge `563e871e` (its checkpoint `10702707` with this
branch at `2f5a1849`) was merged as `e0160530`; this card's files came
through byte for byte. On top, `b85b7345` moves the calls onto what a
launch prepares:

- `crew toolbox mcp` reaches only a server of the member's active toolbox
  (`toolbox_item`), with the credentials captured for that launch
  (`toolbox_references`), and scrubs every source name the launch
  recorded (`ActiveToolbox.scrub_env`);
- `crew toolbox` (with `--runtime`, or before a member has run) and `crew
  toolbox check` map a configuration with `toolbox_preview`;
- `crew toolbox reload` stages the team file's toolboxes with
  `toolbox_reload`;
- a move to another CLI is checked once, in `move_to_runtime`
  (`toolbox_runtime_check`);
- a subagent's launch pins its servers from its parent's snapshots
  (`snapshot_owner`), so nothing is copied at spawn.

## Left to end-to-end QA

T14's second half (after `crew resume --restart` the new description
appears, the digest changes, `toolbox.changed` is recorded) and T18 (resume
after removing a server) need real runtimes and a full launch. They belong
to `toolbox-docs-qa`. The proxy's part of T14 (refresh replaces a pin;
nothing changes without it) is covered above.

## Early security review

cache-reviewer looked at `6c4e9369` before the card was complete and
reproduced four findings with a canary server. Each is fixed and has a
regression test in `tests/toolbox_proxy.rs` or the unit tests.

- P1, redaction covered only tool results: `d1a00993`.
- P2, a stalled TLS or SSE server outlived the preflight deadline: `2f175928`.
- P3, SSE events and header lines were not fully counted: `2f175928`.
- P4, a broken answer left a request waiting forever: `2f175928`, plus the
  non-JSON SSE message case in the commit after it.

One limit stays: a host name lookup cannot be cancelled. The caller gives up
on it at the deadline; the lookup thread is left to finish on its own.
