// Native MCP inventory before/after trust. No server or model is started.
// Linux: bubblewrap is required; every native invocation has no network.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const out = path.resolve(process.env.TOOLBOX_OUT || 'target/toolbox-t12/codex-trust');
fs.mkdirSync(out, { recursive: true });
const root = fs.mkdtempSync(path.join(out, 'fixture-'));
const home = path.join(root, 'home');
const work = path.join(root, 'work');
for (const dir of [path.join(home, '.codex'), path.join(work, '.codex'), path.join(work, '.git/objects'), path.join(work, '.git/refs/heads')]) {
  fs.mkdirSync(dir, { recursive: true });
}
fs.writeFileSync(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
fs.writeFileSync(path.join(work, '.git/config'), '[core]\nrepositoryformatversion=0\nbare=false\n');
fs.writeFileSync(path.join(work, '.codex/config.toml'), '[mcp_servers.project-sentinel]\ncommand="/bin/false"\n');
const env = {
  PATH: process.env.PATH,
  HOME: home,
  CODEX_HOME: path.join(home, '.codex'),
  XDG_CONFIG_HOME: path.join(home, '.config'),
  LANG: 'C.UTF-8',
};
const binary = process.env.TOOLBOX_CODEX || 'codex';
const run = args => spawnSync('bwrap', [
  '--die-with-parent', '--unshare-net', '--ro-bind', '/', '/',
  '--bind', root, root, '--chdir', work, '--', binary, ...args,
], { env, encoding: 'utf8', timeout: 15_000, maxBuffer: 1024 * 1024 });
const version = run(['--version']);
assert.equal(version.status, 0, version.error?.message || version.stderr);
const report = { version: version.stdout.trim(), provider_calls: 0, cases: [] };
for (const trust of ['untrusted', 'trusted']) {
  fs.writeFileSync(path.join(home, '.codex/config.toml'),
    `[projects.${JSON.stringify(work)}]\ntrust_level=${JSON.stringify(trust)}\n`);
  for (const disabled of [false, true]) {
    const result = run([
      ...(disabled ? ['-c', 'mcp_servers={"project-sentinel"={enabled=false}}'] : []),
      'mcp', 'list', '--json',
    ]);
    const name = `${trust}-${disabled ? 'disabled' : 'baseline'}`;
    fs.writeFileSync(path.join(out, `${name}.stdout`), result.stdout || '');
    fs.writeFileSync(path.join(out, `${name}.stderr`), result.stderr || '');
    if (trust === 'untrusted' && disabled) {
      assert.equal(result.status, 1, result.error?.message || result.stderr);
      assert.match(result.stderr, /invalid transport/);
      report.cases.push({ trust, disabled, code: result.status, reason: 'invalid transport' });
    } else {
      assert.equal(result.status, 0, result.error?.message || result.stderr);
      const servers = JSON.parse(result.stdout).map(({ name, enabled }) => ({ name, enabled }));
      assert.deepEqual(servers, trust === 'untrusted' ? [] : [{ name: 'project-sentinel', enabled: !disabled }]);
      report.cases.push({ trust, disabled, code: result.status, servers });
    }
  }
}
fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify(report, null, 2));
