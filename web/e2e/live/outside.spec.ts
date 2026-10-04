// What stays out (notes/web-design.md §tests "outside.spec.ts"): another
// origin, the bare loopback addresses, and an agent's own token.
import { randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { createServer, request } from "node:http";
import type { AddressInfo } from "node:net";
import { connect } from "node:net";
import { join } from "node:path";
import { expect, test } from "./harness.ts";

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

/** One WebSocket request, written by hand so the test sets Host and Origin; the first reply to `frame`. */
function rawWs(port: number, host: string, origin: string, frame: object): Promise<{ upgraded: boolean; reply?: Record<string, unknown> }> {
  return new Promise((resolve) => {
    const sock = connect(port, "127.0.0.1");
    const key = randomBytes(16).toString("base64");
    let buf = Buffer.alloc(0);
    let upgraded = false;
    const done = (v: { upgraded: boolean; reply?: Record<string, unknown> }) => (sock.destroy(), resolve(v));
    sock.setTimeout(8000, () => done({ upgraded }));
    sock.on("error", () => done({ upgraded }));
    sock.on("connect", () => sock.write(`GET /ws HTTP/1.1\r\nHost: ${host}\r\nOrigin: ${origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`));
    sock.on("data", (d) => {
      buf = Buffer.concat([buf, d]);
      if (!upgraded) {
        const end = buf.indexOf("\r\n\r\n");
        if (end < 0) return;
        if (!buf.subarray(0, 12).toString().includes("101")) return done({ upgraded: false });
        upgraded = true;
        buf = buf.subarray(end + 4);
        const payload = Buffer.from(JSON.stringify(frame));
        const mask = randomBytes(4);
        const head = payload.length < 126 ? Buffer.from([0x81, 0x80 | payload.length]) : Buffer.from([0x81, 0x80 | 126, payload.length >> 8, payload.length & 255]);
        sock.write(Buffer.concat([head, mask, Buffer.from(payload.map((b, i) => b ^ mask[i % 4]))]));
      }
      // An unmasked server text frame.
      if (upgraded && buf.length >= 2 && (buf[0] & 0x0f) === 1) {
        let len = buf[1] & 0x7f, off = 2;
        if (len === 126) (len = buf.readUInt16BE(2)), (off = 4);
        if (buf.length >= off + len) return done({ upgraded, reply: JSON.parse(buf.subarray(off, off + len).toString()) });
      }
    });
  });
}

test("another origin cannot open the socket; the bare addresses show nothing; an agent's token is unauthorized", async ({ page, daemon, openLive }) => {
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
  expect((await rawWs(info.port, label, `http://127.0.0.1:${otherPort}`, { id: 1, method: "ping" })).upgraded).toBe(false);
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

  // An agent's own token is not a web session: hello is unauthorized, and nothing else works.
  const identity = JSON.parse(readFileSync(join(daemon.ws, ".crew/agents/echo/identity.json"), "utf8")) as { token: string };
  const proto = JSON.parse(daemon.c(["status", "--json"])).session.protocol ?? 5;
  const r = await rawWs(info.port, label, `http://${label}`, { id: 1, method: "hello", params: { role: "agent", agent_id: "echo", token: identity.token, protocol: proto } });
  expect(r.upgraded).toBe(true);
  expect((r.reply as { error?: { code?: string } }).error?.code).toBe("unauthorized");
});
