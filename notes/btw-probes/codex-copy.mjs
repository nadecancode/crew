// /btw probe for Codex, part three (review m-aa42af439982): can an aside
// fork a frozen copy of the member's rollout while keeping the member's
// CODEX_HOME, and so its login? Tries a rollout path in place of the thread
// id, and `-c experimental_resume=<path>`. Offline.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/codex-copy');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_CODEX || 'codex';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const sha = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const home = path.join(base, 'home');
const codexHome = path.join(base, 'codex-home');
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
const stub = await startStub();
put(path.join(codexHome, 'config.toml'), `model = "gpt-6"\nmodel_provider = "stub"\n[model_providers.stub]\nname = "Stub"\nbase_url = "${stub.url}/v1"\nwire_api = "responses"\nenv_key = "BTW_API_KEY"\n[features]\nplugins = false\n[projects.${JSON.stringify(work)}]\ntrust_level = "trusted"\n`);
const run = (label, args) => new Promise(resolve => {
  stub.setLabel(label);
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: codexHome, BTW_API_KEY: 'btw-probe-not-a-secret', LANG: 'C.UTF-8', TZ: 'UTC' };
  const child = spawn(binary, ['--no-daemon', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
  let out = '', err = '';
  child.stdout.on('data', d => out += d); child.stderr.on('data', d => err += d);
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 60000);
  child.on('close', code => { clearTimeout(timer); resolve({ code, out, err: err.slice(-400) }); });
});
const exec = ['exec', '--skip-git-repo-check', '--json'];
const live1 = await run('live-1', [...exec, '--dangerously-bypass-approvals-and-sandbox', 'First task message.']);
const thread = live1.out.split('\n').map(l => { try { return JSON.parse(l); } catch { return null; } }).find(e => e?.type === 'thread.started')?.thread_id;
await run('live-2', [...exec, '--dangerously-bypass-approvals-and-sandbox', 'resume', thread, 'Second task message.']);
const walk = d => fs.readdirSync(d, { withFileTypes: true }).flatMap(e => (e.isDirectory() ? walk(path.join(d, e.name)) : [path.join(d, e.name)]));
const rollout = walk(path.join(codexHome, 'sessions')).find(f => f.includes(thread));
const frozen = path.join(base, 'aside-dir', path.basename(rollout));
put(frozen, fs.readFileSync(rollout));
await run('live-3', [...exec, '--dangerously-bypass-approvals-and-sandbox', 'resume', thread, 'Third task message, after the snapshot.']);
const liveHash = sha(rollout);
const seen = label => {
  const body = stub.requests.filter(r => r.label === label && r.path.includes('/responses')).at(-1)?.body;
  return body ? { turns: body.input.filter(i => i.role === 'user').map(i => JSON.stringify(i.content).match(/(First|Second|Third)[^"\\]*/)?.[0]).filter(Boolean), cache_key: body.prompt_cache_key } : null;
};
const report = { thread };
const a = await run('fork-by-path', [...exec, '--sandbox', 'read-only', '-c', 'approval_policy="never"', '-c', 'web_search="live"', 'fork', '--ephemeral', frozen, 'BTW side question.']);
report.fork_by_path = { exit: a.code, err: a.err, request: seen('fork-by-path') };
const b = await run('experimental-resume', [...exec, '--sandbox', 'read-only', '-c', 'approval_policy="never"', '-c', 'web_search="live"', '--ephemeral', '-c', `experimental_resume=${JSON.stringify(frozen)}`, 'BTW side question.']);
report.experimental_resume = { exit: b.code, err: b.err, request: seen('experimental-resume') };
report.live_rollout_unchanged = sha(rollout) === liveHash;
await stub.close();
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
