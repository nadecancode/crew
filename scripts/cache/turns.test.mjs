import assert from 'node:assert/strict';
import { test } from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { inboxResultKind, inboxEntries, messageIds, classifyInput, isNoActionNotice, extractEvents, analyzeSession, requestCost, normalizeOpening, finalize, costText, tokenCell, run } from './turns.mjs';

const rows = values => values.map((value, i) => ({ value, line: i + 1 }));
const usage = { input_tokens: 10, cache_creation_input_tokens: 20, cache_read_input_tokens: 70, output_tokens: 5 };
const req = (line, extra = {}) => ({ source_line: line, model: 'claude-opus-5-5', input_total_tokens: 100, cache_read_tokens: 70, cache_creation_tokens: 20, uncached_tail_tokens: 10, output_tokens: 5, cache_write_5m_tokens: null, cache_write_1h_tokens: 20, ...extra });

// Claude records
const userText = text => ({ type: 'user', message: { role: 'user', content: text } });
const fold = text => ({ type: 'user', isMeta: true, message: { role: 'user', content: [{ type: 'text', text }] } });
const call = (msg, id, name, input = {}) => ({ type: 'assistant', message: { id: msg, model: 'claude-opus-5-5', usage, content: [{ type: 'tool_use', id, name, input }] } });
const say = (msg, text) => ({ type: 'assistant', message: { id: msg, model: 'claude-opus-5-5', usage, content: [{ type: 'text', text }] } });
const result = (id, text) => ({ type: 'user', message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: id, content: [{ type: 'text', text }] }] } });
const claudeSession = (records, requests) => { const events = extractEvents('claude', rows(records)); return analyzeSession({ runtime: 'claude', file: '/s.jsonl', agent_id: 'a', requests }, events); };
const requestsAt = records => records.flatMap((r, i) => r.type === 'assistant' && records.findIndex(x => x.message?.id === r.message.id) === i ? [req(i + 1)] : []);
const of = (out, pattern) => out.findings.filter(f => f.pattern === pattern);

test('inbox result classifier recognizes the real empty and message strings', () => {
  assert.equal(inboxResultKind('No new messages.'), 'empty');
  assert.equal(inboxResultKind('0 message(s):'), 'empty');
  assert.equal(inboxResultKind('2 message(s):\n\n--- 12:00:00.1 from qa to ui · type status · id m-aaaaaa111111\nx'), 'messages');
  assert.equal(inboxResultKind("no message 'm-106279e56a2b' sent or received by you"), 'id_not_found');
  assert.equal(inboxResultKind('incompatible crew protocol: client speaks protocol 6, daemon speaks 7;'), 'error');
  assert.deepEqual(messageIds('--- x · id m-aaaaaa111111\n--- y · id m-bbbbbb222222'), ['m-aaaaaa111111', 'm-bbbbbb222222']);
});

test('input classifier: wrapped notices by body, messages by type, batches and direct notices', () => {
  const nudge = classifyInput('[crew] Message from scheduler (type status, id m-4eee3e3ca172):\n\n[crew] Your last turn ended without a tool call, so nothing came of it');
  assert.equal(nudge.cls, 'notice:no_tool_call'); assert.ok(isNoActionNotice(nudge));
  assert.match(nudge.opening, /^\[crew\] Message from <agent> \(type status\): \[crew\] Your last turn ended/);
  assert.equal(classifyInput('[crew] Message from scheduler (type status, id m-4ad23a556c09):\n\nYour conversation was just compacted: what came before').cls, 'notice:compacted');
  const task = classifyInput('[crew] Message from orchestrator (type task, id m-52819ec47a2a):\n\nTask x is yours');
  assert.equal(task.cls, 'message:task'); assert.equal(task.body, true); assert.deepEqual(task.ids, ['m-52819ec47a2a']); assert.ok(!isNoActionNotice(task));
  assert.equal(classifyInput('[crew] Message from qa (type status, id m-111111111111):\n\nGreen.').cls, 'message:status');
  const batch = classifyInput('[crew] 2 new messages:\n\n(1) from ui · type status · id m-a1fae792de35\nA\n\n(2) from qa · type status · id m-a1fae792de36\nB');
  assert.equal(batch.cls, 'batch:status'); assert.deepEqual(batch.ids, ['m-a1fae792de35', 'm-a1fae792de36']);
  assert.equal(classifyInput('[crew] Team messages came as your last turn ended (above). Act on them.').cls, 'notice:team_messages_came');
  assert.equal(classifyInput('[crew] Reminder: task "Build it" (id web-ui) is still open.').cls, 'notice:task_reminder');
  assert.equal(classifyInput('hello').crew, false);
  assert.equal(normalizeOpening('[crew] You are online as qa. Nothing is assigned to you yet. 3 tasks'), '[crew] You are online as <agent>. Nothing is assigned to you yet. N tasks');
});

test('Claude: empty inbox calls after an in-band delivery are candidates associated with their consuming request', () => {
  const records = [
    userText('[crew] Message from orchestrator (type task, id m-aaaaaa111111):\n\nDo the thing'),
    call('r1', 't1', 'mcp__crew__agent_inbox'), result('t1', 'No new messages.'),
    call('r2', 't2', 'mcp__crew__agent_inbox'), result('t2', '1 message(s):\n\n--- 12:00:00.1 from orchestrator to a · type task · id m-aaaaaa111111\nDo the thing'),
    call('r3', 't3', 'Bash', { command: 'ls' }), result('t3', 'x'),
    call('r4', 't4', 'mcp__crew__agent_inbox'), result('t4', 'No new messages.'),
    say('r5', 'done'),
  ];
  const out = claudeSession(records, requestsAt(records));
  assert.equal(of(out, 'inbox_empty').length, 2);
  const post = of(out, 'inbox_after_delivery');
  assert.deepEqual(post.map(f => [f.sub, f.round]), [['empty alone', 1], ['duplicate_only alone', 2]]); // the r4 call is outside N=3
  assert.equal(of(out, 'inbox_duplicate_only').length, 1);
  assert.equal(post[0].requests[0][0].source_line, 4); // consumed by r2's request
  assert.equal(post[0].requests[0][1], 1);
  const trips = of(out, 'delivery_round_trips')[0];
  assert.deepEqual(trips.categories, { crew_read: 3, crew_status: 0, crew_other: 0, non_crew: 1 });
  assert.equal(trips.first_call, 'crew_read');
});

test('Codex: a bundled inbox call is charged bytes only; an inbox-only exec is charged its whole consuming sampling', () => {
  const p = payload => ({ type: 'response_item', payload });
  const item = (id, tool, text) => ({ type: 'event_msg', payload: { type: 'item_completed', item: { type: 'McpToolCall', id, server: 'crew', tool, arguments: {}, status: 'completed', result: { content: [{ type: 'text', text }] } } } });
  const records = [
    { type: 'event_msg', payload: { type: 'task_started', turn_id: 't' } },
    p({ type: 'message', role: 'user', content: [{ type: 'input_text', text: '[crew] Message from ui (type answer, id m-cccccc333333):\n\nHere it is' }] }),
    p({ type: 'reasoning' }), p({ type: 'custom_tool_call', call_id: 'c1', name: 'exec', input: '...' }),
    item('i1', 'agent_inbox', 'No new messages.'), item('i2', 'tasks_list', '[]'),
    p({ type: 'custom_tool_call_output', call_id: 'c1', output: [] }),
    { type: 'event_msg', payload: { type: 'token_count' } }, // issuing sampling (line 8)
    p({ type: 'reasoning' }), p({ type: 'custom_tool_call', call_id: 'c2', name: 'exec', input: '...' }),
    item('i3', 'agent_inbox', 'No new messages.'),
    p({ type: 'custom_tool_call_output', call_id: 'c2', output: [] }),
    { type: 'event_msg', payload: { type: 'token_count' } }, // line 13
    p({ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'ok' }] }),
    { type: 'event_msg', payload: { type: 'token_count' } }, // line 15
  ];
  const events = extractEvents('codex', rows(records));
  const requests = [8, 13, 15].map(l => req(l, { model: 'gpt-6.1-sol' }));
  const out = analyzeSession({ runtime: 'codex', file: '/r.jsonl', agent_id: 'qa', requests }, events);
  const post = of(out, 'inbox_after_delivery');
  assert.deepEqual(post.map(f => [f.sub, f.round]), [['empty bundled', 1], ['empty alone', 2]]);
  assert.deepEqual(post[0].requests, []); // bundled with tasks_list: result bytes only
  assert.equal(post[0].result_bytes, 16); // "No new messages."
  assert.equal(post[1].requests[0][0].source_line, 15); // inbox-only exec caused the next sampling
  assert.equal(post[1].requests[0][1], 1);
  assert.deepEqual(of(out, 'inbox_empty').map(f => f.sub), ['bundled', 'alone']);
  const trips = of(out, 'delivery_round_trips')[0];
  assert.equal(trips.requests[0][0].source_line, 13); assert.equal(trips.requests[0][1], 0.5); // round trips split among siblings
  assert.equal(requestCost(requests[0], new Map()).status, 'no_price');
});

test('repeated reads: identical results count unless an own mutating call intervenes', () => {
  const records = [
    userText('go'),
    call('r1', 'a', 'mcp__crew__task_get', { id: 'x' }), result('a', '{"status":"open"}'),
    call('r2', 'b', 'mcp__crew__task_get', { id: 'x' }), result('b', '{"status":"open"}'),
    call('r3', 'c', 'mcp__crew__task_get', { id: 'y' }), result('c', '{"status":"open"}'),
    call('r4', 'd', 'mcp__crew__task_update', { id: 'x', note: 'n' }), result('d', 'ok'),
    call('r5', 'e', 'mcp__crew__task_get', { id: 'x' }), result('e', '{"status":"open"}'),
    call('r6', 'f', 'mcp__crew__git_status'), result('f', 'clean'),
    call('r7', 'g', 'Edit', { file_path: '/x' }), result('g', 'ok'),
    call('r8', 'h', 'mcp__crew__git_status'), result('h', 'clean'),
    call('r9', 'i', 'mcp__crew__git_status'), result('i', 'clean'),
  ];
  const out = claudeSession(records, requestsAt(records));
  assert.deepEqual(of(out, 'repeated_read').map(f => [f.tool, f.line]), [['task_get', 4], ['git_status', 18]]);
  assert.equal(of(out, 'repeated_read')[0].result_bytes, 17);
});

test('status-only turns, notice turns and openings', () => {
  const records = [
    userText('[crew] Message from scheduler (type status, id m-dddddd444444):\n\n[crew] Your last turn ended without a tool call, so nothing came of it'),
    call('r1', 'a', 'mcp__crew__agent_status', { status: 'working', note: 'n' }), result('a', 'ok'),
    call('r2', 'b', 'mcp__crew__agent_send', { to: 'lead', type: 'status', body: 'b' }), result('b', 'ok'),
    say('r3', 'Done.'),
    userText('[crew] Message from qa (type status, id m-eeeeee555555):\n\nAll green.'),
    say('r4', 'Noted.'),
    userText('[crew] Message from lead (type task, id m-ffffff666666):\n\nFix it'),
    call('r5', 'c', 'Bash', { command: 'make' }), result('c', 'ok'),
    call('r6', 'd', 'mcp__crew__agent_send', { to: 'lead', type: 'answer', body: 'fixed' }), result('d', 'ok'),
    fold('[crew] Message from qa (type status, id m-999999777777):\n\nStill green.'),
    say('r7', 'x'.repeat(400)),
    userText('[crew] Message from lead (type question, id m-aaaaaa888888):\n\nWhy?'),
    say('r8', 'x'.repeat(400)),
  ];
  const out = claudeSession(records, requestsAt(records));
  const status = of(out, 'status_only_turn');
  assert.deepEqual(status.map(f => f.input_class), ['notice:no_tool_call', 'message:status']);
  assert.equal(status[0].requests.length, 3);
  const notices = of(out, 'notice_turn');
  assert.deepEqual(notices.map(f => [f.sub, f.candidate]), [['notice:no_tool_call', true], ['message:status', true]]);
  assert.deepEqual(out.openings.map(o => [o.cls, o.fold]), [['notice:no_tool_call', false], ['message:status', false], ['message:task', false], ['message:status', true], ['message:question', false]]);
  const trips = of(out, 'delivery_round_trips').find(f => f.input_class === 'message:task');
  assert.deepEqual(trips.categories, { crew_read: 0, crew_status: 0, crew_other: 1, non_crew: 1 });
});

test('input-side list cost uses explicit write buckets', () => {
  const prices = new Map([['claude-opus-5-5', { base: 4, w5m: 5, w1h: 8, read: 0.2 }]]);
  // 10 tail × 4 + 20 1h writes × 8 + 70 reads × 0.2 = 214 per million
  assert.deepEqual(requestCost(req(1), prices), { status: 'priced', lower: 214 / 1e6, upper: 214 / 1e6, ttl_unknown: false });
  // No TTL split recorded: 20 writes priced from 5 (5m) to 8 (1h).
  assert.deepEqual(requestCost(req(1, { cache_write_1h_tokens: null }), prices), { status: 'priced', lower: (40 + 100 + 14) / 1e6, upper: (40 + 160 + 14) / 1e6, ttl_unknown: true });
});

test('inbox novelty: entries never delivered in-band are new, with their types', () => {
  assert.deepEqual(inboxEntries('2 message(s):\n\n--- 12:28:43.102 from orchestrator · type message · id m-a37da7ec4b3c\nx\n--- 12:29:00.000 from qa to ui · type status · id m-b37da7ec4b3c\ny'),
    [{ from: 'orchestrator', to: null, type: 'message', id: 'm-a37da7ec4b3c' }, { from: 'qa', to: 'ui', type: 'status', id: 'm-b37da7ec4b3c' }]);
  const records = [
    userText('[crew] Message from orchestrator (type task, id m-aaaaaa111111):\n\nDo it'),
    call('r1', 't1', 'mcp__crew__agent_inbox'),
    result('t1', '2 message(s):\n\n--- 1 from orchestrator to a · type task · id m-aaaaaa111111\nDo it\n--- 2 from qa to a · type status · id m-bbbbbb222222\nGreen'),
    say('r2', 'ok'),
  ];
  const out = claudeSession(records, requestsAt(records));
  assert.deepEqual(out.inboxNovelty.map(x => [x.entries, x.new_inband]), [[2, [{ type: 'status', from: 'qa' }]]]);
  assert.equal(of(out, 'inbox_duplicate_only').length, 0);
});

test('text-only turns, reminder turns with background work, read sizes', () => {
  const notify = id => ({ type: 'user', origin: { kind: 'task-notification' }, message: { role: 'user', content: `<task-notification>\n<task-id>b1</task-id>\n<tool-use-id>${id}</tool-use-id>\n<status>completed</status>\n</task-notification>` } });
  const records = [
    userText('go'),
    call('r1', 'bg1', 'Bash', { command: 'make test', run_in_background: true }), result('bg1', 'Command running in background with ID: b1. Output is being written to: /tmp/x'),
    call('r2', 'tl', 'mcp__crew__tasks_list'), result('tl', 'x'.repeat(1000)),
    say('r3', 'waiting'),
    userText('[crew] Reminder: task "Build" (id b) is still open. Continue working on it; call task_complete'),
    call('r4', 's1', 'mcp__crew__agent_status', { status: 'working', note: 'tests running' }), result('s1', 'ok'),
    notify('bg1'),
    say('r5', 'done'),
    userText('[crew] Reminder: task "Build" (id b) is still open.'),
    say('r6', 'still on it'),
    userText('[crew] Message from orchestrator (type message, id m-cccccc333333):\n\nmain fast-forwarded to abcdef1: rebase when idle'),
    say('r7', 'Ack, will rebase.'),
  ];
  const out = claudeSession(records, requestsAt(records));
  const rem = of(out, 'reminder_turn');
  assert.deepEqual(rem.map(f => [f.sub, f.only_agent_status, f.ended_no_tool_call]), [['background_running', true, false], ['no_background_running', false, true]]);
  // The <task-notification> input opens its own (non-crew) turn.
  assert.deepEqual(of(out, 'text_only_turn').map(f => [f.sub, f.requests.length]), [['non_crew', 1], ['notice:task_reminder', 1], ['notice:fast_forward', 1]]);
  assert.deepEqual(of(out, 'forward_candidate_reply').map(f => [f.sub, f.candidate]), [['not_forwarded', false], ['not_forwarded', false], ['message_or_question_from_member_or_orchestrator', true]]);
  assert.deepEqual(out.readSizes.map(r => [r.tool, r.bytes]), [['tasks_list', 1000]]);
});

// ---- review fixes

test('T1: crew\'s empty inbox result with appended guidance is empty (prefix match)', () => {
  assert.equal(inboxResultKind('No new messages. One that arrives later is delivered to you: if you are waiting for it, end your turn, or park your card with agent_status("waiting", …), instead of checking again.'), 'empty');
  assert.equal(inboxResultKind('No unread messages for you.'), 'empty');
  assert.equal(inboxResultKind('0 message(s):'), 'empty');
  assert.equal(inboxResultKind('10 message(s):\n\n--- 1 from qa · type status · id m-aaaaaa111111'), 'messages');
});

// The reviewer's window fixture: three turns at 01:00, 02:00 and 03:00.
const stampAt = hour => `2026-10-08T${hour}:00:00Z`;
function windowRecords() {
  const records = [];
  for (const [hour, i] of [['01', 1], ['02', 2], ['03', 3]]) {
    records.push({ type: 'user', timestamp: stampAt(hour), message: { role: 'user', content: `[crew] Message from qa (type status, id m-aaaaaa00000${i}):\n\nFixture ready.` } });
    records.push({ type: 'assistant', timestamp: stampAt(hour), message: { id: `r${i}`, model: 'claude-fixture', usage, content: [{ type: 'tool_use', id: `c${i}`, name: 'mcp__crew__agent_inbox', input: {} }] } });
    records.push({ type: 'user', timestamp: stampAt(hour), message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: `c${i}`, content: [{ type: 'text', text: 'No new messages.' }] }] } });
    records.push({ type: 'assistant', timestamp: stampAt(hour), message: { id: `s${i}`, model: 'claude-fixture', usage, content: [{ type: 'text', text: 'Ready.' }] } });
  }
  return records;
}
const windowAnalysis = (since, until) => {
  const records = windowRecords();
  const requests = requestsAt(records).map(r => ({ ...r, timestamp_ms: Date.parse(records[r.source_line - 1].timestamp) }));
  return analyzeSession({ runtime: 'claude', file: '/w.jsonl', agent_id: 'a', requests }, extractEvents('claude', rows(records)), { since: since && Date.parse(since), until: until && Date.parse(until) });
};

test('T2: the window selects subjects; calls join their exact consuming request first', async t => {
  const out = windowAnalysis('2026-10-08T01:30:00Z', '2026-10-08T02:30:00Z');
  const empty = of(out, 'inbox_empty');
  assert.equal(empty.length, 1); assert.equal(empty[0].line, 6);
  assert.deepEqual(empty[0].requests.map(([r, share]) => [r.source_line, share]), [[8, 1]]); // the 02:00 reply, not the 01:00 call's
  assert.equal(out.turns, 1); assert.equal(out.calls, 1); assert.equal(out.requests, 2);
  assert.equal(out.turnStats.length, 1); assert.equal(out.openings.length, 1); assert.equal(out.inboxKinds.length, 1);
  // Unwindowed: each call joins the response right after its own result.
  const all = windowAnalysis();
  assert.deepEqual(of(all, 'inbox_empty').map(f => f.requests[0][0].source_line), [4, 8, 12]);
  // End to end through run(): cohort collected unwindowed, window applied after joins; labels never claim waste.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'crew-turns-window-')); t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const file = path.join(dir, 'input', 'session.jsonl'); fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, windowRecords().map(x => JSON.stringify(x)).join('\n') + '\n');
  const report = await run({ workspace: path.join(dir, 'ws'), sessionFiles: [`claude=${file}`], since: Date.parse('2026-10-08T01:30:00Z'), until: Date.parse('2026-10-08T02:30:00Z'),
    out: path.join(dir, 'report'), prices: path.join(dir, 'absent-prices'), window: 3, shortReplyChars: 300 });
  assert.equal(report.cohort.claude.turns, 1); assert.equal(report.patterns.inbox_empty.count, 1); assert.equal(report.patterns.inbox_empty.associated_requests, 1);
  const md = fs.readFileSync(path.join(dir, 'report', 'summary.md'), 'utf8');
  assert.doesNotMatch(md, /wast/i); assert.match(md, /Associated requests \(upper bound\)/); assert.match(md, /Associated list cost \(input side, upper bound\)/);
});

test('T2: until is inclusive at both boundaries; a window without usage associates nothing', () => {
  const exact = windowAnalysis('2026-10-08T02:00:00Z', '2026-10-08T02:00:00Z');
  assert.equal(of(exact, 'inbox_empty').length, 1); assert.equal(exact.turns, 1);
  const empty = windowAnalysis('2026-10-08T01:10:00Z', '2026-10-08T01:20:00Z');
  assert.equal(empty.findings.length, 0); assert.equal(empty.turns, 0); assert.equal(empty.requests, 0); assert.equal(empty.calls, 0);
  // A subject without a timestamp is outside a set window.
  const undated = analyzeSession({ runtime: 'claude', file: '/u', requests: [] }, extractEvents('claude', rows([userText('go'), call('r1', 'x', 'mcp__crew__agent_inbox'), result('x', 'No new messages.')])), { since: 0 });
  assert.equal(undated.findings.length, 0);
});

test('T3: write TTL unknown is a range, missing usage is unknown, missing fields give a lower bound', () => {
  const prices = new Map([['fixture', { base: 4, w5m: 5, w1h: 8, read: 0.2 }]]);
  assert.deepEqual(requestCost({ model: 'fixture', cache_creation_tokens: 1e6, cache_read_tokens: 0, uncached_tail_tokens: 0 }, prices), { status: 'priced', lower: 5, upper: 8, ttl_unknown: true });
  assert.equal(requestCost({ model: 'fixture' }, prices).status, 'unknown_usage');
  assert.deepEqual(requestCost({ model: 'fixture', uncached_tail_tokens: 1e6, cache_creation_tokens: 0, cache_write_5m_tokens: 0, cache_write_1h_tokens: 0 }, prices), { status: 'partial', lower: 4, upper: null, ttl_unknown: false });
  const bucket = reqs => ({ count: reqs.length, sessions: new Set(['s']), agents: new Set(['a']), result_bytes: 0, reqs: new Map(reqs.map(r => [r, 1])) });
  const split = { model: 'fixture', uncached_tail_tokens: 0, cache_read_tokens: 0, cache_creation_tokens: 1e6, cache_write_5m_tokens: 0, cache_write_1h_tokens: 1e6 };
  const noSplit = { model: 'fixture', uncached_tail_tokens: 0, cache_read_tokens: 0, cache_creation_tokens: 1e6, cache_write_5m_tokens: null, cache_write_1h_tokens: null };
  const mixed = finalize(bucket([split, noSplit, { model: 'fixture' }, { model: 'gpt-x', input_total_tokens: 5 }]), prices);
  assert.deepEqual([mixed.list_cost_lower_usd, mixed.list_cost_upper_usd, mixed.priced_requests, mixed.ttl_unknown_requests, mixed.unknown_usage_requests, mixed.unpriced_requests], [13, 16, 2, 1, 1, 1]);
  assert.equal(costText(mixed), '$13.00–$16.00 (+1 req no price) (+1 req usage unknown)');
  const partial = finalize(bucket([split, { model: 'fixture', uncached_tail_tokens: 1e6 }]), prices);
  assert.equal(partial.list_cost_upper_usd, null); assert.equal(costText(partial), 'at least $12.00');
  assert.equal(costText(finalize(bucket([split]), prices)), '$8.00');
  assert.equal(costText(finalize(bucket([{ model: 'gpt-x', input_total_tokens: 5 }]), prices)), 'no price (+1 req no price)');
});

test('T4: only fully delivered bodies count as delivered; mentioned ids are references', () => {
  const notice = classifyInput('[crew] Your last turn ended without a tool call. You were given review_request from qa (id m-abcdef123456).');
  assert.deepEqual([notice.ids, notice.refs], [[], ['m-abcdef123456']]);
  const records = [
    userText('[crew] Your last turn ended without a tool call. You were given review_request from qa (id m-abcdef123456).'),
    call('r1', 'a', 'mcp__crew__agent_inbox', { id: 'm-abcdef123456' }),
    result('a', '1 message(s):\n\n--- 12:00:00 from qa · type review_request · id m-abcdef123456\nFull source not previously supplied.'),
    say('r2', 'Received.'),
  ];
  assert.equal(of(claudeSession(records, []), 'inbox_duplicate_only').length, 0);
  // A wrapped notice mentions other ids; only its own header id was delivered.
  const wrapped = classifyInput('[crew] Message from scheduler (type status, id m-111111111111):\n\n[crew] Your last turn ended without a tool call. You were given task from orchestrator (m-222222222222).');
  assert.deepEqual([wrapped.ids, wrapped.refs], [['m-111111111111'], ['m-222222222222']]);
  // Clipped bodies are not deliveries; the batch's other block still is.
  const clipped = classifyInput('[crew] Message from qa (type message, id m-333333333333):\n\nLong text\n… (truncated — call agent_inbox for the full text)');
  assert.deepEqual([clipped.ids, clipped.body], [[], false]);
  const batch = classifyInput('[crew] 2 new messages:\n\n(1) from ui · type status · id m-444444444444\nShort.\n\n(2) from qa · type message · id m-555555555555\nLong\n… (truncated — call agent_inbox for the full text)');
  assert.deepEqual([batch.ids, batch.refs, batch.body], [['m-444444444444'], ['m-555555555555'], false]);
  const batched = [
    userText('[crew] 2 new messages:\n\n(1) from ui · type status · id m-444444444444\nShort.\n\n(2) from qa · type message · id m-555555555555\nLong\n… (truncated — call agent_inbox for the full text)'),
    call('r1', 'b', 'mcp__crew__agent_inbox', { id: 'm-555555555555' }),
    result('b', '--- 12:00:00 from qa to a · type message · id m-555555555555\nLong text in full'),
    call('r2', 'c', 'mcp__crew__agent_inbox', { id: 'm-444444444444' }),
    result('c', '--- 12:00:00 from ui to a · type status · id m-444444444444\nShort.'),
    say('r3', 'ok'),
  ];
  const out = claudeSession(batched, requestsAt(batched));
  assert.deepEqual(of(out, 'inbox_duplicate_only').map(f => f.line), [4]);
  assert.equal(of(out, 'inbox_after_delivery').length, 0); // a clipped block makes the inbox call legitimate
});

test('T6: forward candidates follow crew forwarding rules: status opener, message opener, explicit mention', () => {
  const records = [
    userText('[crew] Message from qa (type status, id m-666666666666):\n\nGreen.'),
    say('r1', 'Noted.'),
    userText('[crew] Message from qa (type message, id m-777777777777):\n\nCan you check the build?'),
    say('r2', 'Build is fine.'),
    userText('[crew] Message from scheduler (type status, id m-888888888888):\n\nAgent qa is unresponsive: no activity for 600s.'),
    say('r3', 'Checking.\n@qa: are you stuck?'),
    userText('[crew] Message from qa (type question, id m-999999999999):\n\nWhich port?'),
    call('r4', 's', 'mcp__crew__agent_send', { to: 'qa', type: 'answer', body: '8080' }), result('s', 'ok'),
    say('r5', 'Answered qa.'),
  ];
  const out = claudeSession(records, requestsAt(records));
  assert.deepEqual(of(out, 'forward_candidate_reply').map(f => [f.input_class, f.sub, f.candidate]), [
    ['message:status', 'not_forwarded', false],
    ['message:message', 'message_or_question_from_member_or_orchestrator', true],
    ['notice:unresponsive', 'explicit_mention', true],
  ]); // the question turn wrote to qa itself, so its reply is not forwarded and not listed
  assert.equal(of(out, 'text_only_turn').length, 3);
});

// ---- second review round

test('T2: with --until a turn is measured only up to the cutoff; later appends change nothing', () => {
  const at = minute => `2026-10-08T${minute}:00Z`;
  const head = [
    { ...userText('[crew] Message from qa (type status, id m-abab12345678):\n\nGreen.'), timestamp: at('02:00') },
    { ...say('r1', 'Noted.'), timestamp: at('02:01') },
  ];
  const later = [
    { ...call('r2', 'b', 'Bash', { command: 'make' }), timestamp: at('03:00') },
    { ...result('b', 'ok'), timestamp: at('03:00') },
    { ...say('r3', 'Built.'), timestamp: at('03:01') },
  ];
  const analyse = (records, until) => {
    const requests = requestsAt(records).map(r => ({ ...r, timestamp_ms: Date.parse(records[r.source_line - 1].timestamp) }));
    return analyzeSession({ runtime: 'claude', file: '/p.jsonl', agent_id: 'a', requests }, extractEvents('claude', rows(records)), { until: until && Date.parse(until) });
  };
  const view = o => JSON.stringify({ findings: o.findings, turnStats: o.turnStats, calls: o.calls, turns: o.turns, partial: o.partialTurns, requests: o.requests });
  const before = analyse(head, at('02:30')), after = analyse([...head, ...later], at('02:30'));
  assert.equal(view(after), view(before));
  const notice = of(after, 'notice_turn')[0];
  assert.deepEqual([notice.candidate, notice.calls, notice.partial, after.calls, after.partialTurns], [true, 0, true, 0, 1]);
  // An end marker before the cutoff closes the turn; without --until the whole turn counts.
  const ended = analyse([...head, { type: 'system', subtype: 'turn_duration', timestamp: at('02:02') }, ...later], at('02:30'));
  assert.equal(of(ended, 'notice_turn')[0].partial, false);
  const full = analyse([...head, ...later]);
  assert.deepEqual([of(full, 'notice_turn')[0].candidate, of(full, 'notice_turn')[0].calls, full.partialTurns], [false, 1, 0]);
});

test('T2: a tool result written after the cutoff is not linked to an earlier call', () => {
  const at = minute => `2026-10-08T${minute}:00Z`;
  const head = [
    { ...userText('[crew] Message from qa (type answer, id m-abab12345679):\n\nDone.'), timestamp: at('02:00') },
    { ...call('r1', 'i', 'mcp__crew__agent_inbox'), timestamp: at('02:01') },
  ];
  const late = [
    { ...result('i', 'No new messages.'), timestamp: at('03:00') },
    { ...say('r2', 'Ok.'), timestamp: at('03:01') },
  ];
  const analyse = records => {
    const requests = requestsAt(records).map(r => ({ ...r, timestamp_ms: Date.parse(records[r.source_line - 1].timestamp) }));
    return analyzeSession({ runtime: 'claude', file: '/l.jsonl', agent_id: 'a', requests }, extractEvents('claude', rows(records)), { until: Date.parse(at('02:30')) });
  };
  const view = o => JSON.stringify({ findings: o.findings, readSizes: o.readSizes, inboxKinds: o.inboxKinds, calls: o.calls });
  const before = analyse(head), after = analyse([...head, ...late]);
  assert.equal(view(after), view(before));
  assert.equal(of(after, 'inbox_empty').length, 0);
  assert.equal(after.readSizes.length, 0);
});

test('T4: token sums keep per-metric coverage; unreported metrics are null, never 0', () => {
  const bucket = reqs => ({ count: reqs.length, sessions: new Set(['s']), agents: new Set(['a']), result_bytes: 0, reqs: new Map(reqs.map(r => [r, 1])) });
  const unknown = finalize(bucket([{ model: 'gpt-x' }]), new Map());
  assert.deepEqual(Object.values(unknown.tokens), [null, null, null, null, null, null, null]);
  for (const k of ['input_total', 'cache_read', 'cache_write', 'cache_write_5m', 'cache_write_1h', 'uncached_tail', 'output']) assert.equal(tokenCell(unknown, k), 'not reported');
  // Codex-like: no native write counter or TTL split; Claude-like: everything reported.
  const codex = { model: 'gpt-x', input_total_tokens: 100, cache_read_tokens: 80, cache_creation_tokens: null, cache_write_5m_tokens: null, cache_write_1h_tokens: null, uncached_tail_tokens: null, output_tokens: 7 };
  const claude = { model: 'gpt-x', input_total_tokens: 50, cache_read_tokens: 40, cache_creation_tokens: 10, cache_write_5m_tokens: 0, cache_write_1h_tokens: 10, uncached_tail_tokens: 0, output_tokens: null };
  const mixed = finalize(bucket([codex, claude]), new Map());
  assert.deepEqual(mixed.tokens, { input_total: 150, cache_read: 120, cache_write: 10, cache_write_5m: 0, cache_write_1h: 10, uncached_tail: 0, output: 7 });
  assert.deepEqual(mixed.token_coverage.cache_write, { known: 1, unknown: 1 });
  assert.deepEqual([tokenCell(mixed, 'input_total'), tokenCell(mixed, 'cache_write'), tokenCell(mixed, 'output')], ['150', '10 (1 of 2)', '7 (1 of 2)']);
  assert.equal(tokenCell(finalize(bucket([codex]), new Map()), 'cache_write'), 'not reported');
  assert.equal(tokenCell(finalize(bucket([]), new Map()), 'output'), '—');
});
