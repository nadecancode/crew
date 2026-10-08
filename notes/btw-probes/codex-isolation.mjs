// /btw probe for Codex, part four (review m-8848a6dcb2b3, R2): a member
// whose CODEX_HOME holds the operator's own configuration (an ambient MCP
// server and a user hook, each leaving a marker when it runs), forked in that
// home because the login lives there. With `features.hooks=false`, the ambient
// server disabled by name and crew's server replaced by a stand-in, does
// anything ambient start? Does the request still match the member's? And
// beyond file bytes, does the fork change any row in Codex's SQLite state?
// Offline: requests go to the localhost stub. Needs sqlite3 on PATH.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn, execFileSync } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/codex-isolation');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_CODEX || 'codex';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const home = path.join(base, 'home');
const codexHome = path.join(base, 'codex-home');
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
const server = path.join(base, 'server.mjs');
put(server, `import fs from 'node:fs'; import readline from 'node:readline';
const [marker, name] = process.argv.slice(2);
fs.appendFileSync(marker, 'started\\n');
readline.createInterface({ input: process.stdin }).on('line', line => {
  let q; try { q = JSON.parse(line); } catch { return; }
  if (q.method === 'tools/call') fs.appendFileSync(marker, 'call ' + q.params?.name + '\\n');
  if (q.id === undefined) return;
  const result = q.method === 'initialize' ? { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name, version: '1' } }
    : q.method === 'tools/list' ? { tools: [{ name: name === 'crew' ? 'agent_send' : 'lookup', description: name + ' tool.', inputSchema: { type: 'object', properties: { q: { type: 'string' } } } }] }
    : q.method === 'tools/call' ? { isError: true, content: [{ type: 'text', text: 'refused' }] } : {};
  console.log(JSON.stringify({ jsonrpc: '2.0', id: q.id, result }));
});
`);
const marks = { ambient: path.join(base, 'ambient-mcp.log'), crewLive: path.join(base, 'crew-live.log'), crewAside: path.join(base, 'crew-aside.log'), hook: path.join(base, 'user-hook.log') };
const stub = await startStub();
put(path.join(codexHome, 'config.toml'), `model = "gpt-6"\nmodel_provider = "stub"\n[model_providers.stub]\nname = "Stub"\nbase_url = "${stub.url}/v1"\nwire_api = "responses"\nenv_key = "BTW_API_KEY"\n[features]\nplugins = false\n[mcp_servers.ambient]\ncommand = ${JSON.stringify(process.execPath)}\nargs = [${JSON.stringify(server)}, ${JSON.stringify(marks.ambient)}, "ambient"]\n[projects.${JSON.stringify(work)}]\ntrust_level = "trusted"\n`);
json(path.join(codexHome, 'hooks.json'), { hooks: { SessionStart: [{ hooks: [{ type: 'command', command: `echo hook >> '${marks.hook}'` }] }], UserPromptSubmit: [{ hooks: [{ type: 'command', command: `echo prompt >> '${marks.hook}'` }] }] } });

const run = (label, args) => new Promise(resolve => {
  stub.setLabel(label);
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: codexHome, BTW_API_KEY: 'btw-probe-not-a-secret', LANG: 'C.UTF-8', TZ: 'UTC' };
  const child = spawn(binary, ['--no-daemon', 'exec', '--skip-git-repo-check', '--json', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
  let out = '';
  child.stdout.on('data', d => out += d);
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 60000);
  child.on('close', code => { clearTimeout(timer); resolve({ code, out }); });
});
const lines = f => (fs.existsSync(f) ? fs.readFileSync(f, 'utf8').trim().split('\n').filter(Boolean) : []);
// The member as crew launches it: crew's server, the ambient one disabled by
// name (the toolbox closing step), hooks as the operator has them.
const crewServer = log => ['-c', `mcp_servers.crew.command=${JSON.stringify(process.execPath)}`, '-c', `mcp_servers.crew.args=${JSON.stringify([server, log, 'crew'])}`];
const member = [...crewServer(marks.crewLive), '-c', 'mcp_servers.ambient.enabled=false', '--dangerously-bypass-approvals-and-sandbox', '--dangerously-bypass-hook-trust'];
const live1 = await run('live-1', [...member, 'First task message.']);
const thread = live1.out.split('\n').map(l => { try { return JSON.parse(l); } catch { return null; } }).find(e => e?.type === 'thread.started')?.thread_id;
await run('live-2', [...member, 'resume', thread, 'Second task message.']);
const report = { thread, member_runs: { hook_lines: lines(marks.hook), ambient: lines(marks.ambient), crew: lines(marks.crewLive) } };

// Logical contents of every SQLite file, rows only (no file bytes).
const dbs = () => fs.readdirSync(codexHome).filter(f => f.endsWith('.sqlite')).sort();
const dump = () => Object.fromEntries(dbs().map(db => [db, execFileSync('sqlite3', [path.join(codexHome, db), '.dump'], { encoding: 'utf8' })]));
const before = dump();
for (const f of [marks.hook, marks.ambient]) fs.rmSync(f, { force: true });
// The aside: the member's flags, with hooks off, the stand-in for crew and
// the read-only sandbox with approvals off.
const aside = [...crewServer(marks.crewAside), '-c', 'mcp_servers.ambient.enabled=false', '-c', 'features.hooks=false', '-c', 'features.memories=false',
  '--sandbox', 'read-only', '-c', 'approval_policy="never"', '-c', 'web_search="live"'];
// The member ran in full access; the aside's sandbox changes its permissions
// message, not the tools (codex.mjs). Compare tools and the history.
await run('aside', [...aside, 'fork', '--ephemeral', thread, 'BTW side question.']);
const after = dump();
const diff = {};
for (const db of new Set([...Object.keys(before), ...Object.keys(after)])) {
  const a = new Set((before[db] || '').split('\n')), b = new Set((after[db] || '').split('\n'));
  const added = [...b].filter(l => !a.has(l)), removed = [...a].filter(l => !b.has(l));
  if (added.length || removed.length) diff[db] = { added: added.map(l => l.slice(0, 200)), removed: removed.map(l => l.slice(0, 200)) };
}
const last = l => stub.requests.filter(r => r.label === l && r.path.includes('/responses')).at(-1)?.body;
// An input item without its own top-level id; tools are compared whole.
const strip = v => JSON.stringify(v && typeof v === 'object' && !Array.isArray(v) ? (({ id, ...rest }) => rest)(v) : v ?? null);
const live = last('live-2'), fork = last('aside');
let common = 0; while (common < live.input.length && strip(live.input[common]) === strip(fork.input[common])) common += 1;
report.aside = {
  hook_lines: lines(marks.hook), ambient_server: lines(marks.ambient), crew_stand_in: lines(marks.crewAside),
  tools_identical: JSON.stringify(live.tools) === JSON.stringify(fork.tools),
  tools: { live: live.tools.map(t => t.name || t.type), aside: fork.tools.map(t => t.name || t.type) },
  instructions_identical: live.instructions === fork.instructions,
  live_items_repeated: `${common} of ${live.input.length}`,
  cache_key_is_parent: fork.prompt_cache_key === thread,
  sqlite_logical_changes: diff,
};

json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));

// In place, the fork reads the rollout when it starts. A member writing at
// that moment can leave a half-written last line: does the fork still start,
// with the history before it? (This home is a throwaway; the line is removed
// afterwards.)
{
  const walk = d => fs.readdirSync(d, { withFileTypes: true }).flatMap(e => (e.isDirectory() ? walk(path.join(d, e.name)) : [path.join(d, e.name)]));
  const rollout = walk(path.join(codexHome, 'sessions')).find(f => f.includes(thread));
  const whole = fs.readFileSync(rollout);
  fs.appendFileSync(rollout, '{"timestamp":"2026-10-08T00:00:00.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"half');
  const r = await run('aside-partial', [...aside, 'fork', '--ephemeral', thread, 'BTW side question.']);
  const body = stub.requests.filter(q => q.label === 'aside-partial' && q.path.includes('/responses')).at(-1)?.body;
  fs.writeFileSync(rollout, whole);
  report.partial_last_line = { exit: r.code, request_sent: Boolean(body), history_items: body?.input.length, mentions_half: JSON.stringify(body ?? '').includes('"half') };
  console.log(JSON.stringify({ partial_last_line: report.partial_last_line }));
  json(path.join(base, 'report.json'), report);
}
await stub.close();
