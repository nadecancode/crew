#!/usr/bin/env node
// Native identity regression. Requires Codex 0.159.0, bubblewrap, Python 3,
// a built Crew CLI, and the immutable QA recipe commit in this checkout.
// Uses synthetic homes, dummy auth, and a loopback-only provider in a net namespace.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {execFileSync, spawnSync} from 'node:child_process';

const options = new Map();
for (let index = 2; index < process.argv.length; index += 2) {
  options.set(process.argv[index], process.argv[index + 1]);
}
const repo = process.cwd();
const required = name => {
  const value = options.get(name);
  if (!value) throw new Error(`Missing ${name}`);
  return path.resolve(value);
};
const crew = required('--crew');
const native = required('--codex');
const python = required('--python');
const output = required('--out');
if (!output.startsWith(repo + path.sep) || fs.existsSync(output)) {
  throw new Error('--out must be a new directory under this checkout');
}
const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
const probe = path.join(output, 'recipe');
fs.mkdirSync(path.join(probe, 'bin'), {recursive: true});
const revision = '378d0b43';
const sources = [];
for (const name of ['native.py', 'provider.py', 'discovery.py']) {
  const source = execFileSync('jj', ['file', 'show', '-r', revision, `scripts/toolboxes/${name}`]);
  sources.push({name, sha256: sha(source)});
  let text = source.toString();
  if (name === 'native.py') {
    text = text.replace('ROOT = Path(__file__).resolve().parents[2]', `ROOT = Path(${JSON.stringify(repo)})`);
  }
  if (name === 'discovery.py') {
    text = text.replace('from native import ROOT, Session, hashes, write_json',
      'from native import ROOT, Session, hashes, write_json, native_receipt');
    const anchor = '        if args.case == "catalogs":';
    if (!text.includes(anchor)) throw new Error('QA recipe changed');
    text = text.replace(anchor, `        if args.child:
            report["queued_after_children"] = []
            for control in report["child_controls"]:
                member = control["parent"]
                session.member = member
                phase = member + "-queue"
                expected = control["original_parent_session_id"]
                offset = session.provider.set_phase(phase, "reply")
                session.command("send", member, "Toolbox acceptance " + phase + ". Reply briefly.")
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    agent = session.poll()
                    receipt = native_receipt(session.root, args.runtime, expected, phase, member)
                    if receipt is not None and len(session.provider.observed()) > offset:
                        report["queued_after_children"].append({"member":member,"expected_parent":expected,
                            "actual_session_id":agent.get("session_id"),"native_receipt":receipt})
                        break
                    time.sleep(.2)
                else:
                    raise TimeoutError("queued delivery did not reach parent")
` + anchor);
  }
  fs.writeFileSync(path.join(probe, name), text);
}
const notifyLog = path.join(output, 'notifications.jsonl');
const queueLog = path.join(output, 'queue.jsonl');
const logger = path.join(probe, 'notify.py');
fs.writeFileSync(logger, `import json,os,sys,time
original=json.loads(sys.argv[1])
record={"payload":json.loads(sys.argv[2]),"raw_payload":sys.argv[2],"at":time.time()}
fd=os.open(${JSON.stringify(notifyLog)},os.O_WRONLY|os.O_CREAT|os.O_APPEND,0o600)
os.write(fd,(json.dumps(record)+"\\n").encode());os.close(fd)
os.execv(original[0],original+[sys.argv[2]])
`);
fs.writeFileSync(path.join(probe, 'bin/codex'), `#!${python}
import json,os,sys
args=sys.argv[1:]
if args and args[0]=="queue":
 record={key:args[args.index(key)+1] for key in ["--thread","--remote"] if key in args}
 fd=os.open(${JSON.stringify(queueLog)},os.O_WRONLY|os.O_CREAT|os.O_APPEND,0o600)
 os.write(fd,(json.dumps(record)+"\\n").encode());os.close(fd)
for index,arg in enumerate(args):
 if index and args[index-1] in ("-c","--config") and arg.startswith("notify="):
  original=json.loads(arg[len("notify="):])
  args[index]="notify="+json.dumps([${JSON.stringify(python)},${JSON.stringify(logger)},json.dumps(original)])
os.execv(${JSON.stringify(native)},[${JSON.stringify(native)}]+args)
`, {mode: 0o755});
const fixture = path.join(output, 'fixture');
const args = ['--unshare-net', '--die-with-parent', '--bind', '/', '/', '--dev', '/dev',
  '--chdir', repo, '--', python, path.join(probe, 'discovery.py'), '--crew', crew,
  '--sha256', sha(fs.readFileSync(crew)), '--runtime', 'codex', '--case', 'sources',
  '--child', '--out', fixture];
const env = {PATH: path.join(probe, 'bin') + ':' + process.env.PATH,
  HOME: path.join(output, 'runner-home'), LANG: 'C.UTF-8', PYTHONDONTWRITEBYTECODE: '1'};
const log = fs.openSync(path.join(output, 'run.log'), 'w');
const result = spawnSync('bwrap', args, {env, stdio: ['ignore', log, log], timeout: 240000});
fs.closeSync(log);
const manifest = {recipe_revision: revision, recipe_sources: sources,
  crew_sha256: sha(fs.readFileSync(crew)), codex_sha256: sha(fs.readFileSync(native)),
  command: ['bwrap', ...args], exit: result.status, signal: result.signal,
  provider_calls: 0, log_sha256: sha(fs.readFileSync(path.join(output, 'run.log')))};
fs.writeFileSync(path.join(output, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
if (result.error) throw result.error;
if (result.status !== 0) throw new Error('Native probe failed; see run.log');
const report = JSON.parse(fs.readFileSync(path.join(fixture, 'report.json')));
const queues = fs.readFileSync(queueLog, 'utf8').trim().split('\n').map(JSON.parse);
const roots = new Set(report.child_controls.map(control => control.original_parent_session_id));
const passed = report.child_controls.length === 2 && queues.length === 2 &&
  report.child_controls.every(control => control.original_parent_session_id === control.daemon_session_id_after_child) &&
  queues.every(queue => roots.has(queue['--thread'])) &&
  report.queued_after_children.every(control => control.expected_parent === control.actual_session_id && control.native_receipt.reply_observed_after_phase_prompt) &&
  report.cleanup.remaining_processes.length === 0;
fs.writeFileSync(path.join(output, 'proof.json'), JSON.stringify({passed,
  parents: [...roots], queue_threads: queues.map(queue => queue['--thread']),
  cleanup: report.cleanup, report_sha256: sha(fs.readFileSync(path.join(fixture, 'report.json')))}, null, 2) + '\n');
if (!passed) throw new Error('Native parent identity assertions failed');
console.log('Native parent identity and post-child queue targets verified; zero provider calls.');
