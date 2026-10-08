import assert from 'node:assert/strict';
import { test } from 'node:test';
import { inboxResultKind, inboxEntries, messageIds, classifyInput, isNoActionNotice, extractEvents, analyzeSession, requestCost, normalizeOpening } from './turns.mjs';

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

test('Claude: empty inbox calls after an in-band delivery are wasted and charged their consuming request', () => {
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
  assert.equal(requestCost(requests[0], new Map()), null);
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
  assert.deepEqual(notices.map(f => [f.sub, f.wasted]), [['notice:no_tool_call', true], ['message:status', true]]);
  assert.deepEqual(out.openings.map(o => [o.cls, o.fold]), [['notice:no_tool_call', false], ['message:status', false], ['message:task', false], ['message:status', true], ['message:question', false]]);
  const trips = of(out, 'delivery_round_trips').find(f => f.input_class === 'message:task');
  assert.deepEqual(trips.categories, { crew_read: 0, crew_status: 0, crew_other: 1, non_crew: 1 });
});

test('input-side list cost uses explicit write buckets', () => {
  const prices = new Map([['claude-opus-5-5', { base: 4, w5m: 5, w1h: 8, read: 0.2 }]]);
  // 10 tail × 4 + 20 1h writes × 8 + 70 reads × 0.2 = 214 per million
  assert.equal(requestCost(req(1), prices), 214 / 1e6);
  assert.equal(requestCost(req(1, { cache_write_1h_tokens: null }), prices), (40 + 100 + 14) / 1e6);
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

test('text-only replies to orchestrator/status messages, reminder turns with background work, read sizes', () => {
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
  const text = of(out, 'text_only_reply_turn');
  assert.equal(text.length, 1); assert.match(text[0].sub, /^from orchestrator type message/); assert.equal(text[0].requests.length, 1);
  assert.deepEqual(out.readSizes.map(r => [r.tool, r.bytes]), [['tasks_list', 1000]]);
});
