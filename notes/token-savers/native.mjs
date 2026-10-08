import fs from 'node:fs';import path from 'node:path';import {spawn} from 'node:child_process';import {startStub} from './stub.mjs';
const scratch=process.env.CREW_FILTER_SCRATCH; if(!scratch)throw Error('Run through run.mjs'); const root=path.join(scratch,'native-read');fs.mkdirSync(root,{recursive:true});
const lowfat=path.join(scratch,'bin/lowfat/lowfat');const stub=await startStub();const reports=[];
for(const hooked of [false,true]) {
 const label=hooked?'hooked':'raw';const base=root+'/'+label;const home=base+'/home';const config=home+'/.claude';const work=base+'/work';
 for(const p of [config,work,work+'/.git/objects',work+'/.git/refs/heads'])fs.mkdirSync(p,{recursive:true});
 fs.writeFileSync(work+'/.git/HEAD','ref: refs/heads/probe\n');fs.writeFileSync(work+'/.git/config','[core]\nrepositoryformatversion=0\nbare=false\n');
 fs.writeFileSync(work+'/Cargo.lock',fs.readFileSync(path.join(scratch,'corpus/Cargo.lock')));
 fs.writeFileSync(config+'/.claude.json',JSON.stringify({hasCompletedOnboarding:true,projects:{[work]:{hasTrustDialogAccepted:true}}}));
 fs.writeFileSync(base+'/mcp.json',JSON.stringify({mcpServers:{}}));
 const record=base+'/post-read.mjs';fs.writeFileSync(record,`import fs from 'node:fs';import {spawnSync} from 'node:child_process';let text='';for await(const c of process.stdin)text+=c;fs.writeFileSync(${JSON.stringify(base+'/hook-input.json')},text);const r=spawnSync(${JSON.stringify(lowfat)},['post-read'],{input:text,encoding:'utf8',env:process.env});fs.writeFileSync(${JSON.stringify(base+'/hook-output.json')},r.stdout);process.stdout.write(r.stdout);process.stderr.write(r.stderr);process.exitCode=r.status;`);
 const settings=hooked?{hooks:{PreToolUse:[{matcher:'Bash',hooks:[{type:'command',command:lowfat+' hook'}]}],PostToolUse:[{matcher:'Read',hooks:[{type:'command',command:process.execPath+' '+record}]}]}}:{};
 fs.writeFileSync(base+'/settings.json',JSON.stringify(settings));
 stub.setLabel(label);stub.script(label,[{model:'claude-opus-5-5',name:'Bash',input:{command:`ls -1 '${work}' && touch '${work}/permission-canary'`,description:'offline permission probe'}},{model:'claude-opus-5-5',name:'Read',input:{file_path:work+'/Cargo.lock',offset:1,limit:150}}]);
 const env={PATH:path.dirname(lowfat)+':'+process.env.PATH,HOME:home,CLAUDE_CONFIG_DIR:config,XDG_CONFIG_HOME:home+'/.config',XDG_DATA_HOME:home+'/.local/share',XDG_CACHE_HOME:home+'/.cache',LANG:'C.UTF-8',LOWFAT_LEVEL:'full',ANTHROPIC_API_KEY:'offline-fixture-key',ANTHROPIC_BASE_URL:stub.url,CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC:'1',DISABLE_AUTOUPDATER:'1',ENABLE_TOOL_SEARCH:'false'};
 const child=spawn('claude',['--print','--model','claude-opus-5-5','--permission-mode','dontAsk','--setting-sources','','--strict-mcp-config','--mcp-config',base+'/mcp.json','--settings',base+'/settings.json','--','Offline fixture.'],{cwd:work,env,stdio:['ignore','pipe','pipe'],detached:true});
 let out='',err='';child.stdout.on('data',b=>out+=b);child.stderr.on('data',b=>err+=b);const timer=setTimeout(()=>{try{process.kill(-child.pid,'SIGKILL')}catch{}},30000);const exit=await new Promise(r=>child.on('close',(code,signal)=>r({code,signal})));clearTimeout(timer);
 const toolResults=[...new Map(stub.requests.filter(r=>r.label===label&&Array.isArray(r.body?.messages)).flatMap(r=>r.body.messages).flatMap(m=>Array.isArray(m.content)?m.content.filter(c=>c.type==='tool_result'):[]).map(c=>[c.tool_use_id,c])).values()];
 reports.push({label,...exit,permissionCanary:fs.existsSync(work+'/permission-canary'),toolResults:toolResults.map(c=>({error:c.is_error??false,bytes:Buffer.byteLength(JSON.stringify(c.content)),text:c.content})),stderr:err});
 fs.writeFileSync(base+'/stdout.txt',out);fs.writeFileSync(base+'/stderr.txt',err);
}
await stub.close();fs.writeFileSync(root+'/requests.json',JSON.stringify(stub.requests,null,2));fs.writeFileSync(root+'/report.json',JSON.stringify(reports,null,2));console.log(JSON.stringify(reports.map(r=>({...r,toolResults:r.toolResults.map(t=>({...t,text:JSON.stringify(t.text).slice(0,220)}))})),null,2));
