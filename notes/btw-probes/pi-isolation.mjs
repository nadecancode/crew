// /btw probe for pi, part two (review m-8848a6dcb2b3, R2): an operator agent
// directory with an ambient extension (it leaves a marker when loaded) and an
// ambient MCP server in mcp.json (a marker when started). Which aside launch
// keeps both from running before any tool guard can act? Offline: requests
// go to the localhost stub.
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/pi-isolation');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_PI || path.resolve('target/toolbox-audit/pi-runtime/node_modules/.bin/pi');
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const home = path.join(base, 'home');
const agentDir = path.join(base, 'agent');
const sessions = path.join(base, 'member-sessions');
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
const marks = { ext: path.join(base, 'ambient-extension.log'), mcp: path.join(base, 'ambient-mcp.log') };
put(path.join(base, 'ambient.ts'), `import fs from 'node:fs';\nexport default function (pi: any) { fs.appendFileSync(${JSON.stringify(marks.ext)}, 'loaded\\n'); }\n`);
const server = path.join(base, 'server.mjs');
put(server, `import fs from 'node:fs'; import readline from 'node:readline';
fs.appendFileSync(${JSON.stringify(marks.mcp)}, 'started\\n');
readline.createInterface({ input: process.stdin }).on('line', line => {
  let q; try { q = JSON.parse(line); } catch { return; }
  if (q.id === undefined) return;
  const result = q.method === 'initialize' ? { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'ambient', version: '1' } }
    : q.method === 'tools/list' ? { tools: [{ name: 'lookup', description: 'Ambient tool.', inputSchema: { type: 'object', properties: {} } }] } : {};
  console.log(JSON.stringify({ jsonrpc: '2.0', id: q.id, result }));
});
`);
put(path.join(base, 'guard.ts'), `export default function (pi: any) {\n  pi.on('tool_call', async (event: any) => ({ block: true, reason: 'An aside cannot act: ' + event.toolName + ' is refused.' }));\n}\n`);
const stub = await startStub();
json(path.join(agentDir, 'models.json'), { providers: { stub: { baseUrl: stub.url, api: 'anthropic-messages', apiKey: 'btw-probe-not-a-secret', models: [{ id: 'claude-opus-5-5', contextWindow: 200000, maxTokens: 8192 }] } } });
json(path.join(agentDir, 'settings.json'), { extensions: [path.join(base, 'ambient.ts')] });
json(path.join(agentDir, 'mcp.json'), { mcpServers: { ambient: { command: process.execPath, args: [server] } } });
const run = (label, args) => new Promise(resolve => {
  stub.setLabel(label);
  const env = { PATH: process.env.PATH, HOME: home, PI_CODING_AGENT_DIR: agentDir, PI_OFFLINE: '1', LANG: 'C.UTF-8', TZ: 'UTC' };
  const child = spawn(process.execPath, [binary, '--print', '--provider', 'stub', '--model', 'claude-opus-5-5', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
  let out = '', err = '';
  child.stdout.on('data', d => out += d); child.stderr.on('data', d => err += d);
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 60000);
  child.on('close', code => { clearTimeout(timer); resolve({ code, out: out.slice(0, 120), err: err.slice(-200) }); });
});
const lines = f => (fs.existsSync(f) ? fs.readFileSync(f, 'utf8').trim().split('\n').filter(Boolean) : []);
const reset = () => { for (const f of Object.values(marks)) fs.rmSync(f, { force: true }); };
const report = {};
await run('member', ['--session-dir', sessions, '--session-id', 'btwiso1', 'First task message.']);
report.member_default = { extension: lines(marks.ext), mcp: lines(marks.mcp) };
const file = fs.readdirSync(sessions).map(f => path.join(sessions, f)).find(f => f.endsWith('.jsonl'));
const variants = {
  'no-extensions + builtin:mcp': ['--no-extensions', '-e', 'builtin:mcp', '-e', path.join(base, 'guard.ts')],
  'no-extensions': ['--no-extensions', '-e', path.join(base, 'guard.ts')],
  'no-mcp': ['--no-extensions', '--no-mcp', '-e', path.join(base, 'guard.ts')],
};
for (const [name, extra] of Object.entries(variants)) {
  reset();
  const r = await run(name, ['--session-dir', path.join(base, 'aside', name.replace(/\W+/g, '-')), '--fork', file, ...extra, 'BTW side question.']);
  await new Promise(res => setTimeout(res, 500));
  const body = stub.requests.filter(q => q.label === name && q.path.includes('/messages')).at(-1)?.body;
  report[name] = { exit: r.code, err: r.err, ambient_extension: lines(marks.ext), ambient_mcp: lines(marks.mcp), tools: (body?.tools || []).map(t => t.name) };
}
await stub.close();
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
