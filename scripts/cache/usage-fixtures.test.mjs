// The committed usage-metrics fixtures read through the baseline collector
// give the same per-member totals as the lifecycle reports, so the Rust
// parity test (crates/crew/src/usage_metrics/parity.rs) and baseline.mjs
// agree on the same evidence.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { readSession, summarize } from './baseline.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const fixtures = path.join(root, 'crates/crew/tests/fixtures/usage/lifecycle');

test('baseline totals of the lifecycle fixtures match the reports', async () => {
  const manifest = JSON.parse(fs.readFileSync(path.join(fixtures, 'manifest.json')));
  assert.equal(manifest.fixtures.length, 9);
  for (const f of manifest.fixtures) {
    const report = JSON.parse(fs.readFileSync(path.join(root, f.report)));
    const agent = report.agents.find(a => a.agent === f.agent);
    const session = await readSession(path.join(fixtures, f.fixture), f.runtime);
    const total = summarize(session.requests);
    assert.equal(session.malformed_lines, 0, f.fixture);
    assert.equal(total.input_total_tokens, agent.input_tokens, `${f.fixture} input`);
    assert.equal(total.cache_read_tokens, agent.cached_tokens, `${f.fixture} read`);
    assert.equal(total.output_tokens, agent.output_tokens, `${f.fixture} output`);
    assert.equal(total.requests, agent.request_count, `${f.fixture} requests`);
    if (f.runtime === 'codex') {
      // The rollout reports writes, all zero here; the report's watchdog left them out.
      assert.equal(total.cache_creation_tokens, 0, f.fixture);
      assert.ok(session.requests.every(r => r.delta_matches_last_usage === true), f.fixture);
    }
  }
});
