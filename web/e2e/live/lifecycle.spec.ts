// The attacks of notes/web-design.md §security "One origin per server start" and
// "A document whose server is gone", as regressions against a real daemon
// (§tests "lifecycle.spec.ts"): the daemon is killed without a word, someone
// else takes its port on both addresses, and a second document of the dead
// origin reads and rewrites its storage. A restarted daemon is a new origin.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import http from "node:http";
import type { Duplex } from "node:stream";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "./harness.ts";

const FIXTURES = join(fileURLToPath(new URL(".", import.meta.url)), "../../../crates/crew/tests/fixtures/web");
const fixture = (name: string) => JSON.parse(readFileSync(join(FIXTURES, name), "utf8"));

const scriptsAsked: string[] = [];
const hostilePage = `<!doctype html><title>hostile</title><script>
  window.__seen = []; addEventListener("storage", (e) => __seen.push([e.key, e.newValue]));
</script>`;

function hostile(port: number, address: string, servers: http.Server[]): Promise<http.Server> {
  const server = http.createServer((req, res) => {
    if (req.url!.endsWith(".js")) {
      scriptsAsked.push(req.url!);
      res.writeHead(200, { "content-type": "text/javascript" }).end(`window.__owned = true; export default function () { return null; }`);
      return;
    }
    res.writeHead(200, { "content-type": "text/html" }).end(hostilePage);
  });
  servers.push(server);
  return new Promise((ready, failed) => {
    server.once("error", failed);
    server.listen(port, address, () => ready(server));
  });
}
const close = (server: http.Server) => new Promise((done) => (server.closeAllConnections(), server.close(done)));

/** A WebSocket endpoint in a few lines: text frames only. */
function forgedSocket(server: http.Server, sockets: Duplex[], answer: (request: { method: string }) => unknown | Promise<unknown>) {
  server.on("upgrade", (req, socket: Duplex) => {
    sockets.push(socket);
    const accept = createHash("sha1").update(req.headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest("base64");
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    let buf = Buffer.alloc(0);
    socket.on("data", (d: Buffer) => {
      buf = Buffer.concat([buf, d]);
      for (;;) {
        if (buf.length < 2) return;
        let len = buf[1] & 0x7f, at = 2;
        if (len === 126) {
          if (buf.length < 4) return;
          len = buf.readUInt16BE(2);
          at = 4;
        }
        if (buf.length < at + 4 + len) return;
        const mask = buf.subarray(at, at + 4), body = Buffer.from(buf.subarray(at + 4, at + 4 + len));
        const opcode = buf[0] & 0x0f;
        buf = buf.subarray(at + 4 + len);
        if (opcode !== 1) continue;
        for (let i = 0; i < body.length; i++) body[i] ^= mask[i % 4];
        const request = JSON.parse(body.toString());
        void Promise.resolve(answer(request)).then((result) => {
          const out = Buffer.from(JSON.stringify({ id: request.id, result }));
        const head = out.length < 65536 ? (out.length < 126 ? Buffer.from([0x81, out.length]) : Buffer.from([0x81, 126, out.length >> 8, out.length & 0xff])) : (() => { const h = Buffer.alloc(10); h[0] = 0x81; h[1] = 127; h.writeBigUInt64BE(BigInt(out.length), 2); return h; })();
          socket.write(Buffer.concat([head, out]));
        });
      }
    });
    socket.on("error", () => {});
  });
}

const FORGED = (() => {
  const s = fixture("snapshot.json");
  s.session.project = "forged";
  s.agents[0].id = "FORGED";
  s.agents[0].name = "FORGED";
  return s;
})();

test("a killed daemon, a squatter on its port, a rewritten storage: the open tab refuses the server, and a restarted daemon is a new origin", async ({ context, page, daemon, openLive }) => {
  test.setTimeout(120_000);
  const servers: http.Server[] = [];
  const sockets: Duplex[] = [];
  // What happened, in order. The squatter holds the open tab's `hello` until the second
  // document has read and rewritten the storage, so the forged answer is given with the
  // stolen verifier in hand, not before.
  const order: string[] = [];
  let release!: () => void;
  const released = new Promise<void>((r) => (release = r));
  let proof = "";
  try {
    daemon.start();
    const info = daemon.web();
    const oldOrigin = new URL(info.url).origin;
    // A tab is live; its terminal was never opened.
    await openLive(info);
    const agents = await page.locator("[data-agent]").count();
    expect(agents).toBe(6);
    const stored = await page.evaluate(() => Object.fromEntries(Object.entries(localStorage)));
    const session = JSON.parse(stored["crew.web.session"]) as { token: string; verifier: string };
    expect(session.verifier).toMatch(/^[0-9a-f]{64}$/);

    // The daemon goes without a word.
    daemon.kill();
    daemon.until("the daemon to be gone", () => {
      try {
        daemon.snapshot();
        return false;
      } catch {
        return true;
      }
    });
    // Someone else takes the port on both addresses and answers for any Host.
    const squat = [await hostile(info.port, "127.0.0.1", servers), await hostile(info.port, "::1", servers)];
    for (const s of squat)
      forgedSocket(s, sockets, async (request) => {
        if (request.method === "hello") {
          order.push("hello-held");
          await released;
          order.push(`hello-answered:${proof}`);
          return { ...fixture("results.json").hello[0], web_proof: proof };
        }
        return request.method === "snapshot" ? (order.push("snapshot-forged"), FORGED) : { subscribed: true };
      });

    // The open tab reconnects by itself and is held at its `hello`.
    await expect.poll(() => order.includes("hello-held"), { timeout: 40_000, message: "the open tab's hello at the squatter" }).toBe(true);
    // (a) A second tab of the old address: it is the squatter's page. It reads what the origin
    // holds, hands the verifier to the squatter as its proof, and overwrites the storage.
    const second = await context.newPage();
    await second.goto(info.url);
    expect(await second.title()).toBe("hostile");
    const taken = await second.evaluate(() => Object.fromEntries(Object.entries(localStorage)));
    expect(taken).toEqual(stored);
    order.push("stolen");
    proof = (JSON.parse(taken["crew.web.session"]) as { verifier: string }).verifier;
    expect(proof).toBe(session.verifier);
    await second.evaluate(() => {
      for (const k of Object.keys(localStorage)) localStorage.setItem(k, "attacker");
    });
    order.push("rewritten");

    // (b) Only now is the open tab's `hello` answered, with the stolen verifier as the proof:
    // it refuses the server, draws nothing of the made-up team, ends stopped with every control
    // disabled, and makes no further request.
    release();
    await expect(page.locator('[data-conn="stopped"]')).toBeAttached({ timeout: 40_000 });
    expect(order.slice(0, 4)).toEqual(["hello-held", "stolen", "rewritten", `hello-answered:${session.verifier}`]);
    expect(order).not.toContain("snapshot-forged");
    await expect(page.locator("[data-agent]")).toHaveCount(agents);
    await expect(page.locator("[data-agent]").filter({ hasText: "FORGED" })).toHaveCount(0);
    await expect(page.locator(".conn-bar")).toContainText("stopped");
    for (const button of await page.locator("footer button, #side-panel button").all()) {
      if (await button.isVisible()) await expect(button).toBeDisabled();
    }
    const asked: string[] = [];
    page.on("request", (r) => asked.push(r.url()));
    // Opening a terminal tab requests no script, and the squatter's mark is not set.
    scriptsAsked.length = 0;
    await page.locator('[data-agent="echo"] .node-head').click();
    await page.getByRole("tab", { name: "terminal", exact: true }).click().catch(() => {});
    await page.waitForTimeout(1500);
    expect(await page.evaluate(() => (window as unknown as { __owned?: boolean }).__owned)).toBeUndefined();
    expect(scriptsAsked).toEqual([]);
    expect(asked).toEqual([]);

    // (c) The squatter lets go; the daemon starts again: another label, the same port.
    for (const s of sockets) s.destroy();
    await Promise.all(servers.splice(0).map(close));
    daemon.start();
    const next = daemon.web();
    expect(next.port).toBe(info.port);
    expect(new URL(next.url).origin).not.toBe(oldOrigin);
    const fresh = await context.newPage();
    await openLive(next, fresh);
    await expect(fresh.locator("[data-agent]")).toHaveCount(6);
    // The page of (a) has seen no storage event with a value since (the stopped tab removed its
    // own session: a `null`), reads no session of the new server, and the two origins differ.
    const seen = await second.evaluate(() => (window as unknown as { __seen: [string, string | null][] }).__seen);
    expect(seen.filter(([, value]) => value !== null)).toEqual([]);
    const newSession = await fresh.evaluate(() => localStorage.getItem("crew.web.session"));
    expect(newSession).toBeTruthy();
    expect(await second.evaluate(() => Object.values(localStorage))).not.toContain(newSession);
    expect(await second.evaluate(() => localStorage.getItem("crew.web.session"))).toBeNull();
    expect(await second.evaluate(() => location.origin)).toBe(oldOrigin);
    expect(await fresh.evaluate(() => location.origin)).toBe(new URL(next.url).origin);
    // The stopped tab stays stopped.
    await expect(page.locator('[data-conn="stopped"]')).toBeAttached();
  } finally {
    release();
    for (const s of sockets) s.destroy();
    await Promise.all(servers.map(close));
  }
});
