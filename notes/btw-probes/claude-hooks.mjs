// /btw probe for Claude Code, part four: hooks from the member's own setting
// sources (a project hook here) would run in an aside too. Does
// `disableAllHooks` in the aside's flag settings stop them without changing
// the request the member's next turn would send? Offline.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/claude-hooks');
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
const log = path.join(base, 'project-hooks.log');
const hook = event => ({ hooks: [{ type: 'command', command: `echo ${event} >> '${log}'` }] });
json(path.join(work, '.claude/settings.json'), { hooks: { SessionStart: [hook('SessionStart')], UserPromptSubmit: [hook('UserPromptSubmit')], Stop: [hook('Stop')] } });
json(path.join(base, 'member.json'), { permissions: { deny: ['AskUserQuestion'] } });
json(path.join(base, 'aside.json'), { permissions: { deny: ['AskUserQuestion'] }, disableAllHooks: true });
json(path.join(base, 'mcp.json'), { mcpServers: {} });
const stub = await startStub();
const env = { PATH: process.env.PATH, HOME: home, LANG: 'C.UTF-8', TZ: 'UTC', CLAUDE_CONFIG_DIR: config, ANTHROPIC_API_KEY: 'btw-probe-not-a-secret', ANTHROPIC_BASE_URL: stub.url, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_AUTOUPDATER: '1' };
const run = (label, args) => new Promise(resolve => {
  stub.setLabel(label);
  const child = spawn(binary, ['--print', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'] });
  const timer = setTimeout(() => child.kill('SIGKILL'), 60000);
  child.on('close', code => { clearTimeout(timer); resolve(code); });
});
const read = () => (fs.existsSync(log) ? fs.readFileSync(log, 'utf8').trim().split('\n') : []);
const plain = v => JSON.stringify(v ?? null, (k, x) => (k === 'cache_control' ? undefined : x));
const last = l => stub.requests.filter(r => r.label === l && r.path.includes('/messages') && !r.path.includes('count_tokens') && r.body?.model === 'claude-opus-5-5').at(-1)?.body;
const session = crypto.randomUUID();
const flags = ['--model', 'claude-opus-5-5', '--mcp-config', path.join(base, 'mcp.json'), '--strict-mcp-config'];
await run('live-1', ['--session-id', session, ...flags, '--settings', path.join(base, 'member.json'), '--dangerously-skip-permissions', '--', 'First task message.']);
await run('live-2', ['--resume', session, ...flags, '--settings', path.join(base, 'member.json'), '--dangerously-skip-permissions', '--', 'Second task message.']);
const report = { hooks_after_live: read() };
fs.rmSync(log, { force: true });
await run('aside', ['--resume', session, '--fork-session', '--no-session-persistence', ...flags, '--settings', path.join(base, 'aside.json'), '--permission-mode', 'dontAsk', '--', 'BTW side question.']);
const live = last('live-2'), aside = last('aside');
let common = 0;
while (common < live.messages.length && plain(live.messages[common]) === plain(aside.messages[common])) common += 1;
report.aside = { hooks_fired: read(), tools_identical: plain(live.tools) === plain(aside.tools), system_identical: plain(live.system) === plain(aside.system), live_messages_repeated: `${common} of ${live.messages.length}` };
await stub.close();
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
