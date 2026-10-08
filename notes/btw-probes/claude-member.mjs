// /btw probe for Claude Code, part two: a member set up the way crew sets one
// up (an MCP server named crew, hooks in its settings), forked with aside
// settings that keep the member's state untouched. Which aside settings keep
// the live request's tools and system prompt, so the fork reads the member's
// cache? Offline: requests go to the localhost stub. Run from the repository
// root after claude.mjs is not required; this probe is independent.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/claude-member');
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
json(path.join(config, '.claude.json'), { hasCompletedOnboarding: true, bypassPermissionsModeAccepted: true, projects: { [work]: { hasTrustDialogAccepted: true } } });

// An MCP server with crew-like tools. The live member's and the observer's
// copies serve the same definitions; each records that it started and every
// call made to it.
const server = path.join(base, 'server.mjs');
put(server, `import fs from 'node:fs'; import readline from 'node:readline';
const [marker] = process.argv.slice(2);
fs.appendFileSync(marker, 'started\\n');
const tools = [
  { name: 'agent_send', description: 'Send a message to a teammate.', inputSchema: { type: 'object', properties: { to: { type: 'string' }, body: { type: 'string' } }, required: ['to', 'body'] } },
  { name: 'task_complete', description: 'Finish your current task.', inputSchema: { type: 'object', properties: { result: { type: 'string' } }, required: ['result'] } },
];
readline.createInterface({ input: process.stdin }).on('line', line => {
  let q; try { q = JSON.parse(line); } catch { return; }
  if (q.method === 'tools/call') fs.appendFileSync(marker, 'call ' + q.params?.name + '\\n');
  if (q.id === undefined) return;
  const result = q.method === 'initialize' ? { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'crew', version: '1' } }
    : q.method === 'tools/list' ? { tools } : q.method === 'tools/call' ? { isError: true, content: [{ type: 'text', text: 'An aside cannot act.' }] } : {};
  console.log(JSON.stringify({ jsonrpc: '2.0', id: q.id, result }));
});
`);
const liveMarker = path.join(base, 'live-crew-mcp.log');
const observerMarker = path.join(base, 'observer-crew-mcp.log');
json(path.join(base, 'live-mcp.json'), { mcpServers: { crew: { command: process.execPath, args: [server, liveMarker] } } });
json(path.join(base, 'observer-mcp.json'), { mcpServers: { crew: { command: process.execPath, args: [server, observerMarker] } } });
json(path.join(base, 'no-mcp.json'), { mcpServers: {} });
// Hooks as crew installs them: every one reports to the daemon. Here each
// appends its event to a file.
const hookLog = path.join(base, 'hooks.log');
const hook = event => ({ hooks: [{ type: 'command', command: `echo ${event} >> '${hookLog}'` }] });
json(path.join(base, 'live-settings.json'), { permissions: { deny: ['AskUserQuestion'] }, hooks: { SessionStart: [hook('SessionStart')], UserPromptSubmit: [hook('UserPromptSubmit')], Stop: [hook('Stop')] } });
json(path.join(base, 'aside-settings.json'), { permissions: { deny: ['AskUserQuestion'] } });
put(path.join(base, 'briefing.md'), 'BTW_BRIEFING_CANARY: standing instructions for the probe member.\n');

const stub = await startStub();
const runs = {};
function run(label, args, extraEnv = {}) {
  stub.setLabel(label);
  const env = {
    PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: path.join(home, '.config'), LANG: 'C.UTF-8', TZ: 'UTC',
    CLAUDE_CONFIG_DIR: config, ANTHROPIC_API_KEY: 'btw-probe-not-a-secret', ANTHROPIC_BASE_URL: stub.url,
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_AUTOUPDATER: '1', ...extraEnv,
  };
  const child = spawn(binary, ['--print', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', d => stdout += d);
  child.stderr.on('data', d => stderr += d);
  const timer = setTimeout(() => child.kill('SIGKILL'), 60000);
  return new Promise(resolve => child.on('close', (code, signal) => {
    clearTimeout(timer);
    put(path.join(base, 'runs', `${label}.stdout`), stdout);
    put(path.join(base, 'runs', `${label}.stderr`), stderr);
    runs[label] = { code, signal };
    resolve({ code, stdout });
  }));
}
const read = file => (fs.existsSync(file) ? fs.readFileSync(file, 'utf8').trim().split('\n').filter(Boolean) : []);
const plain = value => JSON.parse(JSON.stringify(value ?? null, (k, v) => (k === 'cache_control' ? undefined : v)));
const same = (a, b) => JSON.stringify(plain(a)) === JSON.stringify(plain(b));
const last = label => stub.requests.filter(r => r.label === label && r.path.includes('/messages') && !r.path.includes('count_tokens') && r.body?.model === 'claude-opus-5-5').at(-1)?.body;
function compare(live, fork) {
  if (!live || !fork) return { missing: !live ? 'live' : 'fork' };
  let common = 0;
  while (common < live.messages.length && same(live.messages[common], fork.messages[common])) common += 1;
  return {
    tools_identical: same(live.tools, fork.tools),
    tool_names_removed: (live.tools || []).map(t => t.name).filter(n => !(fork.tools || []).some(t => t.name === n)),
    tool_names_added: (fork.tools || []).map(t => t.name).filter(n => !(live.tools || []).some(t => t.name === n)),
    system_identical: same(live.system, fork.system),
    // The live request's messages that the fork repeats unchanged; the last
    // live message is the per-request token note, rewritten each turn.
    live_messages_repeated: `${common} of ${live.messages.length}`,
    params_differing: Object.keys({ ...live, ...fork }).filter(k => !['messages', 'tools', 'system', 'metadata'].includes(k) && !same(live[k], fork[k])),
    fork_tail: fork.messages.slice(common).map(m => `${m.role}: ${JSON.stringify(m.content).slice(0, 160)}`),
  };
}

const report = { runs };
for (const search of ['true', 'false']) {
  // With tool search (the first-party default) MCP tools are deferred; without
  // it (a gateway endpoint) their definitions are in the tools prefix.
  const extraEnv = { ENABLE_TOOL_SEARCH: search };
  const session = crypto.randomUUID();
  for (const f of [liveMarker, observerMarker, hookLog]) fs.rmSync(f, { force: true });
  const member = ['--model', 'claude-opus-5-5', '--append-system-prompt-file', path.join(base, 'briefing.md')];
  const live = ['--mcp-config', path.join(base, 'live-mcp.json'), '--strict-mcp-config', '--settings', path.join(base, 'live-settings.json'), '--dangerously-skip-permissions', '--allowedTools', 'mcp__crew'];
  await run(`search-${search}-live-1`, ['--session-id', session, ...member, ...live, '--', 'First task message.'], extraEnv);
  await run(`search-${search}-live-2`, ['--resume', session, ...member, ...live, '--', 'Second task message.'], extraEnv);
  const liveCalls = read(liveMarker).length;
  const hooksLive = read(hookLog).length;
  const aside = (mcp, mode) => ['--resume', session, '--fork-session', '--no-session-persistence', '--output-format', 'json', '--max-budget-usd', '0.50',
    ...member, '--mcp-config', path.join(base, mcp), '--strict-mcp-config', '--settings', path.join(base, 'aside-settings.json'), ...mode, '--', 'BTW side question: what are you working on?'];
  const forks = {
    'same-flags': ['--resume', session, '--fork-session', '--no-session-persistence', '--output-format', 'json', ...member, ...live, '--', 'BTW side question: what are you working on?'],
    'aside-dontask': aside('observer-mcp.json', ['--permission-mode', 'dontAsk']),
    'aside-default-noprompts': aside('observer-mcp.json', ['--permission-prompts', 'none']),
    'aside-plan': aside('observer-mcp.json', ['--permission-mode', 'plan']),
    'aside-no-mcp': aside('no-mcp.json', ['--permission-mode', 'dontAsk']),
  };
  const out = { live_mcp_marker_lines: liveCalls, hook_lines_after_live: hooksLive };
  for (const [name, args] of Object.entries(forks)) {
    for (const f of [observerMarker, hookLog]) fs.rmSync(f, { force: true });
    const before = read(liveMarker).length;
    const result = await run(`search-${search}-${name}`, args, extraEnv);
    let parsed = null; try { parsed = JSON.parse(result.stdout); } catch {}
    out[name] = {
      exit: result.code,
      result: parsed?.result, is_error: parsed?.is_error, subtype: parsed?.subtype,
      hooks_fired: read(hookLog),
      live_crew_mcp_started_again: read(liveMarker).length > before,
      observer_crew_mcp: read(observerMarker),
      vs_live: compare(last(`search-${search}-live-2`), last(`search-${search}-${name}`)),
    };
  }
  report[`tool_search_${search}`] = out;
}
await stub.close();
fs.writeFileSync(path.join(base, 'requests.json'), JSON.stringify(stub.requests, null, 2));
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
