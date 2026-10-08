# Toolbox acceptance fixtures

Offline preparation for `notes/toolboxes.md` T01–T21. These fixtures do not
launch a model, read an operator's real home, or mark product acceptance as
passed. `cases.json` lists the evidence each acceptance case still needs.

## Run

```sh
fixtures/toolboxes/run self-test
fixtures/toolboxes/run prepare --out "$PWD/target/toolbox-fixture"
fixtures/toolboxes/run cases
```

The fixture contract is also checked directly through the real Rust loader
and resolver, without invoking Python:

```sh
./scripts/build-env cargo test --locked -p crew-core --test toolbox_fixtures
./scripts/build-env cargo clippy --locked -p crew-core --test toolbox_fixtures -- -D warnings
cargo fmt --all -- --check
```

Initial validation: 16 Python harness checks and 6 Rust fixture tests passed;
strict Clippy and workspace formatting passed. These results validate the
scaffolding and core fixture contracts, not runtime acceptance T01–T21.

Python 3.11 or newer, standard library only. The runner uses `TOOLBOX_PYTHON`
when set, otherwise `python3` on PATH, otherwise `nix shell nixpkgs#python3`.
On the development host Python is not on PATH; the Nix fallback runs the
self-tests. Set `TOOLBOX_PYTHON` to a provisioned interpreter for a fully
offline environment with no Nix resolution. No Python dependency is added
to the Rust test suites.

`prepare` refuses an existing output path. It creates a private output tree
with fixture runtime wrappers under `bin`, a synthetic operator home, project
discovery roots, secret input files with mode 0600, an empty member directory
with earlier-session leftovers, and a manifest. It never starts a daemon or
changes the invoking process's environment. All credentials are conspicuously
fake test strings. Only the test child receives the synthetic home.

Team and style files use JSON syntax, which is valid YAML, so both crew's
actual YAML loader and the dependency-free Python harness can read them.
`@ROOT@`, `@PYTHON@`, and `@HTTP_URL@` are preparation placeholders; do not
run a checked-in template directly. File-item paths remain relative to their
team or style file. The `~/` style path targets the synthetic home.

## Inputs

- `teams/example.yaml`: the spec's T02 example, with local fixture programs
  and a loopback remote server instead of third-party services.
- `teams/full.yaml`: every toolbox field and inheritance level, all five
  item kinds, explicit runtime filters, required and strict policies,
  operator `none`/`all`/kind selection, project selection, exclusions,
  resets, a same-role peer, pi and shell members. The unselected `sse-field`
  bundle covers the schema field; this Python server does not implement SSE.
- `teams/legacy.yaml`, `teams/legacy-inherit.yaml`: absence of toolbox keys
  and the legacy `inherit_mcp` mapping.
- `teams/resolution.yaml`: defaults exclusion followed by member re-add,
  whole-item replacement and reset of defaults' policies.
- `styles/fixture.yaml`: style-relative and home-relative file items.
- `validation.json`: core validation errors plus launch-only endpoint and
  secret-alias collision inputs. Launch-only cases are not claimed by the
  core tests.
- `items/`: selected skills, nested skill content, a Claude Code plugin
  with a skill, native subagent and bundled MCP sentinel, and shared subagent
  Markdown. A Claude plugin is intentionally not a Codex or pi plugin.

Generated `faults/` includes file and directory symlinks that leave their
item, a missing `SKILL.md`, malformed front matter, a sparse file of
16 MiB + 1 byte, and two source directories with identical content. Link
targets remain inside the owned fixture tree. `member/rw/home/<runtime>`
contains a stale generated toolbox file and a separate ambient skill from
an earlier session, to distinguish cleanup from source enumeration.
`faults/readonly-skill` has nested files with mode 0444; testing the mounted
store's read-only behavior still requires the real sandbox.

`sentinel_home.py` generates operator and project MCP, skills, plugins,
subagents, hooks, settings and memory sentinels for Claude Code, Codex and
pi's supported surfaces. The manifest inventories each sentinel and hashes
the operator home before any test. Native runtimes must verify their own
discovery: the presence of a synthetic config is not proof that it loads.
Never put the fixture wrappers in front of real runtimes for a native probe.

## Shared Python helpers

Load `harness.py` by adding this directory to a test's module search path.
For example:

```python
with tempfile.TemporaryDirectory() as temporary:
    root = Path(temporary) / "fixture"
    before = harness.prepare(root)
    env = harness.isolated_environment(root)
    with harness.RpcProcess(
        [before["python"], str(root / "mcp_stub.py")], env=env, cwd=root
    ) as server:
        server.request("initialize", {"protocolVersion": "2025-06-18"})
        tools = server.request("tools/list")["result"]
    assert harness.tree_snapshot(root / "operator-home") == before["operator_snapshot"]
```

`RpcProcess` accepts any JSON-lines MCP command, including scheduler's Rust
`crew fake-mcp`, and has bounded requests and owned-process cleanup.
`http_stub(root, control=...)` starts a loopback-only HTTP child on an
OS-selected port and yields its `/mcp` URL. Replace the example team's
`toolboxes.github.mcp.docs.url` with that URL before running product code.
The default prepared URL uses port 9 and is deliberately not a live service.
`http_request` refuses non-loopback URLs and ignores proxy environment.

`isolated_environment` supplies an allowlisted environment for owned test
children. It does not inherit the invoking agent's crew identity, socket,
provider tokens, endpoint variables, Git repository selectors or proxies.
Pass required synthetic daemon inputs explicitly in its `extra` argument.
For T05 those are `DOCS_TOKEN` and `DAEMON_TOKEN` using `SECRET_VALUES`;
the generated crew config maps `SHOP_TOKEN` to `DAEMON_TOKEN`, reads
`SHOP_GITHUB_TOKEN` from a file for member `release`, and supplies a distinct
endpoint `ENDPOINT_SENTINEL` file.

Evidence helpers:

- `tree_snapshot`: byte hashes and link targets, without following links.
  Compare the same member's generated files across restart or resume; do not
  compare identity-bearing files across different members.
- `assert_no_secrets`: scan supplied generated bytes for resolved synthetic
  values. Failure identifies the sentinel name, not its value. Callers must
  supply every relevant file, argv, payload and transcript.
- `assert_scrubbed`: reject secret reference and alias-source names in the
  observed member environment.
- `assert_pinned_tools`: reject unsorted, duplicate or changing downstream
  catalogs. It never sorts observations to make a test pass.
- `CaseLedger`: initially marks every case `not_run`; recording a pass,
  failure or deferral requires evidence. Fixture self-tests do not pass T01–T21.

## MCP upstream controls

`mcp_stub.py` defaults to JSON-lines stdio; `--http --ready-file PATH`
serves JSON Streamable HTTP at `127.0.0.1` with an ephemeral port. It accepts
numeric and string request IDs, initialize/initialized, tools/list,
tools/call and ping. It deliberately advertises upstream `listChanged`.

`--mode` takes `normal`, `exit-initialize`, `hang-list`, `exit-on-call` or
`echo-error`. `exit-on-call` succeeds at preflight but fails when a session
uses a tool. Test helpers use short deadlines for the stub self-test; product
T20 must separately prove the real 30-second preflight deadline.

`--control FILE` reads a JSON control file on each request:

```json
{
  "description": "initial",
  "mutate_after_first": true,
  "always_load": true,
  "account": "release",
  "expected_env_sha256": {"GITHUB_TOKEN": "expected SHA-256"},
  "expected_headers_sha256": {"authorization": "expected SHA-256"}
}
```

The list rotates deterministically on each request, mutation changes a
description, and account names add distinct tools. The proxy must pin the
observed definitions; the stub never does it on the proxy's behalf. Change
`description` in the control file to preserve an upstream change across
process restarts. `inspect_environment` returns variable/header names and
digest-match booleans, including negative controls, never values. Header keys
are lowercase. `--log FILE` records request methods and names only.

`echo-error` intentionally emits the synthetic value named by `echo_env`
(default `GITHUB_TOKEN`) in an upstream error so the real proxy's redaction
can be tested. Do not give this fixture genuine credentials. Ordinary stub
responses and logs never include credential values.

This independent Python server does not cover legacy SSE, HTTP session
negotiation, streaming notifications or every upstream protocol error.
Scheduler's Rust `crew fake-mcp` supplies those proxy integration seams.
`runtime_stub.py` only records arguments, cwd and environment names, then
exits unless one of its explicit test modes is selected:

- `--fixture-mcp-catalog PATH` returns the supplied JSON for `mcp list
  --json`; preparation writes `codex-mcp-list.json` in the observed native
  response shape. This response does not interpret or enforce `-c` overrides.
- `--fixture-app-server` accepts JSON-lines `initialize`, `initialized`, and
  `skills/extraRoots/set` with absolute `extraRoots`; the last returns
  `{ "result": {} }`. `--fixture-reject-roots` injects an error. Other
  methods, including `skills/list`, fail explicitly.

Both modes record requests for assertion by the adapter tests. Neither proves
runtime-native discovery, TUI propagation, or a successful resume.

T16 requires separately authorized provider measurements; no model is called
by this harness. T01/T03/T12/T18 still need native evidence, T19 needs a real
sandbox, and T15 needs each actual consumer. The case manifest states these
boundaries so preparation cannot be mistaken for completed acceptance.
