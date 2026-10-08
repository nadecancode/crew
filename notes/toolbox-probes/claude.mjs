// Offline capability probe. Run from the repository root; see README.md.
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { spawn } from 'node:child_process';

const base = path.resolve('target/toolbox-audit/claude-discovery');
const binary = (process.env.TOOLBOX_CLAUDE || 'claude');
const put = (file, body) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, body);
};
const json = (file, value) => put(file, JSON.stringify(value, null, 2));

const mcp = path.join(base, 'server.mjs');
put(mcp, "import fs from 'node:fs'; import readline from 'node:readline';\nfs.writeFileSync(process.argv[2], 'started');\nreadline.createInterface({input:process.stdin}).on('line',line=>{let q;try{q=JSON.parse(line)}catch{return}fs.appendFileSync(process.argv[2]+'.requests',q.method+'\\n');if(q.id===undefined)return;\nlet result=q.method==='initialize'?{protocolVersion:'2025-06-18',capabilities:{tools:{}},serverInfo:{name:'fixture',version:'1'}}:q.method==='tools/list'?{tools:[{name:'canary',description:'Offline fixture tool.',inputSchema:{type:'object',properties:{}}}]}:{};\nconsole.log(JSON.stringify({jsonrpc:'2.0',id:q.id,result}));});");

for (const variant of ['all-sources', 'all-sources-inherit-mcp', 'project-local', 'private-config', 'project-local-selected', 'private-config-selected']) {
  const root = path.join(base, variant);
  const home = path.join(root, 'home');
  const config = path.join(home, '.claude');
  const work = path.join(root, 'work');
  const plugin = path.join(config, 'plugins/cache/probe-market/probe-plugin/1.0.0');
  const privateConfig = path.join(root, 'private-config');
  const selected = path.join(root, 'crew-toolbox');
  for (const dir of [config, work, privateConfig]) fs.mkdirSync(dir, { recursive: true });
  // Bound project discovery to this disposable repository.
  for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
  put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
  put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
  const skill = name => `---\nname: ${name}\ndescription: Offline toolbox fixture ${name}.\n---\nDo not run any command.\n`;
  put(path.join(config, 'skills/user-skill/SKILL.md'), skill('user_skill_canary'));
  put(path.join(config, 'agents/user-agent.md'), '---\nname: user_agent_canary\ndescription: Offline user agent fixture.\ntools: Read\n---\nDo not run any command.\n');
  put(path.join(config, 'CLAUDE.md'), 'USER_MEMORY_CANARY: no action required.\n');
  put(path.join(work, '.claude/skills/project-skill/SKILL.md'), skill('project_skill_canary'));
  json(path.join(plugin, '.claude-plugin/plugin.json'), { name: 'probe-plugin', version: '1.0.0' });
  put(path.join(plugin, 'skills/plugin-skill/SKILL.md'), skill('plugin_skill_canary'));
  const mcpMarker = path.join(root, 'plugin-mcp-started');
  fs.rmSync(mcpMarker, { force: true });
  json(path.join(plugin, '.mcp.json'), { mcpServers: { pluginprobe: { command: process.execPath, args: [mcp, mcpMarker] } } });
  const marketplace = path.join(root, 'marketplace');
  json(path.join(marketplace, '.claude-plugin/marketplace.json'), { name: 'probe-market', owner: { name: 'Fixture' }, plugins: [{ name: 'probe-plugin', source: './probe-plugin' }] });
  fs.cpSync(plugin, path.join(marketplace, 'probe-plugin'), { recursive: true });
  json(path.join(config, 'plugins/known_marketplaces.json'), { 'probe-market': { source: { source: 'directory', path: marketplace }, installLocation: marketplace, lastUpdated: '2026-10-07T00:00:00.000Z' } });
  json(path.join(config, 'plugins/installed_plugins.json'), {
    version: 2,
    plugins: { 'probe-plugin@probe-market': [{ scope: 'user', installPath: plugin, version: '1.0.0', installedAt: '2026-10-07T00:00:00.000Z', lastUpdated: '2026-10-07T00:00:00.000Z' }] },
  });
  const marker = path.join(root, 'user-hook-fired');
  fs.rmSync(marker, { force: true });
  fs.rmSync(path.join(root, 'debug.log'), { force: true });
  // Only fixture paths are interpolated; these generated paths contain no quotes.
  json(path.join(config, 'settings.json'), {
    enabledPlugins: { 'probe-plugin@probe-market': true },
    hooks: { SessionStart: [{ hooks: [{ type: 'command', command: `touch '${marker}'` }] }] },
  });
  json(path.join(root, 'crew-mcp.json'), { mcpServers: {} });
  const selectedHook = path.join(root, 'selected-hook-fired');
  fs.rmSync(selectedHook, { force: true });
  json(path.join(selected, '.claude-plugin/plugin.json'), { name: 'crew-toolbox', version: '1.0.0' });
  put(path.join(selected, 'skills/selected/SKILL.md'), skill('selected_skill_canary'));
  put(path.join(selected, 'agents/selected.md'), '---\nname: selected_agent_canary\ndescription: Offline selected agent fixture.\ntools: Read\n---\nNo actions required.\n');
  json(path.join(selected, 'hooks/hooks.json'), { hooks: { SessionStart: [{ hooks: [{ type: 'command', command: `touch '${selectedHook}'` }] }] } });
  const requests = [];
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', data => body += data);
    req.on('end', () => {
      let parsed;
      try { parsed = JSON.parse(body); } catch { parsed = body; }
      requests.push({ method: req.method, path: req.url, body: parsed });
      // No provider call: fail at the first local request after discovery.
      res.writeHead(400, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ type: 'error', error: { type: 'invalid_request_error', message: 'Offline toolbox fixture complete.' } }));
    });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const endpoint = `http://127.0.0.1:${server.address().port}`;
  const args = ['--print', '--model', 'claude-opus-5-5', '--dangerously-skip-permissions', '--mcp-config', path.join(root, 'crew-mcp.json'), '--debug-file', path.join(root, 'debug.log')];
  if (variant !== 'all-sources-inherit-mcp') args.push('--strict-mcp-config');
  if (variant.startsWith('project-local')) args.push('--setting-sources', 'project,local');
  if (variant.endsWith('-selected')) args.push('--plugin-dir', selected);
  args.push('--', 'Toolbox fixture. Do not run tools.');
  const env = {
    PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: path.join(home, '.config'), LANG: 'C.UTF-8',
    ANTHROPIC_API_KEY: 'toolbox-fixture-not-a-secret', ANTHROPIC_BASE_URL: endpoint,
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_AUTOUPDATER: '1', ENABLE_TOOL_SEARCH: 'false',
  };
  if (variant.startsWith('private-config')) env.CLAUDE_CONFIG_DIR = privateConfig;
  const child = spawn(binary, args, { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', data => stdout += data);
  child.stderr.on('data', data => stderr += data);
  const timer = setTimeout(() => child.kill('SIGKILL'), 20000);
  const exit = await new Promise(resolve => child.on('close', (code, signal) => resolve({ code, signal })));
  clearTimeout(timer);
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
  json(path.join(root, 'requests.json'), requests);
  put(path.join(root, 'stdout.txt'), stdout);
  put(path.join(root, 'stderr.txt'), stderr);
  const captured = JSON.stringify(requests);
  const result = { variant, ...exit, requests: requests.map(req => req.path), hook: fs.existsSync(marker), selectedHook: fs.existsSync(selectedHook), pluginMcpStarted: fs.existsSync(mcpMarker), ...Object.fromEntries(['user_skill_canary', 'project_skill_canary', 'plugin_skill_canary', 'user_agent_canary', 'USER_MEMORY_CANARY', 'selected_skill_canary', 'selected_agent_canary'].map(name => [name, captured.includes(name)])) };
  json(path.join(root, 'result.json'), result);
  console.log(JSON.stringify(result));
}
