#!/usr/bin/env node
/** Read runtime-native usage and crew metadata; never modify a source file. */
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import readline from 'node:readline';
import crypto from 'node:crypto';
import { DatabaseSync } from 'node:sqlite';
import { pathToFileURL } from 'node:url';

export const SCHEMA_VERSION = 1;
const number = v => Number.isSafeInteger(v) && v >= 0 ? v : null;
const time = v => typeof v === 'number' ? v : typeof v === 'string' ? Date.parse(v) : NaN;
const ratio = (a, b) => a !== null && b > 0 ? a / b : null;
const total = values => values.every(v => v !== null) ? values.reduce((a, b) => a + b, 0) : null;
const sub = (a, b) => a !== null && b !== null && a >= b ? a - b : null;

function event(type, v, line, extra = {}) {
  return { type, timestamp_ms: Number.isFinite(time(v.timestamp)) ? time(v.timestamp) : null, source_line: line, ...extra };
}

function baseRequest(v, line, id, usage) {
  return {
    source_id: id, source_line: line, last_source_line: line,
    timestamp_ms: Number.isFinite(time(v.timestamp)) ? time(v.timestamp) : null,
    model: v.message?.model ?? null, request_id: v.requestId ?? null,
    ...usage,
  };
}

function claudeUsage(usage) {
  const tail = number(usage.input_tokens), read = number(usage.cache_read_input_tokens);
  const write = number(usage.cache_creation_input_tokens);
  return {
    input_total_tokens: total([tail, read, write]), cache_read_tokens: read,
    cache_creation_tokens: write, uncached_tail_tokens: tail,
    output_tokens: number(usage.output_tokens),
    cache_write_5m_tokens: number(usage.cache_creation?.ephemeral_5m_input_tokens),
    cache_write_1h_tokens: number(usage.cache_creation?.ephemeral_1h_input_tokens),
  };
}

function codexUsage(usage) {
  const input = number(usage.input_tokens), read = number(usage.cached_input_tokens);
  const write = number(usage.cache_write_input_tokens);
  return {
    input_total_tokens: input, cache_read_tokens: read, cache_creation_tokens: write,
    uncached_tail_tokens: write === null ? null : sub(sub(input, read), write),
    output_tokens: number(usage.output_tokens),
    cache_write_5m_tokens: null, cache_write_1h_tokens: null,
  };
}

/** File order is authoritative: native attachment timestamps may regress. */
export function parseRecords(runtime, records) {
  const requests = [], signals = [], diagnostics = new Map(), messages = new Map();
  const stats = { raw_usage_records: 0, duplicate_usage_records: 0, skipped_sidechain: 0, skipped_synthetic: 0, cumulative_resets: 0, missing_cumulative: 0 };
  let previous = null, model = null, sessionId = null, cwd = null, turnId = null;
  let firstUserSeen = false, crewPrompt = false, promptAgent = null;
  let turn = 0, folded = false, lastTurnFolded = null, firstInTurn = true;
  for (let i = 0; i < records.length; i++) {
    const { value: v, line } = records[i];
    if (runtime === 'claude' && v.isSidechain) { stats.skipped_sidechain++; continue; }
    if (runtime === 'pi' && v.type === 'session') { sessionId = v.id ?? sessionId; cwd = v.cwd ?? cwd; }
    if (v.type === 'session_meta') { sessionId = v.payload?.id ?? sessionId; cwd = v.payload?.cwd ?? cwd; }
    if (v.sessionId) sessionId = v.sessionId;
    if (v.cwd) cwd = v.cwd;
    if (v.type === 'turn_context') { model = v.payload?.model ?? model; turnId = v.payload?.turn_id ?? turnId; }
    if (runtime === 'codex' && !firstUserSeen && v.payload?.user_prefix !== undefined) {
      firstUserSeen = true; crewPrompt = v.payload.user_prefix.startsWith('[crew]');
      promptAgent = v.payload.user_prefix.match(/^\[crew\] You are online as ([a-zA-Z0-9_-]+)\./)?.[1] ?? null;
    }
    if (v.type === 'compacted' || v.payload?.type === 'context_compacted') signals.push(event('compaction', v, line));
    if (runtime === 'claude') {
      if (v.type === 'system' && v.subtype === 'compact_boundary') signals.push(event('compaction', v, line));
      if (v.type === 'system' && v.subtype === 'turn_duration') {
        signals.push(event('turn_end', v, line, { turn_index: turn, folded }));
        lastTurnFolded = folded; folded = false; turn++; firstInTurn = true;
      }
      if (v.type === 'user') {
        const content = v.message?.content;
        const text = typeof content === 'string' ? content : content?.[0]?.text;
        if (!firstUserSeen && typeof content === 'string') {
          firstUserSeen = true; crewPrompt = text.startsWith('[crew]');
          promptAgent = text.match(/^\[crew\] You are online as ([a-zA-Z0-9_-]+)\./)?.[1] ?? null;
        }
        if (v.isMeta === true && Array.isArray(content) && text?.startsWith('[crew]')) {
          signals.push(event('fold', v, line)); folded = true;
        }
        else if (typeof content === 'string' && text.startsWith('[crew]')) signals.push(event('delivery', v, line));
        // Inspect prefixes only; prompt contents are never retained in output.
      }
      if (v.attachment?.type === 'silent_turn_reminder') signals.push(event('silent_reminder', v, line));
      if (v.attachment?.type === 'thinking_drop') {
        const a = v.attachment;
        const safe = event('thinking_drop', v, line, {
          request_id: a.requestId ?? null,
          kinds: a.clientChange?.kinds ?? null,
          first_changed_message_index: a.clientChange?.firstChangedMessageIndex ?? null,
          baseline: a.clientChange?.baseline ?? null,
          call_number: a.clientChange?.callNumber ?? null,
          reason: a.newlyDropped?.reason ?? null,
          dropped_blocks: a.newlyDropped?.blockCount ?? null,
          dropped_turns: a.newlyDropped?.turnCount ?? null,
        });
        signals.push(safe);
        if (safe.request_id) diagnostics.set(safe.request_id, safe);
      }
      if (v.type !== 'assistant' || !v.message?.usage) continue;
      if (v.message.model === '<synthetic>') { stats.skipped_synthetic++; continue; }
      stats.raw_usage_records++;
      const id = v.message.id ?? `line:${line}`;
      const usage = claudeUsage(v.message.usage);
      if (messages.has(id)) {
        const r = messages.get(id);
        Object.assign(r, usage, { last_source_line: line });
        stats.duplicate_usage_records++;
      } else {
        const r = baseRequest(v, line, id, usage);
        r.turn_index = turn; r.first_request_in_turn = firstInTurn;
        r.preceding_completed_turn_folded = firstInTurn ? lastTurnFolded : null;
        firstInTurn = false;
        messages.set(id, r); requests.push(r);
      }
    } else if (runtime === 'codex') {
      if (v.type !== 'event_msg' || v.payload?.type !== 'token_count' || !v.payload.info) continue;
      stats.raw_usage_records++;
      const info = v.payload.info, cumulative = info.total_token_usage;
      if (!cumulative) { stats.missing_cumulative++; continue; }
      const keys = ['input_tokens', 'cached_input_tokens', 'cache_write_input_tokens', 'output_tokens'];
      if (previous && keys.every(k => cumulative[k] === previous[k])) { stats.duplicate_usage_records++; continue; }
      const reset = previous && keys.some(k => number(cumulative[k]) !== null && number(previous[k]) !== null && cumulative[k] < previous[k]);
      let delta;
      if (reset) {
        stats.cumulative_resets++;
        // Last-request usage is direct evidence. Never fabricate a negative
        // cumulative delta or treat a counter reset as zero usage.
        delta = info.last_token_usage ?? {};
      } else {
        delta = Object.fromEntries(keys.map(k => [k, previous ? sub(number(cumulative[k]), number(previous[k])) : number(cumulative[k])]));
      }
      const r = baseRequest(v, line, `token_count:${line}`, codexUsage(delta));
      r.model = model; r.turn_id = turnId; r.cumulative_reset = Boolean(reset);
      r.native_measurement = reset ? 'last_usage_after_counter_reset' : 'cumulative_delta';
      r.delta_matches_last_usage = info.last_token_usage ? keys.filter(k => number(delta[k]) !== null && number(info.last_token_usage[k]) !== null).every(k => delta[k] === info.last_token_usage[k]) : null;
      requests.push(r); previous = cumulative;
    } else if (runtime === 'pi') {
      const m = v.message;
      if (m?.role !== 'assistant' || !m.usage) continue;
      stats.raw_usage_records++;
      const u = m.usage;
      const tail = number(u.input), read = number(u.cacheRead), write = number(u.cacheWrite);
      const r = baseRequest(v, line, v.id ?? `line:${line}`, {
        input_total_tokens: total([tail, read, write]), cache_read_tokens: read, cache_creation_tokens: write,
        uncached_tail_tokens: tail, output_tokens: number(u.output), cache_write_5m_tokens: null, cache_write_1h_tokens: null,
      });
      r.model = m.model ?? null; requests.push(r);
    }
  }
  for (const r of requests) {
    r.native_diagnostic = r.request_id ? diagnostics.get(r.request_id) ?? null : null;
    r.uncached_input_tokens = sub(r.input_total_tokens, r.cache_read_tokens);
    r.hit_rate = ratio(r.cache_read_tokens, r.input_total_tokens);
    r.counter_valid = r.input_total_tokens === null || r.cache_read_tokens === null || r.cache_read_tokens <= r.input_total_tokens;
    if (!r.counter_valid) r.hit_rate = null;
    const kinds = r.native_diagnostic?.kinds;
    const changed = typeof kinds === 'string' ? kinds.split(/[, ]+/).includes('messagesHistoryChanged') : Array.isArray(kinds) && kinds.includes('messagesHistoryChanged');
    r.native_history_changed = Boolean(changed && r.native_diagnostic.reason === 'prefix_mismatch');
  }
  return { requests, signals, stats, session_id: sessionId, cwd, crew_prompt: crewPrompt, prompt_agent: promptAgent };
}

// Drop content at the read boundary. Parsing requires only the crew wrapper
// classification, never a saved prompt, tool result, event text or credential.
function safeRecord(v) {
  const m = v.message, a = v.attachment;
  const content = m?.content, text = typeof content === 'string' ? content : content?.[0]?.text;
  const prefix = typeof text === 'string' && text.startsWith('[crew]') ? (text.match(/^\[crew\] You are online as [a-zA-Z0-9_-]+\./)?.[0] ?? '[crew]') : '';
  const usage = m?.usage;
  const p = v.payload;
  const userText = p?.type === 'user_message' ? p.message : p?.role === 'user' ? p.content?.find(c => c.type === 'input_text' || c.type === 'text')?.text : undefined;
  return {
    type: v.type, subtype: v.subtype, timestamp: v.timestamp, sessionId: v.sessionId, cwd: v.cwd,
    id: v.id, requestId: v.requestId, isSidechain: v.isSidechain, isMeta: v.isMeta,
    message: m ? { id: m.id, role: m.role, model: m.model, usage: usage ? {
      input_tokens: usage.input_tokens, output_tokens: usage.output_tokens,
      cache_creation_input_tokens: usage.cache_creation_input_tokens, cache_read_input_tokens: usage.cache_read_input_tokens,
      cache_creation: usage.cache_creation ? { ephemeral_5m_input_tokens: usage.cache_creation.ephemeral_5m_input_tokens, ephemeral_1h_input_tokens: usage.cache_creation.ephemeral_1h_input_tokens } : undefined,
      input: usage.input, output: usage.output, cacheRead: usage.cacheRead, cacheWrite: usage.cacheWrite,
    } : undefined, content: typeof content === 'string' ? prefix : Array.isArray(content) ? [{ text: prefix }] : undefined } : undefined,
    attachment: a ? { type: a.type, requestId: a.requestId,
      clientChange: a.clientChange ? { kinds: a.clientChange.kinds, firstChangedMessageIndex: a.clientChange.firstChangedMessageIndex, baseline: a.clientChange.baseline, callNumber: a.clientChange.callNumber } : undefined,
      newlyDropped: a.newlyDropped ? { reason: a.newlyDropped.reason, blockCount: a.newlyDropped.blockCount, turnCount: a.newlyDropped.turnCount } : undefined } : undefined,
    payload: p ? { type: p.type, id: p.id, cwd: p.cwd, model: p.model, turn_id: p.turn_id,
      user_prefix: typeof userText === 'string' ? (userText.match(/^\[crew\] You are online as [a-zA-Z0-9_-]+\./)?.[0] ?? (userText.startsWith('[crew]') ? '[crew]' : '')) : undefined,
      info: p.info ? { total_token_usage: p.info.total_token_usage, last_token_usage: p.info.last_token_usage } : undefined } : undefined,
  };
}

export async function readSession(file, runtime, options = {}) {
  const records = [], hash = crypto.createHash('sha256');
  let line = 0, malformed = 0, bytes = 0;
  const before = fs.statSync(file);
  if (!before.isFile()) throw new Error('Native source is not a regular file');
  if (!before.size) return { ...parseRecords(runtime, []), file, source_sha256: hash.digest('hex'), source_bytes: 0, malformed_lines: 0, source_lines: 0,
    snapshot: { size_at_start: 0, size_at_end: 0, changed_during_read: false } };
  // Bound a live transcript to its initial byte size, including a possibly
  // incomplete final line. Appended usage belongs to a later measurement.
  const stream = fs.createReadStream(file, before.size ? { end: before.size - 1 } : {});
  stream.on('data', chunk => { hash.update(chunk); bytes += chunk.length; });
  for await (const text of readline.createInterface({ input: stream, crlfDelay: Infinity })) {
    line++;
    // Avoid parsing every tool output in multi-gigabyte transcripts.
    if (!/"usage"|token_count|session_meta|turn_context|thinking_drop|compact_boundary|turn_duration|"compacted"|context_compacted|silent_turn_reminder|"isMeta"|"type"\s*:\s*"(user|session)"|user_message|"role"\s*:\s*"user"/.test(text)) continue;
    let value;
    try { value = JSON.parse(text); } catch { malformed++; continue; }
    if (!value || typeof value !== 'object' || Array.isArray(value)) { malformed++; continue; }
    records.push({ value: safeRecord(value), line });
  }
  const parsed = parseRecords(runtime, records);
  const after = fs.statSync(file);
  return { ...parsed, file, source_sha256: hash.digest('hex'), source_bytes: bytes, malformed_lines: malformed, source_lines: line,
    snapshot: { size_at_start: before.size, size_at_end: after.size, changed_during_read: before.size !== after.size || before.mtimeMs !== after.mtimeMs } };
}

function regularFiles(dir, predicate = () => true) {
  const out = [];
  if (!fs.existsSync(dir)) return out;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) out.push(...regularFiles(p, predicate));
    else if (e.isFile() && predicate(p)) out.push(p);
  }
  return out;
}

/** Only metadata queries; tokens, prompt/message bodies and event text stay private. */
export function readWorkspace(workspace, stateDir = null) {
  workspace = path.resolve(workspace);
  const state = stateDir ?? (path.basename(workspace) === '.crew' ? workspace : path.join(workspace, '.crew'));
  const dbFile = path.join(state, 'crew.db');
  const result = { workspace: path.basename(workspace) === '.crew' ? path.dirname(workspace) : workspace, state_dir: state, agents: [], member_ids: [], events: [], deliveries: [], coverage: [], database: null };
  if (!fs.existsSync(dbFile)) { result.coverage.push('crew_database_missing'); return result; }
  const db = new DatabaseSync(dbFile, { readOnly: true });
  try {
    db.exec('BEGIN');
    const snapshot = db.prepare('SELECT MAX(seq) AS max_seq,MAX(timestamp) AS last_event_ms FROM events').get();
    result.database = { file: dbFile, read_at: new Date().toISOString(), ...snapshot };
    result.member_ids = db.prepare('SELECT DISTINCT agent_id FROM events WHERE agent_id IS NOT NULL').all().map(r => r.agent_id);
    for (const row of db.prepare('SELECT id,data FROM agents').all()) {
      const a = JSON.parse(row.data);
      result.agents.push({ id: row.id, role: a.role ?? null, runtime: a.runtime ?? null, model: a.running_model ?? a.model ?? null,
        session_id: a.session_id ?? null, workdir: a.workdir ?? null });
    }
    result.events = db.prepare('SELECT seq,timestamp,event_type,agent_id FROM events ORDER BY seq').all().filter(r => r.event_type !== 'message.delivered' && /^(agent\.(spawned|restart|runtime_set|model_set|model_restored|model_fallback|model_policy|compacted|compact_queued|reminded|unanswered|unresponsive|limited|limit_lifted|turn_failed|input)|task\.resumed|session\.started|delivery\.fallback)$/.test(r.event_type)).map(r => ({ source: 'crew_event', source_id: r.seq, timestamp_ms: r.timestamp, type: r.event_type, agent_id: r.agent_id }));
    result.deliveries = db.prepare('SELECT message_id,recipient,delivered_at FROM deliveries WHERE delivered_at IS NOT NULL').all().map(r => ({ source: 'crew_delivery', source_id: r.message_id, timestamp_ms: r.delivered_at, type: 'message.delivered', agent_id: r.recipient }));
    db.exec('COMMIT');
  } catch (error) { result.coverage.push(`crew_database_schema_unavailable:${error.message}`); }
  finally { db.close(); }
  return result;
}

function agentFor(session, workspace) {
  const exact = workspace.agents.find(a => a.session_id && (a.session_id === session.session_id || path.basename(session.file).includes(a.session_id)));
  if (exact) return exact;
  const declared = session.private_agent ?? session.prompt_agent;
  if (declared) return workspace.agents.find(a => a.id === declared) ?? { id: declared, role: null };
  const cwd = session.cwd;
  const byCwd = cwd ? workspace.agents.filter(a => a.workdir === cwd) : [];
  // The workspace root is often shared by historical members. Its current
  // occupant is not proof of who authored a historical session.
  if (byCwd.length === 1 && cwd !== workspace.workspace) return byCwd[0];
  const needle = path.join(workspace.state_dir, 'worktrees') + path.sep;
  if (cwd?.startsWith(needle)) return { id: cwd.slice(needle.length).split(path.sep)[0], role: null };
  // Claude project directories encode the working directory with '-' in
  // place of separators. Exact current workdirs disambiguate unusual ids.
  const parent = path.dirname(session.file);
  const encodedMatches = workspace.agents.filter(a => a.workdir && a.workdir !== workspace.workspace && path.basename(parent) === encodeProject(a.workdir));
  if (encodedMatches.length === 1) return encodedMatches[0];
  const encoded = workspace.state_dir.replace(/[^a-zA-Z0-9]/g, '-') + '-worktrees-';
  const encodedAt = parent.indexOf(encoded);
  if (encodedAt !== -1) return { id: parent.slice(encodedAt + encoded.length).split(path.sep)[0], role: null };
  return null;
}

const metrics = ['input_total_tokens', 'cache_read_tokens', 'cache_creation_tokens', 'uncached_input_tokens', 'uncached_tail_tokens', 'output_tokens', 'cache_write_5m_tokens', 'cache_write_1h_tokens'];
export function summarize(requests) {
  const out = { requests: requests.length, measured_requests: 0, missing_metric_requests: 0 };
  for (const key of metrics) {
    const known = requests.filter(r => r.counter_valid !== false && r[key] !== null && r[key] !== undefined);
    out[key] = known.length ? known.reduce((s, r) => s + r[key], 0) : null;
    out[`${key}_coverage`] = known.length;
  }
  const measured = requests.filter(r => r.counter_valid !== false && r.input_total_tokens !== null && r.cache_read_tokens !== null);
  out.measured_requests = measured.length; out.missing_metric_requests = requests.length - measured.length;
  out.hit_rate = measured.length ? ratio(measured.reduce((s, r) => s + r.cache_read_tokens, 0), measured.reduce((s, r) => s + r.input_total_tokens, 0)) : null;
  out.request_hit_rate = measured.length ? measured.filter(r => r.cache_read_tokens > 0).length / measured.length : null;
  out.invalid_counter_requests = requests.filter(r => r.counter_valid === false).length;
  return out;
}

/** Evidence association is deterministic and disjoint; it is not causal attribution. */
export function enrich(session, workspace, window = {}) {
  const owner = agentFor(session, workspace);
  const agentId = owner?.id ?? null;
  const events = [...workspace.events, ...workspace.deliveries].filter(e => e.agent_id === agentId).sort((a, b) => a.timestamp_ms - b.timestamp_ms);
  let previous = null;
  for (const r of session.requests) {
    r.gap_ms = previous?.timestamp_ms !== null && previous ? r.timestamp_ms - previous.timestamp_ms : null;
    r.idle_flags = { over_5m: r.gap_ms !== null && r.gap_ms > 300_000, over_30m: r.gap_ms !== null && r.gap_ms > 1_800_000, over_1h: r.gap_ms !== null && r.gap_ms > 3_600_000 };
    r.native_signals = session.signals.filter(e => e.source_line <= r.source_line && e.source_line > (previous?.source_line ?? 0));
    r.crew_events = events.filter(e => e.timestamp_ms <= r.timestamp_ms && e.timestamp_ms > (previous?.timestamp_ms ?? r.timestamp_ms - 300_000));
    const historyChanged = r.native_history_changed;
    const types = new Set([...r.native_signals, ...r.crew_events].map(e => e.type));
    r.association = historyChanged ? 'native_history_changed' : types.has('compaction') || types.has('agent.compacted') ? 'compaction_associated'
      : previous && r.model && previous.model && r.model !== previous.model ? 'model_change_associated'
      : types.has('agent.model_set') || types.has('agent.runtime_set') ? 'model_or_runtime_change_associated'
      : types.has('agent.restart') ? 'restart_associated' : !previous ? 'session_first_request'
      : r.idle_flags.over_1h ? 'idle_over_1h' : r.idle_flags.over_30m ? 'idle_30m_to_1h'
      : r.idle_flags.over_5m ? 'idle_5m_to_30m' : r.preceding_completed_turn_folded === true || types.has('fold') ? 'fold_associated'
      : types.has('agent.reminded') || types.has('silent_reminder') ? 'reminder_associated'
      : types.has('agent.limit_lifted') || types.has('task.resumed') ? 'resume_associated'
      : types.has('agent.unanswered') || types.has('agent.unresponsive') ? 'nudge_associated'
      : types.has('message.delivered') || types.has('delivery') ? 'delivery_associated' : 'normal_append_or_unobserved';
    r.avoidable_miss_tokens = null;
    previous = r;
  }
  const requests = session.requests.filter(r => r.timestamp_ms !== null && r.timestamp_ms >= (window.since ?? -Infinity) && r.timestamp_ms <= (window.until ?? Infinity));
  return { ...session, requests, agent_id: agentId, role: owner?.role ?? null, role_source: owner?.role ? 'current_roster_not_historical_role_proof' : null,
    run: workspace.state_dir, runtime: session.runtime, models: [...new Set(requests.map(r => r.model).filter(Boolean))],
    totals: summarize(requests), coverage: { diagnostics_observed: requests.some(r => r.native_diagnostic), owner_linked: Boolean(owner), native_usage_observed: session.requests.length > 0,
      undated_requests: session.requests.filter(r => r.timestamp_ms === null).length } };
}

function aggregate(sessions, key) {
  const map = new Map();
  for (const s of sessions) { const label = s[key] ?? 'unknown'; if (!map.has(label)) map.set(label, []); map.get(label).push(...s.requests); }
  return [...map].map(([name, requests]) => ({ name, ...summarize(requests) }));
}

export function reportFromSessions(sessions, workspace, window = {}) {
  return combineReports([makeRun(sessions, workspace, window)], window);
}

function makeRun(sessions, workspace, window) {
  const enriched = sessions.map(s => enrich(s, workspace, window));
  return { workspace: workspace.workspace, state_dir: workspace.state_dir, database: workspace.database,
    agents: workspace.agents, sessions: enriched, coverage: { workspace: workspace.coverage,
      current_members_without_measured_session: workspace.agents.filter(a => !enriched.some(s => s.agent_id === a.id && s.totals.measured_requests)).map(a => a.id) } };
}

function combineReports(runs, window) {
  const enriched = runs.flatMap(r => r.sessions), requests = enriched.flatMap(s => s.requests);
  const classes = new Map();
  for (const r of requests) { if (!classes.has(r.association)) classes.set(r.association, []); classes.get(r.association).push(r); }
  const foldCohorts = [true, false].map(folded => {
    const rs = requests.filter(r => r.first_request_in_turn && r.preceding_completed_turn_folded === folded);
    const changed = rs.filter(r => r.native_history_changed);
    const observable = enriched.filter(s => s.coverage.diagnostics_observed).flatMap(s => s.requests).filter(r => r.first_request_in_turn && r.preceding_completed_turn_folded === folded);
    return { preceding_turn_folded: folded, ...summarize(rs), diagnostic_history_changed_requests: changed.length,
      diagnostic_history_changed: summarize(changed), in_sessions_with_any_diagnostic: summarize(observable),
      sessions_with_diagnostics: enriched.filter(s => s.coverage.diagnostics_observed).length, absent_diagnostic_is_unchanged_history: false };
  });
  const eventClasses = [...new Set(requests.flatMap(r => r.crew_events.map(e => e.type)))].sort().map(name => ({ name,
    ...summarize(requests.filter(r => r.crew_events.some(e => e.type === name))), overlap_allowed: true, avoidable_miss_tokens: null }));
  return { schema_version: SCHEMA_VERSION, generated_at: new Date().toISOString(), window,
    methodology: { hit_rate: 'sum(cache_read_tokens) / sum(input_total_tokens) on measured requests',
      claude_input: 'input_tokens + cache_creation_input_tokens + cache_read_input_tokens',
      codex_input: 'input_tokens already includes cached_input_tokens; cumulative counter deltas',
      attribution: 'native client diagnostics or event/time associations only; avoidable loss and invoice cost unknown',
      ttl: '5m/30m/1h idle flags are thresholds, not evidence of configured retention',
      primary_class_priority: 'history diagnostic, compaction, model/runtime, restart, first request, idle, fold, reminder, resume, nudge, delivery, unobserved',
      event_join: 'same identified member; (previous request time, current request time]; first request uses a 5m lookback; deliveries table once, not duplicate delivery event',
      roles: 'current roster role where available; historical removed members have unknown roles', prompt_contents_exported: false,
      costs: 'token volumes only; avoidable loss, native subscription invoice, cache retention setting unknown' },
    workspace: runs[0]?.workspace ?? null, totals: summarize(requests), by_runtime: aggregate(enriched, 'runtime'),
    by_agent: runs.flatMap(run => aggregate(run.sessions, 'agent_id').map(a => ({ ...a, run: run.state_dir }))), by_role: aggregate(enriched, 'role'),
    miss_associations: [...classes].map(([name, rs]) => ({ name, ...summarize(rs), avoidable_miss_tokens: null })).sort((a, b) => (b.uncached_input_tokens ?? 0) - (a.uncached_input_tokens ?? 0)),
    fold_cohorts: foldCohorts, event_associations: eventClasses,
    coverage: { workspace: runs[0]?.coverage.workspace ?? [], current_members_without_measured_session: runs[0]?.coverage.current_members_without_measured_session ?? [] },
    runs: runs.map(({ sessions, ...r }) => ({ ...r, totals: summarize(sessions.flatMap(s => s.requests)) })), sessions: enriched };
}

export function markdown(report) {
  const percent = v => v === null ? 'unknown' : `${(100 * v).toFixed(2)}%`;
  let out = `# Prompt cache baseline\n\nGenerated ${report.generated_at}. Source usage metadata only; no prompt contents.\n\nToken-weighted hit rate: ${percent(report.totals.hit_rate)} across ${report.totals.measured_requests} measured requests. Missing-metric requests: ${report.totals.missing_metric_requests}.\n\n| Runtime | Requests | Total input | Cache read | Cache write | Hit rate |\n|---|---:|---:|---:|---:|---:|\n`;
  for (const r of report.by_runtime) out += `| ${r.name} | ${r.requests} | ${r.input_total_tokens ?? 'unknown'} | ${r.cache_read_tokens ?? 'unknown'} | ${r.cache_creation_tokens ?? 'unknown'} | ${percent(r.hit_rate)} |\n`;
  out += '\n| Agent | Requests | Input | Cache read | Hit rate |\n|---|---:|---:|---:|---:|\n';
  for (const r of report.by_agent) out += `| ${r.name} | ${r.requests} | ${r.input_total_tokens ?? 'unknown'} | ${r.cache_read_tokens ?? 'unknown'} | ${percent(r.hit_rate)} |\n`;
  out += '\n| Role (current roster) | Requests | Input | Cache read | Hit rate |\n|---|---:|---:|---:|---:|\n';
  for (const r of report.by_role) out += `| ${r.name.replaceAll('|', '\\|').replaceAll('\n', ' ')} | ${r.requests} | ${r.input_total_tokens ?? 'unknown'} | ${r.cache_read_tokens ?? 'unknown'} | ${percent(r.hit_rate)} |\n`;
  out += '\n## Event-associated input not read from cache\n\nThese classes partition measured requests; association is not proof of cause or avoidable loss. Native diagnostics are client-declared, not provider diagnostics.\n\n| Class | Requests | Non-read input | Write tokens |\n|---|---:|---:|---:|\n';
  for (const r of report.miss_associations) out += `| ${r.name} | ${r.requests} | ${r.uncached_input_tokens ?? 'unknown'} | ${r.cache_creation_tokens ?? 'unknown'} |\n`;
  out += `\n## Limits\n\nClaude streaming records are deduplicated by message id, last usage wins. Codex repeated cumulative snapshots are ignored; decreases are flagged and only direct last-request usage is retained. Output includes reasoning already; it is never added again. Missing cache fields remain null. Missing diagnostics do not establish unchanged history. Native records do not identify avoidable token loss or subscription invoice cost.\n\nIdle flags at five minutes, thirty minutes and one hour are comparisons, not claims about configured TTL. Explicit Claude write buckets are retained where present.\n\nCurrent members without measured sessions: ${report.coverage.current_members_without_measured_session.join(', ') || 'none'}.\n`;
  return out;
}

async function metadata(file) {
  for await (const line of readline.createInterface({ input: fs.createReadStream(file), crlfDelay: Infinity })) {
    try { const v = JSON.parse(line); if (v.type === 'session_meta') return { session_id: v.payload?.id, cwd: v.payload?.cwd }; if (v.sessionId || v.cwd) return { session_id: v.sessionId, cwd: v.cwd }; } catch {}
  }
  return {};
}

const encodeProject = cwd => cwd.replace(/[^a-zA-Z0-9]/g, '-');
function projectNames(run) {
  const names = [run.workspace, ...run.agents.map(a => a.workdir).filter(Boolean),
    ...run.member_ids.map(id => path.join(run.state_dir, 'worktrees', id))];
  const dirs = path.join(run.state_dir, 'worktrees');
  if (fs.existsSync(dirs)) for (const e of fs.readdirSync(dirs, { withFileTypes: true })) if (e.isDirectory()) names.push(path.join(dirs, e.name));
  return new Set(names.map(encodeProject));
}

function workerCwd(cwd, run) {
  const root = path.join(run.state_dir, 'worktrees') + path.sep;
  return typeof cwd === 'string' && ((cwd.startsWith(root) && !cwd.slice(root.length).includes(path.sep)) || run.agents.some(a => a.workdir === cwd && cwd !== run.workspace));
}

function registeredRuns(home) {
  const dir = path.join(process.env.XDG_STATE_HOME || path.join(home, '.local/state'), 'crew/sessions');
  const found = [];
  if (fs.existsSync(dir)) for (const file of fs.readdirSync(dir)) if (file.endsWith('.json')) {
    try { const v = JSON.parse(fs.readFileSync(path.join(dir, file), 'utf8')); if (typeof v.workspace === 'string' && typeof v.state_dir === 'string') found.push(readWorkspace(v.workspace, v.state_dir)); } catch {}
  }
  return found;
}

export async function collect(options) {
  options = { ...options, until: options.until ?? Date.now() };
  const workspace = readWorkspace(options.workspace), homes = options.home ?? os.homedir(), candidates = [], errors = [];
  const workspaces = [workspace], sourceDirs = new Set([path.join(workspace.state_dir, 'agents')]);
  const claude = process.env.CLAUDE_CONFIG_DIR || path.join(homes, '.claude');
  const codex = process.env.CODEX_HOME || path.join(homes, '.codex');
  sourceDirs.add(path.join(claude, 'projects')); sourceDirs.add(path.join(codex, 'sessions'));
  if (options.allRuns) for (const run of registeredRuns(homes)) if (!workspaces.some(w => w.state_dir === run.state_dir)) workspaces.push(run);
  const explicit = options.sessionFiles ?? [];
  for (const item of explicit) { const at = item.indexOf('='); if (at < 1 || !item.slice(at + 1) || !['claude', 'codex', 'pi'].includes(item.slice(0, at))) throw new Error('--session-file expects claude|codex|pi=PATH'); const file = path.resolve(item.slice(at + 1)); candidates.push({ runtime: item.slice(0, at), file, explicit: true, workspace }); sourceDirs.add(path.dirname(file)); }
  if (!explicit.length) {
    const projects = path.join(claude, 'projects');
    sourceDirs.add(projects); sourceDirs.add(path.join(codex, 'sessions'));
    // Longest path first keeps a nested registered crew run separate from its
    // enclosing team's worktree. Do not include arbitrary benchmark/temp dirs.
    const ordered = [...workspaces].sort((a, b) => b.workspace.length - a.workspace.length);
    const projectsByRun = new Map(ordered.map(w => [w.state_dir, projectNames(w)]));
    if (fs.existsSync(projects)) for (const e of fs.readdirSync(projects, { withFileTypes: true })) if (e.isDirectory()) {
      const run = ordered.find(w => projectsByRun.get(w.state_dir).has(e.name));
      if (run) for (const f of fs.readdirSync(path.join(projects, e.name), { withFileTypes: true })) if (f.isFile() && f.name.endsWith('.jsonl')) candidates.push({ runtime: 'claude', file: path.join(projects, e.name, f.name), workspace: run });
    }
    for (const file of regularFiles(path.join(codex, 'sessions'), p => p.endsWith('.jsonl'))) {
      const m = await metadata(file);
      const run = ordered.find(w => m.cwd === w.workspace || workerCwd(m.cwd, w) || w.agents.some(a => a.session_id && path.basename(file).includes(a.session_id)));
      if (run) candidates.push({ runtime: 'codex', file, workspace: run });
    }
    for (const run of ordered) {
      sourceDirs.add(path.join(run.state_dir, 'agents'));
      const privateAgents = path.join(run.state_dir, 'agents');
      if (!fs.existsSync(privateAgents)) continue;
      for (const a of fs.readdirSync(privateAgents, { withFileTypes: true })) if (a.isDirectory()) {
      const rw = path.join(privateAgents, a.name, 'rw');
      for (const [runtime, dir] of [['claude', path.join(rw, 'home/claude/projects')], ['codex', path.join(rw, 'home/codex/sessions')], ['pi', path.join(rw, 'pi-sessions')], ['pi', path.join(privateAgents, a.name, 'pi-sessions')]]) for (const file of regularFiles(dir, p => p.endsWith('.jsonl'))) candidates.push({ runtime, file, private_agent: a.name, workspace: run });
      }
    }
  }
  const sessions = new Map(workspaces.map(w => [w.state_dir, []])), seen = new Set(), excluded = [];
  for (const candidate of candidates) {
    if (seen.has(candidate.file)) continue; seen.add(candidate.file);
    try {
      const parsed = await readSession(candidate.file, candidate.runtime, options);
      parsed.runtime = candidate.runtime; parsed.private_agent = candidate.private_agent ?? null;
      // A nested registered run may share its cwd/project directory with a
      // member of the enclosing team. Exact native identity wins over cwd.
      const run = candidate.private_agent ? candidate.workspace : workspaces.find(w => w.agents.some(a => a.session_id && (a.session_id === parsed.session_id || path.basename(candidate.file).includes(a.session_id))))
        ?? workspaces.find(w => parsed.prompt_agent && w.agents.some(a => a.id === parsed.prompt_agent && a.workdir === parsed.cwd)) ?? candidate.workspace;
      const known = run.agents.some(a => a.session_id && (a.session_id === parsed.session_id || path.basename(candidate.file).includes(a.session_id)));
      const inWorkerDir = workerCwd(parsed.cwd, run)
        || (projectNames(run).has(path.basename(path.dirname(candidate.file))) && path.basename(path.dirname(candidate.file)) !== encodeProject(run.workspace));
      if (candidate.explicit || known || candidate.private_agent || parsed.crew_prompt || inWorkerDir) sessions.get(run.state_dir).push(parsed);
      else excluded.push({ file: candidate.file, reason: 'root_session_not_identified_as_crew', runtime: candidate.runtime });
    } catch (error) { errors.push({ file: candidate.file, error: error.message }); }
  }
  const report = combineReports(workspaces.map(w => makeRun(sessions.get(w.state_dir), w, { since: options.since ?? null, until: options.until ?? null })), { since: options.since ?? null, until: options.until ?? null });
  report.coverage.discovery_errors = errors; report.coverage.discovered_files = candidates.length;
  report.coverage.excluded_files = excluded; report.coverage.source_dirs = [...sourceDirs];
  report.coverage.source_files = workspaces.flatMap(w => ['crew.db', 'crew.db-wal', 'crew.db-shm'].map(f => path.join(w.state_dir, f)));
  report.coverage.discovery_scope = options.allRuns ? 'explicit workspace plus local crew registry; historical sessions in their native roots' : 'explicit workspace only';
  report.coverage.native_sources = { claude: 'operator/private home projects JSONL', codex: 'operator/private home rollout JSONL', pi: 'private rw/pi-sessions JSONL' };
  return report;
}

function canonical(file) {
  let cursor = path.resolve(file), suffix = [];
  while (!fs.existsSync(cursor)) { const parent = path.dirname(cursor); if (parent === cursor) break; suffix.unshift(path.basename(cursor)); cursor = parent; }
  return path.join(fs.realpathSync(cursor), ...suffix);
}

/** Validate both outputs before any mkdir/write, including aliases and links. */
export function guardOutputs(outputs, report) {
  const inputFiles = [...report.sessions.map(s => s.file), ...(report.coverage.source_files ?? [])];
  const sources = inputFiles.map(canonical);
  const dirs = report.coverage.source_dirs.map(canonical);
  const aliases = new Set();
  for (const file of outputs) {
    const out = canonical(file);
    if (aliases.has(out)) throw new Error('Outputs must be distinct');
    aliases.add(out);
    const sameInode = fs.existsSync(file) && inputFiles.filter(f => fs.existsSync(f)).some(f => { const a = fs.statSync(file), b = fs.statSync(f); return a.ino === b.ino && a.dev === b.dev; });
    if (sources.includes(out) || sameInode || dirs.some(dir => out === dir || out.startsWith(dir + path.sep))) throw new Error('Refusing to overwrite a measurement source or write in its directory');
  }
}

export async function main(argv = process.argv.slice(2)) {
  const options = { workspace: process.cwd(), sessionFiles: [] };
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i];
    if (key === '--all-runs') { options.allRuns = true; continue; }
    if (key === '--help') { process.stdout.write('Usage: node scripts/cache/baseline.mjs --workspace PATH [--all-runs] [--session-file claude|codex|pi=PATH] [--since ISO --until ISO] [--output JSON --markdown MD]\nNode 24 required. Reads native usage and metadata only. Outputs must be outside native source/state directories.\n'); return; }
    const value = argv[++i];
    if (!value) throw new Error(`Missing value for ${key}`);
    if (key === '--workspace') options.workspace = path.resolve(value);
    else if (key === '--output') options.output = path.resolve(value);
    else if (key === '--markdown') options.markdown = path.resolve(value);
    else if (key === '--since' || key === '--until') { const n = time(value); if (!Number.isFinite(n)) throw new Error(`Invalid ${key}`); options[key.slice(2)] = n; }
    else if (key === '--session-file') options.sessionFiles.push(value);
    else throw new Error(`Unknown argument: ${key}`);
  }
  if (options.since > options.until) throw new Error('--since must not exceed --until');
  const report = await collect(options);
  guardOutputs([options.output, options.markdown].filter(Boolean), report);
  for (const output of [options.output, options.markdown].filter(Boolean)) {
    fs.mkdirSync(path.dirname(output), { recursive: true });
  }
  if (options.output) fs.writeFileSync(options.output, `${JSON.stringify(report, null, 2)}\n`);
  else process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  if (options.markdown) fs.writeFileSync(options.markdown, markdown(report));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) main().catch(error => { console.error(error.message); process.exitCode = 1; });
