// Run the native acceptance fixture in a private, network-isolated namespace.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const here = path.dirname(fileURLToPath(import.meta.url));
const options = { runtime: 'all' };
for (let i = 2; i < process.argv.length; i++) {
  const name = process.argv[i];
  if (!['--crew', '--scratch', '--runtime'].includes(name) || !process.argv[i + 1]) throw Error(`Invalid option ${name}`);
  options[name.slice(2)] = process.argv[++i];
}
if (!options.crew || !options.scratch || !['all', 'claude', 'codex', 'pi'].includes(options.runtime)) {
  throw Error('Usage: node notes/compact-cli-probes/run.mjs --crew target/cw/debug/crew --scratch target/NEW-DIR [--runtime claude|codex|pi|all]');
}
const root = path.resolve(options.scratch);
const target = fs.realpathSync('target');
if (!root.startsWith(`${target}/`) || fs.realpathSync(path.dirname(root)) !== target) throw Error('Scratch must be a new direct child of this worktree target');
fs.mkdirSync(root, { mode: 0o700 }); // Never overwrite evidence from a previous run.
fs.mkdirSync(path.join(root, 'tmp'), { mode: 0o700 });
fs.mkdirSync(path.join(root, 'readonly'), { mode: 0o700 });
const executable = (name, override) => {
  const candidate = override || process.env.PATH.split(path.delimiter).map(dir => path.join(dir, name)).find(file => {
    try { fs.accessSync(file, fs.constants.X_OK); return true; } catch { return false; }
  });
  if (!candidate) throw Error(`Missing ${name}`);
  return fs.realpathSync(candidate);
};
const config = {
  root, runtime: options.runtime, crew: fs.realpathSync(options.crew),
  claude: executable('claude', process.env.COMPACT_CLAUDE),
  codex: executable('codex', process.env.COMPACT_CODEX),
  pi: fs.realpathSync(process.env.COMPACT_PI || 'target/toolbox-audit/pi-runtime/node_modules/.bin/pi'),
};
const configPath = path.join(root, 'fixture.json');
fs.writeFileSync(configPath, JSON.stringify(config, null, 2), { mode: 0o600 });
const child = spawn('bwrap', [
  '--die-with-parent', '--new-session', '--unshare-net', '--unshare-pid', '--unshare-ipc',
  '--ro-bind', '/', '/', '--bind', root, root, '--bind', path.join(root, 'tmp'), '/tmp',
  '--ro-bind', path.join(root, 'readonly'), path.join(root, 'readonly'),
  '--dev', '/dev', '--proc', '/proc', '--', process.execPath, path.join(here, 'native.mjs'), configPath,
], { env: { PATH: process.env.PATH, LANG: 'C.UTF-8' }, stdio: 'inherit' });
child.on('error', error => { throw error; });
child.on('close', (code, signal) => {
  if (signal) throw Error(`Probe killed by ${signal}`);
  process.exitCode = code ?? 1;
});
