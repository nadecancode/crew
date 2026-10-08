// Offline probe: which Claude Code setting sources carry which user, local
// and project components, and whether MCP servers follow --setting-sources.
// Run from the repository root; see README.md.
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { spawn } from 'node:child_process';

const base = path.resolve('target/toolbox-audit/claude-sources');
const binary = process.env.TOOLBOX_CLAUDE || 'claude';
const put = (file, body) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, body);
};
const json = (file, value) => put(file, JSON.stringify(value, null, 2));

// A stdio MCP server that records its start and serves one canary tool.
const server = path.join(base, 'server.mjs');
put(server, "import fs from 'node:fs'; import readline from 'node:readline';\nfs.writeFileSync(process.argv[2], 'started');\nreadline.createInterface({input:process.stdin}).on('line',line=>{let q;try{q=JSON.parse(line)}catch{return}if(q.id===undefined)return;\nlet result=q.method==='initialize'?{protocolVersion:'2025-06-18',capabilities:{tools:{}},serverInfo:{name:'fixture',version:'1'}}:q.method==='tools/list'?{tools:[{name:process.argv[3],description:'Offline fixture tool.',inputSchema:{type:'object',properties:{}}}]}:{};\nconsole.log(JSON.stringify({jsonrpc:'2.0',id:q.id,result}));});");

// Each variant: the --setting-sources value (null: flag left out) and
// whether --strict-mcp-config is passed.
const variants = [
  ['default-strict', null, true],
  ['default-open', null, false],
  ['project-local-strict', 'project,local', true],
  ['project-local-open', 'project,local', false],
  ['project-strict', 'project', true],
  ['project-open', 'project', false],
  ['local-strict', 'local', true],
  ['local-open', 'local', false],
  ['user-strict', 'user', true],
  ['user-open', 'user', false],
  ['empty-strict', '', true],
  ['empty-open', '', false],
];

const canaries = [
  'user_skill_canary', 'user_agent_canary', 'USER_MEMORY_CANARY',
  'project_skill_canary', 'project_agent_canary', 'PROJECT_MEMORY_CANARY',
  'LOCAL_MEMORY_CANARY',
  'mcp__usermcp__user_mcp_canary', 'mcp__localmcp__local_mcp_canary',
  'mcp__projectmcp__project_mcp_canary',
];

for (const [variant, sources, strict] of variants) {
  const root = path.join(base, variant);
  fs.rmSync(root, { recursive: true, force: true });
  const home = path.join(root, 'home');
  const config = path.join(home, '.claude');
  const work = path.join(root, 'work');
  for (const dir of [config, work]) fs.mkdirSync(dir, { recursive: true });
  // Bound project discovery to this disposable repository.
  for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
  put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
  put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
  const skill = name => `---\nname: ${name}\ndescription: Offline toolbox fixture ${name}.\n---\nDo not run any command.\n`;
  const agent = name => `---\nname: ${name}\ndescription: Offline agent fixture ${name}.\ntools: Read\n---\nDo not run any command.\n`;
  const marker = name => path.join(root, `${name}-fired`);

  // User sources: ~/.claude and ~/.claude.json (user-scope MCP).
  put(path.join(config, 'skills/user-skill/SKILL.md'), skill('user_skill_canary'));
  put(path.join(config, 'agents/user-agent.md'), agent('user_agent_canary'));
  put(path.join(config, 'CLAUDE.md'), 'USER_MEMORY_CANARY: no action required.\n');
  json(path.join(config, 'settings.json'), {
    hooks: { SessionStart: [{ hooks: [{ type: 'command', command: `touch '${marker('user-hook')}'` }] }] },
  });
  // Project sources: the checked-in .claude/ and .mcp.json.
  put(path.join(work, '.claude/skills/project-skill/SKILL.md'), skill('project_skill_canary'));
  put(path.join(work, '.claude/agents/project-agent.md'), agent('project_agent_canary'));
  put(path.join(work, 'CLAUDE.md'), 'PROJECT_MEMORY_CANARY: no action required.\n');
  json(path.join(work, '.claude/settings.json'), {
    hooks: { SessionStart: [{ hooks: [{ type: 'command', command: `touch '${marker('project-hook')}'` }] }] },
  });
  json(path.join(work, '.mcp.json'), {
    mcpServers: { projectmcp: { command: process.execPath, args: [server, marker('project-mcp'), 'project_mcp_canary'] } },
  });
  // Local sources: uncommitted settings, CLAUDE.local.md, and local-scope
  // MCP in ~/.claude.json. The local settings approve the project server.
  json(path.join(work, '.claude/settings.local.json'), {
    enableAllProjectMcpServers: true,
    hooks: { SessionStart: [{ hooks: [{ type: 'command', command: `touch '${marker('local-hook')}'` }] }] },
  });
  put(path.join(work, 'CLAUDE.local.md'), 'LOCAL_MEMORY_CANARY: no action required.\n');
  json(path.join(home, '.claude.json'), {
    hasCompletedOnboarding: true,
    mcpServers: { usermcp: { command: process.execPath, args: [server, marker('user-mcp'), 'user_mcp_canary'] } },
    projects: {
      [work]: {
        hasTrustDialogAccepted: true,
        enabledMcpjsonServers: ['projectmcp'],
        mcpServers: { localmcp: { command: process.execPath, args: [server, marker('local-mcp'), 'local_mcp_canary'] } },
      },
    },
  });
  json(path.join(root, 'crew-mcp.json'), { mcpServers: {} });

  const requests = [];
  const stub = http.createServer((req, res) => {
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
  await new Promise(resolve => stub.listen(0, '127.0.0.1', resolve));
  const endpoint = `http://127.0.0.1:${stub.address().port}`;
  const args = ['--print', '--model', 'claude-opus-5-5', '--dangerously-skip-permissions',
    '--mcp-config', path.join(root, 'crew-mcp.json'), '--debug-file', path.join(root, 'debug.log')];
  if (strict) args.push('--strict-mcp-config');
  if (sources !== null) args.push('--setting-sources', sources);
  args.push('--', 'Toolbox fixture. Do not run tools.');
  const env = {
    PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: path.join(home, '.config'), LANG: 'C.UTF-8',
    ANTHROPIC_API_KEY: 'toolbox-fixture-not-a-secret', ANTHROPIC_BASE_URL: endpoint,
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_AUTOUPDATER: '1', ENABLE_TOOL_SEARCH: 'false',
  };
  const child = spawn(binary, args, { cwd: work, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', data => stdout += data);
  child.stderr.on('data', data => stderr += data);
  const timer = setTimeout(() => child.kill('SIGKILL'), 30000);
  const exit = await new Promise(resolve => child.on('close', (code, signal) => resolve({ code, signal })));
  clearTimeout(timer);
  stub.closeAllConnections();
  await new Promise(resolve => stub.close(resolve));
  json(path.join(root, 'requests.json'), requests);
  put(path.join(root, 'stdout.txt'), stdout);
  put(path.join(root, 'stderr.txt'), stderr);
  const captured = JSON.stringify(requests);
  const started = name => fs.existsSync(marker(name));
  const result = {
    variant, sources, strict, ...exit, requests: requests.length,
    hooks: { user: started('user-hook'), local: started('local-hook'), project: started('project-hook') },
    mcpStarted: { user: started('user-mcp'), local: started('local-mcp'), project: started('project-mcp') },
    ...Object.fromEntries(canaries.map(name => [name, captured.includes(name)])),
  };
  json(path.join(root, 'result.json'), result);
  console.log(JSON.stringify(result));
}
