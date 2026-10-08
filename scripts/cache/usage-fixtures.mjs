#!/usr/bin/env node
/**
 * Extract allowlisted native usage fixtures for the Rust usage-metrics
 * parity tests from the sessions named in committed lifecycle reports.
 *
 * Reads only the exact native session files the reports name, never edits
 * them, and keeps only record types, ids, models, usage counters, timestamps
 * and turn markers: no prompt, reply, tool, reasoning text, working
 * directory, rate limit or credential. Each source is bounded to the
 * report's finish time, so records a session appended later are left out;
 * a Codex source must also match the native prefix digest the report's
 * `-native.json` recorded.
 *
 *   node scripts/cache/usage-fixtures.mjs [--check]
 *
 * `--check` re-extracts and compares with the committed fixtures without
 * writing.
 */
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const reports = ['01', '02', '03'].map(n => `notes/prompt-cache/lifecycle-before-${n}.json`);
const out = 'crates/crew/tests/fixtures/usage/lifecycle';

const sha256 = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const int = v => Number.isSafeInteger(v) ? v : v === null ? null : undefined;
const pick = (object, keys, map = int) => {
  if (!object || typeof object !== 'object' || Array.isArray(object)) return undefined;
  const kept = {};
  for (const k of keys) {
    const v = map(object[k]);
    if (v !== undefined) kept[k] = v;
  }
  return kept;
};
const str = v => typeof v === 'string' ? v : undefined;
const bool = v => typeof v === 'boolean' ? v : undefined;
const kinds = v => typeof v === 'string' ? v : Array.isArray(v) ? v.filter(k => typeof k === 'string') : undefined;
const clean = v => JSON.parse(JSON.stringify(v));

function claudeLine(v) {
  const base = { type: str(v.type), timestamp: str(v.timestamp) };
  if (v.type === 'system' && (v.subtype === 'turn_duration' || v.subtype === 'compact_boundary')) {
    return { ...base, subtype: v.subtype };
  }
  if (v.attachment?.type === 'thinking_drop') {
    const a = v.attachment;
    return { ...base, attachment: {
      type: 'thinking_drop', requestId: str(a.requestId),
      clientChange: a.clientChange ? { kinds: kinds(a.clientChange.kinds) } : undefined,
      newlyDropped: a.newlyDropped ? { reason: str(a.newlyDropped.reason) } : undefined,
    } };
  }
  const m = v.message;
  if (v.type !== 'assistant' || !m?.usage) return null;
  const usage = pick(m.usage, ['input_tokens', 'cache_read_input_tokens', 'cache_creation_input_tokens', 'output_tokens']);
  const buckets = pick(m.usage.cache_creation, ['ephemeral_5m_input_tokens', 'ephemeral_1h_input_tokens']);
  if (buckets) usage.cache_creation = buckets;
  return { ...base, sessionId: str(v.sessionId), requestId: str(v.requestId), isSidechain: bool(v.isSidechain),
    message: { id: str(m.id), role: str(m.role), model: str(m.model), usage } };
}

function codexLine(v) {
  const base = { type: str(v.type), timestamp: str(v.timestamp) };
  const p = v.payload;
  if (v.type === 'session_meta') return { ...base, payload: { id: str(p?.id) } };
  if (v.type === 'turn_context') return { ...base, payload: { model: str(p?.model), turn_id: str(p?.turn_id) } };
  if (v.type === 'compacted') return base;
  if (v.type === 'event_msg' && p?.type === 'context_compacted') return { ...base, payload: { type: p.type } };
  if (v.type !== 'event_msg' || p?.type !== 'token_count') return null;
  const keys = ['input_tokens', 'cached_input_tokens', 'cache_write_input_tokens', 'output_tokens', 'reasoning_output_tokens', 'total_tokens'];
  const info = p.info ? { total_token_usage: pick(p.info.total_token_usage, keys), last_token_usage: pick(p.info.last_token_usage, keys) } : null;
  return { ...base, payload: { type: 'token_count', info } };
}

function extract(report, agent, native) {
  const file = agent.source_path;
  const stat = fs.lstatSync(file);
  if (!stat.isFile()) throw new Error(`${file}: not a regular file`);
  const bytes = fs.readFileSync(file);
  if (agent.runtime === 'codex') {
    const recorded = native.sessions.find(s => s.session_id === agent.session_id);
    if (!recorded?.native_prefix_bytes) throw new Error(`${agent.agent}: no recorded native prefix`);
    const digest = sha256(bytes.subarray(0, recorded.native_prefix_bytes));
    if (digest !== recorded.native_prefix_sha256) throw new Error(`${agent.agent}: native prefix digest differs`);
  }
  const finished = Date.parse(report.finished_at);
  const lines = [];
  let offset = 0, number = 0, bound = 0;
  while (offset < bytes.length) {
    const nl = bytes.indexOf(0x0a, offset);
    if (nl < 0) break; // an incomplete final line is not a record
    const text = bytes.subarray(offset, nl).toString('utf8');
    number++;
    let value = null;
    try { value = JSON.parse(text); } catch { /* not a record */ }
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      const t = Date.parse(value.timestamp);
      if (Number.isFinite(t) && t > finished) break;
      const kept = agent.runtime === 'claude' ? claudeLine(value) : codexLine(value);
      if (kept) lines.push(JSON.stringify(clean(kept)));
    }
    offset = nl + 1;
    bound = offset;
  }
  return {
    text: lines.map(l => `${l}\n`).join(''),
    source: { bytes_read: bound, lines_read: number, sha256: sha256(bytes.subarray(0, bound)) },
  };
}

function main(argv) {
  const check = argv.includes('--check');
  const manifest = { schema: 1, script: 'scripts/cache/usage-fixtures.mjs', fixtures: [] };
  for (const rel of reports) {
    const reportBytes = fs.readFileSync(path.join(root, rel));
    const report = JSON.parse(reportBytes);
    const native = JSON.parse(fs.readFileSync(path.join(root, rel.replace('.json', '-native.json'))));
    const run = rel.match(/lifecycle-before-(\d+)\.json$/)[1];
    for (const agent of report.agents) {
      const name = `${run}-${agent.agent}.jsonl`;
      const { text, source } = extract(report, agent, native);
      const target = path.join(root, out, name);
      if (check) {
        if (fs.readFileSync(target, 'utf8') !== text) throw new Error(`${name}: differs from the committed fixture`);
      } else {
        fs.mkdirSync(path.dirname(target), { recursive: true });
        fs.writeFileSync(target, text);
      }
      manifest.fixtures.push({ fixture: name, report: rel, report_sha256: sha256(reportBytes), run, agent: agent.agent,
        runtime: agent.runtime, session_id: agent.session_id, bound: `records timestamped at or before ${report.finished_at}`,
        source, fixture_sha256: sha256(text) });
    }
  }
  const body = `${JSON.stringify(manifest, null, 2)}\n`;
  const target = path.join(root, out, 'manifest.json');
  if (check) {
    if (fs.readFileSync(target, 'utf8') !== body) throw new Error('manifest.json differs');
    console.log(`${manifest.fixtures.length} fixtures match`);
  } else {
    fs.writeFileSync(target, body);
    console.log(`${manifest.fixtures.length} fixtures written to ${out}`);
  }
}

main(process.argv.slice(2));
