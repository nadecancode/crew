// The `live` project (notes/web-design.md §tests): a real crew daemon with a
// scripted team, and the address `crew web --json` prints. Every test gets
// its own daemon, workspace and state, torn down after it.
//
//   CREW_BIN       the crew binary (default ../target/debug/crew)
//   CREW_TEST_VCS  jj | git | unset (auto), as for the Rust suites

import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { test as base, expect } from "@playwright/test";
import type { Page } from "@playwright/test";

export { expect };

const here = dirname(fileURLToPath(import.meta.url));
export const CREW_BIN = resolve(process.env.CREW_BIN ?? join(here, "../../../target/debug/crew"));

export type WebInfo = { url: string; port: number; pair_url: string; expires_in_secs: number; started: boolean };
export type CrewEvent = { seq: number; source: string; event_type: string; level: string; agent_id: string | null; message: string; data: unknown };
export type VcsData = { action: string; outcome: string; op: string; backend: string; agent: string; peer?: string | null; commit?: string | null; files?: string[] };

/** The scripted team the live specs share. Agents act when the test sends them a message. */
export function teamYaml(ws: string, vcs: string, extra = 0): string {
  const agent = (id: string, role: string, steps: string[]) =>
    [
      `  - id: ${id}`,
      `    role: ${role}`,
      `    command:`,
      `      - "{crew}"`,
      `      - scripted-agent`,
      ...steps.map((s) => `      - ${JSON.stringify(`--on-message=${s}`)}`),
    ].join("\n");
  return [
    `project: web-live`,
    `objective: live browser tests`,
    `workspace:`,
    `  path: ${JSON.stringify(ws)}`,
    `  init: true`,
    `  vcs: ${vcs}`,
    `  sandbox:`,
    `    mode: ${process.env.CREW_TEST_SANDBOX === "require" ? "required" : "off"}`,
    `backend: pty`,
    `orchestrator:`,
    `  agent: false`,
    `defaults:`,
    `  runtime: shell`,
    `agents:`,
    // dev commits and publishes to peer; peer cherry-picks, then asks the operator.
    agent("dev", "schema", ["write schema.sql CREATE TABLE t (id INTEGER);", "commit Add schema", "publish peer The schema is ready."]),
    agent("peer", "ledger", ["cherry-pick", "send operator question Ship the schema?"]),
    // rival commits a clashing schema.sql, then merges dev: a conflict.
    agent("rival", "rival", ["write schema.sql CREATE TABLE other (x TEXT);", "commit Rival schema", "merge dev"]),
    agent("echo", "echo", ["reply ack from echo", "tool git_status {}"]),
    agent("idle", "idle", []),
    // prober runs node as the agent (in its sandbox when there is one) and probes the dashboard socket.
    [
      `  - id: prober`,
      `    role: probe`,
      `    command:`,
      ...[process.execPath, "--input-type=module", "-e", PROBER_SRC].map((a) => `      - ${JSON.stringify(a)}`),
    ].join("\n"),
    ...Array.from({ length: extra }, (_, i) => agent(`w${i + 1}`, "worker", ["reply ok"])),
    "",
  ].join("\n");
}

export class Daemon {
  readonly root: string;
  readonly ws: string;
  readonly env: NodeJS.ProcessEnv;
  constructor(readonly vcs: string = process.env.CREW_TEST_VCS || "auto") {
    // A short path: Unix socket paths are limited to ~108 bytes.
    this.root = mkdtempSync("/tmp/crew-wl");
    this.ws = join(this.root, "ws");
    mkdirSync(this.ws, { recursive: true });
    this.env = {
      ...process.env,
      XDG_RUNTIME_DIR: join(this.root, "run"),
      XDG_STATE_HOME: join(this.root, "state"),
      XDG_CONFIG_HOME: join(this.root, "config"),
      CREW_GLOBAL_CONFIG: join(this.root, "etc/config.toml"),
      CODEX_HOME: join(this.root, "codex-home"),
      CLAUDE_CONFIG_DIR: join(this.root, "claude-home"),
    };
    for (const k of ["HERDR_ENV", "HERDR_SOCKET_PATH", "HERDR_PANE_ID", "CREW_IDENTITY", "CREW_SOCKET"]) delete this.env[k];
    mkdirSync(join(this.root, "run"), { recursive: true, mode: 0o700 });
  }

  /** `crew <args> -C <ws>`; stdout, or the failure. */
  crew(args: string[], opts: { env?: NodeJS.ProcessEnv; input?: string } = {}): { ok: boolean; stdout: string; stderr: string } {
    const r = spawnSync(CREW_BIN, args, { cwd: this.root, env: { ...this.env, ...opts.env }, encoding: "utf8", input: opts.input ?? "" });
    return { ok: r.status === 0, stdout: r.stdout, stderr: r.stderr };
  }
  ok(args: string[]): string {
    const r = this.crew(args);
    if (!r.ok) throw new Error(`crew ${args.join(" ")} failed:\n${r.stdout}\n${r.stderr}`);
    return r.stdout;
  }
  c(args: string[]): string {
    return this.ok([...args, "-C", this.ws]);
  }

  /** Write the team and `crew run` it; wait for the agents to be up. */
  start(yaml: string = teamYaml(this.ws, this.vcs)): void {
    const file = join(this.root, "team.yaml");
    writeFileSync(file, yaml);
    this.ok(["run", "team.yaml", "--no-attach"]);
    this.until("the agents to come up", () => this.snapshot().agents.every((a: { status: string }) => a.status !== "spawning"));
  }

  snapshot(): { agents: { id: string; status: string }[]; tasks: unknown[]; session: Record<string, unknown> } {
    return JSON.parse(this.c(["status", "--json"]));
  }
  events(): CrewEvent[] {
    return this.c(["logs", "--json", "-n", "3000"])
      .split("\n")
      .filter((l) => l.trim())
      .map((l) => JSON.parse(l));
  }
  vcsEvents(): (CrewEvent & { vcs: VcsData })[] {
    return this.events()
      .filter((e) => e.source === "vcs" && e.event_type.startsWith("vcs."))
      .map((e) => ({ ...e, vcs: e.data as VcsData }));
  }
  /** `crew web --json`: starts the server if needed, and mints a pairing code. */
  web(extra: string[] = []): WebInfo {
    return JSON.parse(this.c(["web", "--json", ...extra]));
  }
  send(to: string, body: string): void {
    this.c(["send", to, body]);
  }
  /** Synchronous poll (the daemon is another process). */
  until(what: string, done: () => boolean, ms = 40_000): void {
    const end = Date.now() + ms;
    for (;;) {
      try {
        if (done()) return;
      } catch {
        /* not yet */
      }
      if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
      execFileSync("sleep", ["0.1"]);
    }
  }
  stop(): void {
    this.crew(["stop", "-C", this.ws]);
  }
  /** SIGKILL the daemon: no `shutdown` push, no goodbye. */
  kill(): void {
    const pid = Number(this.snapshot().session.daemon_pid);
    if (!pid) throw new Error("no daemon pid in the session");
    process.kill(pid, "SIGKILL");
  }
  cleanup(): void {
    this.stop();
    rmSync(this.root, { recursive: true, force: true });
  }
}

type Fixtures = {
  daemon: Daemon;
  /** Open `info.pair_url` in `target` (default `page`) and wait until live. */
  openLive: (info: WebInfo, target?: Page) => Promise<Page>;
};

export const test = base.extend<Fixtures>({
  daemon: async ({}, use) => {
    const d = new Daemon();
    try {
      await use(d);
    } finally {
      d.cleanup();
    }
  },
  openLive: async ({ page }, use) => {
    await use(async (info, target = page) => {
      await target.goto(info.pair_url);
      await target.locator('[data-conn="live"]').waitFor({ state: "attached", timeout: 15_000 });
      return target;
    });
  },
});

/** The program of the `prober` agent: waits for `probe.json` in its working directory, then
 * opens the dashboard's socket as the page would and says `hello` with the agent's own token
 * (a sandboxed agent shares the host's loopback network). It writes `probe.out`. */
const PROBER_MAIN = `
import { existsSync, readFileSync, writeFileSync } from "node:fs";
while (!existsSync("probe.json")) await new Promise((r) => setTimeout(r, 100));
await new Promise((r) => setTimeout(r, 100));
const p = JSON.parse(readFileSync("probe.json", "utf8"));
const identity = JSON.parse(readFileSync(process.env.CREW_IDENTITY, "utf8"));
const out = {};
const ws = await rawWs(p.port, p.host, p.origin);
out.upgraded = ws.upgraded;
if (ws.upgraded) {
  out.hello = await ws.call("hello", { role: "agent", agent_id: p.agent, token: identity.token, protocol: p.protocol });
  out.snapshot = await ws.call("snapshot", {});
  out.pair = await ws.call("web_pair", { code: "x" });
  ws.close();
}
writeFileSync("probe.out", JSON.stringify(out));
setInterval(() => {}, 1 << 30);
`;
export const PROBER_SRC = readFileSync(join(here, "rawws.mjs"), "utf8").replace(/^export /gm, "") + PROBER_MAIN;
