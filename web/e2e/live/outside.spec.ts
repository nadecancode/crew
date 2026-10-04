// What stays out (notes/web-design.md §tests "outside.spec.ts"): another
// origin, the bare loopback addresses, and an agent's own token, presented
// from inside an agent's own process (in its sandbox under
// CREW_TEST_SANDBOX=require).
import { readFileSync, writeFileSync } from "node:fs";
import { createServer, request } from "node:http";
import type { AddressInfo } from "node:net";
import { join } from "node:path";
import { expect, test } from "./harness.ts";
import { rawWs } from "./rawws.mjs";

const get = (port: number, host: string) =>
  new Promise<{ status: number; body: string }>((resolve, reject) => {
    const r = request({ host: "127.0.0.1", port, path: "/", headers: { Host: host }, agent: false }, (res) => {
      let body = "";
      res.on("data", (c) => (body += c));
      res.on("end", () => resolve({ status: res.statusCode ?? 0, body }));
    });
    r.on("error", reject);
    r.end();
  });

test("another origin cannot open the socket; the bare addresses show nothing", async ({ page, daemon, openLive }) => {
  daemon.start();
  const info = daemon.web();
  const label = new URL(info.url).host;
  const live = await openLive(info);
  await expect(live.locator("[data-agent]").first()).toBeVisible();

  // A page of another origin: the socket never opens (the daemon checks Origin).
  const other = createServer((_, res) => res.end("<!doctype html><title>other</title>"));
  await new Promise<void>((r) => other.listen(0, "127.0.0.1", r));
  const otherPort = (other.address() as AddressInfo).port;
  const stranger = await page.context().newPage();
  await stranger.goto(`http://127.0.0.1:${otherPort}/`);
  const opened = await stranger.evaluate(
    (url) =>
      new Promise<boolean>((resolve) => {
        const ws = new WebSocket(url);
        ws.onopen = () => resolve(true);
        ws.onerror = () => resolve(false);
        ws.onclose = () => resolve(false);
      }),
    `ws://${label}/ws`,
  );
  expect(opened).toBe(false);
  // The same with the origin written by hand: refused at the upgrade.
  expect((await rawWs(info.port, label, `http://127.0.0.1:${otherPort}`)).upgraded).toBe(false);
  await stranger.close();
  other.close();

  // The bare loopback addresses and any other name: misdirected, and not the dashboard.
  for (const host of [`127.0.0.1:${info.port}`, `localhost:${info.port}`, `[::1]:${info.port}`, `evil.example:${info.port}`]) {
    const r = await get(info.port, host);
    expect(r.status, host).toBe(421);
    expect(r.body, host).not.toMatch(/<script|crew-web|data-agent/);
  }
  // The same in a browser: nothing of the dashboard.
  const bare = await page.context().newPage();
  await bare.goto(`http://127.0.0.1:${info.port}/`).catch(() => {});
  await expect(bare.locator("[data-agent]")).toHaveCount(0);
  await bare.close();
});

test("an agent's own token, presented from the agent's own process, is unauthorized", async ({ daemon }) => {
  test.setTimeout(90_000);
  daemon.start();
  const info = daemon.web();
  const label = new URL(info.url).host;
  const protocol = 6;
  // The prober agent (node, started with the team) reads probe.json from its working directory.
  const dir = join(daemon.ws, ".crew/worktrees/prober");
  const probe = { port: info.port, host: label, origin: `http://${label}`, agent: "prober", protocol, workspace: info.workspace };
  writeFileSync(join(dir, "probe.json"), JSON.stringify(probe));
  let out = "";
  daemon.until("the prober's report", () => ((out = readFileSync(join(dir, "probe.out"), "utf8")), out.length > 0), 30_000);
  const report = JSON.parse(out) as { upgraded: boolean; hello?: { error?: { code?: string } }; snapshot?: { error?: { code?: string } }; pair?: { error?: { code?: string } } };
  // It reaches the port (the loopback network is shared) and the upgrade passes for the right
  // name; the token it holds is no web session.
  expect(report.upgraded).toBe(true);
  expect(report.hello?.error?.code).toBe("unauthorized");
  expect(report.snapshot?.error?.code).toBe("unauthorized");
  // And on a workspace connection: refused, whatever the workspace.
  const wr = report as unknown as { ws_hello?: { error?: { code?: string } }; ws_snapshot?: { error?: { code?: string } } };
  expect(wr.ws_hello?.error?.code).toBe("unauthorized");
  expect(wr.ws_snapshot?.error?.code).not.toBe(undefined);
  // Nor can it pair: the code is not its to guess.
  expect(report.pair?.error?.code).toBeTruthy();
});
