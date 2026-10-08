// Offline probe: which context files (AGENTS.md, CLAUDE.md) and system prompt
// files (SYSTEM.md, APPEND_SYSTEM.md) reach pi's system prompt, and which
// flags remove them. Needs install-pi.mjs first. No turn is submitted: an
// observation extension reads the system prompt after startup and shuts pi
// down. Run from the repository root; see README.md.
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';

const base = path.resolve('target/toolbox-audit/pi-context');
const cli = path.resolve('target/toolbox-audit/pi-runtime/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js');
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));

// crew passes its briefing with --append-system-prompt; every variant but
// the first does the same.
const briefing = ['--append-system-prompt', 'CREW_BRIEFING_CANARY'];
const variants = {
  'baseline': [['--approve'], false],
  'crew-today': [['--approve', ...briefing], false],
  'no-context-files': [['--approve', ...briefing, '--no-context-files'], false],
  'empty-system-prompt': [['--approve', ...briefing, '--system-prompt', ''], false],
  'closed': [['--approve', ...briefing, '--no-context-files', '--system-prompt', ''], false],
  'no-approve': [['--no-approve', ...briefing], false],
  'private-agent-dir': [['--approve', ...briefing], true],
};
const canaries = [
  'USER_CONTEXT_CANARY', 'USER_SYSTEM_CANARY', 'USER_APPEND_CANARY',
  'PROJECT_CONTEXT_CANARY', 'PROJECT_SYSTEM_CANARY', 'PROJECT_APPEND_CANARY',
  'CREW_BRIEFING_CANARY',
];

for (const [variant, [flags, privateDir]] of Object.entries(variants)) {
  const root = path.join(base, variant);
  fs.rmSync(root, { recursive: true, force: true });
  const home = path.join(root, 'home');
  const config = path.join(home, '.pi/agent');
  const privateConfig = path.join(root, 'private-config');
  const work = path.join(root, 'work');
  for (const dir of [config, privateConfig, work]) fs.mkdirSync(dir, { recursive: true });
  for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
  put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
  put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion=0\nbare=false\n');
  // The operator's files in the agent directory.
  put(path.join(config, 'AGENTS.md'), 'USER_CONTEXT_CANARY: no action required.\n');
  put(path.join(config, 'SYSTEM.md'), 'USER_SYSTEM_CANARY: replacement system prompt.\n');
  put(path.join(config, 'APPEND_SYSTEM.md'), 'USER_APPEND_CANARY: no action required.\n');
  // The repository's files.
  put(path.join(work, 'AGENTS.md'), 'PROJECT_CONTEXT_CANARY: no action required.\n');
  put(path.join(work, '.pi/SYSTEM.md'), 'PROJECT_SYSTEM_CANARY: replacement system prompt.\n');
  put(path.join(work, '.pi/APPEND_SYSTEM.md'), 'PROJECT_APPEND_CANARY: no action required.\n');
  const observedFile = path.join(root, 'observed.json');
  const observe = path.join(root, 'observe.ts');
  put(observe, `import fs from 'node:fs'; export default function(pi) {
    pi.on('session_start', (_event, ctx) => { setTimeout(() => {
      fs.writeFileSync(${JSON.stringify(observedFile)}, JSON.stringify({ system: ctx.getSystemPrompt() }, null, 2));
      ctx.shutdown();
    }, 500) });
  }`);
  const args = [cli, '--mode', 'rpc', '--offline', '--no-session', '--no-extensions', '--no-skills', ...flags, '--extension', observe];
  const env = {
    PATH: process.env.PATH, HOME: home, PI_CODING_AGENT_DIR: privateDir ? privateConfig : config,
    XDG_CONFIG_HOME: path.join(home, '.config'), PI_OFFLINE: '1', LANG: 'C.UTF-8',
  };
  const child = spawn(process.execPath, args, { cwd: work, env, stdio: ['pipe', 'pipe', 'pipe'], detached: true });
  let stdout = '', stderr = '';
  child.stdout.on('data', data => stdout += data);
  child.stderr.on('data', data => stderr += data);
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 12000);
  const finish = setInterval(() => { if (fs.existsSync(observedFile)) child.stdin.end(); }, 50);
  const exit = await new Promise(resolve => child.on('close', (code, signal) => resolve({ code, signal })));
  clearTimeout(timer);
  clearInterval(finish);
  put(path.join(root, 'stdout.txt'), stdout);
  put(path.join(root, 'stderr.txt'), stderr);
  const system = fs.existsSync(observedFile) ? JSON.parse(fs.readFileSync(observedFile, 'utf8')).system ?? '' : null;
  const result = {
    variant, flags, privateDir, ...exit, observed: system !== null,
    // pi's built-in prompt opens with this; a SYSTEM.md replaces it.
    defaultPrompt: system !== null && /coding assistant|expert coding/i.test(system.slice(0, 400)),
    ...Object.fromEntries(canaries.map(name => [name, system !== null && system.includes(name)])),
    head: system === null ? null : system.slice(0, 120),
    errors: stderr.slice(0, 600),
  };
  json(path.join(root, 'result.json'), result);
  console.log(JSON.stringify(result));
}
