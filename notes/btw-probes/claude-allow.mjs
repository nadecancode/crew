// /btw probe for Claude Code, part six (review m-aa42af439982): the member's
// project grants Write, Edit and Bash in `.claude/settings.json`, and the
// member's own settings allow its crew tools. Does `dontAsk` still refuse
// them? Which deny rules in the aside's flag settings refuse every acting
// call while keeping the member's tool definitions? Offline.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/claude-allow');
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
// The permissive project, and a user-level allow for good measure.
json(path.join(work, '.claude/settings.json'), { permissions: { allow: ['Write', 'Edit', 'Bash', 'Bash(*)', 'mcp__crew', 'mcp__crew__agent_send', 'WebFetch'] } });
json(path.join(config, 'settings.json'), { permissions: { allow: ['Bash', 'Write'] } });
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
json(path.join(base, 'member.json'), { permissions: { deny: ['AskUserQuestion'] } });
const acting = ['Write', 'Edit', 'MultiEdit', 'NotebookEdit', 'Bash', 'WebFetch', 'WebSearch', 'Agent', 'Task', 'Skill', 'mcp__crew', 'mcp__crew__agent_send'];
const variants = {
  'dontask-only': { permissions: { deny: ['AskUserQuestion'] }, disableAllHooks: true },
  'deny-names': { permissions: { deny: ['AskUserQuestion', ...acting] }, disableAllHooks: true },
  'deny-patterns': { permissions: { deny: ['AskUserQuestion', 'Write(*)', 'Edit(*)', 'NotebookEdit(*)', 'Bash(*)', 'WebFetch(*)', 'mcp__crew__agent_send(*)'] }, disableAllHooks: true },
  // Ask rules outrank allow rules, and dontAsk refuses whatever would ask.
  'ask-names': { permissions: { deny: ['AskUserQuestion'], ask: acting }, disableAllHooks: true },
  // Fail closed: a rule for every tool, whatever the member's sources allow.
  'ask-all': { permissions: { deny: ['AskUserQuestion'], ask: ['*'] }, disableAllHooks: true },
  'ask-all-mcp': { permissions: { deny: ['AskUserQuestion'], ask: ['*', 'mcp__*'] }, disableAllHooks: true },
  'ask-patterns': { permissions: { deny: ['AskUserQuestion'], ask: ['Write(*)', 'Edit(*)', 'MultiEdit(*)', 'NotebookEdit(*)', 'Bash(*)', 'WebFetch(*)', 'WebSearch(*)', 'mcp__crew__agent_send(*)'] }, disableAllHooks: true },
};
for (const [name, settings] of Object.entries(variants)) json(path.join(base, `aside-${name}.json`), settings);

const stub = await startStub();
const env = { PATH: process.env.PATH, HOME: home, LANG: 'C.UTF-8', TZ: 'UTC', CLAUDE_CONFIG_DIR: config, ANTHROPIC_API_KEY: 'btw-probe-not-a-secret', ANTHROPIC_BASE_URL: stub.url, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_AUTOUPDATER: '1', ENABLE_TOOL_SEARCH: 'false' };
const run = (label, args) => new Promise(resolve => {
  stub.setLabel(label);
  const child = spawn(binary, ['--print', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'] });
  const timer = setTimeout(() => child.kill('SIGKILL'), 90000);
  child.on('close', code => { clearTimeout(timer); resolve(code); });
});
const plain = v => JSON.stringify(v ?? null, (k, x) => (k === 'cache_control' ? undefined : x));
const turns = l => stub.requests.filter(r => r.label === l && r.path.includes('/messages') && !r.path.includes('count_tokens') && r.body?.model === 'claude-opus-5-5');
const flags = ['--model', 'claude-opus-5-5', '--mcp-config', path.join(base, 'mcp.json'), '--strict-mcp-config'];
const session = crypto.randomUUID();
await run('live-1', ['--session-id', session, ...flags, '--settings', path.join(base, 'member.json'), '--allowedTools', 'mcp__crew', '--dangerously-skip-permissions', '--', 'First task message.']);
await run('live-2', ['--resume', session, ...flags, '--settings', path.join(base, 'member.json'), '--allowedTools', 'mcp__crew', '--dangerously-skip-permissions', '--', 'Second task message.']);
const live = turns('live-2').at(-1).body;
const report = {};
for (const name of Object.keys(variants)) {
  const label = `aside-${name}`;
  fs.rmSync(mcpLog, { force: true });
  stub.script(label, [
    { model: 'claude-opus-5-5', name: 'Write', input: { file_path: path.join(work, 'aside-wrote.txt'), content: 'x' } },
    { model: 'claude-opus-5-5', name: 'Bash', input: { command: `touch '${path.join(work, 'aside-bash.txt')}'`, description: 'probe' } },
    { model: 'claude-opus-5-5', name: 'mcp__crew__agent_send', input: { to: 'orchestrator', body: 'from an aside' } },
    { model: 'claude-opus-5-5', name: 'Read', input: { file_path: path.join(work, 'README.md') } },
  ]);
  await run(label, ['--resume', session, '--fork-session', '--no-session-persistence', '--output-format', 'json', ...flags, '--settings', path.join(base, `${label}.json`), '--permission-mode', 'dontAsk', '--', 'BTW side question.']);
  const first = turns(label)[0]?.body;
  const results = turns(label).flatMap(r => r.body.messages).flatMap(m => (Array.isArray(m.content) ? m.content.filter(c => c.type === 'tool_result') : []));
  let common = 0; while (first && common < live.messages.length && plain(live.messages[common]) === plain(first.messages[common])) common += 1;
  report[name] = {
    wrote: fs.existsSync(path.join(work, 'aside-wrote.txt')),
    bash_ran: fs.existsSync(path.join(work, 'aside-bash.txt')),
    mcp_calls: fs.existsSync(mcpLog) ? fs.readFileSync(mcpLog, 'utf8').trim() : '',
    results: [...new Map(results.map(r => [r.tool_use_id, r])).values()].map(r => `${r.is_error ? 'refused' : 'ran'}: ${JSON.stringify(r.content).slice(0, 90)}`),
    tools_identical: JSON.stringify(live.tools) === JSON.stringify(first?.tools),
    tools_removed: (live.tools || []).map(t => t.name).filter(n => !(first?.tools || []).some(t => t.name === n)),
    system_identical: JSON.stringify(live.system) === JSON.stringify(first?.system),
    live_messages_repeated: `${common} of ${live.messages.length}`,
  };
  for (const f of ['aside-wrote.txt', 'aside-bash.txt']) fs.rmSync(path.join(work, f), { force: true });
}
await stub.close();
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
