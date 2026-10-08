// Offline capability probe. Run from the repository root; see README.md.
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';

const root = path.resolve('target/toolbox-audit/codex-server');
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const home = path.join(root, 'home');
const config = path.join(root, 'config');
const work = path.join(root, 'work');
const external = path.join(root, 'external-skills');
for (const d of [home, config, work, external]) fs.mkdirSync(d, { recursive: true });
for (const d of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', d), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
const skill = name => `---\nname: ${name}\ndescription: Offline ${name} fixture.\n---\nNo actions required.\n`;
put(path.join(home, '.agents/skills/user/SKILL.md'), skill('user_canary'));
put(path.join(config, 'skills/codex/SKILL.md'), skill('codex_canary'));
put(path.join(work, '.agents/skills/project/SKILL.md'), skill('project_canary'));
put(path.join(external, 'external/SKILL.md'), skill('external_canary'));
put(path.join(config, 'config.toml'), `[features]\nplugins = false\n[projects.${JSON.stringify(work)}]\ntrust_level = "trusted"\n`);
put(path.join(config, 'agents/probe.toml'), 'name="user_probe_agent"\ndescription="Offline custom agent fixture."\ndeveloper_instructions="No actions required."\n');
put(path.join(work, '.codex/agents/probe.toml'), 'name="project_probe_agent"\ndescription="Offline project agent fixture."\ndeveloper_instructions="No actions required."\n');
json(path.join(config, 'hooks.json'), { hooks: { SessionStart: [{ hooks: [{ type: 'command', command: 'echo USER_HOOK_CANARY' }] }] } });
json(path.join(work, '.codex/hooks.json'), { hooks: { SessionStart: [{ hooks: [{ type: 'command', command: 'echo PROJECT_HOOK_CANARY' }] }] } });

for (const variant of ['baseline', 'empty-hooks', 'private-config', 'skip-host-skills', 'hooks-off']) {
  const chosen = variant === 'private-config' ? path.join(root, 'private-config') : config;
  fs.mkdirSync(chosen, { recursive: true });
  if (chosen !== config) put(path.join(chosen, 'config.toml'), `[features]\nplugins = false\n[projects.${JSON.stringify(work)}]\ntrust_level = "trusted"\n`);
  const overrides = {
    'empty-hooks': ['-c', 'hooks={}'],
    'skip-host-skills': ['-c', 'features.skip_host_skill_discovery=true'],
    'hooks-off': ['-c', 'features.hooks=false'],
  };
  const args = ['app-server', '--stdio', ...(overrides[variant] || [])];
  const env = { PATH: process.env.PATH, HOME: home, CODEX_HOME: chosen, XDG_CONFIG_HOME: path.join(home, '.config'), LANG: 'C.UTF-8' };
  const child = spawn((process.env.TOOLBOX_CODEX || 'codex'), args, { cwd: work, env, stdio: ['pipe', 'pipe', 'pipe'] });
  const records = [];
  const pending = new Map();
  let serial = 0, buffer = '', stderr = '';
  child.stderr.on('data', data => stderr += data);
  child.stdout.on('data', data => {
    buffer += data;
    for (;;) {
      const at = buffer.indexOf('\n'); if (at < 0) break;
      const line = buffer.slice(0, at); buffer = buffer.slice(at + 1);
      let message; try { message = JSON.parse(line); } catch { continue; }
      records.push(message);
      if (pending.has(message.id)) { pending.get(message.id)(message); pending.delete(message.id); }
    }
  });
  const request = async (method, params) => {
    const id = ++serial;
    const received = new Promise(resolve => pending.set(id, resolve));
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
    return received;
  };
  const timer = setTimeout(() => { child.kill('SIGKILL'); for (const resolve of pending.values()) resolve({ error: 'timeout' }); pending.clear(); }, 15000);
  try {
    await request('initialize', { clientInfo: { name: 'toolbox-probe', title: 'Offline toolbox probe', version: '1' }, capabilities: { experimentalApi: true, requestAttestation: false } });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'initialized' }) + '\n');
    const skills = await request('skills/list', { cwds: [work], forceReload: true });
    const hooks = await request('hooks/list', { cwds: [work] });
    const configuration = await request('config/read', { cwd: work, includeLayers: true });
    await request('skills/extraRoots/set', { extraRoots: [external] });
    const extra = await request('skills/list', { cwds: [work], forceReload: true });
    const names = r => r.result?.data?.flatMap(d => d.skills?.map(s => s.name) || []) || r;
    const summary = { variant, skills: names(skills), extraSkills: names(extra), hooks: hooks.result?.data?.flatMap(d => d.hooks?.map(h => ({ event: h.eventName, command: h.command, source: h.source, enabled: h.enabled, trust: h.trustStatus }))) || hooks, agents: configuration.result?.config?.agents || null };
    json(path.join(root, variant + '.json'), { skills, hooks, configuration, extra });
    json(path.join(root, variant + '-summary.json'), summary);
    console.log(JSON.stringify(summary));
  } finally {
    clearTimeout(timer);
    child.stdin.end();
    child.kill('SIGTERM');
    await new Promise(resolve => child.on('close', resolve));
    put(path.join(root, variant + '.stderr'), stderr);
    json(path.join(root, variant + '-protocol.json'), records);
  }
}
