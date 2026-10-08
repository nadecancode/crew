#!/usr/bin/env node
/**
 * Candidate low-value turns and tool calls in crew member sessions, with the
 * requests associated with them (an upper bound, not avoidable cost), from
 * native session files only. Read-only sibling of baseline.mjs: the cohort, session discovery,
 * agent identity and per-request usage come from baseline.mjs's collect(); this
 * script then re-reads each cohort file (bounded to the byte size baseline
 * read) for tool calls, tool results and delivered inputs.
 */
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import readline from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { collect, guardOutputs } from './baseline.mjs';

export const SCHEMA_VERSION = 1;
const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '../..');
export const DEFAULTS = { window: 3, shortReplyChars: 300 };

// Crew tool families. Names are the crew MCP tool names without any prefix.
export const CREW_READ_TOOLS = new Set(['agent_inbox', 'tasks_list', 'task_get', 'agents_list']);
const REPEAT_READ_TOOLS = new Set(['tasks_list', 'task_get', 'agents_list', 'git_status', 'git_diff', 'usage_status', 'agent_inbox']);
const LIGHT_TOOLS = new Set(['agent_inbox', 'agent_status', 'task_update']);
const CREW_NON_MUTATING = new Set(['agent_inbox', 'tasks_list', 'task_get', 'agents_list', 'git_status', 'git_diff', 'usage_status', 'agent_send', 'operator_ask']);
const FILE_MUTATORS = new Set(['Bash', 'bash', 'Edit', 'MultiEdit', 'Write', 'NotebookEdit', 'CommandExecution', 'FileChange', 'apply_patch', 'shell', 'exec_command']);
const CODEX_INNER_ITEMS = new Set(['McpToolCall', 'CommandExecution', 'FileChange', 'ImageView', 'WebSearch', 'Extension']);

// ---------------------------------------------------------------- classifiers

/** Result text of the crew `agent_inbox` tool. */
export function inboxResultKind(text) {
  const t = (text ?? '').trim();
  // crew may append guidance after the sentence ("No new messages. One that arrives later…").
  if (/^(No new messages\.|No unread messages|0 message\(s\))/i.test(t)) return 'empty';
  if (/^\d+ message\(s\):/.test(t) || /^--- .* from \S+ (to \S+ )?· type \w+ · id /.test(t)) return 'messages';
  if (/^no message '[^']*' sent or received by you/.test(t)) return 'id_not_found';
  if (/incompatible crew protocol|^Error:|^error:/.test(t)) return 'error';
  if (/^<persisted-output>/.test(t)) return 'messages_persisted';
  return 'other';
}

/** Entries of an agent_inbox result: "--- 12:00:00.1 from a [to b] · type t · id m-…". */
export const inboxEntries = text => [...(text ?? '').matchAll(/^--- \S+ from (\S+)(?: to (\S+))? · type (\w+) · id (m-[0-9a-f]+)/gm)].map(m => ({ from: m[1], to: m[2] ?? null, type: m[3], id: m[4] }));
export const messageIds = text => [...new Set([...(text ?? '').matchAll(/\bid (m-[0-9a-f]{6,})\b/g)].map(m => m[1]))];

const NOTICES = [
  [/^\[crew\] Your last turn ended without a tool call/, 'notice:no_tool_call'],
  [/^(\[crew\] )?Your conversation was just compacted/, 'notice:compacted'],
  [/^(\[crew\] )?.*\bfast-forward/i, 'notice:fast_forward'],
  [/^\[crew\] Team messages came as your last turn ended/, 'notice:team_messages_came'],
  [/^\[crew\] Reminder: task/, 'notice:task_reminder'],
  [/^\[crew\] The usage limit that stopped you has lifted/, 'notice:usage_lifted'],
  [/^\[crew\] You are online as [\w-]+\. Nothing is assigned/, 'notice:online_idle'],
  [/^\[crew\] Your session was restarted/, 'notice:restarted'],
  [/^\[crew\] .*\bweek has reset/, 'notice:model_restored'],
  [/^\[crew\] You take over/, 'handover'],
  [/^\[crew\] (You are online|The team is online)/, 'kickoff'],
  // Daemon notices wrapped as status messages (sender shown as scheduler).
  [/^Agent [\w-]+ is unresponsive/, 'notice:unresponsive'],
  [/refused its turn|cannot act on your message .*refus/, 'notice:refusal'],
  [/^Card [\w-]+ revised by/, 'notice:card_revised'],
  [/^All \d+ tasks are done/, 'notice:all_done'],
  [/hit a usage limit/, 'notice:usage_limit'],
  [/^Task [\w-]+ \(/, 'notice:task_event'],
];
const noticeClass = line => NOTICES.find(([re]) => re.test(line))?.[1] ?? null;
/** Short, groupable head of a wrapped notice body: up to the first clause break. */
const noticeHead = line => {
  const s = normalizeOpening(line.replace(/^Agent [\w-]+ is/, 'Agent <agent> is').replace(/^[\w-]+(?='s model| cannot act)/, '<agent>').replace(/^Card [\w-]+ revised/, 'Card <id> revised').replace(/^Task [\w-]+ \([^)]*\)/, 'Task <id> (…)').replace(/ (was \w+ by) [\w-]+/, ' $1 <agent>'));
  const m = s.slice(12).search(/[:;,.(]/); return (m === -1 ? s.slice(0, 60) : s.slice(0, 12 + m)).trim();
};

/** Collapse ids, names, numbers and quoted titles so openings group. */
export function normalizeOpening(s) {
  return s.replace(/\b([mw])-[0-9a-f]{6,}\b/g, '$1-X').replace(/\b[0-9a-f]{7,40}\b/g, '<sha>')
    .replace(/"[^"]*"/g, '"…"').replace(/\(id [^)]*\)/g, '(id …)')
    .replace(/\b(online as|work of|from|You are still) [\w-]+/g, '$1 <agent>')
    .replace(/\d+(\.\d+)?/g, 'N').trim().slice(0, 90);
}

// crew's clip()/clip_for_argv suffix: the full text is only in agent_inbox.
const TRUNCATED = '… (truncated';
const mentionedIds = text => [...new Set([...(text ?? '').matchAll(/\bm-[0-9a-f]{6,}\b/g)].map(m => m[0]))];

/**
 * Classify a delivered input (user turn or mid-turn fold) by its wrapper and
 * first line only. `ids` are message ids whose full body this input
 * delivered (header id of a non-empty, untruncated "Message from" body; each
 * untruncated block of a "N new messages" batch). Ids merely mentioned
 * (notices, truncated blocks, quoted references) are `refs`, never
 * deliveries. `body` is true when every delivered message arrived in full
 * (so the inbox cannot hold anything more from this input).
 */
export function classifyInput(text) {
  if (typeof text !== 'string' || !text.startsWith('[crew]')) return { crew: false, cls: 'non_crew', opening: null, ids: [], refs: [], body: false, delivery: false };
  const first = text.split('\n', 1)[0];
  const wrap = text.match(/^\[crew\] Message from (\S+) \(type (\w+), id ([\w-]+)\):[ \t]*\n*([\s\S]*)$/);
  if (wrap) {
    const [, sender, type, id, body] = wrap;
    const bodyFirst = body.split('\n', 1)[0];
    const notice = noticeClass(bodyFirst) ?? (bodyFirst.startsWith('[crew]') ? 'notice:other' : null);
    const full = body.trim().length > 0 && !body.includes(TRUNCATED);
    return { crew: true, cls: notice ?? `message:${type}`, type, sender, entries: [{ from: sender, type, id }], ids: full ? [id] : [],
      refs: mentionedIds(body).filter(x => x !== id).concat(full ? [] : [id]), body: full, truncated: body.includes(TRUNCATED), delivery: true,
      opening: notice ? `[crew] Message from <agent> (type ${type}): ${noticeHead(bodyFirst)}` : `[crew] Message from <agent> (type ${type})` };
  }
  const batch = first.match(/^\[crew\] (\d+) new messages:/);
  if (batch) {
    const heads = [...text.matchAll(/^\(\d+\) from (\S+) · type (\w+) · id ([\w-]+)/gm)];
    const entries = heads.map((h, i) => {
      const block = text.slice(h.index, heads[i + 1]?.index ?? text.length);
      return { from: h[1], type: h[2], id: h[3], full: !block.includes(TRUNCATED) };
    });
    const types = [...new Set(entries.map(e => e.type))].sort();
    const ids = entries.filter(e => e.full).map(e => e.id);
    return { crew: true, cls: types.length && types.every(t => t === 'status') ? 'batch:status' : 'batch', types, entries, ids,
      refs: mentionedIds(text).filter(x => !ids.includes(x)), body: entries.length > 0 && ids.length === entries.length, truncated: ids.length < entries.length,
      delivery: true, opening: '[crew] N new messages:' };
  }
  return { crew: true, cls: noticeClass(first) ?? 'notice:other', ids: [], refs: mentionedIds(text), body: false, delivery: false, opening: normalizeOpening(first) };
}

/** Inputs that need no action unless the agent has something new to say. */
export const isNoActionNotice = info => info.crew && (info.cls.startsWith('notice:') || info.cls === 'message:status' || info.cls === 'batch:status');

export function toolName(name) {
  const m = typeof name === 'string' && name.match(/^(?:mcp__crew__|crew\.)(\w+)$/);
  return m ? { crew: true, name: m[1] } : { crew: false, name: String(name) };
}

export function callCategory(call) {
  if (!call.crew) return 'non_crew';
  if (CREW_READ_TOOLS.has(call.name)) return 'crew_read';
  if (isStatusCall(call)) return 'crew_status';
  return 'crew_other';
}
export const isStatusCall = c => c.crew && (c.name === 'agent_status' || c.name === 'task_update' || (c.name === 'agent_send' && c.args?.type === 'status'));

const stable = v => Array.isArray(v) ? `[${v.map(stable).join(',')}]` : v && typeof v === 'object' ? `{${Object.keys(v).sort().map(k => `${JSON.stringify(k)}:${stable(v[k])}`).join(',')}}` : JSON.stringify(v ?? null);
const parseJson = s => { if (typeof s !== 'string') return s ?? {}; try { return JSON.parse(s); } catch { return { raw: s }; } };
const blockText = c => typeof c === 'string' ? c : Array.isArray(c) ? c.map(b => typeof b === 'string' ? b : b?.type === 'text' || b?.type === 'input_text' || b?.type === 'output_text' ? b.text ?? '' : b?.type === 'image' ? '[image]' : '').join('') : c == null ? '' : JSON.stringify(c);
const bytes = s => Buffer.byteLength(s ?? '', 'utf8');

// ------------------------------------------------------------ event extraction

/**
 * Turn native records into one ordered stream of input/model/call/result
 * events. records: [{ value, line }] in file order.
 */
export function extractEvents(runtime, records) {
  const events = [];
  if (runtime === 'claude' || runtime === 'pi') {
    const seen = new Set();
    for (const { value: v, line } of records) {
      if (v.isSidechain) continue;
      const m = v.message, c = m?.content;
      const role = v.type === 'message' ? m?.role : v.type;
      const ts = Date.parse(v.timestamp);
      // Background task completion: <task-notification> names the launching tool_use id.
      if (role === 'user' || v.type === 'attachment') {
        const raw = role === 'user' ? (typeof c === 'string' ? c : Array.isArray(c) && !c.some(b => b?.type === 'tool_result') ? blockText(c) : '') : JSON.stringify(v.attachment ?? '');
        if (raw.includes('<task-notification>')) for (const x of raw.matchAll(/<tool-use-id>([\w-]+)<\/tool-use-id>/g)) events.push({ ts, kind: 'bg_done', line, ref: x[1] });
        if (v.type === 'attachment') continue;
      }
      // A completed turn (Claude Code writes one per finished turn).
      if (v.type === 'system' && v.subtype === 'turn_duration') { events.push({ ts, kind: 'turn_end', line }); continue; }
      if (role === 'user') {
        if (Array.isArray(c) && c.some(b => b?.type === 'tool_result')) {
          for (const b of c) if (b?.type === 'tool_result') events.push({ ts, kind: 'result', line, ref: b.tool_use_id, text: blockText(b.content), error: b.is_error === true });
          continue;
        }
        if (v.isCompactSummary) continue;
        const text = typeof c === 'string' ? c : Array.isArray(c) ? c.filter(b => b?.type === 'text').map(b => b.text).join('\n') : null;
        if (!text) continue;
        if (v.isMeta) { if (text.startsWith('[crew]')) events.push({ ts, kind: 'input', line, text, fold: true }); continue; }
        events.push({ ts, kind: 'input', line, text, fold: false });
      } else if (role === 'toolResult') {
        events.push({ ts, kind: 'result', line, ref: m.toolCallId, text: blockText(c), error: m.isError === true });
      } else if (role === 'assistant') {
        if (m?.model === '<synthetic>') continue;
        const id = m?.id ?? v.id ?? `line:${line}`;
        if (!seen.has(id)) { seen.add(id); events.push({ ts, kind: 'model', line, ref: id }); }
        if (Array.isArray(c)) for (const b of c) {
          if (b?.type === 'tool_use' || b?.type === 'toolCall') {
            const args = b.input ?? b.arguments ?? {};
            const bg = ['Bash', 'Agent', 'Monitor'].includes(b.name) ? b.name : null; // confirmed by its result text
            events.push({ ts, kind: 'call', line, id: b.id, ...toolName(b.name), args, bg });
          }
          else if (b?.type === 'text' && b.text) events.push({ ts, kind: 'text', line, text: b.text });
        }
      }
    }
    return events;
  }
  const items = new Set();
  let lastModel = false;
  for (const { value: v, line } of records) {
    const p = v.payload ?? {};
    const ts = Date.parse(v.timestamp); // every event carries its record time (window filtering)
    if (v.type === 'event_msg' && p.type === 'task_started') { events.push({ ts, kind: 'turn_start', line, ref: p.turn_id ?? null }); lastModel = false; continue; }
    if (v.type === 'event_msg' && p.type === 'task_complete') { if (p.error) events.push({ ts, kind: 'turn_error', line, text: String(p.error.message ?? p.error) }); events.push({ ts, kind: 'turn_end', line }); continue; }
    if (v.type === 'response_item') {
      if (p.type === 'message' && p.role === 'user') {
        const text = blockText(p.content);
        if (text && !text.startsWith('<')) { events.push({ ts, kind: 'input', line, ts: Date.parse(v.timestamp), text, fold: null }); lastModel = false; }
        continue;
      }
      const isModel = ['reasoning', 'custom_tool_call', 'function_call', 'local_shell_call', 'web_search_call'].includes(p.type) || (p.type === 'message' && p.role === 'assistant');
      if (isModel) {
        if (!lastModel) events.push({ ts, kind: 'model', line, ref: `line:${line}` });
        lastModel = true;
        if (p.type === 'message') { const t = blockText(p.content); if (t) events.push({ ts, kind: 'text', line, text: t }); }
        else if (p.type === 'function_call') events.push({ ts, kind: 'call', line, id: p.call_id, ...toolName(p.namespace ? `${p.namespace}.${p.name}` : p.name), args: parseJson(p.arguments) });
        else if (p.type === 'custom_tool_call') events.push({ ts, kind: 'wrapper', line, id: p.call_id, name: p.name });
        continue;
      }
      if (p.type === 'function_call_output') { events.push({ ts, kind: 'result', line, ref: p.call_id, text: blockText(p.output?.content ?? p.output) }); lastModel = false; }
      else if (p.type === 'custom_tool_call_output') { events.push({ ts, kind: 'output', line, ref: p.call_id }); lastModel = false; }
      continue;
    }
    if (v.type === 'event_msg' && p.type === 'item_completed' && CODEX_INNER_ITEMS.has(p.item?.type)) {
      const it = p.item;
      if (it.id) { if (items.has(it.id)) continue; items.add(it.id); }
      if (it.type === 'McpToolCall') {
        const text = it.result ? blockText(it.result.content) : typeof it.error === 'string' ? it.error : it.error?.message ?? '';
        events.push({ ts, kind: 'call', line, id: it.id, inner: true, crew: it.server === 'crew', name: it.server === 'crew' ? it.tool : `${it.server}.${it.tool}`,
          args: parseJson(it.arguments), text, error: it.status !== 'completed' || it.result?.isError === true });
      } else events.push({ ts, kind: 'call', line, id: it.id, inner: true, crew: false, name: it.type, args: {} });
    }
  }
  return events;
}

// ------------------------------------------------------------------- analysis

const firstIndexAtLeast = (sorted, key, value) => { let lo = 0, hi = sorted.length; while (lo < hi) { const mid = (lo + hi) >> 1; if (key(sorted[mid]) < value) lo = mid + 1; else hi = mid; } return lo; };

/** Subject time filter: inclusive [since, until] like baseline; unknown time is outside a set window. */
export const inWindow = (ts, { since = null, until = null } = {}) => (since == null && until == null) || (Number.isFinite(ts) && ts >= (since ?? -Infinity) && ts <= (until ?? Infinity));
/** A line opening with one or more @names and a colon: crew forwards that part as a message. */
export const hasExplicitMention = text => /^[ \t]*@[\w-]+(?:[ \t,]+@[\w-]+)*[ \t]*:/m.test(text ?? '');

/**
 * Pure per-session analysis. session: { runtime, file, agent_id, requests }
 * where requests are ALL baseline.mjs request observations of the session
 * (source_line + usage + timestamp_ms), unfiltered, so that calls join their
 * exact native consuming request before any window applies. options.since /
 * options.until select subjects (calls, turns, inputs) and associated
 * requests; state before the window still feeds delivered ids and windows.
 * Returns findings (one per candidate call / turn) with associated requests.
 */
export function analyzeSession(session, events, options = {}) {
  const { window = DEFAULTS.window, shortReplyChars = DEFAULTS.shortReplyChars } = options;
  const span = { since: options.since ?? null, until: options.until ?? null };
  const inSpan = ts => inWindow(ts, span);
  // Nothing written after the cutoff is known at the cutoff: drop later or
  // undated events (tool results included) before linking, so appending
  // records cannot change a result. Earlier context and the request list,
  // which joins by source line, stay whole.
  if (span.until != null) events = events.filter(e => Number.isFinite(e.ts) && e.ts <= span.until);
  const runtime = session.runtime, agent = session.agent_id ?? 'unknown', file = session.file;
  const requests = [...(session.requests ?? [])].sort((a, b) => a.source_line - b.source_line);
  const reqIn = r => inSpan(r.timestamp_ms);
  const keep = pairs => pairs.filter(([r]) => reqIn(r));
  const calls = events.filter(e => e.kind === 'call');
  const byId = new Map(calls.filter(c => c.id).map(c => [c.id, c]));
  // Link results: Claude tool_result ids; Codex function outputs; Codex inner
  // items return with the next exec output.
  let pendingInner = [];
  for (const e of events) {
    if (e.kind === 'result') { const c = byId.get(e.ref); if (c) { c.resultLine = e.line; if (c.text === undefined) c.text = e.text; c.error ||= e.error; } }
    else if (e.kind === 'call' && e.inner) pendingInner.push(e);
    else if (e.kind === 'output') { for (const c of pendingInner) c.resultLine = e.line; pendingInner = []; }
  }
  for (const c of pendingInner) c.resultLine ??= c.line;
  const models = events.filter(e => e.kind === 'model');
  // Turns: Claude starts one per non-meta user input; Codex per task_started.
  const turns = [];
  let cur = null;
  const newTurn = (e, input) => { cur = { index: turns.length, startLine: e.line, ts: e.ts, input, folds: [], calls: [], wrappers: 0, wrappersIn: 0, models: 0, textChars: 0, texts: [], requests: [], evs: [] }; turns.push(cur); };
  for (const e of events) {
    if (e.kind === 'turn_start') newTurn(e, null);
    else if (e.kind === 'input') {
      if (runtime === 'codex') {
        if (cur && !cur.input && !cur.models) { cur.input = e; e.fold = false; if (!Number.isFinite(cur.ts)) cur.ts = e.ts; }
        else if (!cur) { newTurn(e, e); e.fold = false; }
        else { e.fold = true; cur.folds.push(e); }
      } else if (e.fold) { if (!cur) newTurn(e, null); cur.folds.push(e); }
      else newTurn(e, e);
      e.turn = cur.index;
    } else {
      if (!cur) newTurn(e, null);
      if (['model', 'call', 'wrapper', 'text', 'turn_error', 'turn_end'].includes(e.kind)) cur.evs.push(e);
      if (e.kind === 'model') { cur.models++; e.turn = cur.index; }
      else if (e.kind === 'call') { cur.calls.push(e); e.turn = cur.index; }
      else if (e.kind === 'wrapper') { cur.wrappers++; if (inSpan(e.ts)) cur.wrappersIn++; }
      else if (e.kind === 'text') { cur.textChars += e.text.length; cur.texts.push(e); }
      else if (e.kind === 'turn_error') cur.error = e.text;
    }
  }
  // A turn's subject time is its start (Codex task_started) or its input.
  for (const t of turns) { t.inSpan = inSpan(t.input?.ts ?? t.ts); }
  for (const r of requests) { const i = firstIndexAtLeast(turns, t => t.startLine, r.source_line + 1) - 1; if (i >= 0) turns[i].requests.push(r); }
  // With --until set, a measured turn is seen only up to the cutoff: its
  // events from the first one not provably at or before `until` (later or
  // undated) are dropped, earlier context is kept, so appending records after
  // the cutoff never changes the result. A turn with no end marker (Claude
  // turn_duration, Codex task_complete) and no next turn start by the cutoff
  // is `partial`.
  const startTs = t => t.input?.ts ?? t.ts;
  for (const t of turns) {
    if (span.until == null) { t.v = { calls: t.calls, texts: t.texts, models: t.models, wrappers: t.wrappers, textChars: t.textChars, requests: t.requests, error: t.error, partial: false }; continue; }
    const cut = t.evs.findIndex(e => !(Number.isFinite(e.ts) && e.ts <= span.until));
    const evs = cut === -1 ? t.evs : t.evs.slice(0, cut), cutLine = cut === -1 ? Infinity : t.evs[cut].line;
    const texts = evs.filter(e => e.kind === 'text'), next = turns[t.index + 1];
    const ended = evs.some(e => e.kind === 'turn_end') || (next !== undefined && Number.isFinite(startTs(next)) && startTs(next) <= span.until);
    t.v = { calls: evs.filter(e => e.kind === 'call'), texts, models: evs.filter(e => e.kind === 'model').length, wrappers: evs.filter(e => e.kind === 'wrapper').length,
      textChars: texts.reduce((a, e) => a + e.text.length, 0), requests: t.requests.filter(r => r.source_line < cutLine), error: evs.find(e => e.kind === 'turn_error')?.text, partial: !ended };
  }
  // Issuing model response, and the exact consuming request: the request
  // recorded between the next model response after the result and the model
  // response after that (by source line); otherwise none.
  const modelLines = models.map(m => m.line);
  for (const c of calls) {
    c.modelIndex = firstIndexAtLeast(modelLines, x => x, c.line + 1) - 1;
    const k = firstIndexAtLeast(modelLines, x => x, (c.resultLine ?? c.line) + 1);
    const next = models[k], after = models[k + 1];
    const r = next ? requests[firstIndexAtLeast(requests, x => x.source_line, next.line)] : null;
    c.consumer = r && (!after || r.source_line < after.line) ? r : null;
  }
  const siblings = new Map();
  for (const c of calls) if (c.consumer) siblings.set(c.consumer, (siblings.get(c.consumer) ?? 0) + 1);
  const callReqs = c => c.consumer ? keep([[c.consumer, 1 / siblings.get(c.consumer)]]) : [];
  // Candidate-call association: a call whose consuming request consumed only
  // light crew calls (agent_inbox / agent_status / task_update) is associated
  // with that whole request ("alone"; an upper bound). A call bundled with
  // other real tool work only added its result bytes ("bundled").
  const group = new Map();
  for (const c of calls) if (c.consumer) { if (!group.has(c.consumer)) group.set(c.consumer, []); group.get(c.consumer).push(c); }
  const association = c => {
    if (!c.consumer) return { bundle: 'no_consumer', requests: [] };
    return group.get(c.consumer).every(x => x.crew && LIGHT_TOOLS.has(x.name)) ? { bundle: 'alone', requests: keep([[c.consumer, 1]]) } : { bundle: 'bundled', requests: [] };
  };

  const findings = [];
  const base = { runtime, agent, file };
  const push = (pattern, f) => findings.push({ pattern, ...base, ...f });
  for (const t of turns) for (const e of [t.input, ...t.folds]) if (e) e.info = classifyInput(e.text);

  // Delivered-in-band ids (full bodies only), and inbox calls within N model
  // responses after a delivery whose messages all arrived in full.
  const inputs = turns.flatMap(t => [t.input, ...t.folds]).filter(Boolean).sort((a, b) => a.line - b.line);
  const windowOf = new Map();
  for (const d of inputs) {
    if (!d.info.crew || !d.info.body) continue;
    const start = firstIndexAtLeast(modelLines, x => x, d.line + 1);
    for (let k = start; k < Math.min(models.length, start + window); k++) {
      if (models[k].turn !== d.turn) break;
      if (!windowOf.has(k)) windowOf.set(k, { d, round: k - start + 1 });
    }
  }
  const delivered = new Set();
  let inputCursor = 0;
  const windowCounts = new Map(), inboxSeen = new Set(), inboxNovelty = [];
  for (const c of [...calls].sort((a, b) => a.line - b.line)) {
    while (inputCursor < inputs.length && inputs[inputCursor].line < c.line) { for (const id of inputs[inputCursor].info.ids) delivered.add(id); inputCursor++; }
    if (!(c.crew && c.name === 'agent_inbox')) continue;
    const kind = inboxResultKind(c.text);
    const ids = messageIds(c.text);
    const duplicateOnly = ids.length > 0 && ids.every(id => delivered.has(id));
    const subject = inSpan(c.ts);
    c.inbox = { kind, ids: ids.length, duplicateOnly, inSpan: subject };
    if (kind === 'messages') {
      const entries = inboxEntries(c.text);
      const newInband = entries.filter(e => !delivered.has(e.id)), newAny = newInband.filter(e => !inboxSeen.has(e.id));
      if (subject) inboxNovelty.push({ runtime, agent, entries: entries.length, new_inband: newInband.map(e => ({ type: e.type, from: e.from })), new_any: newAny.length, by_id: Boolean(c.args?.id) });
      for (const e of entries) inboxSeen.add(e.id);
    }
    if (!subject) continue;
    const assoc = association(c);
    const f = { line: c.line, tool: 'agent_inbox', args: c.args, result: c.text ?? '', result_bytes: bytes(c.text), requests: assoc.requests, bundle: assoc.bundle, inbox_kind: kind };
    if (kind === 'empty') push('inbox_empty', { ...f, sub: assoc.bundle });
    if (duplicateOnly) push('inbox_duplicate_only', { ...f, sub: assoc.bundle });
    const w = windowOf.get(c.modelIndex), d = w?.d;
    if (d) {
      const sub = kind === 'empty' ? 'empty' : duplicateOnly ? 'duplicate_only' : kind === 'messages' || kind === 'messages_persisted' ? 'new_messages' : kind;
      const key = d.line; windowCounts.set(key, (windowCounts.get(key) ?? 0) + (sub === 'empty' || sub === 'duplicate_only' ? 1 : 0));
      push('inbox_after_delivery', { ...f, sub: `${sub} ${assoc.bundle}`, candidate: sub === 'empty' || sub === 'duplicate_only', delivery_line: d.line, delivery_opening: d.info.opening, delivery_snippet: d.text.slice(0, 200), round: w.round });
    }
  }
  for (const f of findings) if (f.pattern === 'inbox_after_delivery') f.score = windowCounts.get(f.delivery_line) ?? 0;

  // Repeated reads with byte-identical results and no intervening own
  // state change (crew mutators reset everything; file mutators reset git).
  const last = new Map();
  let crewEpoch = 0, fileEpoch = 0;
  for (const c of [...calls].sort((a, b) => a.line - b.line)) {
    if (c.crew && !CREW_NON_MUTATING.has(c.name)) crewEpoch++;
    if (FILE_MUTATORS.has(c.name) || (c.crew && /^git_(commit|merge|cherry_pick)$/.test(c.name))) fileEpoch++;
    if (!c.crew || !REPEAT_READ_TOOLS.has(c.name) || c.text === undefined || c.error) continue;
    if (c.name === 'agent_inbox' && !c.args?.id) continue;
    const key = `${c.name}:${stable(c.args)}`;
    const epoch = /^git_/.test(c.name) ? `${crewEpoch}/${fileEpoch}` : `${crewEpoch}`;
    const prev = last.get(key);
    if (prev && prev.text === c.text && prev.epoch === epoch && inSpan(c.ts)) {
      push('repeated_read', { line: c.line, previous_line: prev.line, tool: c.name, args: c.args, result: c.text, result_bytes: bytes(c.text), ...(({ requests, bundle }) => ({ requests, bundle, sub: `${c.name} ${bundle}` }))(association(c)) });
    }
    last.set(key, { text: c.text, epoch, line: c.line });
  }

  // Background work (Claude): Bash run_in_background / async Agent / Monitor
  // launches confirmed by their result, until a <task-notification> names them.
  const bgDone = new Map(events.filter(e => e.kind === 'bg_done').map(e => [e.ref, e.line]));
  const bgLaunches = calls.filter(c => c.bg && /Command running in background with ID|Async agent|Monitor started/.test(c.text ?? ''));
  const runningAt = (line, ts) => {
    const open = bgLaunches.filter(c => c.line < line && !(bgDone.get(c.id) < line));
    return { any: open.length, recent: open.filter(c => Number.isFinite(ts) && Number.isFinite(c.ts) && ts - c.ts <= 2 * 3600_000).length, kinds: [...new Set(open.map(c => c.bg))] };
  };

  // Turn-level patterns (subject: the turn's start/input inside the window).
  const turnStats = [];
  for (const turn of turns) {
    if (!turn.inSpan) continue;
    // Measure the turn as seen up to the cutoff (identical to the full turn without --until).
    const t = { ...turn, ...turn.v };
    const info = t.input?.info ?? { crew: false, cls: t.input ? 'non_crew' : 'no_input', opening: null };
    const textOnly = t.models > 0 && t.calls.length === 0 && t.wrappers === 0;
    // A turn with no model response (e.g. still usage limited) is not a status-only reply.
    const statusOnly = t.models > 0 && (t.calls.length > 0 ? t.calls.every(isStatusCall) : t.textChars <= shortReplyChars && t.wrappers === 0);
    const statusOrRead = t.calls.length > 0 && t.calls.every(c => isStatusCall(c) || (c.crew && CREW_READ_TOOLS.has(c.name)));
    const turnReqs = t.requests.filter(reqIn);
    const reqs = turnReqs.map(r => [r, 1]);
    const cats = { crew_read: 0, crew_status: 0, crew_other: 0, non_crew: 0 };
    for (const c of t.calls) cats[callCategory(c)]++;
    turnStats.push({ runtime, agent, cls: info.cls, crew: info.crew, input: Boolean(t.input), error: t.error ? normalizeOpening(t.error).slice(0, 70) : null, calls: t.calls.length, requests: turnReqs.length, models: t.models, textOnly, statusOnly, partial: t.partial, reqs: turnReqs });
    const f = { line: t.input?.line ?? t.startLine, partial: t.partial, input_class: info.cls, opening: info.opening, input: t.input?.text?.slice(0, 200) ?? '',
      calls: t.calls.length, tools: t.calls.map(c => c.name), requests: reqs, text_chars: t.textChars, model_tool_calls: runtime === 'codex' ? t.wrappers + t.calls.filter(c => !c.inner).length : t.calls.length };
    if (t.input && statusOnly) push('status_only_turn', { ...f, sub: info.crew ? info.cls.split(':')[0] : 'non_crew', result: '' });
    if (t.input && !statusOnly && statusOrRead) push('status_or_read_only_turn', { ...f, sub: info.crew ? info.cls.split(':')[0] : 'non_crew', result: '' });
    if (t.input && isNoActionNotice(info)) push('notice_turn', { ...f, sub: info.cls, sender: info.sender ?? null, status_only: statusOnly, candidate: statusOnly || statusOrRead, result: '' });
    if (t.input && textOnly) push('text_only_turn', { ...f, sub: info.cls, result: '', reply_chars: t.textChars });
    // End-of-turn reply forwarding (crew daemon/turns.rs forward_reply): an
    // explicit "@id:" line goes to those ids as a message; otherwise the reply
    // goes, as an answer, to the non-scheduler senders of the Message/Question
    // openers addressed to this member, unless it wrote to them in the turn.
    // Broadcast (to "*") openers are not forwarded; native files do not show
    // whether an opener was direct or broadcast.
    const lastCall = t.calls.at(-1)?.line ?? -1;
    const finalReply = t.texts.filter(x => x.line > lastCall).map(x => x.text).join('\n').trim();
    if (t.input && finalReply) {
      const openers = (info.entries ?? []).filter(e => (e.type === 'message' || e.type === 'question') && e.from !== 'scheduler');
      const wroteTo = new Set(t.calls.filter(c => c.crew && c.name === 'agent_send').map(c => c.args?.to));
      const askers = openers.filter(e => !wroteTo.has(e.from));
      const sub = hasExplicitMention(finalReply) ? 'explicit_mention' : askers.length ? 'message_or_question_from_member_or_orchestrator' : 'not_forwarded';
      if (sub !== 'not_forwarded' || textOnly) push('forward_candidate_reply', { ...f, sub, candidate: sub !== 'not_forwarded', requests: turnReqs.slice(-1).map(r => [r, 1]),
        result: finalReply.slice(0, 200), reply_chars: finalReply.length, text_only: textOnly, opener_senders: askers.map(e => e.from) });
    }
    if (t.input && info.cls === 'notice:task_reminder') {
      const bg = runtime === 'claude' ? runningAt(t.input.line, t.input.ts) : null;
      const noTool = t.calls.length === 0 && t.wrappers === 0;
      const onlyStatus = t.calls.length > 0 && t.calls.every(c => c.crew && c.name === 'agent_status');
      push('reminder_turn', { ...f, sub: bg === null ? 'background_undetectable_codex' : bg.any ? 'background_running' : 'no_background_running', result: '',
        ended_no_tool_call: noTool, only_agent_status: onlyStatus, status_only: statusOnly, background_running: bg?.any ?? null, background_running_2h: bg?.recent ?? null,
        background_kinds: bg?.kinds ?? null, no_model_response: t.models === 0, candidate: noTool || onlyStatus });
    }
    if (t.input && info.crew && (info.delivery || info.cls === 'notice:team_messages_came')) {
      const reads = t.calls.filter(c => c.crew && CREW_READ_TOOLS.has(c.name));
      push('delivery_round_trips', { ...f, sub: info.cls, categories: cats, first_call: t.calls[0] ? callCategory(t.calls[0]) : 'none',
        read_calls: reads.length, read_tools: reads.map(c => c.name), requests: reads.flatMap(callReqs), turn_requests: turnReqs.length,
        turn_tokens_requests: reqs, result: reads.map(c => c.text ?? '').join('\n').slice(0, 200), result_bytes: reads.reduce((s, c) => s + bytes(c.text), 0) });
    }
  }
  const openings = [];
  for (const e of inputs) if (e.info.crew && inSpan(e.ts)) openings.push({ opening: e.info.opening, cls: e.info.cls, fold: Boolean(e.fold), sender: e.info.sender ?? null, runtime });
  const callsIn = calls.filter(c => inSpan(c.ts));
  const readSizes = callsIn.filter(c => c.crew && (REPEAT_READ_TOOLS.has(c.name) || CREW_READ_TOOLS.has(c.name)) && c.text !== undefined).map(c => ({ runtime, tool: c.name, bytes: bytes(c.text), file, line: c.line }));
  const inboxKinds = callsIn.filter(c => c.inbox).map(c => ({ runtime, kind: c.inbox.kind, first: (c.text ?? '').trim().split('\n', 1)[0].slice(0, 80) }));
  return { findings, turnStats, openings, inboxKinds, readSizes, inboxNovelty, calls: callsIn.length, crewCalls: callsIn.filter(c => c.crew).length, turns: turns.filter(t => t.inSpan).length, partialTurns: turns.filter(t => t.inSpan && t.v.partial).length,
    wrappers: turns.reduce((s, t) => s + t.wrappersIn, 0), requests: requests.filter(reqIn).length };
}

// ------------------------------------------------------------- aggregation

export function loadPrices(file = path.join(REPO, 'notes/prompt-cache-audit/prices.tsv')) {
  const prices = new Map();
  if (!fs.existsSync(file)) return prices;
  const [head, ...rows] = fs.readFileSync(file, 'utf8').trim().split('\n').map(l => l.split('\t'));
  for (const r of rows) { const o = Object.fromEntries(head.map((h, i) => [h, r[i]])); prices.set(o.model, { base: +o.base_input, w5m: +o.write_5m, w1h: +o.write_1h, read: +o.read }); }
  return prices;
}

/**
 * Input-side list cost in USD as a range. status: 'no_price' (model not in the
 * price table), 'unknown_usage' (no input-side usage field at all), 'priced'
 * (lower/upper finite; a write without a known 5m/1h split is a range from the
 * cheaper to the dearer write price, ttl_unknown) or 'partial' (some input
 * field missing: the known part is a lower bound, upper unknown = null).
 */
export function requestCost(r, prices) {
  const p = prices.get(r.model);
  if (!p) return { status: 'no_price', lower: null, upper: null, ttl_unknown: false };
  const tail = r.uncached_tail_tokens, read = r.cache_read_tokens, write = r.cache_creation_tokens;
  const w5 = r.cache_write_5m_tokens, w1 = r.cache_write_1h_tokens;
  if ([tail, read, write, w5, w1].every(v => v == null)) return { status: 'unknown_usage', lower: null, upper: null, ttl_unknown: false };
  let lower = 0, upper = 0, bounded = true, ttlUnknown = false;
  const exact = (tokens, price) => { if (tokens == null) { bounded = false; return; } lower += tokens * price; upper += tokens * price; };
  exact(tail, p.base); exact(read, p.read);
  const known = (w5 ?? 0) * p.w5m + (w1 ?? 0) * p.w1h;
  lower += known; upper += known;
  if (w5 == null || w1 == null) {
    // Remaining write tokens whose TTL bucket is not recorded.
    const rest = write == null ? null : write - (w5 ?? 0) - (w1 ?? 0);
    if (rest == null) { if (w5 == null && w1 == null) bounded = false; else if (write == null) bounded = false; }
    else if (rest > 0) { lower += rest * Math.min(p.w5m, p.w1h); upper += rest * Math.max(p.w5m, p.w1h); ttlUnknown = true; }
  }
  return { status: bounded ? 'priced' : 'partial', lower: lower / 1e6, upper: bounded ? upper / 1e6 : null, ttl_unknown: ttlUnknown };
}

const TOKEN_KEYS = [['input_total', 'input_total_tokens'], ['cache_read', 'cache_read_tokens'], ['cache_write', 'cache_creation_tokens'], ['cache_write_5m', 'cache_write_5m_tokens'], ['cache_write_1h', 'cache_write_1h_tokens'], ['uncached_tail', 'uncached_tail_tokens'], ['output', 'output_tokens']];

function newBucket() { return { count: 0, sessions: new Set(), agents: new Set(), result_bytes: 0, reqs: new Map() }; }
function addTo(b, f, reqs) {
  b.count++; b.sessions.add(f.file); b.agents.add(f.agent); b.result_bytes += f.result_bytes ?? 0;
  for (const [r, share] of reqs) b.reqs.set(r, Math.min(1, (b.reqs.get(r) ?? 0) + share));
}
export function finalize(b, prices) {
  // Per metric: the known sum and how many associated requests report it.
  // A metric no associated request reports is null, never 0.
  const tokens = Object.fromEntries(TOKEN_KEYS.map(([k]) => [k, 0]));
  const coverage = Object.fromEntries(TOKEN_KEYS.map(([k]) => [k, { known: 0, unknown: 0 }]));
  let lower = 0, upper = 0, unbounded = false, priced = 0, unpriced = 0, unknownUsage = 0, ttlUnknown = 0, partial = 0, associated = 0;
  const unpricedModels = new Set();
  for (const [r, share] of b.reqs) {
    associated += share;
    for (const [k, src] of TOKEN_KEYS) {
      if (Number.isFinite(r[src])) { tokens[k] += r[src] * share; coverage[k].known++; } else coverage[k].unknown++;
    }
    const c = requestCost(r, prices);
    if (c.status === 'no_price') { unpriced += share; unpricedModels.add(r.model ?? 'unknown'); }
    else if (c.status === 'unknown_usage') unknownUsage += share;
    else {
      priced += share; lower += c.lower * share;
      if (c.upper === null) { unbounded = true; partial += share; } else upper += c.upper * share;
      if (c.ttl_unknown) ttlUnknown += share;
    }
  }
  for (const k of Object.keys(tokens)) tokens[k] = coverage[k].known ? Math.round(tokens[k]) : null;
  const r2 = v => +v.toFixed(2), r4 = v => +v.toFixed(4);
  return { count: b.count, sessions: b.sessions.size, agents: [...b.agents].sort(), associated_requests: r2(associated), distinct_requests: b.reqs.size,
    tokens, token_coverage: coverage, list_cost_lower_usd: priced ? r4(lower) : null, list_cost_upper_usd: priced && !unbounded ? r4(upper) : null,
    priced_requests: r2(priced), unpriced_requests: r2(unpriced), unknown_usage_requests: r2(unknownUsage), ttl_unknown_requests: r2(ttlUnknown), partial_usage_requests: r2(partial),
    unpriced_models: [...unpricedModels].sort(), result_bytes: b.result_bytes };
}

const PATTERNS = {
  inbox_empty: 'candidate agent_inbox calls whose result says no new messages; associated request = the next request when the call was alone (upper bound), else result bytes only',
  inbox_after_delivery: 'agent_inbox calls within N model responses after an in-band delivery whose messages all arrived in full; candidate = empty or duplicate-only result',
  inbox_duplicate_only: 'candidate agent_inbox calls whose returned message ids were all delivered in full in-band earlier in the session',
  repeated_read: 'candidate read tool calls (tasks_list, task_get, agents_list, git_status, git_diff, usage_status, agent_inbox by id) byte-identical to the previous same tool+args call with no own state change between',
  status_only_turn: 'candidate turns whose only tool calls are agent_status / task_update / agent_send type=status, or with no tool calls and a short reply; associated = every request in the turn',
  status_or_read_only_turn: 'candidate turns (not already status-only) whose tool calls are only status calls plus crew reads (agent_inbox, tasks_list, task_get, agents_list)',
  notice_turn: 'turns started by crew notices or status messages; candidate = the turn was status-only or status/read-only',
  delivery_round_trips: 'turns started by a delivered crew message: tool calls by category; associated = requests that consumed the crew read calls (split among sibling calls)',
  text_only_turn: 'descriptive: turns with an input that ended with text only (a model response, no tool call), any opener class; associated = every request in the turn',
  forward_candidate_reply: 'end-of-turn replies crew may forward (daemon forward_reply): explicit_mention = a reply line opens with @id:, forwarded as a message; message_or_question_from_member_or_orchestrator = opener of type message/question from a non-scheduler sender the turn did not write to, forwarded as an answer only if the opener was addressed directly (not a broadcast; not visible natively); not_forwarded = text-only turns with any other opener (status, scheduler, answer, artifact, notices); associated = the last request of the turn',
  reminder_turn: 'turns opened by "[crew] Reminder: task" (idle reminder); candidate = ended with no tool call or only agent_status; Claude sub-kinds say whether a background Bash/Agent/Monitor launch was still un-notified at that moment',
};

function aggregate(results, prices, sessions) {
  const all = results.flatMap(r => r.findings);
  const out = {};
  for (const pattern of Object.keys(PATTERNS)) {
    const fs_ = all.filter(f => f.pattern === pattern);
    const selected = ['inbox_after_delivery', 'notice_turn', 'forward_candidate_reply'].includes(pattern) ? fs_.filter(f => f.candidate) : fs_;
    const total = newBucket(), byRuntime = new Map(), byAgent = new Map(), bySub = new Map();
    const get = (m, k) => { if (!m.has(k)) m.set(k, newBucket()); return m.get(k); };
    for (const f of selected) {
      addTo(total, f, f.requests); addTo(get(byRuntime, f.runtime), f, f.requests);
      addTo(get(byAgent, `${f.runtime}:${f.agent}`), f, f.requests);
      if (f.sub) addTo(get(bySub, f.sub), f, f.requests);
    }
    const fin = m => Object.fromEntries([...m].sort((a, b) => b[1].count - a[1].count).map(([k, b]) => [k, finalize(b, prices)]));
    out[pattern] = { description: PATTERNS[pattern], ...finalize(total, prices), partial_turns: selected.filter(f => f.partial).length, by_runtime: fin(byRuntime), by_agent: fin(byAgent), by_sub: fin(bySub) };
    if (pattern === 'inbox_after_delivery') {
      const allB = newBucket(); for (const f of fs_) addTo(allB, f, f.requests);
      out[pattern].all_post_delivery_inbox_calls = finalize(allB, prices);
      const subs = {}; for (const f of fs_) subs[f.sub] = (subs[f.sub] ?? 0) + 1; out[pattern].all_post_delivery_by_result = subs;
    }
    if (pattern === 'notice_turn') {
      const allB = newBucket(), byCls = new Map();
      for (const f of fs_) { addTo(allB, f, f.requests); const b = get(byCls, f.sub); addTo(b, f, f.requests); b.candidate = (b.candidate ?? 0) + (f.candidate ? 1 : 0); b.statusOnly = (b.statusOnly ?? 0) + (f.status_only ? 1 : 0); }
      out[pattern].all_notice_turns = finalize(allB, prices);
      // Turn-starting status messages by sender (teammate status only starts a
      // turn when the recipient was waiting; scheduler = daemon notices).
      const bySender = new Map();
      for (const f of fs_.filter(f => f.sub === 'message:status')) {
        const k = f.sender ?? 'unknown';
        if (!bySender.has(k)) bySender.set(k, { all: newBucket(), candidate: newBucket(), status_only: 0 });
        const e = bySender.get(k); addTo(e.all, f, f.requests); if (f.candidate) addTo(e.candidate, f, f.requests); if (f.status_only) e.status_only++;
      }
      out[pattern].status_message_turns_by_sender = Object.fromEntries([...bySender].sort((a, b) => b[1].all.count - a[1].all.count).map(([k, e]) => [k, { turns: e.all.count, candidate_turns: e.candidate.count, status_only_turns: e.status_only,
        all: finalize(e.all, prices), candidate: finalize(e.candidate, prices) }]));
      out[pattern].all_by_class = Object.fromEntries([...byCls].sort((a, b) => b[1].count - a[1].count).map(([k, b]) => [k, { ...finalize(b, prices), candidate_turns: b.candidate, status_only_turns: b.statusOnly }]));
    }
    if (pattern === 'reminder_turn') {
      const flags = rows => ({ turns: rows.length, ended_no_tool_call: rows.filter(f => f.ended_no_tool_call && !f.no_model_response).length, only_agent_status: rows.filter(f => f.only_agent_status).length,
        status_only: rows.filter(f => f.status_only).length, no_model_response: rows.filter(f => f.no_model_response).length,
        background_running: rows.filter(f => f.background_running > 0).length, background_running_2h: rows.filter(f => f.background_running_2h > 0).length });
      out[pattern].flags = { total: flags(fs_), claude: flags(fs_.filter(f => f.runtime === 'claude')), codex: flags(fs_.filter(f => f.runtime === 'codex')) };
      const w = newBucket(); for (const f of fs_.filter(f => f.candidate)) addTo(w, f, f.requests);
      out[pattern].candidates = finalize(w, prices);
    }
    if (pattern === 'forward_candidate_reply') {
      const all = new Map(), byOpener = new Map();
      for (const f of fs_) { addTo(get(all, f.sub), f, f.requests); addTo(get(byOpener, `${f.sub} | ${f.input_class}`), f, f.requests); }
      out[pattern].all_by_sub = fin(all);
      out[pattern].by_sub_and_opener = fin(byOpener);
      out[pattern].text_only_share = { candidates_text_only: selected.filter(f => f.text_only).length, candidates_with_tool_calls: selected.filter(f => !f.text_only).length };
    }
    if (pattern === 'delivery_round_trips') {
      const stats = rt => {
        const rows = fs_.filter(f => !rt || f.runtime === rt);
        const calls = rows.map(f => f.calls).sort((a, b) => a - b);
        const q = p => calls.length ? calls[Math.min(calls.length - 1, Math.floor(p * calls.length))] : null;
        const sum = k => rows.reduce((s, f) => s + f.categories[k], 0);
        const turnB = newBucket(); for (const f of rows) addTo(turnB, f, f.turn_tokens_requests);
        return { delivered_turns: rows.length, tool_calls: calls.reduce((a, b) => a + b, 0), mean_calls: rows.length ? +(calls.reduce((a, b) => a + b, 0) / rows.length).toFixed(2) : null,
          median_calls: q(0.5), p90_calls: q(0.9), crew_read_calls: sum('crew_read'), crew_status_calls: sum('crew_status'), crew_other_calls: sum('crew_other'), non_crew_calls: sum('non_crew'),
          turns_with_read: rows.filter(f => f.read_calls > 0).length, first_call_read: rows.filter(f => f.first_call === 'crew_read').length,
          read_tools: rows.flatMap(f => f.read_tools).reduce((m, n) => (m[n] = (m[n] ?? 0) + 1, m), {}),
          model_requests_in_turns: rows.reduce((s, f) => s + f.turn_requests, 0), mean_requests_per_delivery: rows.length ? +(rows.reduce((s, f) => s + f.turn_requests, 0) / rows.length).toFixed(2) : null,
          model_tool_calls: rows.reduce((s, f) => s + f.model_tool_calls, 0), all_tokens_of_delivered_turns: finalize(turnB, prices) };
      };
      out[pattern].distribution = { total: stats(null), claude: stats('claude'), codex: stats('codex'), pi: stats('pi') };
    }
  }
  // Notice openings and inbox strings.
  const openings = new Map();
  for (const o of results.flatMap(r => r.openings)) {
    const k = o.opening;
    if (!openings.has(k)) openings.set(k, { opening: k, cls: o.cls, count: 0, turn_start: 0, folded: 0, claude: 0, codex: 0, pi: 0, senders: {} });
    const e = openings.get(k); e.count++; e[o.fold ? 'folded' : 'turn_start']++; e[o.runtime]++;
    if (o.sender) e.senders[o.sender] = (e.senders[o.sender] ?? 0) + 1;
  }
  const openingList = [...openings.values()].sort((a, b) => b.count - a.count);
  const inboxKinds = {}, inboxStrings = {};
  for (const k of results.flatMap(r => r.inboxKinds)) {
    inboxKinds[k.runtime] ??= {}; inboxKinds[k.runtime][k.kind] = (inboxKinds[k.runtime][k.kind] ?? 0) + 1;
    if (k.kind !== 'messages') { const key = normalizeOpening(k.first); inboxStrings[key] = (inboxStrings[key] ?? 0) + 1; }
  }
  // Text-only and no-model turns by input class (all turns with an input).
  const byCls = new Map();
  for (const t of results.flatMap(r => r.turnStats)) {
    if (!t.input) continue;
    const k = `${t.runtime} ${t.cls}`;
    if (!byCls.has(k)) byCls.set(k, { turns: 0, text_only: 0, status_only: 0, no_model_response: 0, text_only_bucket: newBucket() });
    const e = byCls.get(k); e.turns++; if (t.models === 0) e.no_model_response++; if (t.statusOnly) e.status_only++;
    if (t.textOnly) { e.text_only++; addTo(e.text_only_bucket, { file: '', agent: t.agent }, t.reqs.map(r => [r, 1])); }
  }
  const turnErrors = {};
  for (const t of results.flatMap(r => r.turnStats)) if (t.error) { const k = `${t.runtime} ${t.error}`; turnErrors[k] = (turnErrors[k] ?? 0) + 1; }
  const turnClasses = [...byCls].sort((a, b) => b[1].turns - a[1].turns).map(([k, e]) => { const f = finalize(e.text_only_bucket, prices);
    return { class: k, turns: e.turns, text_only: e.text_only, status_only: e.status_only, no_model_response: e.no_model_response, text_only_associated: f }; });
  // Read result sizes.
  const sizes = {};
  for (const r of results.flatMap(x => x.readSizes)) for (const k of [`${r.tool} ${r.runtime}`, `${r.tool} all`]) (sizes[k] ??= []).push(r.bytes);
  const readSizes = Object.fromEntries(Object.entries(sizes).sort().map(([k, v]) => { v.sort((a, b) => a - b); const q = x => v[Math.min(v.length - 1, Math.floor(x * v.length))];
    return [k, { calls: v.length, total_bytes: v.reduce((a, b) => a + b, 0), min: v[0], p50: q(0.5), p90: q(0.9), max: v.at(-1), mean: Math.round(v.reduce((a, b) => a + b, 0) / v.length), over_50kb: v.filter(x => x > 50_000).length, over_100kb: v.filter(x => x > 100_000).length }]; }));
  const largest = results.flatMap(x => x.readSizes).filter(r => r.tool === 'tasks_list').sort((a, b) => b.bytes - a.bytes).slice(0, 5).map(r => ({ file: tilde(r.file), line: r.line, runtime: r.runtime, bytes: r.bytes }));
  // Inbox calls that returned messages: were any ids genuinely new?
  const novelty = rt => {
    const rows = results.flatMap(x => x.inboxNovelty).filter(n => !rt || n.runtime === rt);
    const types = {}, senders = {};
    for (const n of rows) for (const e of n.new_inband) { types[e.type] = (types[e.type] ?? 0) + 1; senders[e.from] = (senders[e.from] ?? 0) + 1; }
    const sort = o => Object.fromEntries(Object.entries(o).sort((a, b) => b[1] - a[1]));
    return { calls_with_messages: rows.length, by_id_lookups: rows.filter(n => n.by_id).length, calls_with_new_inband_id: rows.filter(n => n.new_inband.length).length,
      calls_with_new_id_not_seen_anywhere_before: rows.filter(n => n.new_any).length, calls_all_ids_already_delivered: rows.filter(n => !n.new_inband.length && n.entries).length,
      ids_returned: rows.reduce((s, n) => s + n.entries, 0), new_inband_ids: rows.reduce((s, n) => s + n.new_inband.length, 0), new_inband_types: sort(types), new_inband_senders: sort(senders) };
  };
  const cohort = {};
  for (const s of sessions) {
    cohort[s.runtime] ??= { sessions: 0, requests: 0, turns: 0, partial_turns: 0, tool_calls: 0, crew_tool_calls: 0, codex_exec_wrappers: 0, agents: new Set() };
    const c = cohort[s.runtime], r = results.find(x => x.file === s.file);
    c.sessions++; c.agents.add(s.agent_id ?? 'unknown');
    if (r) { c.requests += r.requests; c.turns += r.turns; c.partial_turns += r.partialTurns; c.tool_calls += r.calls; c.crew_tool_calls += r.crewCalls; c.codex_exec_wrappers += r.wrappers; }
  }
  for (const c of Object.values(cohort)) c.agents = [...c.agents].sort();
  return { patterns: out, notice_openings: openingList.slice(0, 40), notice_only_openings: openingList.filter(o => o.cls.startsWith('notice:') || o.cls === 'message:status' || o.cls === 'batch:status').slice(0, 20),
    inbox_result_kinds: inboxKinds, inbox_non_message_strings: Object.fromEntries(Object.entries(inboxStrings).sort((a, b) => b[1] - a[1])), cohort,
    turn_classes: turnClasses, turn_errors: Object.fromEntries(Object.entries(turnErrors).sort((a, b) => b[1] - a[1])), read_result_sizes: readSizes, largest_tasks_list_results: largest,
    inbox_message_novelty: { total: novelty(null), claude: novelty('claude'), codex: novelty('codex') } };
}

const home = os.homedir();
const tilde = p => p.startsWith(home) ? `~${p.slice(home.length)}` : p;
const short = (v, n = 200) => (typeof v === 'string' ? v : JSON.stringify(v ?? '')).replace(/\s+/g, ' ').slice(0, n);

function examples(results) {
  const out = {};
  const all = results.flatMap(r => r.findings);
  for (const pattern of Object.keys(PATTERNS)) {
    let rows = all.filter(f => f.pattern === pattern && (f.candidate ?? true));
    rows = rows.sort((a, b) => (b.score ?? 0) - (a.score ?? 0) || (b.bundle === 'alone') - (a.bundle === 'alone') || (b.result_bytes ?? 0) - (a.result_bytes ?? 0) || (b.calls ?? 0) - (a.calls ?? 0));
    const picked = [], files = new Set();
    for (const f of rows) { if (picked.length >= 5) break; if (files.has(f.file)) continue; files.add(f.file); picked.push(f); }
    for (const f of rows) { if (picked.length >= 5) break; if (!picked.includes(f)) picked.push(f); }
    out[pattern] = picked.map(f => ({ file: tilde(f.file), line: f.line, runtime: f.runtime, agent: f.agent, tool: f.tool ?? null,
      sub: f.sub ?? null, bundle: f.bundle, round: f.round, args: f.args ? short(f.args, 120) : undefined, previous_line: f.previous_line, delivery_line: f.delivery_line, delivery_opening: f.delivery_opening,
      delivery_snippet: f.delivery_snippet ? short(f.delivery_snippet) : undefined, candidates_in_same_window: f.score,
      input_class: f.input_class, tools: f.tools?.slice(0, 12), snippet: short(f.result || f.input || ''), result_bytes: f.result_bytes }));
  }
  return out;
}

// ------------------------------------------------------------------- output

const n = v => v === null || v === undefined ? '—' : typeof v === 'number' ? v.toLocaleString('en-US') : String(v);
const esc = s => String(s ?? '').replaceAll('|', '\\|').replace(/\n/g, ' ');
/** "$a–$b", "$a", "at least $a", "no price"; plus counts of excluded requests. */
export function costText(s) {
  const d = v => `$${v.toFixed(2)}`;
  let c;
  if (!s.priced_requests) c = s.unpriced_requests ? 'no price' : s.unknown_usage_requests ? 'usage unknown' : '—';
  else if (s.list_cost_upper_usd === null) c = `at least ${d(s.list_cost_lower_usd)}`;
  else c = d(s.list_cost_lower_usd) === d(s.list_cost_upper_usd) ? d(s.list_cost_lower_usd) : `${d(s.list_cost_lower_usd)}–${d(s.list_cost_upper_usd)}`;
  if (s.unpriced_requests) c += ` (+${n(s.unpriced_requests)} req no price)`;
  if (s.unknown_usage_requests) c += ` (+${n(s.unknown_usage_requests)} req usage unknown)`;
  return c;
}
/** Visible pricing assumptions for one table. */
function costBasis(rows) {
  const sum = k => +rows.reduce((a, s) => a + (s[k] ?? 0), 0).toFixed(2);
  const parts = [];
  if (sum('ttl_unknown_requests')) parts.push(`${n(sum('ttl_unknown_requests'))} associated requests with write TTL unknown priced as a range (cheaper to dearer write price)`);
  if (sum('partial_usage_requests')) parts.push(`${n(sum('partial_usage_requests'))} with a missing input field (known part only, lower bound)`);
  if (sum('unknown_usage_requests')) parts.push(`${n(sum('unknown_usage_requests'))} with no usage (cost unknown)`);
  if (sum('unpriced_requests')) parts.push(`${n(sum('unpriced_requests'))} on models without a listed price (${[...new Set(rows.flatMap(s => s.unpriced_models ?? []))].join(', ')})`);
  return `\nCost basis (total row): ${parts.length ? parts.join('; ') : 'all associated requests priced exactly'}.\n`;
}
/** Token cell: the known sum, " (K of N)" when only K of N associated requests report the metric, "not reported" when none does. */
export function tokenCell(s, key) {
  const c = s.token_coverage?.[key], v = s.tokens?.[key];
  if (!c || c.known + c.unknown === 0) return '—';
  if (!c.known || v === null || v === undefined) return 'not reported';
  return c.unknown ? `${n(v)} (${n(c.known)} of ${n(c.known + c.unknown)})` : n(v);
}
function row(label, s) {
  const t = k => tokenCell(s, k);
  return `| ${esc(label)} | ${n(s.count)} | ${n(s.sessions)} | ${n(s.agents.length)} | ${n(s.associated_requests)} | ${t('input_total')} | ${t('cache_read')} | ${t('cache_write')} | ${t('uncached_tail')} | ${t('output')} | ${costText(s)} | ${n(s.result_bytes)} |\n`;
}
const HEAD = '| Slice | Count | Sessions | Agents | Associated requests (upper bound) | Input total | Cache read | Cache write | Uncached tail | Output | Associated list cost (input side, upper bound) | Result bytes |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n';

export function markdown(summary) {
  const p = summary.patterns, o = summary.options;
  const span = o.since != null || o.until != null ? ` Window ${o.since != null ? new Date(o.since).toISOString() : '…'} to ${o.until != null ? new Date(o.until).toISOString() : '…'} (inclusive) on subjects and associated requests.` : '';
  let out = `# Turn efficiency candidates\n\nGenerated ${summary.generated_at} from native session files (cohort and usage via baseline.mjs). Window N=${o.window} model responses; short reply ≤ ${o.short_reply_chars} chars.${span} Counts are candidates selected by tool names and message wrappers; associated requests and costs are upper bounds, not avoidable cost.\n\n## Cohort\n\n| Runtime | Sessions | Requests | Turns | Partial turns at cutoff | Tool calls | Crew tool calls | Codex exec wrappers | Agents |\n|---|---:|---:|---:|---:|---:|---:|---:|---|\n`;
  for (const [rt, c] of Object.entries(summary.cohort)) out += `| ${rt} | ${n(c.sessions)} | ${n(c.requests)} | ${n(c.turns)} | ${n(c.partial_turns)} | ${n(c.tool_calls)} | ${n(c.crew_tool_calls)} | ${n(c.codex_exec_wrappers)} | ${c.agents.join(', ')} |\n`;
  out += `\nagent_inbox result kinds: ${Object.entries(summary.inbox_result_kinds).map(([rt, k]) => `${rt} ${Object.entries(k).map(([a, b]) => `${a}=${b}`).join(', ')}`).join('; ')}.\n\nNon-message inbox result strings (normalized): ${Object.entries(summary.inbox_non_message_strings).map(([k, v]) => `\`${k}\` ×${v}`).join(', ')}.\n`;
  for (const [name, s] of Object.entries(p)) {
    out += `\n## ${name}\n\n${s.description}.${s.partial_turns ? ` ${s.partial_turns} of these turns were still open at the cutoff (partial).` : ''}\n\n${HEAD}${row('total', s)}`;
    for (const [k, v] of Object.entries(s.by_runtime)) out += row(`runtime ${k}`, v);
    for (const [k, v] of Object.entries(s.by_sub).slice(0, 12)) out += row(`kind ${k}`, v);
    out += costBasis([s]);
    if (s.all_post_delivery_inbox_calls) out += `\nAll inbox calls inside a post-delivery window: ${s.all_post_delivery_inbox_calls.count} (${Object.entries(s.all_post_delivery_by_result).map(([a, b]) => `${a} ${b}`).join(', ')}).\n`;
    if (s.all_by_sub) {
      out += `\nAll replies by forwarding class (candidates exclude not_forwarded; ${s.text_only_share.candidates_text_only} candidates were text-only turns, ${s.text_only_share.candidates_with_tool_calls} had tool calls):\n\n${HEAD}`;
      for (const [k, v] of Object.entries(s.all_by_sub)) out += row(k, v);
      out += '\n| Forwarding class | Opener class | Replies |\n|---|---|---:|\n';
      for (const [k, v] of Object.entries(s.by_sub_and_opener).slice(0, 20)) { const [a, b] = k.split(' | '); out += `| ${esc(a)} | ${esc(b)} | ${n(v.count)} |\n`; }
    }
    if (s.all_by_class) {
      out += `\nAll notice/status-started turns: ${s.all_notice_turns.count}.\n\n| Input class | Turns | Candidates (status/read only) | Status-only | Associated requests (upper bound) | Input total | Cache read | Associated list cost (input side, upper bound) |\n|---|---:|---:|---:|---:|---:|---:|---:|\n`;
      for (const [k, v] of Object.entries(s.all_by_class)) out += `| ${esc(k)} | ${n(v.count)} | ${n(v.candidate_turns)} | ${n(v.status_only_turns)} | ${n(v.associated_requests)} | ${tokenCell(v, 'input_total')} | ${tokenCell(v, 'cache_read')} | ${costText(v)} |\n`;
    }
    if (s.status_message_turns_by_sender) {
      out += '\nTurn-starting status messages by sender (scheduler = daemon notices and the scheduler member; a teammate status starts a turn only when the recipient was waiting):\n\n| Sender | Turns | Candidates (status/read only) | Status-only | Associated requests (all) | Input total (all) | Associated list cost (all) | Associated requests (candidates) | Associated list cost (candidates) |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|\n';
      for (const [k, v] of Object.entries(s.status_message_turns_by_sender)) out += `| ${esc(k)} | ${n(v.turns)} | ${n(v.candidate_turns)} | ${n(v.status_only_turns)} | ${n(v.all.associated_requests)} | ${tokenCell(v.all, 'input_total')} | ${costText(v.all)} | ${n(v.candidate.associated_requests)} | ${costText(v.candidate)} |\n`;
    }
    if (s.flags) {
      out += '\n| Runtime | Reminder turns | Ended with no tool call | Only agent_status | Status-only | No model response | Background launch un-notified | …launched within 2h |\n|---|---:|---:|---:|---:|---:|---:|---:|\n';
      for (const [k, v] of Object.entries(s.flags)) out += `| ${k} | ${v.turns} | ${v.ended_no_tool_call} | ${v.only_agent_status} | ${v.status_only} | ${v.no_model_response} | ${k === 'codex' ? 'not detectable' : v.background_running} | ${k === 'codex' ? '—' : v.background_running_2h} |\n`;
      out += `\nCandidate reminder turns (no tool call or only agent_status): ${s.candidates.count}, ${n(s.candidates.associated_requests)} associated requests, ${tokenCell(s.candidates, 'input_total')} input tokens, ${costText(s.candidates)}.\n`;
    }
    if (s.distribution) {
      out += '\n| Runtime | Delivered turns | Tool calls | Mean | Median | p90 | Crew reads | Crew status | Other crew | Non-crew | Turns with a read | First call is a read | Requests in turns | Mean requests |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n';
      for (const [k, v] of Object.entries(s.distribution)) if (v.delivered_turns) out += `| ${k} | ${n(v.delivered_turns)} | ${n(v.tool_calls)} | ${n(v.mean_calls)} | ${n(v.median_calls)} | ${n(v.p90_calls)} | ${n(v.crew_read_calls)} | ${n(v.crew_status_calls)} | ${n(v.crew_other_calls)} | ${n(v.non_crew_calls)} | ${n(v.turns_with_read)} | ${n(v.first_call_read)} | ${n(v.model_requests_in_turns)} | ${n(v.mean_requests_per_delivery)} |\n`;
      out += `\nRead tools after deliveries: ${Object.entries(s.distribution.total.read_tools).map(([a, b]) => `${a} ${b}`).join(', ') || 'none'}.\n`;
    }
    out += `\nTop agents: ${Object.entries(s.by_agent).slice(0, 8).map(([k, v]) => `${k} ${v.count}`).join(', ') || 'none'}.\n`;
  }
  const nv = summary.inbox_message_novelty;
  out += '\n## agent_inbox calls that returned messages: genuinely new?\n\n| Runtime | Calls with messages | By-id lookups | ≥1 id not delivered in full in-band | ≥1 id not seen anywhere before | All ids already delivered in full | Ids returned | New in-band ids |\n|---|---:|---:|---:|---:|---:|---:|---:|\n';
  for (const [k, v] of Object.entries(nv)) out += `| ${k} | ${v.calls_with_messages} | ${v.by_id_lookups} | ${v.calls_with_new_inband_id} | ${v.calls_with_new_id_not_seen_anywhere_before} | ${v.calls_all_ids_already_delivered} | ${v.ids_returned} | ${v.new_inband_ids} |\n`;
  out += `\nTypes of new ids: ${Object.entries(nv.total.new_inband_types).map(([a, b]) => `${a} ${b}`).join(', ')}. Senders: ${Object.entries(nv.total.new_inband_senders).slice(0, 10).map(([a, b]) => `${a} ${b}`).join(', ')}.\n`;
  out += '\n## Read result sizes (bytes)\n\n| Tool / runtime | Calls | Total | Min | p50 | p90 | Max | Mean | >50 KB | >100 KB |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n';
  for (const [k, v] of Object.entries(summary.read_result_sizes)) out += `| ${k} | ${n(v.calls)} | ${n(v.total_bytes)} | ${n(v.min)} | ${n(v.p50)} | ${n(v.p90)} | ${n(v.max)} | ${n(v.mean)} | ${v.over_50kb} | ${v.over_100kb} |\n`;
  out += `\nLargest tasks_list results: ${summary.largest_tasks_list_results.map(r => `${r.file}:${r.line} ${n(r.bytes)} B`).join('; ') || 'none'}.\n`;
  out += '\n## Turns by input class (text-only = a model response without any tool call)\n\n| Runtime + input class | Turns | Text-only | Status-only | No model response | Text-only associated requests | Text-only input tokens | Text-only associated list cost |\n|---|---:|---:|---:|---:|---:|---:|---:|\n';
  for (const v of summary.turn_classes.slice(0, 30)) out += `| ${esc(v.class)} | ${v.turns} | ${v.text_only} | ${v.status_only} | ${v.no_model_response} | ${n(v.text_only_associated.associated_requests)} | ${tokenCell(v.text_only_associated, 'input_total')} | ${costText(v.text_only_associated)} |\n`;
  out += `\nFailed turns (native turn error, usually no model response): ${Object.entries(summary.turn_errors).slice(0, 6).map(([k, v]) => `${esc(k)} ×${v}`).join('; ') || 'none'}.\n`;
  out += '\n## Crew input openings (top 20, all [crew] inputs)\n\n| Opening | Class | Count | Turn start | Folded | Claude | Codex |\n|---|---|---:|---:|---:|---:|---:|\n';
  for (const x of summary.notice_openings.slice(0, 20)) out += `| ${esc(x.opening)} | ${x.cls} | ${x.count} | ${x.turn_start} | ${x.folded} | ${x.claude} | ${x.codex} |\n`;
  out += '\n## Notice and status openings (top 20)\n\n| Opening | Class | Count | Turn start | Folded | Top senders |\n|---|---|---:|---:|---:|---|\n';
  for (const x of summary.notice_only_openings) out += `| ${esc(x.opening)} | ${x.cls} | ${x.count} | ${x.turn_start} | ${x.folded} | ${Object.entries(x.senders).sort((a, b) => b[1] - a[1]).slice(0, 3).map(([a, b]) => `${a} ${b}`).join(', ')} |\n`;
  out += `\n## Limits\n\n${summary.limits.map(l => `- ${l}`).join('\n')}\n`;
  return out;
}

const LIMITS = [
  'These are candidates, not findings of avoidable cost or savings. Cohorts are selected by tool names, result strings and message wrappers only; a candidate call or turn may be useful work (analysis written as text, reviews, waiting/parking transitions, continuations, a status the team needed).',
  'Association is an upper bound, not avoidable cost. A candidate call (inbox_empty, inbox_duplicate_only, inbox_after_delivery, repeated_read) is "alone" when every tool call consumed by the same next model response is agent_inbox/agent_status/task_update; it is then associated with that whole consuming request (deduplicated per pattern), which may also have done other reasoning. A call bundled with other real tool work (Codex exec with exec_command etc., Claude parallel tool_use) is associated with its result bytes only. The issuing response is never associated. delivery_round_trips splits consuming requests among sibling calls. Turn patterns associate every request in the turn; forward_candidate_reply associates the turn\'s last request.',
  'A call joins its exact native consuming request: the request recorded after the next model response and before the model response after that (by source line); otherwise it has none. Joins use all requests of the session; --since/--until (inclusive, like baseline) then select subjects (the call, or the turn\'s start/input) and associated requests. A subject with an unknown timestamp is outside a set window. State before the window still feeds delivered ids and post-delivery windows.',
  'List cost is input side only (uncached tail × base, cache writes × 5m/1h write price, cache reads × read price) from notes/prompt-cache-audit/prices.tsv; output tokens are not priced. A write without a recorded 5m/1h split is priced as a range from the cheaper to the dearer write price; a missing input-side field leaves only a lower bound ("at least"); a request with no usage fields is "usage unknown"; models without a listed price (Codex gpt-*) are "no price". Cache writes are reported as writes; nothing here infers a cold cache.',
  'Delivered ids are only those whose full body reached the member in-band: the header id of a non-empty "[crew] Message from" input and each block of a "[crew] N new messages" batch, unless the block was clipped ("… (truncated"). Ids merely mentioned in notices or bodies are references and never count as delivered.',
  'forward_candidate_reply follows crew\'s forward_reply rules as far as native files show them: the opener\'s recipient (direct vs broadcast "*") is not recorded natively, so message_or_question_from_member_or_orchestrator is an upper bound; explicit_mention matches a reply line opening with @name(s): without checking the names against the roster.',
  'Background work at reminder time is Claude-only: Bash run_in_background, async Agent or Monitor launches confirmed by their tool result and not yet named by a <task-notification>. A process restart can orphan a launch, so the un-notified count is an upper bound; the 2h column limits it to recent launches. Codex background terminals are not detectable from the rollout.',
  'Inbox novelty compares returned message ids with ids delivered in full in-band earlier in the same session file; a session that began after an earlier delivery (restart, takeover) can make an old id look new.',
  'Claude requests are deduplicated by message id with the last streamed usage winning (baseline.mjs). Codex requests are token_count cumulative deltas per sampling; a Codex inner tool call (crew MCP call inside an `exec` code-mode wrapper) returns with its wrapper output, so all inner calls of one wrapper share the consuming request. When two exec wrappers run in one sampling, inner calls are linked to the first wrapper output after them.',
  'Patterns overlap (an empty inbox call after a delivery appears in inbox_empty and inbox_after_delivery; a notice turn may also be a status-only turn). Do not add pattern totals.',
  'Status-only is inferred from tool names and agent_send type only. Short reply = total assistant text in the turn ≤ the threshold, with no tool calls.',
  'Repeated-read detection resets on any own crew mutating call (task_*, review_*, git_commit/merge/cherry_pick, agent_status, agent lifecycle) and, for git_status/git_diff, on any own file-modifying tool call. Other agents may have changed state invisibly; identical bytes mean the read returned nothing new.',
  'Mid-turn folded crew messages (Claude isMeta, Codex inputs after model output in the same task) count as deliveries for the inbox window and openings, but are not turns and carry no turn requests.',
  'With --until set, each measured turn is seen only up to the cutoff (its later or undated events and requests are dropped); a turn with no end marker (Claude turn_duration, Codex task_complete) and no next turn by the cutoff is counted as partial. Appending records after the cutoff does not change the result.',
  'Token columns show the sum over associated requests that report the metric; "(K of N)" means only K of N associated requests report it and "not reported" that none does (e.g. Codex has no native 5m/1h write split, and a missing native write counter stays unknown, never 0). Cost coverage is reported separately and does not qualify token columns.',
  'Sessions are read up to the byte size baseline.mjs read; later appends are excluded.',
];

// ---------------------------------------------------------------------- I/O

export async function readEvents(session) {
  const size = session.snapshot?.size_at_start ?? fs.statSync(session.file).size;
  if (!size) return [];
  const records = [];
  let line = 0;
  const codex = session.runtime === 'codex';
  const rl = readline.createInterface({ input: fs.createReadStream(session.file, { end: size - 1 }), crlfDelay: Infinity });
  for await (const text of rl) {
    line++;
    if (codex) {
      const head = text.slice(0, 600);
      if (/^\{[^{]*"type":"(compacted|token_usage_record|world_state|turn_context|session_meta)"/.test(head)) continue;
      if (head.includes('"type":"event_msg"') && !/"type":"task_(started|complete)"|"item":\{"type":"(McpToolCall|CommandExecution|FileChange|ImageView|WebSearch|Extension)"/.test(head)) continue;
    } else if (!text.includes('"type":"user"') && !text.includes('"type":"assistant"') && !text.includes('"type":"message"') && !text.includes('<task-notification>') && !text.includes('"subtype":"turn_duration"')) continue;
    let value;
    try { value = JSON.parse(text); } catch { continue; }
    if (value && typeof value === 'object') records.push({ value, line });
  }
  return extractEvents(session.runtime, records);
}

function defaultWorkspace(cwd) {
  const at = cwd.indexOf(`${path.sep}.crew${path.sep}worktrees${path.sep}`);
  return at === -1 ? cwd : cwd.slice(0, at);
}

export async function run(options) {
  // The cohort is collected without since/until so every call can join its
  // exact consuming request; the window applies to subjects afterwards.
  const report = await collect({ workspace: options.workspace, sessionFiles: options.sessionFiles ?? [], allRuns: options.allRuns });
  const outputs = ['summary.json', 'summary.md', 'examples.json'].map(f => path.join(options.out, f));
  guardOutputs(outputs, report);
  const prices = loadPrices(options.prices);
  const results = [];
  for (const s of report.sessions) {
    const events = await readEvents(s);
    results.push({ file: s.file, ...analyzeSession(s, events, options) });
  }
  const agg = aggregate(results, prices, report.sessions);
  const summary = { schema_version: SCHEMA_VERSION, generated_at: new Date().toISOString(), workspace: report.workspace,
    options: { window: options.window, short_reply_chars: options.shortReplyChars, since: options.since ?? null, until: options.until ?? null },
    prices: Object.fromEntries(prices), cohort: agg.cohort, excluded_files: report.coverage.excluded_files.length,
    inbox_result_kinds: agg.inbox_result_kinds, inbox_non_message_strings: agg.inbox_non_message_strings,
    inbox_message_novelty: agg.inbox_message_novelty, read_result_sizes: agg.read_result_sizes, largest_tasks_list_results: agg.largest_tasks_list_results, turn_classes: agg.turn_classes, turn_errors: agg.turn_errors,
    patterns: agg.patterns, notice_openings: agg.notice_openings, notice_only_openings: agg.notice_only_openings, limits: LIMITS };
  fs.mkdirSync(options.out, { recursive: true });
  fs.writeFileSync(outputs[0], `${JSON.stringify(summary, null, 2)}\n`);
  fs.writeFileSync(outputs[1], markdown(summary));
  fs.writeFileSync(outputs[2], `${JSON.stringify(examples(results), null, 2)}\n`);
  return summary;
}

export async function main(argv = process.argv.slice(2)) {
  const options = { workspace: defaultWorkspace(process.cwd()), out: null, sessionFiles: [], ...DEFAULTS };
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i];
    if (key === '--help') { process.stdout.write('Usage: node scripts/cache/turns.mjs --out DIR [--workspace PATH] [--window N] [--short-reply CHARS] [--all-runs] [--session-file claude|codex|pi=PATH] [--since ISO] [--until ISO] [--prices TSV]\nWrites DIR/summary.json, DIR/summary.md, DIR/examples.json. Read-only on native sessions; Node 24.\n'); return; }
    if (key === '--all-runs') { options.allRuns = true; continue; }
    const value = argv[++i];
    if (value === undefined) throw new Error(`Missing value for ${key}`);
    if (key === '--out') options.out = path.resolve(value);
    else if (key === '--workspace') options.workspace = path.resolve(value);
    else if (key === '--window') options.window = Number.parseInt(value, 10);
    else if (key === '--short-reply') options.shortReplyChars = Number.parseInt(value, 10);
    else if (key === '--session-file') options.sessionFiles.push(value);
    else if (key === '--prices') options.prices = path.resolve(value);
    else if (key === '--since' || key === '--until') { const t = Date.parse(value); if (!Number.isFinite(t)) throw new Error(`Invalid ${key}`); options[key.slice(2)] = t; }
    else throw new Error(`Unknown argument: ${key}`);
  }
  if (!options.out) throw new Error('--out DIR is required');
  if (!(options.window > 0)) throw new Error('--window must be a positive integer');
  const s = await run(options);
  const p = s.patterns;
  process.stdout.write(`${Object.entries(s.cohort).map(([rt, c]) => `${rt}: ${c.sessions} sessions, ${c.requests} requests`).join('; ')}\n`);
  for (const [k, v] of Object.entries(p)) process.stdout.write(`${k}: ${v.count} in ${v.sessions} sessions, ${v.associated_requests} associated requests, ${costText(v)}\n`);
  process.stdout.write(`wrote ${path.join(options.out, 'summary.md')}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) main().catch(error => { console.error(error.message); process.exitCode = 1; });
