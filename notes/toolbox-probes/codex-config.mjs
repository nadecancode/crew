// Offline config/discovery probe. Run from the repository root; see README.md.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';

const root = path.resolve('target/toolbox-audit/codex-config');
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const home = path.join(root, 'home'), config = path.join(root, 'config'), work = path.join(root, 'work');
for (const dir of [home, config, work, path.join(work, '.git/objects'), path.join(work, '.git/refs/heads')]) fs.mkdirSync(dir, { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion=0\nbare=false\n');
put(path.join(config, 'config.toml'), '[features]\nplugins=false\n[mcp_servers.ambient]\ncommand="/bin/false"\n');
const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: config, XDG_CONFIG_HOME: path.join(home, '.config'), LANG: 'C.UTF-8' };
const run = (name, args) => {
  const result = spawnSync(process.env.TOOLBOX_CODEX || 'codex', args, { env, cwd: work, encoding: 'utf8', timeout: 15000, maxBuffer: 8 * 1024 * 1024 });
  put(path.join(root, name + '.json'), result.stdout || ''); put(path.join(root, name + '.stderr'), result.stderr || '');
  assert.equal(result.status, 0, result.stderr);
  return result.stdout;
};
const mcpCases = {
  baseline: [],
  'crew-leaf': ['-c', 'mcp_servers.crew.command="/bin/false"'],
  'empty-table': ['-c', 'mcp_servers={}'],
  'replacement-table': ['-c', 'mcp_servers={crew={command="/bin/false"}}'],
  'disable-leaf': ['-c', 'mcp_servers.ambient.enabled=false'],
};
for (const [name, overrides] of Object.entries(mcpCases)) {
  const items = JSON.parse(run(name, [...overrides, 'mcp', 'list', '--json']));
  console.log(name, items.map(i => ({ name: i.name, enabled: i.enabled })));
  assert.equal(items.find(i => i.name === 'ambient')?.enabled, name !== 'disable-leaf');
}
const folders = { user_canary: path.join(home, '.agents/skills/user'), home_canary: path.join(config, 'skills/home'), project_canary: path.join(work, '.agents/skills/project'), external_canary: path.join(root, 'external') };
for (const [name, dir] of Object.entries(folders)) put(path.join(dir, 'SKILL.md'), `---\nname: ${name}\ndescription: Offline ${name} fixture.\n---\nNo actions required.\n`);
const skills = {
  'skills-baseline': [],
  'disabled-folder': ['user_canary', 'home_canary'].map(n => ({ path: folders[n], enabled: false })),
  'disabled-file': ['user_canary', 'home_canary'].map(n => ({ path: path.join(folders[n], 'SKILL.md'), enabled: false })),
  'external-enable': [{ path: folders.external_canary, enabled: true }],
};
for (const [name, entries] of Object.entries(skills)) {
  const value = '[' + entries.map(e => `{path=${JSON.stringify(e.path)},enabled=${e.enabled}}`).join(',') + ']';
  const text = run(name, ['-c', `skills.config=${value}`, 'debug', 'prompt-input', 'Offline discovery fixture.']);
  const seen = Object.fromEntries(Object.keys(folders).map(n => [n, text.includes(n)]));
  console.log(name, seen);
  assert.equal(seen.external_canary, false);
  assert.equal(seen.project_canary, true);
  assert.equal(seen.user_canary, name !== 'disabled-file');
  assert.equal(seen.home_canary, name !== 'disabled-file');
}
