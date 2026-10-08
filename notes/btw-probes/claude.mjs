// /btw probe for Claude Code: can a side question be answered from a fork of
// a member's session without touching that session, and does the fork's
// request share the live session's cache prefix? Offline: every request goes
// to the localhost stub (stub.mjs). Run from the repository root.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/claude');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_CLAUDE || 'claude';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const sha = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');

const home = path.join(base, 'home');
const config = path.join(home, '.claude');
const observer = path.join(base, 'observer/.claude');
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
put(path.join(work, 'README.md'), 'btw probe repository\n');
const state = { hasCompletedOnboarding: true, bypassPermissionsModeAccepted: true, projects: { [work]: { hasTrustDialogAccepted: true } } };
json(path.join(config, '.claude.json'), state);
json(path.join(observer, '.claude.json'), state);
// What crew writes per member: an MCP config, settings and a briefing file.
const member = path.join(base, 'member');
json(path.join(member, 'mcp.json'), { mcpServers: {} });
json(path.join(member, 'settings.json'), { permissions: { deny: ['AskUserQuestion'] } });
put(path.join(member, 'briefing.md'), 'BTW_BRIEFING_CANARY: standing instructions for the probe member.\n');

const stub = await startStub();
const session = crypto.randomUUID();
const common = ['--model', 'claude-opus-5-5', '--mcp-config', path.join(member, 'mcp.json'), '--strict-mcp-config',
  '--settings', path.join(member, 'settings.json'), '--append-system-prompt-file', path.join(member, 'briefing.md'),
  '--allowedTools', 'mcp__crew'];
const env = dir => ({
  PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: path.join(home, '.config'), LANG: 'C.UTF-8', TZ: 'UTC',
  CLAUDE_CONFIG_DIR: dir, ANTHROPIC_API_KEY: 'btw-probe-not-a-secret', ANTHROPIC_BASE_URL: stub.url,
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_AUTOUPDATER: '1', ENABLE_TOOL_SEARCH: 'false',
});

const runs = {};
function run(label, args, dir = config) {
  stub.setLabel(label);
  const child = spawn(binary, ['--print', ...args], { cwd: work, env: env(dir), stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', d => stdout += d);
  child.stderr.on('data', d => stderr += d);
  const timer = setTimeout(() => child.kill('SIGKILL'), 60000);
  return new Promise(resolve => child.on('close', (code, signal) => {
    clearTimeout(timer);
    put(path.join(base, 'runs', `${label}.stdout`), stdout);
    put(path.join(base, 'runs', `${label}.stderr`), stderr);
    runs[label] = { code, signal };
    resolve({ code, signal, stdout, stderr });
  }));
}

// Every file under a config directory, with its hash: what a fork changed.
function tree(dir) {
  const out = {};
  const walk = d => {
    for (const entry of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, entry.name);
      if (entry.isDirectory()) walk(p);
      else if (entry.isFile()) out[path.relative(dir, p)] = sha(p);
    }
  };
  if (fs.existsSync(dir)) walk(dir);
  return out;
}
function changed(before, after) {
  const keys = new Set([...Object.keys(before), ...Object.keys(after)]);
  return [...keys].filter(k => before[k] !== after[k]).map(k => `${before[k] ? (after[k] ? 'changed' : 'removed') : 'added'} ${k}`).sort();
}
const sessionFile = dir => {
  const projects = path.join(dir, 'projects');
  for (const project of fs.existsSync(projects) ? fs.readdirSync(projects) : []) {
    const file = path.join(projects, project, `${session}.jsonl`);
    if (fs.existsSync(file)) return file;
  }
  return null;
};

// The model turns of a step: Messages API requests with the probe's model.
const turns = label => stub.requests.filter(r => r.label === label && r.path.includes('/messages') && !r.path.includes('count_tokens') && r.body?.model === 'claude-opus-5-5');
const last = label => turns(label).at(-1)?.body;
// Strip cache markers: they move between requests and are not prefix content.
const plain = value => JSON.parse(JSON.stringify(value ?? null, (k, v) => (k === 'cache_control' ? undefined : v)));
const same = (a, b) => JSON.stringify(plain(a)) === JSON.stringify(plain(b));
function compare(live, fork) {
  if (!live || !fork) return { missing: !live ? 'live' : 'fork' };
  let common = 0;
  while (common < live.messages.length && common < fork.messages.length && same(live.messages[common], fork.messages[common])) common += 1;
  const params = Object.keys({ ...live, ...fork }).filter(k => !['messages', 'tools', 'system'].includes(k) && !same(live[k], fork[k]));
  const systemDiff = (live.system || []).map((block, i) => same(block, fork.system?.[i]) ? null : i).filter(i => i !== null);
  return {
    tools_identical: same(live.tools, fork.tools),
    tool_names: { live: (live.tools || []).length, fork: (fork.tools || []).length, removed: (live.tools || []).map(t => t.name).filter(n => !(fork.tools || []).some(t => t.name === n)) },
    system_identical: same(live.system, fork.system),
    system_blocks_differing: systemDiff,
    live_messages: live.messages.length,
    fork_messages: fork.messages.length,
    leading_messages_identical: common,
    params_differing: params,
    fork_last_user: JSON.stringify(fork.messages.at(-1)).slice(0, 300),
  };
}

const results = {};
// 1. The live member: a session with two turns.
results.live1 = await run('live-1', ['--session-id', session, ...common, '--dangerously-skip-permissions', '--', 'First task message.']);
results.live2 = await run('live-2', ['--resume', session, ...common, '--dangerously-skip-permissions', '--', 'Second task message.']);
const liveFile = sessionFile(config);
const liveHash = liveFile && sha(liveFile);
const before = tree(config);

// 2. Forks with the member's own flags; then with tools restricted.
const fork = (label, extra, dir) => run(label, ['--resume', session, '--fork-session', '--no-session-persistence', '--output-format', 'json', ...common, ...extra, '--', 'BTW side question: what are you working on?'], dir);
results.forkSame = await fork('fork-same', ['--dangerously-skip-permissions']);
const afterSame = tree(config);
const liveUnchangedBySameFork = Boolean(liveFile) && liveHash === sha(liveFile);
results.forkPersist = await run('fork-persist', ['--resume', session, '--fork-session', '--output-format', 'json', ...common, '--dangerously-skip-permissions', '--', 'BTW side question, persisted fork.']);
const afterPersist = tree(config);
results.forkPlan = await fork('fork-plan', ['--permission-mode', 'plan']);
results.forkDisallow = await fork('fork-disallow', ['--dangerously-skip-permissions', '--disallowedTools', 'Bash,Edit,Write,NotebookEdit']);
results.forkToolsRead = await fork('fork-tools-read', ['--dangerously-skip-permissions', '--tools', 'Read,Grep,Glob']);
results.forkExcludeDynamic = await fork('fork-exclude-dynamic', ['--dangerously-skip-permissions', '--exclude-dynamic-system-prompt-sections']);

// 3. A read-only snapshot: the session file copied into an observer's own
// config directory, under the same project, and forked there.
const snapshot = liveFile && path.join(observer, path.relative(config, liveFile));
if (snapshot) put(snapshot, fs.readFileSync(liveFile));
const observerBefore = tree(observer);
results.forkSnapshot = await fork('fork-snapshot', ['--dangerously-skip-permissions'], observer);
const observerAfter = tree(observer);

const afterSnapshotFork = tree(config);

// 4. A fork while the live member is mid-turn: its request is held.
stub.holdNext(true);
const live3 = run('live-3', ['--resume', session, ...common, '--dangerously-skip-permissions', '--', 'Third task message, answered slowly.']);
for (let i = 0; i < 600 && stub.heldCount() === 0; i++) await new Promise(r => setTimeout(r, 50));
stub.holdNext(false);
const heldAtFork = stub.heldCount();
const midFileLines = liveFile ? fs.readFileSync(liveFile, 'utf8').trim().split('\n').length : 0;
results.forkMid = await fork('fork-mid', ['--dangerously-skip-permissions']);
stub.release();
results.live3 = await live3;
const liveAfter = liveFile && fs.readFileSync(liveFile, 'utf8');

await stub.close();
fs.writeFileSync(path.join(base, 'requests.json'), JSON.stringify(stub.requests, null, 2));
const parse = text => { try { return JSON.parse(text); } catch { return null; } };
const report = {
  versions: { claude: (await new Promise(r => { const c = spawn(binary, ['--version']); let o = ''; c.stdout.on('data', d => o += d); c.on('close', () => r(o.trim())); })) },
  exits: runs,
  live_session_file: liveFile && path.relative(base, liveFile),
  fork_same: {
    live_file_unchanged: liveUnchangedBySameFork,
    files_changed: changed(before, afterSame),
    json_result: (({ type, subtype, is_error, session_id, result, usage, total_cost_usd }) => ({ type, subtype, is_error, session_id, result, usage, total_cost_usd }))(parse(results.forkSame.stdout) || {}),
    vs_live: compare(last('live-2'), last('fork-same')),
  },
  fork_persist: { files_changed: changed(afterSame, afterPersist), session_id: parse(results.forkPersist.stdout)?.session_id },
  fork_plan: compare(last('live-2'), last('fork-plan')),
  fork_disallow: compare(last('live-2'), last('fork-disallow')),
  fork_tools_read: compare(last('live-2'), last('fork-tools-read')),
  fork_exclude_dynamic: compare(last('live-2'), last('fork-exclude-dynamic')),
  fork_snapshot: { vs_fork_same: compare(last('fork-same'), last('fork-snapshot')), observer_files_changed: changed(observerBefore, observerAfter), live_tree_untouched_by_snapshot_fork: changed(afterPersist, afterSnapshotFork).length === 0 },
  fork_mid_turn: {
    held_live_requests_at_fork: heldAtFork,
    live_file_lines_at_fork: midFileLines,
    vs_live2: compare(last('live-2'), last('fork-mid')),
    live3_exit: results.live3.code,
    live_file_mentions_btw: liveAfter ? liveAfter.includes('BTW side question') : null,
    live_file_has_third_reply: liveAfter ? liveAfter.includes('(live-3)') : null,
  },
  request_counts: Object.fromEntries([...new Set(stub.requests.map(r => r.label))].map(l => [l, stub.requests.filter(r => r.label === l).map(r => r.path.split('?')[0])])),
};
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
