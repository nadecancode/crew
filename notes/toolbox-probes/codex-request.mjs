// Offline capability probe. Run from the repository root; see README.md.
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import {spawn} from 'node:child_process';

const base = path.resolve('target/toolbox-audit/codex-request');
const put = (f,b) => {fs.mkdirSync(path.dirname(f),{recursive:true});fs.writeFileSync(f,b);};
const json = (f,b) => put(f,JSON.stringify(b,null,2));
for(const variant of ['baseline','private-config','multi-agent-off']) {
  const root=path.join(base,variant),home=path.join(root,'home'),config=path.join(root,'config'),work=path.join(root,'work'),privateConfig=path.join(root,'private-config');
  for(const dir of [home,config,work,privateConfig])fs.mkdirSync(dir,{recursive:true});
  for(const d of ['objects','refs/heads'])fs.mkdirSync(path.join(work,'.git',d),{recursive:true});
  put(path.join(work,'.git/HEAD'),'ref: refs/heads/probe\n');
  put(path.join(work,'.git/config'),'[core]\nrepositoryformatversion=0\nbare=false\n');
  for(const [dir,name]of [[config,'user_probe_agent'],[path.join(work,'.codex'),'project_probe_agent']])put(path.join(dir,'agents',name+'.toml'),`name=${JSON.stringify(name)}\ndescription="Offline fixture ${name}."\ndeveloper_instructions="No actions required."\n`);
  const requests=[];
  const server=http.createServer((req,res)=>{let body='';req.on('data',d=>body+=d);req.on('end',()=>{let parsed;try{parsed=JSON.parse(body)}catch{parsed=body}requests.push({path:req.url,body:parsed});res.writeHead(400,{'content-type':'application/json'});res.end(JSON.stringify({error:{message:'Offline toolbox fixture complete.',type:'invalid_request_error',code:'fixture_complete'}}));});});
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const settings=`model="gpt-6"\nmodel_provider="fixture"\n[model_providers.fixture]\nname="Fixture"\nbase_url="http://127.0.0.1:${server.address().port}/v1"\nwire_api="responses"\nenv_key="TOOLBOX_API_KEY"\n[features]\nplugins=false\nmulti_agent=${variant==='multi-agent-off'?'false':'true'}\n[projects.${JSON.stringify(work)}]\ntrust_level="trusted"\n`;
  put(path.join(config,'config.toml'),settings);put(path.join(privateConfig,'config.toml'),settings);
  const env={PATH:process.env.PATH,HOME:home,CODEX_HOME:variant==='private-config'?privateConfig:config,XDG_CONFIG_HOME:path.join(home,'.config'),TOOLBOX_API_KEY:'fixture-not-a-secret',LANG:'C.UTF-8'};
  const args=['--no-daemon','exec','--skip-git-repo-check','--json','Toolbox fixture. Do not run any tool.'];
  const child=spawn((process.env.TOOLBOX_CODEX || 'codex'),args,{env,cwd:work,stdio:['ignore','pipe','pipe'],detached:true});let stdout='',stderr='';
  child.stdout.on('data',d=>stdout+=d);child.stderr.on('data',d=>stderr+=d);
  const timer=setTimeout(()=>{try{process.kill(-child.pid,'SIGKILL')}catch{}},15000);
  const exit=await new Promise(r=>child.on('close',(code,signal)=>r({code,signal})));clearTimeout(timer);server.closeAllConnections();await new Promise(r=>server.close(r));
  json(path.join(root,'requests.json'),requests);put(path.join(root,'stdout.txt'),stdout);put(path.join(root,'stderr.txt'),stderr);
  const all=JSON.stringify(requests);const result={variant,...exit,requests:requests.map(r=>r.path),userRole:all.includes('user_probe_agent'),projectRole:all.includes('project_probe_agent'),spawnTool:all.includes('spawn_agent'),stderr:stderr.slice(0,700)};json(path.join(root,'result.json'),result);console.log(JSON.stringify(result));
}
