import assert from 'node:assert/strict';
import { test } from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import { spawnSync } from 'node:child_process';
import { parseRecords, readSession, readWorkspace, summarize, enrich, collect, guardOutputs } from './baseline.mjs';

const epoch = Date.parse('2026-10-07T00:00:00Z');
const stamp = offset => new Date(epoch + offset).toISOString();
const rows = values => values.map((value, i) => ({ value, line: i + 1 }));
const assistant = (id, offset = 0, usage = {}) => ({ type: 'assistant', timestamp: stamp(offset), sessionId: 'session', requestId: `req-${id}`,
  message: { id, model: 'claude-test', content: [{ text: 'PRIVATE RESPONSE' }], usage: { input_tokens: 10, cache_creation_input_tokens: 20, cache_read_input_tokens: 70, output_tokens: 5, ...usage } } });
const emptyWorkspace = () => ({ workspace: '/workspace', state_dir: '/workspace/.crew', agents: [], events: [], deliveries: [], coverage: [] });
const counters = (input, read, output, write = 0) => ({ input_tokens: input, cached_input_tokens: read, output_tokens: output, cache_write_input_tokens: write, reasoning_output_tokens: output });
const tokenCount = (offset, cumulative, last = cumulative) => ({ type: 'event_msg', timestamp: stamp(offset), payload: { type: 'token_count', info: { total_token_usage: cumulative, last_token_usage: last } } });
function scratch(t) { const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'crew-cache-baseline-')); t.after(() => fs.rmSync(dir, { recursive: true, force: true })); return dir; }
function writeSession(file, values) { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, values.map(v => JSON.stringify(v)).join('\n') + '\n'); }
function fixtureDb(workspace, agents = []) {
  const file = path.join(workspace, '.crew/crew.db'); fs.mkdirSync(path.dirname(file), { recursive: true });
  const db = new DatabaseSync(file);
  db.exec('CREATE TABLE agents(id TEXT,data TEXT); CREATE TABLE events(seq INTEGER,timestamp INTEGER,event_type TEXT,agent_id TEXT); CREATE TABLE deliveries(message_id TEXT,recipient TEXT,delivered_at INTEGER); CREATE TABLE tokens(token TEXT);');
  for (const a of agents) db.prepare('INSERT INTO agents VALUES(?,?)').run(a.id, JSON.stringify(a));
  db.prepare('INSERT INTO tokens VALUES(?)').run('SECRET TOKEN MUST NEVER APPEAR');
  db.close(); return file;
}

test('Claude input denominator includes writes and reads; last streamed usage wins', () => {
  const a = assistant('a');
  const b = assistant('a', 1, { output_tokens: 9, cache_creation: { ephemeral_5m_input_tokens: 0, ephemeral_1h_input_tokens: 20 }, iterations: [{ input_tokens: 99999 }] });
  const s = parseRecords('claude', rows([a, b, { ...assistant('side'), isSidechain: true }, { ...assistant('synthetic'), message: { model: '<synthetic>', usage: a.message.usage } }]));
  assert.equal(s.requests.length, 1); assert.equal(s.stats.duplicate_usage_records, 1);
  assert.equal(s.requests[0].input_total_tokens, 100); assert.equal(s.requests[0].hit_rate, .7);
  assert.equal(s.requests[0].output_tokens, 9); assert.equal(s.requests[0].cache_write_1h_tokens, 20);
  assert.equal(s.requests[0].cache_write_5m_tokens, 0); assert.equal(s.requests[0].last_source_line, 2);
});

test('missing cache fields stay null, distinct from explicit zero', () => {
  const s = parseRecords('claude', rows([assistant('missing', 0, { cache_read_input_tokens: undefined }), assistant('zero', 1, { cache_read_input_tokens: 0 })]));
  assert.equal(s.requests[0].cache_read_tokens, null); assert.equal(s.requests[0].input_total_tokens, null); assert.equal(s.requests[0].hit_rate, null);
  assert.equal(s.requests[1].cache_read_tokens, 0); assert.equal(s.requests[1].hit_rate, 0);
  const summary = summarize(s.requests); assert.equal(summary.measured_requests, 1); assert.equal(summary.missing_metric_requests, 1); assert.equal(summary.hit_rate, 0);
});

test('thinking_drop joins by request id in file order; only exact history diagnostic counts', () => {
  const values = [assistant('a'),
    { type: 'user', isMeta: true, timestamp: stamp(1), message: { content: [{ text: '[crew] PRIVATE FOLDED BODY' }] } },
    { type: 'system', subtype: 'turn_duration', timestamp: stamp(2) },
    { type: 'attachment', timestamp: stamp(1), attachment: { type: 'thinking_drop', requestId: 'req-b', clientChange: { kinds: 'messagesHistoryChanged', firstChangedMessageIndex: 7 }, newlyDropped: { reason: 'prefix_mismatch', blockCount: 2 } } },
    assistant('b', 3),
    { type: 'system', subtype: 'turn_duration', timestamp: stamp(4) },
    { type: 'attachment', timestamp: stamp(5), attachment: { type: 'thinking_drop', requestId: 'req-c', clientChange: { kinds: 'none' }, newlyDropped: { reason: 'other' } } },
    assistant('c', 6)];
  const s = parseRecords('claude', rows(values));
  assert.equal(s.requests[1].native_diagnostic.source_line, 4);
  assert.equal(s.requests[1].native_history_changed, true); assert.equal(s.requests[1].preceding_completed_turn_folded, true);
  assert.equal(s.requests[2].native_history_changed, false); assert.equal(s.requests[2].preceding_completed_turn_folded, false);
  assert.equal(enrich({ ...s, runtime: 'claude', file: '/session.jsonl' }, emptyWorkspace()).requests[1].association, 'native_history_changed');
  const wrongReason = parseRecords('claude', rows([{ ...values[3], attachment: { ...values[3].attachment, newlyDropped: { reason: 'other' } } }, assistant('b')]));
  assert.equal(wrongReason.requests[0].native_history_changed, false);
});

test('plain crew delivery and WAKE are not folded deliveries', () => {
  const s = parseRecords('claude', rows([{ type: 'user', message: { content: '[crew] You are online as worker.' } },
    { type: 'user', message: { content: '[crew] WAKE' } }, assistant('a'), { type: 'system', subtype: 'turn_duration' }, assistant('b', 1)]));
  assert.equal(s.prompt_agent, 'worker'); assert.equal(s.crew_prompt, true); assert.equal(s.signals.filter(e => e.type === 'fold').length, 0);
  assert.equal(s.requests[1].preceding_completed_turn_folded, false);
});

test('Codex cumulative duplicates do not double count; reasoning is already in output', () => {
  const first = counters(100, 80, 10), next = counters(150, 120, 17);
  const s = parseRecords('codex', rows([{ type: 'session_meta', payload: { id: 'codex-session', cwd: '/workspace/.crew/worktrees/worker' } },
    { type: 'turn_context', payload: { model: 'gpt-test', turn_id: 'turn' } }, tokenCount(0, first), tokenCount(1, first), tokenCount(2, next, counters(50, 40, 7))]));
  const total = summarize(s.requests); assert.equal(s.requests.length, 2); assert.equal(s.stats.duplicate_usage_records, 1);
  assert.equal(total.input_total_tokens, 150); assert.equal(total.cache_read_tokens, 120); assert.equal(total.output_tokens, 17);
  assert.equal(total.hit_rate, .8); assert.equal(s.requests[1].turn_id, 'turn'); assert.equal(s.requests[1].delta_matches_last_usage, true);
});

test('Codex decreasing totals retain only direct last usage and report incomplete evidence', () => {
  const s = parseRecords('codex', rows([tokenCount(0, counters(100, 80, 10)), tokenCount(1, counters(20, 5, 2), counters(12, 3, 1)),
    tokenCount(2, counters(1, 0, 0), null)]));
  assert.equal(s.stats.cumulative_resets, 2); assert.equal(s.requests[1].input_total_tokens, 12);
  assert.equal(s.requests[2].input_total_tokens, null); assert.equal(s.requests[2].hit_rate, null);
  assert.equal(summarize(s.requests).missing_metric_requests, 1);
});

test('Codex absent write split stays unknown; first cumulative batch is flagged', () => {
  const s = parseRecords('codex', rows([tokenCount(0, { input_tokens: 100, cached_input_tokens: 80, output_tokens: 10 }, { input_tokens: 50, cached_input_tokens: 40, output_tokens: 5 })]));
  assert.equal(s.requests[0].cache_creation_tokens, null); assert.equal(s.requests[0].uncached_tail_tokens, null);
  assert.equal(s.requests[0].uncached_input_tokens, 20); assert.equal(s.requests[0].delta_matches_last_usage, false);
});

test('inconsistent counters are excluded from reported token sums and ratios', () => {
  const s = parseRecords('codex', rows([tokenCount(0, counters(10, 20, 1))]));
  const total = summarize(s.requests); assert.equal(s.requests[0].hit_rate, null); assert.equal(total.hit_rate, null);
  assert.equal(total.input_total_tokens, null); assert.equal(total.invalid_counter_requests, 1);
});

test('window filtering preserves preceding request and native event context', () => {
  const s = parseRecords('claude', rows([assistant('a'), { type: 'system', subtype: 'compact_boundary', timestamp: stamp(60) }, assistant('b', 3_600_001)]));
  const e = enrich({ ...s, runtime: 'claude', file: '/session.jsonl' }, emptyWorkspace(), { since: epoch + 1 });
  assert.equal(e.requests.length, 1); assert.equal(e.requests[0].gap_ms, 3_600_001); assert.equal(e.requests[0].idle_flags.over_1h, true);
  assert.equal(e.requests[0].association, 'compaction_associated');
});

test('crew metadata joins correct member and interval without duplicate delivery event', t => {
  const dir = scratch(t), dbFile = fixtureDb(dir, [{ id: 'worker', role: 'test role', session_id: 'session', runtime: 'claude' }]);
  const db = new DatabaseSync(dbFile);
  db.prepare('INSERT INTO events VALUES(?,?,?,?)').run(1, epoch + 1, 'message.delivered', 'worker');
  db.prepare('INSERT INTO events VALUES(?,?,?,?)').run(2, epoch + 2, 'agent.restart', 'worker');
  db.prepare('INSERT INTO events VALUES(?,?,?,?)').run(3, epoch + 2, 'agent.restart', 'other');
  db.prepare('INSERT INTO deliveries VALUES(?,?,?)').run('m-test', 'worker', epoch + 1); db.close();
  const before = fs.readFileSync(dbFile), workspace = readWorkspace(dir), s = parseRecords('claude', rows([assistant('a'), assistant('b', 4)]));
  const e = enrich({ ...s, runtime: 'claude', file: '/session.jsonl' }, workspace);
  assert.equal(e.agent_id, 'worker'); assert.equal(e.role, 'test role'); assert.equal(e.requests[1].crew_events.length, 2);
  assert.equal(e.requests[1].association, 'restart_associated'); assert.equal(e.requests[1].crew_events.filter(x => x.type === 'message.delivered').length, 1);
  assert.equal(JSON.stringify(workspace).includes('SECRET TOKEN'), false); assert.deepEqual(fs.readFileSync(dbFile), before);
});

test('streaming reader exports no prompt/result/credential bodies and fingerprints initial bytes', async t => {
  const dir = scratch(t), file = path.join(dir, 'native/session.jsonl');
  writeSession(file, [{ type: 'session_meta', payload: { id: 'id', cwd: '/workspace', base_instructions: 'PRIVATE SYSTEM TOKEN' } },
    { type: 'user', message: { content: '[crew] You are online as worker. PRIVATE REQUEST' } }, assistant('a')]);
  fs.appendFileSync(file, '{"usage":');
  const before = fs.readFileSync(file), s = await readSession(file, 'claude');
  assert.equal(s.source_sha256, crypto.createHash('sha256').update(before).digest('hex')); assert.equal(s.source_bytes, before.length);
  assert.equal(s.malformed_lines, 1); assert.equal(s.prompt_agent, 'worker'); assert.equal(s.requests.length, 1);
  assert.equal(JSON.stringify(s).includes('PRIVATE'), false); assert.deepEqual(fs.readFileSync(file), before);
});

test('empty sources are coverage gaps, not zero-percent hit rates', async t => {
  const dir = scratch(t), file = path.join(dir, 'native/empty.jsonl'); fs.mkdirSync(path.dirname(file)); fs.writeFileSync(file, '');
  const report = await collect({ workspace: dir, sessionFiles: [`claude=${file}`] });
  assert.equal(report.sessions.length, 1); assert.equal(report.totals.hit_rate, null); assert.equal(report.sessions[0].coverage.native_usage_observed, false);
});

test('outputs cannot replace sources through normal paths, symlinks or hardlinks', t => {
  const dir = scratch(t), sourceDir = path.join(dir, 'native'); fs.mkdirSync(sourceDir);
  const file = path.join(sourceDir, 'session.jsonl'); fs.writeFileSync(file, 'original');
  const symlink = path.join(dir, 'link.json'), hardlink = path.join(dir, 'hard.json'); fs.symlinkSync(file, symlink); fs.linkSync(file, hardlink);
  const report = { sessions: [{ file }], coverage: { source_dirs: [sourceDir] } };
  for (const out of [file, symlink, hardlink, path.join(sourceDir, 'new.json')]) assert.throws(() => guardOutputs([out], report), /Refusing/);
  const good = path.join(dir, 'report.json'); assert.doesNotThrow(() => guardOutputs([good], report));
  assert.throws(() => guardOutputs([good, good], report), /distinct/); assert.equal(fs.readFileSync(file, 'utf8'), 'original');
});

test('CLI writes only output reports and guards both paths before any output', t => {
  const dir = scratch(t), file = path.join(dir, 'native/source.jsonl'), workspace = path.join(dir, 'workspace');
  fixtureDb(workspace); writeSession(file, [assistant('a')]);
  const output = path.join(dir, 'report.json'), script = path.resolve('scripts/cache/baseline.mjs');
  const before = fs.readFileSync(file);
  const bad = spawnSync(process.execPath, [script, '--workspace', workspace, '--session-file', `claude=${file}`, '--output', output, '--markdown', file], { encoding: 'utf8' });
  assert.equal(bad.status, 1); assert.equal(fs.existsSync(output), false); assert.deepEqual(fs.readFileSync(file), before);
  const ok = spawnSync(process.execPath, [script, '--workspace', workspace, '--session-file', `claude=${file}`, '--output', output], { encoding: 'utf8' });
  assert.equal(ok.status, 0, ok.stderr); assert.equal(JSON.parse(fs.readFileSync(output, 'utf8')).totals.hit_rate, .7);
  assert.deepEqual(fs.readFileSync(file), before);
});

test('Pi keeps input, cache reads and writes separate', () => {
  const s = parseRecords('pi', rows([{ type: 'session', id: 'pi-session', cwd: '/workspace' }, { type: 'message', timestamp: stamp(0), id: 'message', message: { role: 'assistant', model: 'test', usage: { input: 10, cacheRead: 70, cacheWrite: 20, output: 5 } } }]));
  assert.equal(s.session_id, 'pi-session'); assert.equal(s.requests[0].input_total_tokens, 100); assert.equal(s.requests[0].hit_rate, .7);
});

test('automatic discovery separates registered runs and excludes operator and nested fixture sessions', async t => {
  const dir = scratch(t), home = path.join(dir, 'home'), workspace = path.join(dir, 'work'), other = path.join(dir, 'other');
  const workerDir = path.join(workspace, '.crew/worktrees/worker'); fs.mkdirSync(workerDir, { recursive: true });
  fixtureDb(workspace, [{ id: 'worker', session_id: 'worker-session', role: 'worker', runtime: 'claude', workdir: workerDir }, { id: 'missing', session_id: 'no-session', role: 'worker', runtime: 'codex' }]);
  fixtureDb(other, [{ id: 'other', session_id: 'other-session', role: 'other', runtime: 'codex', workdir: other }]);
  const native = path.join(home, '.claude/projects'), encoded = p => p.replace(/[^a-zA-Z0-9]/g, '-');
  const usage = { ...assistant('a'), cwd: workerDir, sessionId: 'worker-session' };
  writeSession(path.join(native, encoded(workerDir), 'worker-session.jsonl'), [usage]);
  writeSession(path.join(native, encoded(workspace), 'operator.jsonl'), [{ type: 'user', message: { content: 'PRIVATE OPERATOR QUESTION' } }, { ...assistant('o'), cwd: workspace, sessionId: 'operator' }]);
  writeSession(path.join(native, encoded(path.join(workerDir, 'target/fixture')), 'fixture.jsonl'), [usage]);
  const otherFile = path.join(home, '.codex/sessions/other.jsonl');
  writeSession(otherFile, [{ type: 'session_meta', payload: { id: 'other-session', cwd: other } }, tokenCount(0, counters(100, 90, 3))]);
  const state = path.join(home, '.local/state'), registry = path.join(state, 'crew/sessions'); fs.mkdirSync(registry, { recursive: true });
  fs.writeFileSync(path.join(registry, 'other.json'), JSON.stringify({ workspace: other, state_dir: path.join(other, '.crew') }));
  const previous = new Map(['CLAUDE_CONFIG_DIR', 'CODEX_HOME', 'XDG_STATE_HOME'].map(k => [k, process.env[k]]));
  t.after(() => { for (const [k, v] of previous) if (v === undefined) delete process.env[k]; else process.env[k] = v; });
  delete process.env.CLAUDE_CONFIG_DIR; delete process.env.CODEX_HOME; process.env.XDG_STATE_HOME = state;
  const report = await collect({ workspace, home, allRuns: true });
  assert.equal(report.runs.length, 2); assert.equal(report.sessions.length, 2); assert.equal(report.totals.requests, 2);
  assert.deepEqual(report.by_agent.map(a => a.name).sort(), ['other', 'worker']);
  assert.equal(report.coverage.excluded_files[0].reason, 'root_session_not_identified_as_crew');
  assert.equal(report.coverage.discovered_files, 3); assert.deepEqual(report.coverage.current_members_without_measured_session, ['missing']);
  assert.equal(report.by_runtime.find(r => r.name === 'codex').hit_rate, .9);
});

test('report separates folded-turn correlation, other diagnostics and avoidable loss', async t => {
  const dir = scratch(t), file = path.join(dir, 'native/source.jsonl');
  writeSession(file, [assistant('a'), { type: 'user', isMeta: true, message: { content: [{ text: '[crew] folded' }] } },
    { type: 'system', subtype: 'turn_duration' },
    { type: 'attachment', attachment: { type: 'thinking_drop', requestId: 'req-b', clientChange: { kinds: 'messagesHistoryChanged' }, newlyDropped: { reason: 'prefix_mismatch' } } }, assistant('b', 1)]);
  const report = await collect({ workspace: dir, sessionFiles: [`claude=${file}`] });
  assert.equal(report.fold_cohorts.find(c => c.preceding_turn_folded).requests, 1);
  assert.equal(report.fold_cohorts.find(c => c.preceding_turn_folded).diagnostic_history_changed_requests, 1);
  assert.equal(report.miss_associations.find(c => c.name === 'native_history_changed').avoidable_miss_tokens, null);
  assert.equal(report.sessions[0].requests[1].native_diagnostic.request_id, 'req-b');
});

test('team worktree reports may be written while its database and agent homes stay protected', t => {
  const dir = scratch(t), state = path.join(dir, '.crew'), db = fixtureDb(dir);
  const workerReport = path.join(state, 'worktrees/runtime/notes/prompt-cache/baseline.json');
  const report = { sessions: [], coverage: { source_dirs: [path.join(state, 'agents')], source_files: [db, `${db}-wal`, `${db}-shm`] } };
  assert.doesNotThrow(() => guardOutputs([workerReport], report));
  for (const out of [db, `${db}-wal`, path.join(state, 'agents/worker/rw/native.json')]) assert.throws(() => guardOutputs([out], report), /Refusing/);
});

test('streamed Pi header retains native session identity and cwd', async t => {
  const dir = scratch(t), file = path.join(dir, 'pi/session.jsonl');
  writeSession(file, [{ type: 'session', id: 'pi-id', cwd: '/workspace' }, { type: 'message', timestamp: stamp(0), message: { role: 'assistant', usage: { input: 1, cacheRead: 9, cacheWrite: 0, output: 2 } } }]);
  const parsed = await readSession(file, 'pi'); assert.equal(parsed.session_id, 'pi-id'); assert.equal(parsed.cwd, '/workspace');
});

test('null ids and ambiguous shared cwd cannot override known private owner or attach foreign events', () => {
  const workspace = { ...emptyWorkspace(), agents: [{ id: 'first', session_id: null, workdir: '/shared', role: 'first' }, { id: 'second', session_id: null, workdir: '/shared', role: 'second' }],
    events: [{ type: 'agent.restart', timestamp_ms: epoch, agent_id: 'first' }] };
  const s = { ...parseRecords('codex', rows([tokenCount(0, counters(10, 9, 1))])), cwd: '/shared', file: '/native/session.jsonl', runtime: 'codex' };
  const unknown = enrich(s, workspace); assert.equal(unknown.agent_id, null); assert.equal(unknown.requests[0].crew_events.length, 0);
  const known = enrich({ ...s, private_agent: 'second' }, workspace); assert.equal(known.agent_id, 'second'); assert.equal(known.requests[0].crew_events.length, 0);
});
