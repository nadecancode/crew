// /btw acceptance case B10 for Codex, live: does an aside forked from a
// member's thread (`exec fork --ephemeral`) read the member's cache, and
// does the member's next turn still read its own? Calls the provider with a
// small model (GPT-6-Luna, low effort) through the operator's own Codex login:
// CODEX_HOME is left at its default and nothing is copied.
// `--ignore-user-config` and `features.hooks=false` keep the operator's
// configuration, MCP servers and hooks out; the read-only sandbox with
// approvals off keeps every run from acting. The member's thread is deleted at
// the end with `codex delete`. A handful of short requests. Run from the
// repository root.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';

const base = path.resolve('target/btw-audit/codex-live');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_CODEX || 'codex';
const model = process.env.BTW_LIVE_CODEX_MODEL || 'gpt-6-luna';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
const env = { PATH: process.env.PATH, HOME: os.homedir(), LANG: 'C.UTF-8', TZ: 'UTC', ...(process.env.CODEX_HOME ? { CODEX_HOME: process.env.CODEX_HOME } : {}) };
const instructions = `BTW live probe member. Answer in one short sentence. ${'Reference notes for the probe member, repeated so the prompt is long enough to cache. '.repeat(300)}`;
const common = ['--skip-git-repo-check', '--json', '--ignore-user-config', '-m', model, '-c', 'model_reasoning_effort="low"', '-c', 'features.hooks=false',
  '-c', `developer_instructions=${JSON.stringify(instructions)}`, '--sandbox', 'read-only', '-c', 'approval_policy="never"'];
const runs = {};
const run = (label, args) => new Promise(resolve => {
  const child = spawn(binary, ['--no-daemon', 'exec', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
  let out = '', err = '';
  child.stdout.on('data', d => out += d); child.stderr.on('data', d => err += d);
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 240000);
  child.on('close', code => {
    clearTimeout(timer);
    const events = out.split('\n').map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
    const usage = events.filter(e => e.type === 'turn.completed').at(-1)?.usage || {};
    runs[label] = {
      exit: code, thread: events.find(e => e.type === 'thread.started')?.thread_id,
      answer: (events.filter(e => e.item?.type === 'agent_message').at(-1)?.item.text ?? err.slice(-200)).slice(0, 160),
      input: usage.input_tokens, cached_input: usage.cached_input_tokens, cache_write: usage.cache_write_input_tokens, output: usage.output_tokens, reasoning: usage.reasoning_output_tokens,
    };
    resolve(runs[label]);
  });
});
const m1 = await run('member-1', [...common, 'First task message. Reply: ready.']);
const thread = m1.thread;
await run('member-2', [...common, 'resume', thread, 'Second task message. Reply: noted.']);
await run('aside', [...common, 'fork', '--ephemeral', thread, '[crew aside] The operator asks a side question. Answer it from what you know now. Your tools cannot change anything; do not continue your work.\n\nWhat was your last reply?']);
await run('member-3', [...common, 'resume', thread, 'Third task message. Reply: done.']);
const deleted = await new Promise(resolve => {
  const child = spawn(binary, ['delete', thread], { cwd: work, env, stdio: ['pipe', 'pipe', 'pipe'] });
  child.stdin.end('y\n');
  let out = ''; child.stdout.on('data', d => out += d); child.stderr.on('data', d => out += d);
  child.on('close', code => resolve({ code, out: out.slice(-200) }));
});
const report = { model, effort: 'low', thread, runs, deleted };
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
