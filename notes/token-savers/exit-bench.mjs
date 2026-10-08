import fs from 'node:fs';
import { spawnSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { root } from './harness.mjs';

const rows = [];
for (const [name, args, expected] of [
  ['lowfat', [], 101], ['tokf', ['run'], 0], ['tokf', ['run', '--no-mask-exit-code'], 101],
]) {
  const r = spawnSync('bwrap', [
    '--die-with-parent', '--unshare-net', '--ro-bind', '/', '/', '--bind', root, root,
    '--dev', '/dev', '--proc', '/proc', '--', `${root}/bin/${name}/${name}`, ...args,
    process.execPath, `${root}/replay.mjs`, `${root}/corpus/cargo-failure.txt`, '101',
  ], {
    cwd: root, encoding: 'utf8', timeout: 10000, maxBuffer: 8 * 1024 * 1024,
    env: { PATH: process.env.PATH, HOME: `${root}/runtime-home`,
      XDG_CONFIG_HOME: `${root}/runtime-config`, XDG_DATA_HOME: `${root}/runtime-data`,
      XDG_CACHE_HOME: `${root}/runtime-cache`, TOKF_HOME: `${root}/tokf-home`,
      TOKF_TELEMETRY_ENABLED: 'false', NO_COLOR: '1', LANG: 'C.UTF-8' },
  });
  if (r.error) throw r.error;
  assert.equal(r.status, expected, `${name} ${args.join(' ')} child-exit observation`);
  rows.push({ name, args, status: r.status, stdout: r.stdout, stderr: r.stderr });
}
fs.writeFileSync(`${root}/exit-probe.json`, `${JSON.stringify(rows, null, 2)}\n`);
