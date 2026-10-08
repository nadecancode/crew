import fs from 'node:fs';
import { spawnSync } from 'node:child_process';
import { root, getEncoding } from './harness.mjs';

const enc = getEncoding('cl100k_base');
const env = {
  PATH: `${root}/shim:${process.env.PATH}`, HOME: `${root}/runtime-home`,
  XDG_CONFIG_HOME: `${root}/runtime-config`, XDG_DATA_HOME: `${root}/runtime-data`,
  XDG_CACHE_HOME: `${root}/runtime-cache`, RTK_TELEMETRY_DISABLED: '1',
  LANG: 'C.UTF-8', NO_COLOR: '1',
};
const rows = [];
for (const [file, exit] of [['cargo-success.txt', 0], ['cargo-failure.txt', 101]]) {
  const raw = fs.readFileSync(`${root}/corpus/${file}`, 'utf8');
  const execute = () => {
    const result = spawnSync('bwrap', [
      '--die-with-parent', '--unshare-net', '--ro-bind', '/', '/', '--bind', root, root,
      '--dev', '/dev', '--proc', '/proc', '--', `${root}/bin/rtk/rtk`, 'cargo', 'test',
    ], {
      cwd: root, env: { ...env, REPLAY_FILE: `${root}/corpus/${file}`, REPLAY_EXIT: String(exit) },
      encoding: 'utf8', maxBuffer: 8 * 1024 * 1024, timeout: 10000,
    });
    if (result.error) throw result.error;
    return result;
  };
  const a = execute(), b = execute();
  fs.writeFileSync(`${root}/outputs/${file}--rtk--cargo-test.txt`, a.stdout);
  rows.push({ file, status: a.status, rawTokens: enc.encode(raw).length,
    filteredTokens: enc.encode(a.stdout).length, stable: a.stdout === b.stdout && a.stderr === b.stderr,
    output: a.stdout, stderr: a.stderr });
}
fs.writeFileSync(`${root}/rtk-cargo-results.json`, `${JSON.stringify(rows, null, 2)}\n`);
