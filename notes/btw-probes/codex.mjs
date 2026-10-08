// /btw probe for Codex: `codex exec fork --ephemeral` from a member's thread.
// Does it leave the member's rollout alone, write nothing of its own, and
// send the live thread's prefix? Which read-only settings keep that prefix,
// and are acting tools refused? Offline: requests go to the localhost stub
// (stub.mjs) through a custom Responses provider. Run from the repository root.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/codex');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_CODEX || 'codex';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const sha = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');

const home = path.join(base, 'home');
const codexHome = path.join(base, 'codex-home');
const observerHome = path.join(base, 'observer-home');
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
put(path.join(work, 'README.md'), 'READ_CANARY: the aside may read this.\n');

const stub = await startStub();
const config = `model = "gpt-6"\nmodel_provider = "stub"\n[model_providers.stub]\nname = "Stub"\nbase_url = "${stub.url}/v1"\nwire_api = "responses"\nenv_key = "BTW_API_KEY"\n[features]\nplugins = false\n[projects.${JSON.stringify(work)}]\ntrust_level = "trusted"\n`;
put(path.join(codexHome, 'config.toml'), config);
put(path.join(observerHome, 'config.toml'), config);

const runs = {};
function run(label, args, homeDir = codexHome) {
  stub.setLabel(label);
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: homeDir, XDG_CONFIG_HOME: path.join(home, '.config'), BTW_API_KEY: 'btw-probe-not-a-secret', LANG: 'C.UTF-8', TZ: 'UTC' };
  const child = spawn(binary, ['--no-daemon', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
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
function tree(dir) {
  const out = {};
  const walk = d => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, e.name); if (e.isDirectory()) walk(p); else if (e.isFile()) out[path.relative(dir, p)] = sha(p); } };
  if (fs.existsSync(dir)) walk(dir);
  return out;
}
const changed = (a, b) => [...new Set([...Object.keys(a), ...Object.keys(b)])].filter(k => a[k] !== b[k]).map(k => `${a[k] ? (b[k] ? 'changed' : 'removed') : 'added'} ${k}`).sort();
const events = stdout => stdout.trim().split('\n').map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
const turns = label => stub.requests.filter(r => r.label === label && r.path.includes('/responses'));
const last = label => turns(label).at(-1)?.body;
const strip = v => JSON.parse(JSON.stringify(v ?? null, (k, x) => (['id', 'prompt_cache_key'].includes(k) ? undefined : x)));
const same = (a, b) => JSON.stringify(strip(a)) === JSON.stringify(strip(b));
function compare(live, fork) {
  if (!live || !fork) return { missing: !live ? 'live' : 'fork' };
  let common = 0;
  while (common < live.input.length && same(live.input[common], fork.input[common])) common += 1;
  const names = b => (b.tools || []).map(t => t.name || t.type);
  return {
    instructions_identical: live.instructions === fork.instructions,
    tools_identical: same(live.tools, fork.tools),
    tool_names: { live: names(live), removed: names(live).filter(n => !names(fork).includes(n)), added: names(fork).filter(n => !names(live).includes(n)) },
    live_items_repeated: `${common} of ${live.input.length}`,
    first_differing_item: common < live.input.length ? JSON.stringify(live.input[common]).slice(0, 400) + '\n  vs ' + JSON.stringify(fork.input[common]).slice(0, 400) : null,
    prompt_cache_key: { live: live.prompt_cache_key, fork: fork.prompt_cache_key },
    params_differing: Object.keys({ ...live, ...fork }).filter(k => !['input', 'tools', 'instructions'].includes(k) && JSON.stringify(live[k]) !== JSON.stringify(fork[k])),
    fork_tail: fork.input.slice(common).map(i => JSON.stringify(i).slice(0, 200)),
  };
}

const member = ['-c', 'developer_instructions="BTW_DEV_CANARY: standing instructions for the probe member."'];
const report = { runs };
const live1 = await run('live-1', ['exec', '--skip-git-repo-check', '--json', '--dangerously-bypass-approvals-and-sandbox', ...member, 'First task message.']);
const thread = events(live1.stdout).find(e => e.type === 'thread.started')?.thread_id;
report.thread = thread;
await run('live-2', ['exec', '--skip-git-repo-check', '--json', '--dangerously-bypass-approvals-and-sandbox', ...member, 'resume', thread, 'Second task message.']);
const rollout = Object.keys(tree(codexHome)).find(k => k.includes(thread));
report.rollout = rollout;
const before = tree(codexHome);
const sqlite = (db, sql) => { try { return require('node:child_process').execFileSync('sqlite3', [path.join(codexHome, db), sql], { encoding: 'utf8' }).trim(); } catch (e) { return 'sqlite3 unavailable'; } };
const parentRow = () => sqlite('state_5.sqlite', `select updated_at_ms, tokens_used, recency_at_ms from threads where id = '${thread}'`);
const threadIds = () => sqlite('state_5.sqlite', 'select id from threads order by id').split('\n');
const rowBefore = parentRow(), threadsBefore = threadIds();

const fork = (label, extra, dir) => run(label, ['exec', '--skip-git-repo-check', '--json', ...member, ...extra, 'fork', '--ephemeral', thread, 'BTW side question: what are you working on?'], dir);
// `fork` options: check where exec fork takes them.
await fork('fork-same', ['--dangerously-bypass-approvals-and-sandbox']);
const afterSame = tree(codexHome);
report.fork_same = { parent_thread_row: { before: rowBefore, after: parentRow() }, threads_listed: { before: threadsBefore, after: threadIds() }, files_changed: changed(before, afterSame), answer: events(fs.readFileSync(path.join(base, 'runs/fork-same.stdout'), 'utf8')).filter(e => e.type?.startsWith('item') || e.type === 'turn.completed').map(e => JSON.stringify(e).slice(0, 200)), vs_live: compare(last('live-2'), last('fork-same')) };
await run('fork-persist', ['exec', '--skip-git-repo-check', '--json', '--dangerously-bypass-approvals-and-sandbox', ...member, 'fork', thread, 'BTW side question, persisted.']);
const afterPersist = tree(codexHome);
report.fork_persist = { files_changed: changed(afterSame, afterPersist) };
await fork('fork-read-only', ['--sandbox', 'read-only', '-c', 'approval_policy="never"']);
report.fork_read_only = compare(last('live-2'), last('fork-read-only'));
// The read-only sandbox turns off external web access; asking for live search
// keeps the member's tool definition.
await fork('fork-read-only-search', ['--sandbox', 'read-only', '-c', 'approval_policy="never"', '-c', 'web_search="live"']);
report.fork_read_only_search = compare(last('live-2'), last('fork-read-only-search'));
const afterReadOnly = tree(codexHome);
// A snapshot: the rollout copied into an observer's own home, then forked.
put(path.join(observerHome, rollout), fs.readFileSync(path.join(codexHome, rollout)));
const observerBefore = tree(observerHome);
await fork('fork-snapshot', ['--dangerously-bypass-approvals-and-sandbox'], observerHome);
report.fork_snapshot = { vs_fork_same: compare(last('fork-same'), last('fork-snapshot')), observer_files_changed: changed(observerBefore, tree(observerHome)), member_home_changes_during_snapshot_fork: changed(afterReadOnly, tree(codexHome)) };
const afterSnapshot = tree(codexHome);

// Mid-turn: the live member's third turn is held while an aside forks.
stub.holdNext(true);
const live3 = run('live-3', ['exec', '--skip-git-repo-check', '--json', '--dangerously-bypass-approvals-and-sandbox', ...member, 'resume', thread, 'Third task message, answered slowly.']);
for (let i = 0; i < 600 && stub.heldCount() === 0; i++) await new Promise(r => setTimeout(r, 50));
stub.holdNext(false);
const held = stub.heldCount();
await fork('fork-mid', ['--dangerously-bypass-approvals-and-sandbox']);
stub.release();
const l3 = await live3;
const rolloutText = fs.readFileSync(path.join(codexHome, rollout), 'utf8');
report.fork_mid = { held_live_requests: held, vs_live2: compare(last('live-2'), last('fork-mid')), live3_exit: l3.code, rollout_mentions_btw: rolloutText.includes('BTW side question'), rollout_has_third_reply: rolloutText.includes('(live-3)') };
report.member_home_changes_by_live3 = changed(afterSnapshot, tree(codexHome));

await stub.close();
fs.writeFileSync(path.join(base, 'requests.json'), JSON.stringify(stub.requests, null, 2));
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
