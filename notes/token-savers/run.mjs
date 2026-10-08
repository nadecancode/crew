#!/usr/bin/env node
// Offline replay only. No builds, installers, provider requests or user-home writes.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import zlib from 'node:zlib';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const here = path.dirname(fileURLToPath(import.meta.url));
const options = {};
for (let i = 2; i < process.argv.length; i++) {
  const key = process.argv[i];
  if (key === '--native') options.native = true;
  else if (['--scratch', '--bin', '--tokenizer'].includes(key) && process.argv[i + 1]) {
    options[key.slice(2)] = path.resolve(process.argv[++i]);
  } else throw new Error(`Unknown or incomplete option: ${key}`);
}
if (!options.scratch || !options.bin || !options.tokenizer) {
  throw new Error('Usage: node notes/token-savers/run.mjs --scratch target/NEW-DIR --bin DIR --tokenizer DIR [--native]');
}
const root = options.scratch;
const target = fs.realpathSync(path.resolve(here, '../../target'));
const within = (parent, child) => child.startsWith(`${parent}${path.sep}`);
if (!within(target, root) || !within(target, fs.realpathSync(path.dirname(root))) && path.dirname(root) !== target) {
  throw new Error('Scratch must be a new directory inside this worktree target');
}
// Hook commands in the optional native fixture are shell strings.
if (!/^[a-zA-Z0-9_./-]+$/.test(root) || !/^[a-zA-Z0-9_./-]+$/.test(process.execPath)) {
  throw new Error('Fixture paths must contain only letters, digits, _, -, / and .');
}
const pins = JSON.parse(fs.readFileSync(path.join(here, 'pins.json')));
const sha = data => crypto.createHash('sha256').update(data).digest('hex');
const binaries = new Map();
for (const [name, pin] of Object.entries(pins.binaries)) {
  const bytes = fs.readFileSync(path.join(options.bin, name, name));
  assert.equal(sha(bytes), pin.sha256, `${name} binary hash`);
  binaries.set(name, bytes);
}
const corpus = JSON.parse(zlib.gunzipSync(fs.readFileSync(path.join(here, 'corpus.json.gz')), { maxOutputLength: 1024 * 1024 }));
for (const [name, text] of Object.entries(corpus)) {
  assert.equal(path.basename(name), name);
  assert.equal(sha(text), pins.inputs[name].sha256, `${name} input hash`);
}
fs.mkdirSync(root); // Exclusive creation; never reuse or overwrite an existing run.
for (const dir of ['corpus', 'outputs', 'shim', 'runtime-home', 'runtime-config', 'runtime-data', 'runtime-cache', 'tokf-home', 'native-tmp']) {
  fs.mkdirSync(path.join(root, dir));
}
for (const [name, bytes] of binaries) {
  fs.mkdirSync(path.join(root, 'bin', name), { recursive: true });
  fs.writeFileSync(path.join(root, 'bin', name, name), bytes, { mode: 0o500 });
}
for (const [name, text] of Object.entries(corpus)) fs.writeFileSync(path.join(root, 'corpus', name), text);
for (const [name, pin] of Object.entries(pins.filters)) {
  const bytes = fs.readFileSync(path.join(here, 'filters', name));
  assert.equal(sha(bytes), pin.sha256, `${name} filter hash`);
  fs.mkdirSync(path.dirname(path.join(root, 'filters', name)), { recursive: true });
  fs.writeFileSync(path.join(root, 'filters', name), bytes);
}
fs.writeFileSync(path.join(root, 'shim/cargo'), `#!${process.execPath}\nimport fs from 'node:fs';process.exitCode=Number(process.env.REPLAY_EXIT);process.stdout.write(fs.readFileSync(process.env.REPLAY_FILE));\n`, { mode: 0o700 });
// Establish that replay itself preserves a complete stream and a failing exit.
const replay = spawnSync(path.join(root, 'shim/cargo'), [], {
  env: { REPLAY_FILE: path.join(root, 'corpus/cargo-success.txt'), REPLAY_EXIT: '101' },
  encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024,
});
assert.equal(replay.status, 101);
assert.equal(replay.stdout, corpus['cargo-success.txt']);
process.env.CREW_FILTER_SCRATCH = root;
process.env.CREW_FILTER_TOKENIZER = options.tokenizer;
await import('./bench.mjs');
await import('./cargo-bench.mjs');
await import('./edge-bench.mjs');
await import('./exit-bench.mjs');

const read = name => JSON.parse(fs.readFileSync(path.join(root, name)));
const expected = name => JSON.parse(fs.readFileSync(path.join(here, 'evidence', name)));
for (const [actual, golden] of [[read('results.json').rows, expected('results.json').rows],
  [read('rtk-cargo-results.json'), expected('rtk-cargo-results.json')],
  [read('edge-results.json').rows, expected('edge-results.json').rows]]) {
  assert.equal(actual.length, golden.length);
  for (let i = 0; i < actual.length; i++) {
    for (const key of ['file', 'rawTokens', 'filteredTokens']) assert.equal(actual[i][key], golden[i][key], `${i} ${key}`);
    assert.equal(actual[i].status ?? actual[i].processExit, golden[i].status ?? golden[i].processExit, `${i} exit`);
  }
}
assert.equal(read('edge-results.json').rtkRecall.rawIdentical, true);
assert.equal(read('edge-results.json').lowfatFooter.stderrStable, false);
if (options.native) {
  const version = spawnSync('claude', ['--version'], { encoding: 'utf8', timeout: 5000 });
  assert.match(version.stdout, /2\.1\.291/);
  // Stub and native child share a network namespace with no external network.
  const native = spawnSync('bwrap', [
    '--die-with-parent', '--unshare-net', '--ro-bind', '/', '/', '--bind', root, root,
    '--bind', path.join(root, 'native-tmp'), '/tmp',
    '--dev', '/dev', '--proc', '/proc', '--', process.execPath, path.join(here, 'native.mjs'),
  ], { env: { PATH: process.env.PATH, CREW_FILTER_SCRATCH: root }, encoding: 'utf8', timeout: 75000, maxBuffer: 4 * 1024 * 1024 });
  fs.writeFileSync(path.join(root, 'native-run.log'), native.stdout + native.stderr);
  assert.equal(native.status, 0, native.stderr);
  const nativeReport = read('native-read/report.json');
  assert.equal(nativeReport[0].permissionCanary, false);
  assert.equal(nativeReport[1].permissionCanary, true);
  const post = read('native-read/hooked/hook-output.json').hookSpecificOutput.updatedToolOutput.file;
  assert.equal(post.totalLines, 3);
  assert.equal(post.numLines, 3);
}
console.log(`Verified 57 filter observations; raw recovery exact; variable lowfat footer reproduced. Native=${Boolean(options.native)}. Node=${process.version}`);
