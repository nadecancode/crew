// Called only by run.mjs: localhost stub and every child share an empty network.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { startStub } from '../btw-probes/stub.mjs';

const config = JSON.parse(fs.readFileSync(process.argv[2]));
const { root, crew } = config;
const put = (file, bytes) => { fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 }); fs.writeFileSync(file, bytes); };
const json = (file, value) => put(file, JSON.stringify(value, null, 2));
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
const walk = dir => fs.existsSync(dir) ? fs.readdirSync(dir, { withFileTypes: true }).flatMap(e => e.isDirectory() ? walk(path.join(dir, e.name)) : [path.join(dir, e.name)]) : [];
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const env = home => ({ PATH: process.env.PATH, HOME: home, XDG_CONFIG_HOME: `${home}/config`, XDG_DATA_HOME: `${home}/data`, XDG_STATE_HOME: `${home}/state`, XDG_CACHE_HOME: `${home}/cache`, XDG_RUNTIME_DIR: '/tmp/run', LANG: 'C.UTF-8', TZ: 'UTC' });
const runs = {};
async function run(label, binary, args, options = {}) {
  const child = spawn(binary, args, { env: options.env || env(`${root}/version-home`), cwd: options.cwd || root, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
  let stdout = '', stderr = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  const timer = setTimeout(() => { try { process.kill(-child.pid, 'SIGKILL'); } catch {} }, 45000);
  const exit = await new Promise((resolve, reject) => {
    child.on('error', reject);
    child.on('close', (code, signal) => resolve({ code, signal }));
  }).finally(() => clearTimeout(timer));
  put(`${root}/runs/${label}.stdout`, stdout);
  put(`${root}/runs/${label}.stderr`, stderr);
  runs[label] = exit;
  assert.deepEqual(exit, { code: 0, signal: null }, `${label}: ${stderr.slice(-1600)}`);
  return stdout;
}

const versions = {};
for (const [name, expected] of [['claude', '2.1.291 (Claude Code)'], ['codex', 'codex-cli 0.159.0'], ['pi', '1.1.0']]) {
  const binary = name === 'pi' ? process.execPath : config[name];
  const args = name === 'pi' ? [config.pi, '--version'] : ['--version'];
  versions[name] = (await run(`version-${name}`, binary, args)).trim();
  assert.equal(versions[name], expected, `${name} version is not audited`);
}
json(`${root}/versions.json`, { ...versions, node: process.version, executable_sha256: Object.fromEntries(['crew', 'claude', 'codex', 'pi'].map(name => [name, sha(fs.readFileSync(config[name]))])) });

// Materialize the actual built-in via a real isolated daemon. The member CLI is
// a recorded stand-in; native loading/permission/history are exercised below.
const work = `${root}/workspace`;
const fixtureEnv = { ...env(`${root}/daemon-home`), CLAUDE_CONFIG_DIR: `${root}/fake-home`, CREW_GLOBAL_CONFIG: `${root}/no-global.toml`, PATH: `${root}/bin:${process.env.PATH}` };
put(`${root}/bin/claude`, `#!/bin/sh\nexec ${quote(crew)} fake-cli claude "$@"\n`);
fs.chmodSync(`${root}/bin/claude`, 0o700);
json(`${root}/team.json`, { project: 'compact-probe', objective: 'Offline toolbox fixture', workspace: { path: work, init: true, isolation: 'shared', vcs: 'git', sandbox: { mode: 'off' } }, backend: 'pty', orchestrator: { agent: false }, defaults: { max_restarts: 0 }, agents: [{ id: 'probe', role: 'Offline fixture', runtime: 'claude', toolbox: { include: ['compact-cli'] } }] });
const stub = await startStub();
const reports = [];
try {
  await run('daemon-start', crew, ['run', `${root}/team.json`, '--no-attach'], { env: fixtureEnv });
  const plugin = `${work}/.crew/agents/probe/crew-toolbox`;
  const skill = `${plugin}/skills/compact-cli`;
  for (let i = 0; i < 300 && !fs.existsSync(`${skill}/scripts/output`); i++) await pause(50);
  assert.ok(fs.existsSync(`${skill}/scripts/output`), 'actual launcher materialized compact-cli');
  const pinNames = fs.readdirSync(`${work}/.crew/bin`).filter(name => name.startsWith('crew-'));
  assert.equal(pinNames.length, 1);
  const pin = `${work}/.crew/bin/${pinNames[0]}`;
  const manifest = JSON.parse(fs.readFileSync(`${skill}/helper.json`));
  assert.equal(manifest.helper_sha256, sha(fs.readFileSync(pin)));
  const identity = `${work}/.crew/agents/probe/identity.json`;
  const capture = `${root}/capture.txt`;
  const raw = fs.readFileSync(new URL('../../crates/crew-core/tests/fixtures/output-view/cargo-success.txt', import.meta.url), 'utf8');
  put(capture, raw);
  const binding = { CREW_IDENTITY: identity, CREW_COMPACT_CLI_HELPER: pin };
  const viewArgs = ['view', capture, '--exit-code', '0'];
  const once = await run('helper-first', `${skill}/scripts/output`, viewArgs, { env: { ...fixtureEnv, ...binding } });
  const view = JSON.parse(once);
  assert.equal(view.kind, 'summary');
  await pause(1100);
  assert.equal(await run('helper-clock-repeat', `${skill}/scripts/output`, viewArgs, { env: { ...fixtureEnv, ...binding } }), once);
  const recovery = ['raw', view.source.reference, '--length', '65536'];
  const viewCommand = [quote(`${skill}/scripts/output`), ...viewArgs.map(quote)].join(' ');
  const recoverCommand = [quote(`${skill}/scripts/output`), ...recovery.map(quote)].join(' ');

  for (const runtime of ['claude', 'codex', 'pi']) {
    if (config.runtime !== 'all' && config.runtime !== runtime) continue;
    for (const policy of runtime === 'codex' ? ['workspace-write', 'danger-full-access'] : ['runtime-default']) {
    const arms = [];
    for (const enabled of [false, true]) {
      const label = `${runtime}-${policy}-${enabled ? 'enabled' : 'baseline'}`;
      const base = `${root}/${label}`, home = `${base}/home`, cwd = `${base}/work`;
      const source = Array.from({ length: 170 }, (_, i) => `source line ${i + 1}: exact version=1.${i} checksum=${i.toString(16).padStart(8, '0')}`).join('\n') + '\n';
      put(`${cwd}/source.txt`, source);
      put(`${cwd}/.git/HEAD`, 'ref: refs/heads/probe\n');
      put(`${cwd}/.git/config`, '[core]\nrepositoryformatversion=0\nbare=false\n');
      fs.mkdirSync(`${cwd}/.git/objects`, { recursive: true });
      fs.mkdirSync(`${cwd}/.git/refs/heads`, { recursive: true });
      const canary = policy === 'danger-full-access' ? `${root}/readonly/${label}-forbidden-write` : `${base}/forbidden-write`;
      const deniedCommand = `ls -1 ${quote(cwd)} && touch ${quote(canary)}`;
      const nativeEnv = { ...env(home), ...binding };
      let binary = config[runtime], args, resume, transcript;
      const session = crypto.randomUUID();
      let script;
      if (runtime === 'claude') {
        nativeEnv.CLAUDE_CONFIG_DIR = `${home}/claude`;
        Object.assign(nativeEnv, { ANTHROPIC_API_KEY: 'offline-placeholder', ANTHROPIC_BASE_URL: stub.url, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_AUTOUPDATER: '1', ENABLE_TOOL_SEARCH: 'false' });
        json(`${home}/claude/.claude.json`, { hasCompletedOnboarding: true, projects: { [cwd]: { hasTrustDialogAccepted: true } } });
        json(`${base}/mcp.json`, { mcpServers: {} });
        json(`${base}/settings.json`, { permissions: { allow: ['Read', 'Bash(ls:*)', `Bash(${skill}/scripts/output:*)`] } });
        args = ['--print', '--model', 'claude-opus-5-5', '--permission-mode', 'dontAsk', '--setting-sources', '', '--strict-mcp-config', '--mcp-config', `${base}/mcp.json`, '--settings', `${base}/settings.json`, ...(enabled ? ['--plugin-dir', plugin] : [])];
        resume = ['--resume', session];
        script = [
          { model: 'claude-opus-5-5', name: 'Bash', input: { command: deniedCommand } },
          { model: 'claude-opus-5-5', name: 'Read', input: { file_path: `${cwd}/source.txt`, offset: 1, limit: 150 } },
          ...(enabled ? [{ model: 'claude-opus-5-5', name: 'Bash', input: { command: viewCommand } }, { model: 'claude-opus-5-5', name: 'Bash', input: { command: recoverCommand } }] : []),
        ];
        stub.setLabel(label); stub.script(label, script);
        await run(label, binary, [...args, '--session-id', session, '--', 'Offline fixture.'], { env: nativeEnv, cwd });
        transcript = walk(`${home}/claude/projects`).find(file => file.endsWith(`${session}.jsonl`));
      } else if (runtime === 'codex') {
        nativeEnv.CODEX_HOME = `${home}/codex`;
        nativeEnv.COMPACT_API_KEY = 'offline-placeholder';
        put(`${home}/codex/config.toml`, `model="gpt-6"\nmodel_provider="fixture"\n[model_providers.fixture]\nname="Fixture"\nbase_url=${JSON.stringify(stub.url + '/v1')}\nwire_api="responses"\nenv_key="COMPACT_API_KEY"\n[features]\nplugins=false\n[projects.${JSON.stringify(cwd)}]\ntrust_level="trusted"\n`);
        if (enabled) { fs.mkdirSync(`${home}/.agents/skills`, { recursive: true }); fs.cpSync(skill, `${home}/.agents/skills/compact-cli`, { recursive: true }); }
        args = ['--no-daemon', '--sandbox', policy, '-c', 'approval_policy="never"', 'exec', '--skip-git-repo-check', '--json', '--add-dir', `${work}/.crew/agents/probe/rw`];
        script = [{ name: 'exec_command', input: { cmd: deniedCommand } }, { name: 'exec_command', input: { cmd: `sed -n '1,150p' ${quote(`${cwd}/source.txt`)}` } }, ...(enabled ? [{ name: 'exec_command', input: { cmd: viewCommand } }, { name: 'exec_command', input: { cmd: recoverCommand } }] : [])];
        stub.setLabel(label); stub.script(label, script);
        const out = await run(label, binary, [...args, 'Offline fixture.'], { env: nativeEnv, cwd });
        const id = out.split('\n').filter(Boolean).map(line => JSON.parse(line)).find(event => event.type === 'thread.started')?.thread_id;
        assert.ok(id, 'native thread id');
        transcript = walk(`${home}/codex/sessions`).find(file => file.endsWith(`${id}.jsonl`));
        resume = ['resume', id, 'Follow-up fixture.'];
      } else {
        binary = process.execPath;
        nativeEnv.PI_CODING_AGENT_DIR = `${home}/pi`;
        nativeEnv.PI_OFFLINE = '1';
        json(`${home}/pi/settings.json`, {});
        json(`${home}/pi/models.json`, { providers: { fixture: { baseUrl: stub.url, api: 'anthropic-messages', apiKey: 'offline-placeholder', models: [{ id: 'claude-opus-5-5', contextWindow: 200000, maxTokens: 8192 }] } } });
        // pi has no Claude-style native permission mode. This unchanged fixture
        // policy proves skill loading cannot override an extension's denial.
        put(`${base}/guard.ts`, `export default function(pi) { pi.on('tool_call', event => { if (event.toolName === 'bash' && event.input.command.includes('forbidden-write')) return {block:true,reason:'Fixture denies this write'}; }); }`);
        args = [config.pi, '--print', '--mode', 'json', '--offline', '--provider', 'fixture', '--model', 'claude-opus-5-5', '--no-approve', '--no-extensions', '--extension', `${base}/guard.ts`, '--no-skills', ...(enabled ? ['--skill', skill] : []), '--session-dir', `${base}/sessions`, '--session-id', session];
        resume = ['Follow-up fixture.'];
        script = [{ name: 'bash', input: { command: deniedCommand } }, { name: 'read', input: { path: `${cwd}/source.txt`, offset: 1, limit: 150 } }, ...(enabled ? [{ name: 'bash', input: { command: viewCommand } }, { name: 'bash', input: { command: recoverCommand } }] : [])];
        stub.setLabel(label); stub.script(label, script);
        await run(label, binary, [...args, 'Offline fixture.'], { env: nativeEnv, cwd });
        transcript = walk(`${base}/sessions`).find(file => file.endsWith('.jsonl'));
      }
      assert.equal(fs.existsSync(canary), false, `${label}: denied write never ran`);
      assert.equal(fs.readFileSync(`${cwd}/source.txt`, 'utf8'), source);
      assert.ok(transcript, `${label}: native transcript exists`);
      const before = fs.readFileSync(transcript);
      if (runtime === 'claude') {
        const file = before.toString().split('\n').filter(Boolean).map(line => JSON.parse(line)).map(r => r.toolUseResult?.file).find(Boolean);
        assert.ok(file, 'native Read metadata retained');
        assert.equal(file.numLines, 150);
        assert.equal(file.startLine, 1);
        assert.equal(file.totalLines, 171);
        assert.equal(file.content, source.split('\n').slice(0, 150).join('\n'));
      }
      const requests = () => stub.requests.filter(r => r.label === label && (runtime === 'codex' ? r.path.includes('/responses') : r.path.includes('/messages') && !r.path.includes('count_tokens') && r.body?.model === 'claude-opus-5-5'));
      const history = requests().at(-1).body;
      const outputs = runtime === 'codex'
        ? [...new Map(requests().flatMap(r => r.body.input).filter(item => item.type === 'function_call_output').map(item => [item.call_id, item.output])).values()]
        : [...new Map(requests().flatMap(r => r.body.messages).flatMap(m => Array.isArray(m.content) ? m.content.filter(c => c.type === 'tool_result') : []).map(c => [c.tool_use_id, c.content])).values()];
      assert.equal(outputs.length, enabled ? 4 : 2, `${label}: every scripted tool result observed`);
      const read = JSON.stringify(outputs[1]);
      assert.ok(read.includes('source line 1: exact version=1.0 checksum=00000000'), `${label}: native read starts exactly`);
      assert.ok(read.includes('source line 150: exact version=1.149 checksum=00000095'), `${label}: native read preserves selected line`);
      assert.ok(!read.includes('source line 151:'), `${label}: read range unchanged`);
      for (const line of source.split('\n').slice(0, 150)) assert.ok(read.includes(line), `${label}: every selected source line remains`);
      if (runtime === 'codex') assert.equal(outputs[1].split('\nOutput:\n')[1], source.split('\n').slice(0, 150).join('\n') + '\n');
      if (enabled) {
        if (runtime === 'codex' && policy === 'workspace-write') {
          // Explicit invocation stays denied. Production marks this selection
          // unavailable instead of widening native filesystem/network policy.
          for (const output of outputs.slice(2)) {
            assert.ok(JSON.stringify(output).includes('cannot reach the crew daemon'));
            assert.ok(JSON.stringify(output).includes('Operation not permitted'));
          }
        } else {
        assert.ok(JSON.stringify(outputs[2]).includes('cargo_test_v1'), `${label}: compact view reached native model`);
        assert.ok(JSON.stringify(outputs[3]).includes('test result: ok.'), `${label}: raw recovery reached native model`);
        assert.equal(await run(`${label}-exact-raw`, `${skill}/scripts/output`, recovery, { env: nativeEnv }), raw);
        }
        assert.ok(JSON.stringify(requests()[0].body).includes('compact-cli'), `${label}: skill discoverable`);
      }
      stub.setLabel(`${label}-resume`);
      const follow = runtime === 'claude' ? [...args, ...resume, '--', 'Follow-up fixture.'] : runtime === 'codex' ? [...args.slice(0, -2), ...resume] : [...args, ...resume];
      await run(`${label}-resume`, binary, follow, { env: nativeEnv, cwd });
      const after = fs.readFileSync(transcript);
      assert.ok(after.subarray(0, before.length).equals(before), `${label}: previously appended transcript bytes unchanged`);
      const resumed = stub.requests.find(r => r.label === `${label}-resume` && (runtime === 'codex' ? r.path.includes('/responses') : r.path.includes('/messages') && !r.path.includes('count_tokens') && r.body?.model === 'claude-opus-5-5'))?.body;
      assert.ok(resumed, `${label}: follow-up request captured`);
      const clean = value => JSON.parse(JSON.stringify(value, (key, v) => key === 'cache_control' ? undefined : v));
      const earlier = runtime === 'codex' ? history.input : history.messages;
      const later = runtime === 'codex' ? resumed.input : resumed.messages;
      // Runtime-owned boundary ids/cache markers may change; actual tool results
      // already emitted to the model must remain present byte-for-byte.
      for (const result of outputs) assert.ok(JSON.stringify(clean(later)).includes(JSON.stringify(clean(result))), `${label}: old result remains in resumed history`);
      assert.ok(later.length > earlier.length);
      arms.push({ enabled, denied: true, native_read: read, transcript_prefix_sha256: sha(before), history_stable: true, requests: requests().length });
    }
    // Timings in Codex's shell envelope are native metadata, not Read bytes.
    if (runtime !== 'codex') assert.equal(arms[0].native_read, arms[1].native_read, `${runtime}: enabling skill leaves native Read output/metadata unchanged`);
    reports.push({ runtime, policy, helper_available: runtime !== 'codex' || policy === 'danger-full-access', arms: arms.map(({ native_read, ...rest }) => ({ ...rest, native_read_sha256: sha(native_read) })) });
    console.log(`${runtime}/${policy}: permission, exact read, helper behavior and append-only history passed`);
    }
  }
  const skillBefore = Object.fromEntries(walk(skill).map(file => [path.relative(skill, file), sha(fs.readFileSync(file))]));
  await run('member-restart', crew, ['agent', 'restart', 'probe', '-C', work], { env: fixtureEnv });
  for (let i = 0; i < 300 && !fs.existsSync(`${skill}/scripts/output`); i++) await pause(50);
  assert.deepEqual(Object.fromEntries(walk(skill).map(file => [path.relative(skill, file), sha(fs.readFileSync(file))])), skillBefore);
  assert.equal(await run('helper-after-restart', `${skill}/scripts/output`, viewArgs, { env: { ...fixtureEnv, ...binding } }), once);
  json(`${root}/report.json`, { versions, helper: manifest, runtime_probes: reports, stable_after_clock_and_restart: true, provider_calls: 0, network: 'unshared namespace; localhost stub only', runs });
} finally {
  json(`${root}/requests.json`, stub.requests);
  await stub.close();
  await run('daemon-stop', crew, ['stop', '-C', work], { env: fixtureEnv }).catch(error => { console.error(error.message); });
}
