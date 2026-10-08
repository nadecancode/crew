// /btw probe for the supported pi (1.1.0, installed by
// notes/toolbox-probes/install-pi.mjs under target/). `--fork` cannot be
// combined with `--no-session`, so the aside forks into a session directory
// of its own (`--session-dir`), from a path to the member's session file.
// Does that leave the member's session untouched, and does the fork's
// request repeat the live prefix? Offline: requests go to the localhost stub
// through an Anthropic-compatible provider in models.json. Run from the
// repository root.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/pi');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_PI || path.resolve('target/toolbox-audit/pi-runtime/node_modules/.bin/pi');
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const sha = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');

const home = path.join(base, 'home');
const agentDir = path.join(base, 'agent');
const sessions = path.join(base, 'member-sessions');
const asides = path.join(base, 'aside-sessions');
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
put(path.join(work, 'README.md'), 'READ_CANARY: the aside may read this.\n');

const stub = await startStub();
json(path.join(agentDir, 'models.json'), {
  providers: { stub: { baseUrl: stub.url, api: 'anthropic-messages', apiKey: 'btw-probe-not-a-secret', models: [{ id: 'claude-opus-5-5', contextWindow: 200000, maxTokens: 8192 }] } },
});
json(path.join(agentDir, 'settings.json'), {});

const runs = {};
function run(label, args) {
  stub.setLabel(label);
  const env = { PATH: process.env.PATH, HOME: home, PI_CODING_AGENT_DIR: agentDir, PI_OFFLINE: '1', LANG: 'C.UTF-8', TZ: 'UTC' };
  const child = spawn(process.execPath, [binary, ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
  let stdout = '', stderr = '';
  child.stdout.on('data', d => stdout += d);
  child.stderr.on('data', d => stderr += d);
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 60000);
  return new Promise(resolve => child.on('close', (code, signal) => {
    clearTimeout(timer);
    put(path.join(base, 'runs', `${label}.stdout`), stdout);
    put(path.join(base, 'runs', `${label}.stderr`), stderr);
    runs[label] = { code, signal };
    resolve({ code, stdout, stderr });
  }));
}
const walk = d => (fs.existsSync(d) ? fs.readdirSync(d, { withFileTypes: true }).flatMap(e => (e.isDirectory() ? walk(path.join(d, e.name)) : [path.join(d, e.name)])) : []);
const tree = d => Object.fromEntries(walk(d).map(f => [path.relative(d, f), sha(f)]));
const changed = (a, b) => [...new Set([...Object.keys(a), ...Object.keys(b)])].filter(k => a[k] !== b[k]).map(k => `${a[k] ? (b[k] ? 'changed' : 'removed') : 'added'} ${k}`).sort();
const plain = value => JSON.parse(JSON.stringify(value ?? null, (k, v) => (k === 'cache_control' ? undefined : v)));
const same = (a, b) => JSON.stringify(plain(a)) === JSON.stringify(plain(b));
const last = label => stub.requests.filter(r => r.label === label && r.path.includes('/messages') && !r.path.includes('count_tokens')).at(-1)?.body;
function compare(live, fork) {
  if (!live || !fork) return { missing: !live ? 'live' : 'fork' };
  let common = 0;
  while (common < live.messages.length && same(live.messages[common], fork.messages[common])) common += 1;
  const systemText = b => (Array.isArray(b.system) ? b.system.map(s => s.text).join('\n') : String(b.system ?? ''));
  const a = systemText(live), c = systemText(fork);
  let at = 0; while (at < a.length && a[at] === c[at]) at += 1;
  return {
    tools_identical: JSON.stringify(live.tools) === JSON.stringify(fork.tools),
    tool_names: (live.tools || []).map(t => t.name),
    system_identical: JSON.stringify(live.system) === JSON.stringify(fork.system),
    system_first_difference: a === c ? null : { at, live: a.slice(Math.max(0, at - 60), at + 80), fork: c.slice(Math.max(0, at - 60), at + 80) },
    // Messages equal once cache markers, which move to the newest message, are
    // set aside; and equal byte for byte.
    live_messages_repeated: `${common} of ${live.messages.length}`,
    live_messages_repeated_exact: `${(() => { let n = 0; while (n < live.messages.length && JSON.stringify(live.messages[n]) === JSON.stringify(fork.messages[n])) n += 1; return n; })()} of ${live.messages.length}`,
    params_differing: Object.keys({ ...live, ...fork }).filter(k => !['messages', 'tools', 'system'].includes(k) && !same(live[k], fork[k])),
    fork_tail: fork.messages.slice(common).map(m => `${m.role}: ${JSON.stringify(m.content).slice(0, 160)}`),
  };
}

const model = ['--provider', 'stub', '--model', 'claude-opus-5-5'];
const member = [...model, '--append-system-prompt', 'BTW_BRIEFING_CANARY: standing instructions for the probe member.', '--session-dir', sessions];
const id = 'btwprobe1';
const report = { runs };
await run('live-1', ['--print', ...member, '--session-id', id, 'First task message.']);
await new Promise(r => setTimeout(r, 1100));
await run('live-2', ['--print', ...member, '--session-id', id, 'Second task message.']);
const memberFile = walk(sessions).find(f => f.endsWith('.jsonl'));
report.member_session = memberFile && path.relative(base, memberFile);
const before = tree(sessions);
const agentBefore = tree(agentDir);

// An aside: fork the member's session file into the aside directory.
const aside = (label, extra) => run(label, ['--print', ...model, '--append-system-prompt', 'BTW_BRIEFING_CANARY: standing instructions for the probe member.', '--session-dir', asides, '--fork', memberFile, ...extra, 'BTW side question: what are you working on?']);
await new Promise(r => setTimeout(r, 1100));
const f1 = await aside('fork-same', []);
report.fork_same = { exit: f1.code, stdout: f1.stdout.slice(0, 200), stderr: f1.stderr.slice(0, 300), member_files_changed: changed(before, tree(sessions)), aside_files: Object.keys(tree(asides)), vs_live: compare(last('live-2'), last('fork-same')) };
await aside('fork-read-tools', ['--tools', 'read,grep,find,ls']);
report.fork_read_tools = compare(last('live-2'), last('fork-read-tools'));
await aside('fork-no-tools', ['--no-tools']);
report.fork_no_tools = compare(last('live-2'), last('fork-no-tools'));
// Denial: the stub asks for a write, a shell command and a read.
const toolNames = (last('live-2')?.tools || []).map(t => t.name);
stub.script('fork-deny', [
  { name: toolNames.find(n => /write/i.test(n)) || 'write', input: { path: path.join(work, 'aside-wrote.txt'), content: 'written by an aside' } },
  { name: toolNames.find(n => /bash/i.test(n)) || 'bash', input: { command: `touch '${path.join(work, 'aside-bash.txt')}'` } },
  { name: toolNames.find(n => /read/i.test(n)) || 'read', input: { path: path.join(work, 'README.md') } },
]);
await aside('fork-deny', ['--tools', 'read,grep,find,ls']);
const denyResults = stub.requests.filter(r => r.label === 'fork-deny' && r.path.includes('/messages')).flatMap(r => r.body.messages).flatMap(m => (Array.isArray(m.content) ? m.content.filter(c => c.type === 'tool_result') : []));
report.fork_deny = {
  wrote: fs.existsSync(path.join(work, 'aside-wrote.txt')),
  bash_ran: fs.existsSync(path.join(work, 'aside-bash.txt')),
  tool_results: [...new Map(denyResults.map(r => [r.tool_use_id, r])).values()].map(r => ({ is_error: r.is_error ?? false, text: JSON.stringify(r.content).slice(0, 200) })),
};
// The aside guard: the member's full tool list (so the prefix holds), with
// every call refused except reading. An extension that blocks at call time.
const guard = path.join(base, 'aside-guard.ts');
put(guard, `export default function (pi: any) {
  const reading = new Set(['read', 'grep', 'find', 'ls']);
  pi.on('tool_call', async (event: any) => {
    if (reading.has(event.toolName)) return undefined;
    return { block: true, reason: 'An aside answers from a copy and cannot act: ' + event.toolName + ' is refused.' };
  });
}
`);
stub.script('fork-guard', [
  { name: 'write', input: { path: path.join(work, 'aside-wrote.txt'), content: 'written by an aside' } },
  { name: 'bash', input: { command: `touch '${path.join(work, 'aside-bash.txt')}'` } },
  { name: 'read', input: { path: path.join(work, 'README.md') } },
]);
await aside('fork-guard', ['--extension', guard]);
const guardResults = stub.requests.filter(r => r.label === 'fork-guard' && r.path.includes('/messages')).flatMap(r => r.body.messages).flatMap(m => (Array.isArray(m.content) ? m.content.filter(c => c.type === 'tool_result') : []));
report.fork_guard = {
  wrote: fs.existsSync(path.join(work, 'aside-wrote.txt')),
  bash_ran: fs.existsSync(path.join(work, 'aside-bash.txt')),
  tool_results: [...new Map(guardResults.map(r => [r.tool_use_id, r])).values()].map(r => ({ is_error: r.is_error ?? false, text: JSON.stringify(r.content).slice(0, 200) })),
  first_request_vs_live: compare(last('live-2'), stub.requests.filter(r => r.label === 'fork-guard' && r.path.includes('/messages'))[0]?.body),
};
report.member_files_changed_by_asides = changed(before, tree(sessions));
report.agent_dir_changed_by_asides = changed(agentBefore, tree(agentDir));
await stub.close();
fs.writeFileSync(path.join(base, 'requests.json'), JSON.stringify(stub.requests, null, 2));
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
