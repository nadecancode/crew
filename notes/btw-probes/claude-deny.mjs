// /btw probe for Claude Code, part three: with the aside settings of
// claude-member.mjs (no hooks, an observer MCP server, permission mode
// dontAsk), does the fork refuse every acting tool and still read? The stub
// asks for Write, Bash, an MCP call and Read in turn. Offline.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/claude-deny');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_CLAUDE || 'claude';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const home = path.join(base, 'home');
const config = path.join(home, '.claude');
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
put(path.join(work, 'README.md'), 'READ_CANARY: the aside may read this.\n');
json(path.join(config, '.claude.json'), { hasCompletedOnboarding: true, bypassPermissionsModeAccepted: true, projects: { [work]: { hasTrustDialogAccepted: true } } });
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
const mcpLog = path.join(base, 'mcp-calls.log');
json(path.join(base, 'mcp.json'), { mcpServers: { crew: { command: process.execPath, args: [server, mcpLog] } } });
json(path.join(base, 'settings.json'), { permissions: { deny: ['AskUserQuestion'] } });

const stub = await startStub();
const env = { PATH: process.env.PATH, HOME: home, LANG: 'C.UTF-8', TZ: 'UTC', CLAUDE_CONFIG_DIR: config, ANTHROPIC_API_KEY: 'btw-probe-not-a-secret', ANTHROPIC_BASE_URL: stub.url, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_AUTOUPDATER: '1', ENABLE_TOOL_SEARCH: 'false' };
const run = (label, args) => new Promise(resolve => {
  stub.setLabel(label);
  const child = spawn(binary, ['--print', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '';
  child.stdout.on('data', d => out += d);
  const timer = setTimeout(() => child.kill('SIGKILL'), 90000);
  child.on('close', code => { clearTimeout(timer); resolve({ code, out }); });
});
const session = crypto.randomUUID();
const flags = ['--model', 'claude-opus-5-5', '--mcp-config', path.join(base, 'mcp.json'), '--strict-mcp-config', '--settings', path.join(base, 'settings.json')];
await run('live', ['--session-id', session, ...flags, '--dangerously-skip-permissions', '--', 'First task message.']);
const report = {};
for (const mode of ['dontAsk', 'default']) {
  const label = `fork-${mode}`;
  fs.rmSync(mcpLog, { force: true });
  stub.script(label, [
    { model: 'claude-opus-5-5', name: 'Write', input: { file_path: path.join(work, 'aside-wrote.txt'), content: 'written by an aside' } },
    { model: 'claude-opus-5-5', name: 'Bash', input: { command: `touch '${path.join(work, 'aside-bash.txt')}'`, description: 'probe' } },
    { model: 'claude-opus-5-5', name: 'mcp__crew__agent_send', input: { to: 'orchestrator', body: 'from an aside' } },
    { model: 'claude-opus-5-5', name: 'Read', input: { file_path: path.join(work, 'README.md') } },
  ]);
  const extra = mode === 'default' ? ['--permission-prompts', 'none'] : ['--permission-mode', mode];
  const result = await run(label, ['--resume', session, '--fork-session', '--no-session-persistence', '--output-format', 'json', ...flags, ...extra, '--', 'BTW side question.']);
  // Each tool result the CLI sent back, in order.
  const results = stub.requests.filter(r => r.label === label && r.body?.model === 'claude-opus-5-5')
    .flatMap(r => r.body.messages.at(-2)?.role === 'user' || r.body.messages.at(-1)?.role === 'user' ? r.body.messages : [])
    .flatMap(m => Array.isArray(m.content) ? m.content.filter(c => c.type === 'tool_result') : []);
  const unique = [...new Map(results.map(r => [r.tool_use_id, r])).values()];
  report[mode] = {
    exit: result.code,
    wrote: fs.existsSync(path.join(work, 'aside-wrote.txt')),
    bash_ran: fs.existsSync(path.join(work, 'aside-bash.txt')),
    mcp_calls: fs.existsSync(mcpLog) ? fs.readFileSync(mcpLog, 'utf8').trim() : '',
    tool_results: unique.map(r => ({ is_error: r.is_error ?? false, text: JSON.stringify(r.content).slice(0, 200) })),
  };
  for (const f of ['aside-wrote.txt', 'aside-bash.txt']) fs.rmSync(path.join(work, f), { force: true });
}
await stub.close();
fs.writeFileSync(path.join(base, 'requests.json'), JSON.stringify(stub.requests, null, 2));
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
