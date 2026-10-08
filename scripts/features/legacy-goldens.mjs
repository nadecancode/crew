// Reproduce legacy bytes using the actual frozen v6 crates, not a new-code oracle.
// Run at repository root: node scripts/features/legacy-goldens.mjs [--write]
// All build/extraction output stays in target; no daemon or model is started.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';

const root = process.cwd();
const revision = 'b4f9a4424c53ddf54a6db1d0d82164015221faaa';
const output = 'crates/crew-core/tests/fixtures/features/legacy-v6.json';
const base = path.join(root, 'target/features-legacy-v6');
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
  !key.startsWith('GIT_') && !['CREW_IDENTITY', 'CREW_SOCKET', 'CREW_LAUNCH'].includes(key)));
const read = file => execFileSync('jj', ['file', 'show', '-r', revision, file], { cwd: root, env });
const put = (file, bytes) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, bytes);
};
const files = execFileSync('jj', ['file', 'list', '-r', revision, 'crates/crew-core', 'crates/crew-protocol'],
  { cwd: root, env, encoding: 'utf8' }).trim().split('\n');
const sourceHashes = {};
for (const file of [...files, 'Cargo.lock', 'Cargo.toml']) {
  const data = read(file);
  sourceHashes[file] = crypto.createHash('sha256').update(data).digest('hex');
  put(path.join(base, file), data);
}
const manifest = read('Cargo.toml').toString()
  .replace(/^members = .*$/m, 'members = ["crates/crew-core", "crates/crew-protocol", "probe"]')
  .replace(/^default-members = .*$/m, 'default-members = ["probe"]');
put(path.join(base, 'Cargo.toml'), manifest);
put(path.join(base, 'probe/Cargo.toml'), `[package]
name = "crew-feature-legacy-probe"
version = "0.0.0"
edition = "2021"
[dependencies]
crew-core = { path = "../crates/crew-core" }
serde_json.workspace = true
`);
put(path.join(base, 'probe/src/main.rs'), String.raw`
use crew_core::rpc::{Request, Push, SessionInfo, Snapshot};
use crew_core::Task;
use serde_json::{json, Value};
fn main() {
    let inputs = vec![
        json!({"id":1,"method":"hello","params":{"role":"operator","token":"synthetic-only","client":"fixture","protocol":6}}),
        json!({"id":2,"method":"tasks_list","params":{}}),
        json!({"id":3,"method":"task_get","params":{"id":"fixture"}}),
        json!({"id":4,"method":"task_create","params":{"spec":{"id":"fixture","title":"Fixture","description":"legacy body"}}}),
        json!({"id":5,"method":"task_edit","params":{"id":"fixture","title":"Edited","description":null}}),
        json!({"id":6,"method":"session_complete","params":{"summary":"Legacy report","decisions":[]}}),
        json!({"id":7,"method":"snapshot","params":{"messages":20,"events":30}}),
        json!({"id":8,"method":"subscribe","params":{}}),
    ];
    let requests: Vec<Value> = inputs.into_iter().map(|input| {
        let decoded: Request = serde_json::from_value(input.clone()).unwrap();
        json!({"input":input,"canonical":serde_json::to_string(&decoded).unwrap()})
    }).collect();
    let task: Task = serde_json::from_value(json!({"id":"fixture","title":"Fixture","status":"waiting","created_at":12,"started_at":13})).unwrap();
    let snapshot = Snapshot { tasks: vec![task.clone()], ..Snapshot::default() };
    let pushes = [Push::Task(Box::new(task.clone())), Push::Session(Box::default()),
        serde_json::from_value::<Push>(json!({"kind":"deliver","data":{"ids":["m-fixture"],"text":"synthetic body"}})).unwrap()];
    let refused: Vec<Value> = ["features_negotiate", "task_create_scoped", "task_revise", "epic_complete", "aside_ask"]
        .into_iter().map(|method| {
            let decoded = serde_json::from_value::<Request>(json!({"id":20,"method":method,"params":{}}));
            json!({"method":method,"unknown_variant":decoded.unwrap_err().to_string().contains("unknown variant")})
        }).collect();
    println!("{}", json!({"requests":requests,"task":serde_json::to_string(&task).unwrap(),
        "session":serde_json::to_string(&SessionInfo::default()).unwrap(),
        "snapshot":serde_json::to_string(&snapshot).unwrap(),
        "pushes":pushes.iter().map(|p|serde_json::to_string(p).unwrap()).collect::<Vec<_>>(),
        "scoped_methods_rejected":refused}));
}
`);
const bytes = execFileSync(path.join(root, 'scripts/build-env'), [
  'cargo', 'run', '--offline', '--quiet', '--manifest-path', path.join(base, 'Cargo.toml'), '-p', 'crew-feature-legacy-probe',
], { cwd: root, env: { ...env, CARGO_TARGET_DIR: path.join(base, 'cargo') }, maxBuffer: 4 * 1024 * 1024 });
const result = { source_commit: revision, source_sha256: sourceHashes, ...JSON.parse(bytes) };
const serialized = JSON.stringify(result, null, 2) + '\n';
if (process.argv.includes('--write')) put(output, serialized);
else if (fs.readFileSync(output, 'utf8') !== serialized) throw new Error('legacy fixture differs; inspect before updating');
console.log(`Legacy v6 byte oracle reproduced: ${revision}; ${result.requests.length} requests, Task/Session/Snapshot, ${result.pushes.length} pushes.`);
