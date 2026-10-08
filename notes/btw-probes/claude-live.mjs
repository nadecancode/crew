// /btw acceptance case B10 for Claude Code, live: does an aside forked from
// a member's frozen transcript read the member's cache, and does the
// member's next turn still read its own? This one calls the provider with
// the cheapest model (Claude Haiku 4.5), through the operator's own Claude
// Code login: CLAUDE_CONFIG_DIR is left at its default and nothing is copied.
// Each run uses `--setting-sources project` with a throwaway project and
// `disableAllHooks`, so the operator's hooks, plugins and MCP servers stay
// out. The member's transcript lands in the operator's projects directory
// under this probe's path and is removed at the end. A handful of short
// requests. Run from the repository root.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';

const base = path.resolve('target/btw-audit/claude-live');
fs.rmSync(base, { recursive: true, force: true });
const binary = process.env.BTW_CLAUDE || 'claude';
const model = process.env.BTW_LIVE_MODEL || 'claude-haiku-4-5-20251001';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const work = path.join(base, 'work');
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
json(path.join(base, 'mcp.json'), { mcpServers: {} });
json(path.join(base, 'member.json'), { disableAllHooks: true, permissions: { deny: ['AskUserQuestion'] } });
json(path.join(base, 'aside.json'), { disableAllHooks: true, permissions: { deny: ['AskUserQuestion'], ask: ['*'] } });
// A long standing instruction, so the prefix is well over the cache minimum.
put(path.join(base, 'briefing.md'), `BTW live probe member. Answer in one short sentence.\n\n${'Reference notes for the probe member, repeated so the prompt is long enough to cache. '.repeat(400)}\n`);

const config = process.env.CLAUDE_CONFIG_DIR || path.join(os.homedir(), '.claude');
const env = { PATH: process.env.PATH, HOME: os.homedir(), LANG: 'C.UTF-8', TZ: 'UTC', DISABLE_AUTOUPDATER: '1', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1' };
const runs = {};
const run = (label, args) => new Promise(resolve => {
  const child = spawn(binary, ['--print', '--output-format', 'json', ...args], { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '', err = '';
  child.stdout.on('data', d => out += d); child.stderr.on('data', d => err += d);
  const timer = setTimeout(() => child.kill('SIGKILL'), 180000);
  child.on('close', code => {
    clearTimeout(timer);
    let parsed = null; try { parsed = JSON.parse(out); } catch {}
    const u = parsed?.usage || {};
    runs[label] = {
      exit: code, is_error: parsed?.is_error, result: String(parsed?.result ?? err).slice(0, 160),
      input: u.input_tokens, cache_read: u.cache_read_input_tokens, cache_write: u.cache_creation_input_tokens,
      cache_write_1h: u.cache_creation?.ephemeral_1h_input_tokens, cache_write_5m: u.cache_creation?.ephemeral_5m_input_tokens,
      output: u.output_tokens, cost_usd: parsed?.total_cost_usd, model: Object.keys(parsed?.modelUsage || {}),
    };
    resolve(parsed);
  });
});
const flags = ['--model', model, '--setting-sources', 'project', '--mcp-config', path.join(base, 'mcp.json'), '--strict-mcp-config', '--append-system-prompt-file', path.join(base, 'briefing.md')];
const session = crypto.randomUUID();
const member = ['--settings', path.join(base, 'member.json'), '--dangerously-skip-permissions'];
await run('member-1', ['--session-id', session, ...flags, ...member, '--', 'First task message. Reply: ready.']);
await run('member-2', ['--resume', session, ...flags, ...member, '--', 'Second task message. Reply: noted.']);
// The frozen copy, outside the config directory.
const projects = path.join(config, 'projects');
const projectDir = fs.readdirSync(projects).map(p => path.join(projects, p)).find(p => fs.existsSync(path.join(p, `${session}.jsonl`)));
const frozen = path.join(base, 'aside', `${crypto.randomUUID()}.jsonl`);
put(frozen, fs.readFileSync(path.join(projectDir, `${session}.jsonl`)));
await run('aside', ['--resume', frozen, '--fork-session', '--no-session-persistence', ...flags, '--settings', path.join(base, 'aside.json'), '--permission-mode', 'dontAsk', '--', '[crew aside] The operator asks a side question. Answer it from what you know now. Your tools cannot change anything; do not continue your work.\n\nWhat was your last reply?']);
await run('member-3', ['--resume', session, ...flags, ...member, '--', 'Third task message. Reply: done.']);
// Remove the member's transcripts from the operator's projects directory.
const left = fs.readdirSync(projectDir);
const ours = left.every(f => f.endsWith('.jsonl') || fs.statSync(path.join(projectDir, f)).isDirectory());
if (projectDir.includes('btw-audit-claude-live') && ours) fs.rmSync(projectDir, { recursive: true });
const report = { model, ttl_note: 'cache_write_1h and cache_write_5m are the native buckets', runs, project_dir_removed: !fs.existsSync(projectDir) };
json(path.join(base, 'report.json'), report);
console.log(JSON.stringify(report, null, 2));
