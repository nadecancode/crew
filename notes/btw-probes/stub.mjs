// A localhost model endpoint for the /btw probes. It records every request
// body and answers with a fixed, well-formed reply in the provider's own
// streaming format, so a CLI completes its turn and writes its session
// without any provider call. Nothing is forwarded anywhere.
import http from 'node:http';
import crypto from 'node:crypto';

const sse = events => events.map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`).join('');

// Anthropic Messages API: a text reply, or a JSON message when not streaming.
// `tool` instead asks for one tool call: { name, input }.
function anthropic(body, text, tool) {
  const usage = { input_tokens: 11, output_tokens: 3, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 };
  const message = { id: 'msg_btw_probe', type: 'message', role: 'assistant', model: body.model, content: [], stop_reason: null, stop_sequence: null, usage };
  if (tool && body.stream) {
    const id = `toolu_btw_${crypto.randomUUID().replaceAll('-', '').slice(0, 20)}`;
    return {
      type: 'text/event-stream',
      body: sse([
        ['message_start', { type: 'message_start', message }],
        ['content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'tool_use', id, name: tool.name, input: {} } }],
        ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'input_json_delta', partial_json: JSON.stringify(tool.input) } }],
        ['content_block_stop', { type: 'content_block_stop', index: 0 }],
        ['message_delta', { type: 'message_delta', delta: { stop_reason: 'tool_use', stop_sequence: null }, usage: { output_tokens: 3 } }],
        ['message_stop', { type: 'message_stop' }],
      ]),
    };
  }
  if (!body.stream) {
    return { type: 'application/json', body: JSON.stringify({ ...message, content: [{ type: 'text', text }], stop_reason: 'end_turn' }) };
  }
  return {
    type: 'text/event-stream',
    body: sse([
      ['message_start', { type: 'message_start', message }],
      ['content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }],
      ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text } }],
      ['content_block_stop', { type: 'content_block_stop', index: 0 }],
      ['message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 3 } }],
      ['message_stop', { type: 'message_stop' }],
    ]),
  };
}

// OpenAI Responses API (Codex): one assistant message, then completed.
// `tool` instead asks for one function call: { name, input }.
function responses(body, text, tool) {
  const id = `resp_btw_${Date.now()}`;
  const item = tool
    ? { type: 'function_call', id: `fc_${id}`, call_id: `call_${id}`, ...(tool.namespace ? { namespace: tool.namespace } : {}), name: tool.name, arguments: JSON.stringify(tool.input), status: 'completed' }
    : { type: 'message', id: `msg_${id}`, role: 'assistant', status: 'completed', content: [{ type: 'output_text', text, annotations: [] }] };
  const usage = { input_tokens: 11, input_tokens_details: { cached_tokens: 0 }, output_tokens: 3, output_tokens_details: { reasoning_tokens: 0 }, total_tokens: 14 };
  const response = { id, object: 'response', created_at: Math.floor(Date.now() / 1000), model: body.model, status: 'in_progress', output: [] };
  return {
    type: 'text/event-stream',
    body: sse([
      ['response.created', { type: 'response.created', response }],
      ['response.output_item.added', { type: 'response.output_item.added', output_index: 0, item: { ...item, status: 'in_progress', content: [] } }],
      ...(tool ? [] : [['response.output_text.delta', { type: 'response.output_text.delta', item_id: item.id, output_index: 0, content_index: 0, delta: text }]]),
      ['response.output_item.done', { type: 'response.output_item.done', output_index: 0, item }],
      ['response.completed', { type: 'response.completed', response: { ...response, status: 'completed', output: [item], usage } }],
    ]),
  };
}

/// Start the stub. `label()` names the step each request belongs to; a held
/// request is answered only when `release()` is called.
export async function startStub() {
  const requests = [];
  let label = 'unlabelled';
  let hold = false;
  const held = [];
  let count = 0;
  // Per step, the tool calls to ask for, one per model turn, before text.
  const scripts = new Map();
  const server = http.createServer((req, res) => {
    let raw = '';
    req.on('data', chunk => raw += chunk);
    req.on('end', () => {
      let body;
      try { body = JSON.parse(raw); } catch { body = raw; }
      const entry = { label, method: req.method, path: req.url, body, at: Date.now() };
      requests.push(entry);
      const answer = () => {
        count += 1;
        const text = `stub reply ${count} (${entry.label})`;
        let reply;
        if (req.url.includes('/count_tokens')) reply = { type: 'application/json', body: JSON.stringify({ input_tokens: 11 }) };
        else if (req.url.includes('/responses')) {
          const queue = scripts.get(entry.label);
          reply = responses(body, text, queue?.length ? queue.shift() : undefined);
        }
        else if (req.url.includes('/messages')) {
          // A scripted call goes to the step's main model only, not to side requests.
          const queue = scripts.get(entry.label);
          const tool = body?.stream && queue?.length && (!queue[0].model || queue[0].model === body.model) ? queue.shift() : undefined;
          reply = anthropic(body, text, tool);
        }
        else reply = { type: 'application/json', body: JSON.stringify({ data: [], object: 'list' }) };
        if (!res.destroyed) {
          res.writeHead(200, { 'content-type': reply.type });
          res.end(reply.body);
        }
      };
      const turn = (req.url.includes('/messages') && !req.url.includes('/count_tokens')) || req.url.includes('/responses');
      if (hold && turn) held.push(answer);
      else answer();
    });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return {
    url: `http://127.0.0.1:${server.address().port}`,
    requests,
    setLabel(next) { label = next; },
    script(step, tools) { scripts.set(step, [...tools]); },
    holdNext(on = true) { hold = on; },
    heldCount() { return held.length; },
    release() { hold = false; while (held.length) held.shift()(); },
    async close() { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); },
  };
}
