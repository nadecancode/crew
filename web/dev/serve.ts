// `bun run mock` (or `node dev/serve.ts`): the mock dashboard server for
// `bun run dev`, with two workspaces to switch between (the fixture's, and
// `search` on team10) and one running session that is not joined.
// Vite's proxy forwards /ws to ws://127.0.0.1:47700 with the browser's Host
// and Origin (`localhost:5173`), so the mock does not insist on its own name
// here. With a built bundle it serves that too, under its own name.

import { existsSync } from "node:fs";
import { createInterface } from "node:readline";
import { parseArgs } from "node:util";
import { FIXTURE_NAMES, PROTOCOL, startMock } from "./mock.ts";
import type { FixtureName } from "./mock.ts";
import { advanceScenario, PANEL_METHODS, READ_MODES, SCENARIO_NAMES, setPanelRead } from "./scenarios.ts";
import type { ReadMode, ScenarioName } from "./scenarios.ts";

const usage = `usage: node dev/serve.ts [--port 47700] [--fixture ${FIXTURE_NAMES.join("|")}] [--scenario ${SCENARIO_NAMES.join("|")}] [--reads ${READ_MODES.join("|")}] [--activity]`;
let values: { port: string; fixture: string; scenario?: string; reads?: string; activity?: boolean; help?: boolean };
try {
  values = parseArgs({
    options: {
      port: { type: "string", default: "47700" },
      fixture: { type: "string", default: "snapshot" },
      scenario: { type: "string" },
      reads: { type: "string" },
      activity: { type: "boolean" },
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
for (const [flag, value, choices] of [["scenario", values.scenario, SCENARIO_NAMES], ["reads", values.reads, READ_MODES]] as const) {
  if (value && !(choices as readonly string[]).includes(value)) {
    console.error(`--${flag}: one of ${choices.join(", ")}\n${usage}`);
    process.exit(2);
  }
}

const mock = await startMock({
  port,
  strictHost: false,
  fixture: values.fixture as FixtureName,
  scenario: values.scenario as ScenarioName | undefined,
  // Keep the original no-scenario development workspace switcher.
  ...(values.scenario ? {} : {
    workspaces: [{ fixture: values.fixture as FixtureName }, { fixture: "team10" as const, project: "search", workspace: "/home/op/work/search" }],
    others: [{ id: "4c2e8a1b9d0f3e57", project: "docs", workspace: "/home/op/work/docs" }],
  }),
});
const releaseReads = values.reads ? PANEL_METHODS.map((method) => setPanelRead(mock, method, values.reads as ReadMode)) : [];
const activity = values.activity ? setInterval(() => advanceScenario(mock, (values.scenario ?? "busy") as ScenarioName), 1000) : null;
activity?.unref();
const bundle = existsSync(`${mock.dist}/index.html`);

function codes(): void {
  console.log(`dev        http://localhost:5173/#pair=${mock.code()}`);
  if (bundle) console.log(`bundle     ${mock.pairUrl()}`);
}

console.log(`mock       crew dashboard server, protocol ${PROTOCOL}, workspaces ${mock.workspaces.map((w) => `${w.state.session.project} (${w.fixture})`).join(", ")}, on ${mock.addresses.map((a) => (a.includes(":") ? `[${a}]` : a)).join(" and ")}:${mock.port}`);
codes();
console.log("note       each address pairs once, for 2 minutes; press enter for new ones");
if (values.scenario) console.log(`scenario   ${values.scenario}; clock frozen, activity ${values.activity ? "on" : "off"}; reads ${values.reads ?? "fixture defaults"}`);
console.log("controls   type step for one scenario push; ready releases pending reads; disconnect, stop, remove exercise workspace availability; shutdown stops the dashboard");

// Enter prints fresh codes: a dev page that lost its session (the mock was
// restarted, so its token is unknown) needs one to sign in again.
createInterface({ input: process.stdin }).on("line", (line) => {
  switch (line.trim()) {
    case "step": advanceScenario(mock, (values.scenario ?? "busy") as ScenarioName); break;
    case "ready": releaseReads.splice(0).forEach((release) => release()); break;
    case "disconnect": advanceScenario(mock, "disconnected"); break;
    case "stop": mock.stopDaemon(); break;
    case "remove": mock.remove(mock.workspace().id); break;
    case "shutdown": void mock.shutdown(); break;
    default: codes();
  }
});

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    if (activity) clearInterval(activity);
    releaseReads.forEach((release) => release());
    void mock.stop().then(() => process.exit(0));
  });
}
