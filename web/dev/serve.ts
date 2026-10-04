// `bun run mock` (or `node dev/serve.ts`): the mock daemon for `bun run dev`.
// Vite's proxy forwards /ws to ws://127.0.0.1:47700 with the browser's Host
// and Origin (`localhost:5173`), so the mock does not insist on its own name
// here. With a built bundle it serves that too, under its own name.

import { existsSync } from "node:fs";
import { createInterface } from "node:readline";
import { parseArgs } from "node:util";
import { FIXTURE_NAMES, startMock } from "./mock.ts";
import type { FixtureName } from "./mock.ts";

const usage = `usage: node dev/serve.ts [--port 47700] [--fixture ${FIXTURE_NAMES.join("|")}]`;
let values: { port: string; fixture: string; help?: boolean };
try {
  values = parseArgs({
    options: {
      port: { type: "string", default: "47700" },
      fixture: { type: "string", default: "snapshot" },
      help: { type: "boolean", short: "h" },
    },
  }).values as typeof values;
} catch (e) {
  console.error(`${(e as Error).message}\n${usage}`);
  process.exit(2);
}
if (values.help) {
  console.log(usage);
  process.exit(0);
}
const port = Number(values.port);
if (!Number.isInteger(port) || port < 0 || port > 65_535) {
  console.error(`--port: not a port: ${values.port}\n${usage}`);
  process.exit(2);
}
if (!(FIXTURE_NAMES as readonly string[]).includes(values.fixture)) {
  console.error(`--fixture: one of ${FIXTURE_NAMES.join(", ")}\n${usage}`);
  process.exit(2);
}

const mock = await startMock({ port, fixture: values.fixture as FixtureName, strictHost: false });
const bundle = existsSync(`${mock.dist}/index.html`);

function codes(): void {
  console.log(`dev        http://localhost:5173/#pair=${mock.code()}`);
  if (bundle) console.log(`bundle     ${mock.pairUrl()}`);
}

console.log(`mock       crew daemon, protocol 5, fixture ${mock.fixture}, on ${mock.addresses.map((a) => (a.includes(":") ? `[${a}]` : a)).join(" and ")}:${mock.port}`);
codes();
console.log("note       each address pairs once, for 2 minutes; press enter for new ones");

// Enter prints fresh codes: a dev page that lost its session (the mock was
// restarted, so its token is unknown) needs one to sign in again.
createInterface({ input: process.stdin }).on("line", codes);

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    void mock.stop().then(() => process.exit(0));
  });
}
