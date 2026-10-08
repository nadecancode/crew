// Install the audited pi release only under the ignored evidence directory.
import fs from 'node:fs';
import path from 'node:path';
import { spawn } from 'node:child_process';

const root = path.resolve('target/toolbox-audit/pi-runtime');
for (const dir of ['home', 'cache']) fs.mkdirSync(path.join(root, dir), { recursive: true });
for (const name of ['user.npmrc', 'global.npmrc']) fs.writeFileSync(path.join(root, name), '');
const env = {
  PATH: process.env.PATH, HOME: path.join(root, 'home'), LANG: 'C.UTF-8',
  NPM_CONFIG_USERCONFIG: path.join(root, 'user.npmrc'),
  NPM_CONFIG_GLOBALCONFIG: path.join(root, 'global.npmrc'),
  NPM_CONFIG_CACHE: path.join(root, 'cache'),
};
const child = spawn('npm', ['install', '--prefix', root, '--ignore-scripts', '--no-audit', '--no-fund', '--save-exact', '@earendil-works/pi-coding-agent@1.1.0'], { env, cwd: root, stdio: 'inherit' });
child.on('exit', code => { process.exitCode = code ?? 1; });
