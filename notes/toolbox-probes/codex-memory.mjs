// Offline probe: which instruction files and settings reach Codex's
// model-visible input, and which controls remove them. Uses
// `codex debug prompt-input`, which renders the input without a request.
// Run from the repository root; see README.md.
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const base = path.resolve('target/toolbox-audit/codex-memory');
const binary = process.env.TOOLBOX_CODEX || 'codex';
const put = (file, body) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, body);
};

// [variant, extra arguments, use a private CODEX_HOME]
const variants = [
  ['baseline', [], false],
  ['doc-bytes-zero', ['-c', 'project_doc_max_bytes=0'], false],
  ['fallback-empty', ['-c', 'project_doc_fallback_filenames=[]'], false],
  ['developer-override', ['-c', 'developer_instructions="CREW_BRIEFING_CANARY"'], false],
  ['instructions-empty', ['-c', 'instructions=""'], false],
  ['instructions-set', ['-c', 'instructions="CREW_USER_INSTRUCTIONS_CANARY"'], false],
  ['private-home', [], true],
  ['private-home-doc-bytes-zero', ['-c', 'project_doc_max_bytes=0'], true],
];

const canaries = [
  'USER_AGENTS_CANARY', 'USER_OVERRIDE_CANARY', 'USER_DEVELOPER_CANARY',
  'PROJECT_AGENTS_CANARY', 'PROJECT_OVERRIDE_CANARY', 'NESTED_AGENTS_CANARY',
  'CREW_BRIEFING_CANARY', 'CREW_USER_INSTRUCTIONS_CANARY',
];

for (const [variant, extra, privateHome] of variants) {
  const root = path.join(base, variant);
  fs.rmSync(root, { recursive: true, force: true });
  const home = path.join(root, 'home');
  const config = path.join(root, 'config');
  const privateConfig = path.join(root, 'private-config');
  const work = path.join(root, 'work');
  const nested = path.join(work, 'pkg');
  for (const dir of [home, config, privateConfig, nested]) fs.mkdirSync(dir, { recursive: true });
  for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
  put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
  put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
  // The operator's instruction files and settings in the Codex home.
  put(path.join(config, 'AGENTS.md'), 'USER_AGENTS_CANARY: no action required.\n');
  const settings = `model = "gpt-6"\ndeveloper_instructions = "USER_DEVELOPER_CANARY"\n[projects.${JSON.stringify(work)}]\ntrust_level = "trusted"\n`;
  put(path.join(config, 'config.toml'), settings);
  // A private home has only crew's own settings: no instructions.
  put(path.join(privateConfig, 'config.toml'), `model = "gpt-6"\n[projects.${JSON.stringify(work)}]\ntrust_level = "trusted"\n`);
  // The repository's instruction files.
  put(path.join(work, 'AGENTS.md'), 'PROJECT_AGENTS_CANARY: no action required.\n');
  put(path.join(nested, 'AGENTS.md'), 'NESTED_AGENTS_CANARY: no action required.\n');
  const env = {
    PATH: process.env.PATH, HOME: home, CODEX_HOME: privateHome ? privateConfig : config,
    XDG_CONFIG_HOME: path.join(home, '.config'), LANG: 'C.UTF-8',
  };
  const run = spawnSync(binary, ['debug', 'prompt-input', ...extra, 'Toolbox fixture.'], {
    cwd: nested, env, encoding: 'utf8', timeout: 30000,
  });
  put(path.join(root, 'prompt-input.json'), run.stdout || '');
  put(path.join(root, 'stderr.txt'), run.stderr || '');
  const result = {
    variant, extra, privateHome, code: run.status, signal: run.signal,
    ...Object.fromEntries(canaries.map(name => [name, (run.stdout || '').includes(name)])),
    stderr: (run.stderr || '').slice(0, 400),
  };
  put(path.join(root, 'result.json'), JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
}

// The override file replaces AGENTS.md in the same directory.
{
  const variant = 'override-files';
  const root = path.join(base, variant);
  fs.rmSync(root, { recursive: true, force: true });
  const home = path.join(root, 'home');
  const config = path.join(root, 'config');
  const work = path.join(root, 'work');
  for (const dir of [home, config, work]) fs.mkdirSync(dir, { recursive: true });
  for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
  put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
  put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
  put(path.join(config, 'AGENTS.md'), 'USER_AGENTS_CANARY: no action required.\n');
  put(path.join(config, 'AGENTS.override.md'), 'USER_OVERRIDE_CANARY: no action required.\n');
  put(path.join(config, 'config.toml'), `model = "gpt-6"\n[projects.${JSON.stringify(work)}]\ntrust_level = "trusted"\n`);
  put(path.join(work, 'AGENTS.md'), 'PROJECT_AGENTS_CANARY: no action required.\n');
  put(path.join(work, 'AGENTS.override.md'), 'PROJECT_OVERRIDE_CANARY: no action required.\n');
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: config, XDG_CONFIG_HOME: path.join(home, '.config'), LANG: 'C.UTF-8' };
  const run = spawnSync(binary, ['debug', 'prompt-input', 'Toolbox fixture.'], { cwd: work, env, encoding: 'utf8', timeout: 30000 });
  put(path.join(root, 'prompt-input.json'), run.stdout || '');
  const result = {
    variant, code: run.status,
    ...Object.fromEntries(canaries.map(name => [name, (run.stdout || '').includes(name)])),
    stderr: (run.stderr || '').slice(0, 400),
  };
  put(path.join(root, 'result.json'), JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
}
