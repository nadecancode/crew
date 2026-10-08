import fs from 'node:fs';import path from 'node:path';import crypto from 'node:crypto';import {spawnSync} from 'node:child_process';import {root,getEncoding} from './harness.mjs';
const enc=getEncoding('cl100k_base');
const env={PATH:process.env.PATH,HOME:root+'/runtime-home',XDG_CONFIG_HOME:root+'/runtime-config',XDG_DATA_HOME:root+'/runtime-data',XDG_CACHE_HOME:root+'/runtime-cache',TOKF_HOME:root+'/tokf-home',TOKF_TELEMETRY_ENABLED:'false',RTK_TELEMETRY_DISABLED:'1',LANG:'C.UTF-8',NO_COLOR:'1'};
function run(name,args,input,extra={}){const r=spawnSync('bwrap',['--die-with-parent','--unshare-net','--ro-bind','/','/','--bind',root,root,'--dev','/dev','--proc','/proc','--',root+'/bin/'+name+'/'+name,...args],{cwd:root,env:{...env,...extra},input,encoding:'utf8',maxBuffer:8*1024*1024,timeout:10000});if(r.error)throw r.error;return {status:r.status,stdout:r.stdout,stderr:r.stderr};}
const success=fs.readFileSync(root+'/corpus/cargo-success.txt','utf8');
const fixtures={
'cargo-warning.txt':{text:'warning: fixture unexpected cfg condition at crates/crew/src/lib.rs:1:1\n'+success,note:'Synthetic warning prepended to real successful crew tests.'},
'cargo-zero.txt':{text:'running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 198 filtered out; finished in 0.00s\n',note:'Synthetic cargo zero-tests result; 198 tests excluded.'},
'cargo-unknown.txt':{text:'CUSTOM_RUNNER: not a libtest format; check artifact fixture.results\n',note:'Synthetic unknown runner, exit 0.'},
'cargo-json.txt':{text:'{"reason":"build-finished","success":true}\n',note:'Synthetic cargo machine-output record, exit 0.'},
'mode-only.diff':{text:'diff --git a/scripts/build-env b/scripts/build-env\nold mode 100644\nnew mode 100755\n',note:'Synthetic mode-only change on real crew path.'},
'rename-only.diff':{text:'diff --git a/notes/toolboxes.md b/notes/toolbox-reference.md\nsimilarity index 100%\nrename from notes/toolboxes.md\nrename to notes/toolbox-reference.md\n',note:'Synthetic rename-only change on real crew path.'}
};
for(const [file,data] of Object.entries(fixtures))fs.writeFileSync(root+'/corpus/'+file,data.text);
const rows=[];
function record(file,tool,mode,fn){const raw=fs.readFileSync(root+'/corpus/'+file,'utf8');const a=fn(raw),b=fn(raw);const out=a.stdout;rows.push({file,tool,mode,note:fixtures[file]?.note,status:a.status,rawTokens:enc.encode(raw).length,filteredTokens:enc.encode(out).length,stdoutStable:out===b.stdout,stderrStable:a.stderr===b.stderr,rawIdentical:raw===out,stdout:out,stderr:a.stderr,inputSha256:crypto.createHash('sha256').update(raw).digest('hex')});}
for(const file of Object.keys(fixtures).filter(x=>x.startsWith('cargo'))){
record(file,'rtk','cargo test'+(file.includes('json')?' --message-format=json':''),()=>run('rtk',['cargo','test',...(file.includes('json')?['--message-format=json']:[])],undefined,{PATH:root+'/shim:'+env.PATH,REPLAY_FILE:root+'/corpus/'+file,REPLAY_EXIT:'0'}));
record(file,'tokf','apply cargo-test',()=>run('tokf',['apply',root+'/filters/tokf/cargo-test.toml',root+'/corpus/'+file,'--exit-code','0']));
}
for(const file of Object.keys(fixtures).filter(x=>x.endsWith('.diff'))){
for(const level of ['lite','full'])record(file,'lowfat','git-diff-'+level,raw=>run('lowfat',['filter',root+'/filters/lowfat/git.lf','--sub','diff','--level',level],raw));
record(file,'rtk','diff',raw=>run('rtk',['diff','-'],raw));
record(file,'tokf','git-diff',()=>run('tokf',['apply',root+'/filters/tokf/git-diff.toml',root+'/corpus/'+file]));
}
const fail=fs.readFileSync(root+'/outputs/cargo-failure.txt--rtk--cargo-test.txt','utf8');const id=fail.match(/rtk recall ([a-z0-9]+)/)?.[1];const recall=run('rtk',['recall',id]);
const a=run('lowfat',[process.execPath,root+'/replay.mjs',root+'/corpus/cargo-failure.txt','101']);
await new Promise(r=>setTimeout(r,1100));
const b=run('lowfat',[process.execPath,root+'/replay.mjs',root+'/corpus/cargo-failure.txt','101']);
const evidence={rows,rtkRecall:{id,status:recall.status,rawIdentical:recall.stdout===fs.readFileSync(root+'/corpus/cargo-failure.txt','utf8'),stdoutBytes:Buffer.byteLength(recall.stdout),stderr:recall.stderr},lowfatFooter:{status:[a.status,b.status],stdoutStable:a.stdout===b.stdout,stderrStable:a.stderr===b.stderr,stderr:[a.stderr,b.stderr]}};
fs.writeFileSync(root+'/edge-results.json',JSON.stringify(evidence,null,2)+'\n');
console.log(JSON.stringify(evidence,null,2));
