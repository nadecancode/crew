// Fixture-only checks. The corpus remains usable without TypeScript or Bun.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { mkdirSync, readFileSync } from "node:fs";
import { run, slashCommand } from "../../../web/src/commands";
import { timeoutFor } from "../../../web/src/net/conn";
import { parseRoute } from "../../../web/src/route";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../..");
const read = (file: string) => readFileSync(join(here, file), "utf8");
const sources = JSON.parse(read("sources.json"));
const contexts = JSON.parse(read("contexts.json"));
const capabilities = JSON.parse(read("capabilities.json"));
assert.equal(sources.format_version, 1);
assert.equal(capabilities.format_version, 1);
assert.equal(capabilities.baseline_commit, sources.baseline_commit);
assert.match(sources.baseline_commit, /^[0-9a-f]{40}$/);

const refs = (keys: string[]) => {
  assert.ok(keys.length > 0);
  for (const key of keys) assert.ok(sources.references[key], `unknown source ${key}`);
};
for (const [key, reference] of Object.entries<any>(sources.references)) {
  assert.ok(!reference.path.startsWith("/") && !reference.path.includes(".."), key);
  const lines = readFileSync(join(repo, reference.path), "utf8").split("\n");
  assert.ok(lines[reference.line - 1]?.includes(reference.anchor), `${key}: source anchor moved`);
}

const layers = new Set(["compose", "action", "daemon", "command", "slash", "question_dialog", "confirmation", "operation", "model_dialog", "workspace", "wire", "index", "admission"]);
const cases: any[] = [];
const ids = new Set<string>();
let pure = 0;
for (const file of ["answers", "commands", "controls", "messaging", "wire", "workspaces"]) {
  for (const [line, text] of read(`${file}.jsonl`).trim().split("\n").entries()) {
    const c = JSON.parse(text);
    assert.match(c.id, /^[a-z][a-z0-9-]+$/, `${file}:${line + 1}`);
    assert.ok(!ids.has(c.id), `duplicate case ${c.id}`);
    ids.add(c.id);
    assert.ok(layers.has(c.layer), c.id);
    assert.ok(contexts[c.context_ref], `${c.id}: context`);
    assert.ok(c.input && c.expected, c.id);
    refs(c.source_refs);
    assert.ok(!(c.expected.outcome === "no_call" && (c.expected.call || c.expected.calls)), c.id);
    if (c.expected.error) {
      assert.ok(["invalid", "not_found", "forbidden", "conflict", "too_large", "unavailable", "unauthorized", "internal", "protocol"].includes(c.expected.error.code), c.id);
      assert.equal(typeof c.expected.error.message, "string", c.id);
    }
    const context = { ...contexts[c.context_ref], ...c.given };
    if (c.layer === "command") {
      let actual: unknown, error: unknown;
      try { actual = run(c.input.name, c.input.rest, context); }
      catch (caught) { error = caught; }
      if (c.expected.outcome === "error") {
        assert.ok(error instanceof Error, c.id);
        assert.equal(error.message, c.expected.message, c.id);
      } else {
        assert.equal(error, undefined, c.id);
        assert.deepEqual(actual, c.expected.effect, c.id);
      }
      pure++;
    } else if (c.layer === "slash") {
      assert.deepEqual(slashCommand(c.input.text), c.expected.parsed, c.id);
      pure++;
    } else if (c.layer === "workspace" && c.input.entry === "parseRoute") {
      assert.deepEqual(parseRoute(c.input.hash), c.expected.route, c.id);
      pure++;
    }
    cases.push(c);
  }
}
assert.deepEqual(capabilities.confirmations.kinds, ["kill", "restart-fresh", "restart-paused", "stop"]);
refs(capabilities.confirmations.source_refs);
refs(capabilities.limits.source_refs);
refs(capabilities.read_only.source_refs);
assert.equal(capabilities.read_only.server_read_only_role, false);
for (const state of capabilities.read_only.states) {
  assert.equal(state.writable, state.hub_live && state.workspace_shown === "live");
}
const methods = new Set<string>();
for (const row of capabilities.operations) {
  assert.ok(!methods.has(row.method));
  methods.add(row.method);
  assert.equal(timeoutFor(row.method), row.timeout_ms, row.method);
  assert.deepEqual(row.channels.gateway_device, {
    Read: "pending native-gateway-contract",
    Control: "pending native-gateway-contract",
    Terminal: "pending native-gateway-contract",
  });
  refs(row.source_refs);
}
for (const row of capabilities.excluded_operations) refs(row.source_refs);
for (const kind of capabilities.confirmations.kinds) {
  assert.ok(cases.some(c => c.layer === "confirmation" && c.input.kind === kind && c.input.decision === "confirm"));
  assert.ok(cases.some(c => c.layer === "confirmation" && c.input.kind === kind && c.input.decision === "cancel"));
}
console.log(`${cases.length} cases, ${methods.size} surfaced methods, ${Object.keys(sources.references).length} source anchors checked; ${pure} pure web command/parser/route cases passed.`);

function execute(program: string, args: string[]) {
  const result = spawnSync(program, args, { cwd: repo, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const diagnostics = result.stdout.split("\n").flatMap(line => {
      try { return [JSON.parse(line).message?.rendered].filter(Boolean); }
      catch { return []; }
    }).join("\n");
    throw new Error(`${program} failed (${result.status})\n${diagnostics || result.stdout}\n${result.stderr}`);
  }
  return result.stdout;
}
if (process.argv.includes("--wire") || process.argv.includes("--write-wire")) {
  const built = execute("cargo", ["build", "-p", "crew-core", "--lib", "--message-format=json"]);
  const artifacts = built.trim().split("\n").filter(Boolean).map(line => JSON.parse(line));
  const library = (name: string): string => {
    const artifact = artifacts.find(a => a.reason === "compiler-artifact" && a.target.name === name && a.filenames.some((f: string) => f.endsWith(".rlib")));
    assert.ok(artifact, `no ${name} build artifact`);
    return artifact.filenames.find((f: string) => f.endsWith(".rlib"));
  };
  const core = library("crew_core");
  const json = library("serde_json");
  const cacheRoot = process.env.CREW_NATIVE_FIXTURE_CACHE;
  const cache = cacheRoot ? resolve(repo, cacheRoot, "actions") : join(here, ".cache");
  mkdirSync(cache, { recursive: true });
  const probe = join(cache, "wire-probe");
  execute("rustc", [join(here, "wire-probe.rs"), "--edition=2021", "-L", `dependency=${dirname(core)}`, "-L", `dependency=${dirname(json)}`, "--extern", `crew_core=${core}`, "--extern", `serde_json=${json}`, "-o", probe]);
  const args = [here];
  if (process.argv.includes("--write-wire")) args.push("--write");
  if (process.argv.includes("--print-wire")) args.push("--print");
  process.stdout.write(execute(probe, args));
}
