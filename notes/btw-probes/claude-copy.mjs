// /btw probe for Claude Code, part five (review m-aa42af439982): how does an
// aside fork a frozen copy while keeping the member's config directory, and
// so its login? Tries `--resume <path>`, and a copy under a fresh id in the
// member's own project directory, removed afterwards. Offline.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import { startStub } from './stub.mjs';

const base = path.resolve('target/btw-audit/claude-copy');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_CLAUDE || 'claude';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const sha = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const home = path.join(base, 'home');
const config = path.join(home, '.claude');
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
json(path.join(config, '.claude.json'), { hasCompletedOnboarding: true, bypassPermissionsModeAccepted: true, projects: { [work]: { hasTrustDialogAccepted: true } } });
json(path.join(base, 'mcp.json'), { mcpServers: {} });
json(path.join(base, 'aside.json'), { disableAllHooks: true });
const stub = await startStub();
const env = { PATH: process.env.PATH, HOME: home, LANG: 'C.UTF-8', TZ: 'UTC', CLAUDE_CONFIG_DIR: config, ANTHROPIC_API_KEY: 'btw-probe-not-a-secret', ANTHROPIC_BASE_URL: stub.url, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_AUTOUPDATER: '1', ENABLE_TOOL_SEARCH: 'false' };
const run = (label, args) => new Promise(resolve => {
  stub.setLabel(label);
  const child = spawn(binary, ['--print', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '', err = '';
  child.stdout.on('data', d => out += d); child.stderr.on('data', d => err += d);
  const timer = setTimeout(() => child.kill('SIGKILL'), 60000);
  child.on('close', code => { clearTimeout(timer); resolve({ code, out: out.slice(0, 300), err: err.slice(0, 300) }); });
});
const plain = v => JSON.stringify(v ?? null, (k, x) => (k === 'cache_control' ? undefined : x));
const last = l => stub.requests.filter(r => r.label === l && r.path.includes('/messages') && !r.path.includes('count_tokens') && r.body?.model === 'claude-opus-5-5').at(-1)?.body;
const flags = ['--model', 'claude-opus-5-5', '--mcp-config', path.join(base, 'mcp.json'), '--strict-mcp-config'];
const session = crypto.randomUUID();
await run('live-1', ['--session-id', session, ...flags, '--dangerously-skip-permissions', '--', 'First task message.']);
await run('live-2', ['--resume', session, ...flags, '--dangerously-skip-permissions', '--', 'Second task message.']);
const projectDir = fs.readdirSync(path.join(config, 'projects')).map(p => path.join(config, 'projects', p)).find(p => fs.existsSync(path.join(p, `${session}.jsonl`)));
const liveFile = path.join(projectDir, `${session}.jsonl`);
// The frozen copy, then one more live turn: the fork must not see it.
const frozen = fs.readFileSync(liveFile);
await run('live-3', ['--resume', session, ...flags, '--dangerously-skip-permissions', '--', 'Third task message, after the snapshot.']);
const liveHash = sha(liveFile);
const report = {};
const tail = body => body ? body.messages.map(m => JSON.stringify(m.content)).filter(t => /task message/.test(t)).map(t => t.match(/(First|Second|Third)[^"\\]*/)?.[0]) : null;
const aside = ['--fork-session', '--no-session-persistence', '--output-format', 'json', ...flags, '--settings', path.join(base, 'aside.json'), '--permission-mode', 'dontAsk', '--', 'BTW side question.'];

// A. --resume with a path to the frozen copy outside the config directory.
const outside = path.join(base, 'aside-dir', `${crypto.randomUUID()}.jsonl`);
put(outside, frozen);
const before = fs.readdirSync(projectDir).sort();
const a = await run('resume-path', ['--resume', outside, ...aside]);
report.resume_by_path = { exit: a.code, out: a.out, err: a.err, user_turns_seen: tail(last('resume-path')), project_dir_after: fs.readdirSync(projectDir).sort().filter(f => !before.includes(f)) };

// B. The frozen copy under a fresh id in the member's own project directory.
const copyId = crypto.randomUUID();
const copy = path.join(projectDir, `${copyId}.jsonl`);
put(copy, frozen);
const b = await run('resume-copy-id', ['--resume', copyId, ...aside]);
const copyUnchanged = sha(copy) === crypto.createHash('sha256').update(frozen).digest('hex');
fs.rmSync(copy);
report.copy_under_fresh_id = { exit: b.code, out: b.out, user_turns_seen: tail(last('resume-copy-id')), copy_unchanged_by_fork: copyUnchanged, project_dir_left_after_removal: fs.readdirSync(projectDir).sort().filter(f => !before.includes(f)) };
const live3 = last('live-3'), fork = last('resume-copy-id');
let common = 0; while (common < fork.messages.length && plain(live3.messages[common]) === plain(fork.messages[common])) common += 1;
report.copy_under_fresh_id.vs_member_turn3 = { tools_identical: JSON.stringify(live3.tools) === JSON.stringify(fork.tools), system_identical: JSON.stringify(live3.system) === JSON.stringify(fork.system), leading_messages_identical: `${common} of ${fork.messages.length}` };
const sessionField = body => JSON.parse(body.metadata.user_id).session_id;
report.copy_under_fresh_id.metadata_session = { member: sessionField(last('live-2')), fork: sessionField(fork) };
const byPath = last('resume-path');
let pathCommon = 0; while (pathCommon < byPath.messages.length && plain(live3.messages[pathCommon]) === plain(byPath.messages[pathCommon])) pathCommon += 1;
report.resume_by_path.vs_member_turn3 = { tools_identical: JSON.stringify(live3.tools) === JSON.stringify(byPath.tools), system_identical: JSON.stringify(live3.system) === JSON.stringify(byPath.system), leading_messages_identical: `${pathCommon} of ${byPath.messages.length}` };
report.live_file_unchanged = sha(liveFile) === liveHash;
await stub.close();
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
