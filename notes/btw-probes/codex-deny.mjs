// /btw probe for Codex, part two: an aside forked in an observer home with
// the read-only sandbox, approvals off and live web search (codex.mjs shows
// these keep the member's prefix). Are writes and MCP calls refused, and do
// reads still work? The stub asks for a write, an MCP call and a read. Offline.
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/codex-deny');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_CODEX || 'codex';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const home = path.join(base, 'home');
const member = path.join(base, 'member-home');
const observer = path.join(base, 'observer-home');
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
put(path.join(work, 'README.md'), 'READ_CANARY: the aside may read this.\n');
const server = path.join(base, 'server.mjs');
put(server, `import fs from 'node:fs'; import readline from 'node:readline';
const [log] = process.argv.slice(2);
readline.createInterface({ input: process.stdin }).on('line', line => {
  let q; try { q = JSON.parse(line); } catch { return; }
  if (q.method === 'tools/call') fs.appendFileSync(log, 'call ' + q.params?.name + '\\n');
  if (q.id === undefined) return;
  const result = q.method === 'initialize' ? { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'crew', version: '1' } }
    : q.method === 'tools/list' ? { tools: [{ name: 'agent_send', description: 'Send a message to a teammate.', inputSchema: { type: 'object', properties: { to: { type: 'string' }, body: { type: 'string' } } } }] }
    : q.method === 'tools/call' ? { content: [{ type: 'text', text: 'sent' }] } : {};
  console.log(JSON.stringify({ jsonrpc: '2.0', id: q.id, result }));
});
`);
const stub = await startStub();
const mcpLog = path.join(base, 'mcp-calls.log');
const config = `model = "gpt-6"\nmodel_provider = "stub"\n[model_providers.stub]\nname = "Stub"\nbase_url = "${stub.url}/v1"\nwire_api = "responses"\nenv_key = "BTW_API_KEY"\n[features]\nplugins = false\n[mcp_servers.crew]\ncommand = ${JSON.stringify(process.execPath)}\nargs = [${JSON.stringify(server)}, ${JSON.stringify(mcpLog)}]\n[projects.${JSON.stringify(work)}]\ntrust_level = "trusted"\n`;
put(path.join(member, 'config.toml'), config);
put(path.join(observer, 'config.toml'), config);
const run = (label, homeDir, args) => new Promise(resolve => {
  stub.setLabel(label);
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: homeDir, BTW_API_KEY: 'btw-probe-not-a-secret', LANG: 'C.UTF-8', TZ: 'UTC' };
  const child = spawn(binary, ['--no-daemon', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
  let out = '', err = '';
  child.stdout.on('data', d => out += d);
  child.stderr.on('data', d => err += d);
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 90000);
  child.on('close', code => { clearTimeout(timer); put(path.join(base, 'runs', `${label}.stdout`), out); put(path.join(base, 'runs', `${label}.stderr`), err); resolve({ code, out }); });
});
const live = await run('live', member, ['exec', '--skip-git-repo-check', '--json', '--dangerously-bypass-approvals-and-sandbox', 'First task message.']);
const thread = live.out.split('\n').map(l => { try { return JSON.parse(l); } catch { return null; } }).find(e => e?.type === 'thread.started')?.thread_id;
const walk = d => fs.readdirSync(d, { withFileTypes: true }).flatMap(e => e.isDirectory() ? walk(path.join(d, e.name)) : [path.join(d, e.name)]);
const rollout = walk(path.join(member, 'sessions')).find(f => f.includes(thread));
put(path.join(observer, path.relative(member, rollout)), fs.readFileSync(rollout));
stub.script('aside', [
  { name: 'exec_command', input: { cmd: `touch '${path.join(work, 'aside-wrote.txt')}'` } },
  { namespace: 'mcp__crew', name: 'agent_send', input: { to: 'orchestrator', body: 'from an aside' } },
  { name: 'exec_command', input: { cmd: `cat '${path.join(work, 'README.md')}'` } },
]);
const aside = await run('aside', observer, ['exec', '--skip-git-repo-check', '--json', '--sandbox', 'read-only', '-c', 'approval_policy="never"', '-c', 'web_search="live"', 'fork', '--ephemeral', thread, 'BTW side question.']);
// The function call outputs Codex sent back, in order.
const outputs = [...new Map(stub.requests.filter(r => r.label === 'aside' && r.path.includes('/responses')).flatMap(r => r.body.input).filter(i => i.type === 'function_call_output' || i.type === 'mcp_tool_call_output').map(i => [i.call_id, i])).values()];
const report = {
  thread,
  aside_exit: aside.code,
  wrote: fs.existsSync(path.join(work, 'aside-wrote.txt')),
  mcp_calls: fs.existsSync(mcpLog) ? fs.readFileSync(mcpLog, 'utf8').trim() : '',
  outputs: outputs.map(o => JSON.stringify(o.output).slice(0, 300)),
  events: aside.out.split('\n').filter(Boolean).map(l => l.slice(0, 220)),
};
await stub.close();
fs.writeFileSync(path.join(base, 'requests.json'), JSON.stringify(stub.requests, null, 2));
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
