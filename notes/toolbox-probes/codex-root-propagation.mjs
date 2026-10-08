// Offline propagation probe; run in a PTY. No request leaves localhost.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import net from 'node:net';
import http from 'node:http';
import { spawn } from 'node:child_process';

const { default: WebSocket } = await import(path.resolve('target/toolbox-audit/pi-runtime/node_modules/ws/wrapper.mjs'));
const root = path.resolve('target/toolbox-audit/codex-root-propagation');
const binary = process.env.TOOLBOX_CODEX || 'codex';
const put = (file, body) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, body); };
for (const name of ['home', 'config', 'work', 'skill']) fs.mkdirSync(path.join(root, name), { recursive: true });
put(path.join(root, 'skill/SKILL.md'), '---\nname: propagation_canary\ndescription: Offline propagation marker.\n---\nDo nothing.\n');
const requests = [], protocol = [], connections = [], children = [];
let captured;
const capture = new Promise(resolve => { captured = resolve; });
const httpServer = http.createServer((request, response) => {
  let body = '';
  request.on('data', data => { body += data; });
  request.on('end', () => {
    requests.push({ path: request.url, body: JSON.parse(body) });
    response.writeHead(400, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ error: { message: 'Offline probe complete', type: 'invalid_request_error' } }));
    captured();
  });
});
await new Promise(resolve => httpServer.listen(0, '127.0.0.1', resolve));
put(path.join(root, 'config/config.toml'), `model='gpt-6'\nmodel_provider='fixture'\n[model_providers.fixture]\nname='Fixture'\nbase_url='http://127.0.0.1:${httpServer.address().port}/v1'\nwire_api='responses'\nenv_key='TOOLBOX_DUMMY'\n[projects.${JSON.stringify(path.join(root, 'work'))}]\ntrust_level='trusted'\n[features]\nplugins=false\n`);
const socket = path.resolve(`target/roots-${process.pid}.sock`);
const env = { PATH: process.env.PATH, HOME: path.join(root, 'home'), CODEX_HOME: path.join(root, 'config'), TOOLBOX_DUMMY: 'fixture-not-a-secret', LANG: 'C.UTF-8', TERM: 'xterm-256color' };
const server = spawn(binary, ['app-server', '--listen', 'unix://' + socket], { cwd: path.join(root, 'work'), env, stdio: ['ignore', 'ignore', 'pipe'] });
children.push(server);
let stderr = '', serial = 0;
server.stderr.on('data', data => { stderr += data; });
async function connect() {
  const connection = new WebSocket('ws://localhost/', { createConnection: () => net.createConnection(socket) });
  connections.push(connection);
  await new Promise((resolve, reject) => { connection.once('open', resolve); connection.once('error', reject); });
  const pending = new Map();
  connection.on('message', bytes => {
    const message = JSON.parse(bytes.toString());
    protocol.push(message);
    if (pending.has(message.id)) { pending.get(message.id)(message); pending.delete(message.id); }
  });
  const ask = (method, params) => {
    const id = ++serial;
    const result = new Promise(resolve => pending.set(id, resolve));
    connection.send(JSON.stringify({ id, method, params }));
    return result;
  };
  const initialized = await ask('initialize', { clientInfo: { name: 'toolbox-probe', version: '1', title: 'Probe' }, capabilities: { experimentalApi: true, requestAttestation: false } });
  assert.ok(initialized.result, JSON.stringify(initialized));
  connection.send(JSON.stringify({ method: 'initialized' }));
  return { connection, ask };
}
async function probe() {
  while (!fs.existsSync(socket)) await new Promise(resolve => setTimeout(resolve, 25));
  const setup = await connect();
  const configured = await setup.ask('skills/extraRoots/set', { extraRoots: [path.join(root, 'skill')] });
  assert.deepEqual(configured.result, {});
  setup.connection.close();
  await new Promise(resolve => setup.connection.once('close', resolve));
  const second = await connect();
  const listing = await second.ask('skills/list', { cwds: [path.join(root, 'work')], forceReload: true });
  assert.ok(JSON.stringify(listing).includes('propagation_canary'));
  second.connection.close();
  await new Promise(resolve => second.connection.once('close', resolve));
  // The real TUI connects after both setup clients have gone. It reaches
  // only the local provider stub; no model response or tool call is possible.
  const tui = spawn(binary, ['--remote', 'unix://' + socket, '--no-alt-screen', '--model', 'gpt-6', 'Offline probe. Do not use tools.'], { cwd: path.join(root, 'work'), env, stdio: 'inherit' });
  children.push(tui);
  await capture;
  assert.ok(JSON.stringify(requests).includes('propagation_canary'), 'TUI request lost the extra skill root');
  console.log('Second connection and actual TUI request contain propagation_canary.');
}
let timer;
try {
  await Promise.race([probe(), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('root propagation probe timed out')), 20000); })]);
} finally {
  clearTimeout(timer);
  for (const connection of connections) connection.terminate();
  await Promise.all(children.map(child => {
    if (child.exitCode !== null || child.signalCode !== null) return;
    const closed = new Promise(resolve => child.once('close', resolve));
    child.kill('SIGKILL');
    return closed;
  }));
  httpServer.closeAllConnections();
  await new Promise(resolve => httpServer.close(resolve));
  put(path.join(root, 'protocol.json'), JSON.stringify(protocol, null, 2));
  put(path.join(root, 'requests.json'), JSON.stringify(requests, null, 2));
  put(path.join(root, 'stderr.log'), stderr);
  fs.rmSync(socket, { force: true });
}
