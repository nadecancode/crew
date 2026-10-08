// Native Claude core probe against a local synthetic Anthropic stream. It
// never authenticates to a provider or uses an existing member's session.
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { spawn } from 'node:child_process';

const actualBridge = process.argv.includes('--bridge');
const scenario = process.argv.find(arg=>arg.startsWith('--scenario='))?.slice(11) || 'normal';
const root = path.resolve(actualBridge ? `target/cache-delivery-sdk/native-bridge-${scenario}` : 'target/cache-delivery-sdk/native');
const work = path.join(root, 'work'), config = path.join(root, 'config'), plugin = path.join(root, 'plugin');
const put = (file, text) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, text); };
for (const dir of [work, config]) fs.mkdirSync(dir, { recursive: true });
for (const dir of ['objects', 'refs/heads']) fs.mkdirSync(path.join(work, '.git', dir), { recursive: true });
put(path.join(work, '.git/HEAD'), 'ref: refs/heads/probe\n');
put(path.join(work, '.git/config'), '[core]\nrepositoryformatversion = 0\nbare = false\n');
put(path.join(root, 'mcp.json'), '{"mcpServers":{}}');
put(path.join(plugin, '.claude-plugin/plugin.json'), JSON.stringify({ name: 'native-delivery-probe', version: '0.0.1' }));
put(path.join(plugin, 'hooks/hooks.json'), '{"modules":["./register.ts"]}');
const events = path.join(root, 'events.jsonl'); fs.rmSync(events, { force: true });
const logger = path.join(root, 'log.mjs');
put(logger, "import fs from 'node:fs'; fs.appendFileSync(process.argv[2], process.argv[3]+'\\n');");
put(path.join(plugin, 'hooks/register.ts'), `import type { Register } from 'claude-code'
let sent = false
async function record($: any, type: string, data: object) {
    await $.process.run([${JSON.stringify(process.execPath)}, ${JSON.stringify(logger)}, ${JSON.stringify(events)}, JSON.stringify({type,at:await $.clock.now(),...data})])
}
export const register: Register = on => {
  on('turn.start', async ($, e, next) => {
    await record($,'start',{turn:e.turnId,text:e.text})
    if (!sent) { sent = true; void (async () => {
      await $.clock.sleep(500)
      const one = $.prompt.submit({text:'[crew] Delivery batch fixture-A\\nidentical body',asUser:true})
      await record($,'submitted',{batch:'A'})
      void one.then(r => record($,'accepted',{batch:'A',result:r})).catch(e => record($,'failed',{batch:'A',reason:String(e)}))
      const two = $.prompt.submit({text:'[crew] Delivery batch fixture-B\\nidentical body',asUser:true})
      await record($,'submitted',{batch:'B'})
      void two.then(r => record($,'accepted',{batch:'B',result:r})).catch(e => record($,'failed',{batch:'B',reason:String(e)}))
    })() }
    return next(e)
  })
  on('turn.complete', async ($, e, next) => { const result=await next(e); await record($,'end',{turn:e.turnId}); return result })
}
`);
if (actualBridge) {
  const relay = path.join(root, 'fixture-crew');
  put(relay, `#!/usr/bin/env node
import fs from 'node:fs';
const args=process.argv.slice(2),events=${JSON.stringify(events)};
if(args[0]==='hook'){
 const data=JSON.parse(args.at(-1));fs.appendFileSync(events,JSON.stringify({type:args[1],at:Date.now(),...data})+'\\n');
 console.log(JSON.stringify({ok:true}));
}else if(args[0]==='bridge'){
 console.log(JSON.stringify({native_bridge_ready:{ok:true,launch:'fixture-launch'}}));
 setTimeout(()=>console.log(JSON.stringify({ids:['m-A'],batch_id:'offer-A',text:'identical body',immediate:false})),500);
 setTimeout(()=>console.log(JSON.stringify({ids:['m-B'],batch_id:'offer-B',text:'identical body',immediate:false})),700);
 setTimeout(()=>process.exit(0),${scenario==='restart'?1500:35000});
}else if(args[0]==='delivered'){
 fs.appendFileSync(events,JSON.stringify({type:'immediate-delivered',ids:args.slice(3),at:Date.now()})+'\\n');
}
`);
  fs.chmodSync(relay, 0o700);
  const source=fs.readFileSync('crates/crew/assets/crew-bridge.register.ts','utf8')
    .replace('__CREW_BIN__',JSON.stringify(relay)).replace('__IDENTITY__','"fixture-identity"')
    .replace('__DELIVER__','true').replace('__TONE__','null');
  put(path.join(plugin,'hooks/register.ts'),source);
}
if (scenario==='slow' || scenario==='rewrite' || scenario==='drop' || scenario==='shortcircuit') {
  const guard = path.join(root,'guard');
  put(path.join(guard,'.claude-plugin/plugin.json'),JSON.stringify({name:'delivery-fixture-guard',version:'0.0.1'}));
  put(path.join(guard,'hooks/hooks.json'),'{"modules":["./register.ts"]}');
  const action = scenario==='slow' ? `await $.process.run([${JSON.stringify(process.execPath)}, '-e', 'setTimeout(()=>{},21000)']); return next(e)`
    : scenario==='rewrite' ? `return next({...e,text:e.text.replace('identical body','rewritten body')})`
    : scenario==='drop' ? `return {drop:'fixture refusal'}` : `return {text:e.text}`;
  put(path.join(guard,'hooks/register.ts'),`import type {Register} from 'claude-code'\nexport const register:Register=on=>{on('prompt.submit',async($,e,next)=>{if(e.text.includes('Delivery batch offer-A')){${action}}return next(e)})}\n`);
}
const captures = [], sockets = new Set();
const server = http.createServer((req, res) => {
  let text = ''; req.on('data', b => text += b);
  req.on('end', () => {
    const n = captures.length + 1;
    let body; try { body = JSON.parse(text); } catch { body = null; }
    captures.push({path:req.url,at:Date.now(),body});
    if (!req.url.startsWith('/v1/messages')) { res.writeHead(200, {'content-type':'application/json'}); res.end('{}'); return; }
    res.writeHead(200, {'content-type':'text/event-stream'});
    const send = value => res.write(`event: ${value.type}\ndata: ${JSON.stringify(value)}\n\n`);
    send({type:'message_start',message:{id:'msg_'+n,type:'message',role:'assistant',model:'claude-opus-5-5',content:[],stop_reason:null,stop_sequence:null,usage:{input_tokens:100,output_tokens:0}}});
    const textIndex = scenario==='thinking'?1:0;
    if (scenario==='thinking') {
      send({type:'content_block_start',index:0,content_block:{type:'thinking',thinking:''}});
      send({type:'content_block_delta',index:0,delta:{type:'thinking_delta',thinking:'Synthetic retained reasoning '+n}});
      send({type:'content_block_delta',index:0,delta:{type:'signature_delta',signature:'fixture-signature-'+n}});
      send({type:'content_block_stop',index:0});
    }
    send({type:'content_block_start',index:textIndex,content_block:{type:'text',text:''}});
    setTimeout(() => {
      send({type:'content_block_delta',index:textIndex,delta:{type:'text_delta',text:'Synthetic response '+n}});
      send({type:'content_block_stop',index:textIndex});
      send({type:'message_delta',delta:{stop_reason:'end_turn',stop_sequence:null},usage:{output_tokens:10}});
      send({type:'message_stop'}); res.end();
    }, n===1?3500:500);
  });
});
server.on('connection', socket=>{sockets.add(socket);socket.on('close',()=>sockets.delete(socket));});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const binary='/home/nade/.local/share/claude/versions/2.1.291';
const env={PATH:process.env.PATH,LANG:'C.UTF-8',XDG_CONFIG_HOME:path.join(root,'xdg'),CLAUDE_CONFIG_DIR:config,
 ANTHROPIC_API_KEY:'fixture-not-a-credential',ANTHROPIC_BASE_URL:`http://127.0.0.1:${server.address().port}`,
 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC:'1',DISABLE_AUTOUPDATER:'1',ENABLE_TOOL_SEARCH:'false',CLAUDE_CODE_NO_MODEL_FALLBACK:'1'};
const args=['--print','--input-format','stream-json','--output-format','stream-json','--verbose','--model','claude-opus-5-5','--dangerously-skip-permissions','--mcp-config',path.join(root,'mcp.json'),'--strict-mcp-config','--setting-sources','project,local','--plugin-dir',plugin,'--debug-file',path.join(root,'debug.log')];
if (['slow','rewrite','drop','shortcircuit'].includes(scenario)) args.push('--plugin-dir',path.join(root,'guard'));
const child=spawn(binary,args,{env,cwd:work,stdio:['pipe','pipe','pipe']}); let stdout='',stderr='';
child.stdout.on('data',b=>stdout+=b);child.stderr.on('data',b=>stderr+=b);
child.stdin.write(JSON.stringify({type:'user',message:{role:'user',content:'Initial synthetic foreground turn. Do not run tools.'}})+'\n');
const closeInput=setTimeout(()=>child.stdin.end(),scenario==='slow'?30000:10000);
const deadline=setTimeout(()=>child.kill('SIGKILL'),scenario==='slow'?40000:15000);
const exit=await new Promise(resolve=>child.on('close',(code,signal)=>resolve({code,signal})));
clearTimeout(closeInput);clearTimeout(deadline);for(const socket of sockets)socket.destroy();await new Promise(resolve=>server.close(resolve));
put(path.join(root,'stdout.jsonl'),stdout);put(path.join(root,'stderr.log'),stderr);
put(path.join(root,'requests.json'),JSON.stringify(captures,null,2));
const records=fs.existsSync(events)?fs.readFileSync(events,'utf8').trim().split('\n').filter(Boolean).map(JSON.parse):[];
const report={binary,actual_bridge:actualBridge,scenario,exit,requests:captures.length,provider_calls:0,events:records};put(path.join(root,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report));
