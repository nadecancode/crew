// Offline capability probe. Run from the repository root; see README.md.
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';

const base = path.resolve('target/toolbox-audit/pi-discovery');
const cli = path.resolve('target/toolbox-audit/pi-runtime/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js');
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const mcp = path.join(base, 'server.mjs');
put(mcp, `import fs from 'node:fs'; import readline from 'node:readline';
fs.writeFileSync(process.argv[2], 'started');
readline.createInterface({input:process.stdin}).on('line',line=>{let q;try{q=JSON.parse(line)}catch{return}fs.appendFileSync(process.argv[2]+'.requests',q.method+'\\n');if(q.id===undefined)return;
let result=q.method==='initialize'?{protocolVersion:'2025-06-18',capabilities:{tools:{}},serverInfo:{name:'fixture',version:'1'}}:q.method==='tools/list'?{tools:[{name:'canary',description:'Offline fixture tool.',inputSchema:{type:'object',properties:{}}}]}:{};
console.log(JSON.stringify({jsonrpc:'2.0',id:q.id,result}));});`);
const variants = {
  baseline: [],
  'no-skills': ['--no-skills'],
  'no-extensions': ['--no-extensions'],
  'builtin-mcp-only': ['--no-extensions', '--extension', 'builtin:mcp'],
  'no-mcp': ['--no-mcp'],
  'no-approve': [],
  'private-selected': ['--no-extensions', '--extension', 'builtin:mcp', '--no-skills'],
};
for (const [variant, flags] of Object.entries(variants)) {
  if (process.argv[2] && process.argv[2] !== variant) continue;
  const root = path.join(base, variant), home = path.join(root, 'home'), config = path.join(home, '.pi/agent'), work = path.join(root, 'work');
  for (const dir of [config, work, path.join(root, 'private-config')]) fs.mkdirSync(dir, { recursive: true });
  for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
  put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
  put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion=0\nbare=false\n');
  const skill = name => `---\nname: ${name}\ndescription: Offline ${name} fixture.\n---\nNo action required.\n`;
  for (const [dir, name] of [[path.join(config, 'skills/user'), 'user_canary'], [path.join(home, '.agents/skills/shared'), 'shared_canary'], [path.join(work, '.pi/skills/project'), 'project_canary'], [path.join(root, 'selected'), 'selected_canary']]) put(path.join(dir, 'SKILL.md'), skill(name));
  for (const [dir,name] of [[config,'user_agent_canary'],[path.join(work,'.pi'),'project_agent_canary']]) put(path.join(dir,'agents',name+'.md'),`---\nname: ${name}\ndescription: Offline subagent fixture.\n---\nNo action required.\n`);
  const configEntry = label => ({ command: process.execPath, args: [mcp, path.join(root, label + '-mcp-started')], exposure: 'direct' });
  json(path.join(config, 'mcp.json'), { mcpServers: { ambient: configEntry('ambient') } });
  json(path.join(work, '.pi/mcp.json'), { mcpServers: { project: configEntry('project') } });
  put(path.join(config, 'extensions/ambient.ts'), `import fs from 'node:fs'; export default function(pi) { pi.on('session_start',()=>{fs.writeFileSync(${JSON.stringify(path.join(root, 'extension-started'))},'started')}); pi.registerCommand('ambient_command', {description:'Fixture',handler:async()=>{}}); }`);
  const observe = path.join(root, 'observe.ts');
  put(observe, `import fs from 'node:fs'; export default function(pi) {
    pi.registerMcpServer('crew', ${JSON.stringify(configEntry('crew'))});
    pi.on('session_start', (_event,ctx)=>{setTimeout(()=>{
      fs.writeFileSync(${JSON.stringify(path.join(root, 'observed.json'))},JSON.stringify({tools:pi.getAllTools().map(t=>t.name),active:pi.getActiveTools(),commands:pi.getCommands().map(c=>c.name),system:ctx.getSystemPrompt()},null,2));
      ctx.shutdown();
    },1000)});
  }`);
  for (const f of ['ambient-mcp-started', 'project-mcp-started', 'crew-mcp-started', 'extension-started', 'observed.json']) fs.rmSync(path.join(root, f), { force: true });
  const args = [cli, '--mode', 'rpc', '--offline', '--no-session', ...flags, '--extension', observe];
  args.push(variant === 'no-approve' || variant === 'private-selected' ? '--no-approve' : '--approve');
  if (variant === 'no-skills' || variant === 'private-selected') args.push('--skill', path.join(root, 'selected'));
  const env = { PATH: process.env.PATH, HOME: home, PI_CODING_AGENT_DIR: variant === 'private-selected' ? path.join(root, 'private-config') : config, XDG_CONFIG_HOME: path.join(home, '.config'), PI_OFFLINE: '1', LANG: 'C.UTF-8' };
  const child = spawn(process.execPath, args, { cwd: work, env, stdio: ['pipe', 'pipe', 'pipe'], detached: true });
  let stdout = '', stderr = '';
  child.stdout.on('data', data => stdout += data);
  child.stderr.on('data', data => stderr += data);
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 12000);
  const finish = setInterval(() => { if (fs.existsSync(path.join(root, 'observed.json'))) child.stdin.end(); }, 50);
  const exit = await new Promise(resolve => child.on('close', (code, signal) => resolve({code,signal})));
  clearTimeout(timer);
  clearInterval(finish);
  put(path.join(root, 'stdout.txt'), stdout); put(path.join(root, 'stderr.txt'), stderr);
  const observed = fs.existsSync(path.join(root, 'observed.json')) ? JSON.parse(fs.readFileSync(path.join(root, 'observed.json'),'utf8')) : {};
  const result = { variant, ...exit, mcpStarted: ['ambient', 'project', 'crew'].filter(n => fs.existsSync(path.join(root,n+'-mcp-started'))), extensionStarted: fs.existsSync(path.join(root,'extension-started')), skills: ['user_canary','shared_canary','project_canary','selected_canary'].filter(n=>observed.system?.includes(n)), subagents: ['user_agent_canary','project_agent_canary'].filter(n=>JSON.stringify(observed).includes(n)), tools: observed.tools, errors: stderr.slice(0,1000) };
  json(path.join(root, 'result.json'), result); console.log(JSON.stringify(result));
}
