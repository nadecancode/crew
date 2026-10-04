// Who owns the page's origin: the checks behind §security "One origin per server start".
import { spawn, type ChildProcess } from "node:child_process";
import http from "node:http";
import { expect, test } from "@playwright/test";

const { RELAY, SOCK, DIST } = process.env;
const BASE = Number(process.env.PORT ?? 47615) + 10;
test.skip(!RELAY || !SOCK || !DIST, "needs RELAY, SOCK and DIST (run.sh sets them)");

const children: ChildProcess[] = [];
const servers: http.Server[] = [];
const sockets: { destroy(): void }[] = [];
test.afterEach(async () => {
  for (const c of children.splice(0)) c.kill();
  for (const s of sockets.splice(0)) s.destroy(); // upgraded connections are no longer the server's
  await Promise.all(servers.splice(0).map((s) => new Promise((r) => { s.closeAllConnections(); s.close(r); })));
});

/** The spike relay with its own host label, ready to accept. */
function relay(port: number, token: string, label: string, ...flags: string[]): Promise<ChildProcess> {
  const child = spawn(RELAY!, [SOCK!, DIST!, String(port), token, label, ...flags], { stdio: ["ignore", "ignore", "pipe"] });
  children.push(child);
  return new Promise((ready, failed) => {
    child.stderr!.on("data", (d) => String(d).includes("listening") && ready(child));
    child.on("exit", (code) => failed(new Error(`relay exited ${code}`)));
  });
}
const stop = (child: ChildProcess) => new Promise((done) => { child.once("exit", done); child.kill(); });

/** Another program on the port: answers any Host with a page that watches the origin's storage. */
const HOSTILE = `<!doctype html><title>hostile</title><script>
  window.__seen = []; addEventListener("storage", (e) => __seen.push([e.key, e.newValue]));
</script>`;
/** Script requests the squatter received, and what it answers them with. */
const scriptsAsked: string[] = [];
const HOSTILE_SCRIPT = `window.__owned = true; export default function () { return null; }`;
function hostile(port: number, address: string): Promise<http.Server> {
  const server = http.createServer((req, res) => {
    if (req.url!.endsWith(".js")) { scriptsAsked.push(req.url!); res.writeHead(200, { "content-type": "text/javascript" }).end(HOSTILE_SCRIPT); return; }
    res.writeHead(200, { "content-type": "text/html" }).end(HOSTILE);
  });
  servers.push(server);
  return new Promise((ready) => server.listen(port, address, () => ready(server)));
}
const close = (server: http.Server) => new Promise((done) => { server.closeAllConnections(); server.close(done); });

test("a name under .localhost reaches the server, and is an origin of its own", async ({ page }) => {
  await relay(BASE, "token-one", "one");
  await page.goto(`http://one.localhost:${BASE}/#k=token-one`);
  await expect(page.locator("#conn")).toHaveText("live");
  expect(await page.evaluate(() => location.origin)).toBe(`http://one.localhost:${BASE}`);
});

test("the name resolves to both loopback addresses: holding 127.0.0.1 alone is not owning the origin", async ({ page }) => {
  await relay(BASE + 1, "token-one", "two", "v4only");
  await hostile(BASE + 1, "::1");
  await page.goto(`http://two.localhost:${BASE + 1}/`);
  // Whoever holds [::1]:port answers for the origin.
  expect(await page.title()).toBe("hostile");
});

// The reviewer's finding (m-f0921e2e2bcf): with an origin that outlives the
// server, a page served by whoever held the port in between sees the next
// server's token.
for (const [name, nextLabel, leaks] of [["a reused origin leaks the next token to a page left open", "same", true], ["a fresh label per start does not", "fresh", false]] as const) {
  test(name, async ({ context }) => {
    const port = BASE + (leaks ? 2 : 3);
    const first = await relay(port, "token-one", "same");
    const old = await context.newPage();
    await old.goto(`http://same.localhost:${port}/#k=token-one`);
    await expect(old.locator("#conn")).toHaveText("live");

    // The server stops; someone else takes the port and the old tab is reloaded.
    await stop(first);
    const squat = [await hostile(port, "127.0.0.1"), await hostile(port, "::1")];
    await old.reload();
    expect(await old.title()).toBe("hostile");
    // The squatter lets go; the next server starts on the same port and a new tab signs in.
    await Promise.all(squat.map(close));
    await relay(port, "token-two", nextLabel);
    const fresh = await context.newPage();
    await fresh.goto(`http://${nextLabel}.localhost:${port}/#k=token-two`);
    await expect(fresh.locator("#conn")).toHaveText("live");

    const seen = await old.evaluate(() => (window as any).__seen);
    const readable = await old.evaluate(() => localStorage.getItem("crew.web.session"));
    if (leaks) {
      expect(seen).toContainEqual(["crew.web.session", "token-two"]);
      expect(readable).toBe("token-two");
    } else {
      expect(seen).toEqual([]);
      expect(readable).toBe("token-one"); // the dead server's token, nothing newer
    }
  });
}

// ---------------------------------------------------------------------------
// The reviewer's second finding (m-b89b12d0fa53): the server that is gone, a
// tab of it still open, and a second document of the dead origin that reads
// the origin's storage for whoever holds the port now.

import { createHash } from "node:crypto";
import type { Duplex } from "node:stream";

const sha256 = (s: string) => createHash("sha256").update(s).digest("hex");

/** A WebSocket endpoint in a few lines: text frames only, enough to answer `hello`, `subscribe`, `snapshot`. */
function forgedSocket(server: http.Server, answer: (request: any) => unknown) {
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
        if (len === 126) { if (buf.length < 4) return; len = buf.readUInt16BE(2); at = 4; }
        if (buf.length < at + 4 + len) return;
        const mask = buf.subarray(at, at + 4), body = Buffer.from(buf.subarray(at + 4, at + 4 + len));
        const opcode = buf[0] & 0x0f;
        buf = buf.subarray(at + 4 + len);
        if (opcode !== 1) continue;
        for (let i = 0; i < body.length; i++) body[i] ^= mask[i % 4];
        const request = JSON.parse(body.toString());
        const out = Buffer.from(JSON.stringify({ id: request.id, result: answer(request) }));
        const head = out.length < 126 ? Buffer.from([0x81, out.length]) : Buffer.from([0x81, 126, out.length >> 8, out.length & 0xff]);
        socket.write(Buffer.concat([head, out]));
      }
    });
    socket.on("error", () => {});
  });
}

const FORGED = {
  session: { project: "forged", workspace: "/", state_dir: "/", daemon_started_at: 0, daemon_pid: 1, version: "x" },
  agents: [{ id: "FORGED", name: "FORGED", role: "made up", status: "working", activity: "not your team", runtime: "shell", workdir: "/", backend: "pty", last_activity_at: 0 }],
  tasks: [], messages: [], events: [], questions: [], server_time: 0,
};

for (const [name, mode, fooled] of [
  ["a proof kept in storage can be replayed to a tab that is still open (revision 2)", "stored", true],
  ["a stored hash of the proof cannot (the design)", "hash", false],
] as const) {
  test(name, async ({ context }) => {
    const port = BASE + (fooled ? 4 : 5);
    const secret = "the-servers-secret-" + mode;
    const server = await relay(port, "token-one", "proof", `proof=${secret}`);
    const kept = mode === "stored" ? secret : sha256(secret);
    const trusted = await context.newPage();
    await trusted.goto(`http://proof.localhost:${port}/#k=token-one&v=${kept}&m=${mode}`);
    await expect(trusted.locator("#conn")).toHaveText("live");
    await expect(trusted.locator(".node")).toHaveCount(6);
    // `crypto.subtle` is there: a name under .localhost is a secure context.
    expect(await trusted.evaluate(() => isSecureContext)).toBe(true);

    // The server dies without a word. Someone else takes the port on both addresses.
    server.kill("SIGKILL");
    await new Promise((gone) => server.once("exit", gone));
    let replay = "";
    const squat = [await hostile(port, "127.0.0.1"), await hostile(port, "::1")];
    for (const s of squat) forgedSocket(s, (request) =>
      request.method === "hello" ? { role: "operator", protocol: 4, version: "x", pid: 1, web_proof: replay }
      : request.method === "snapshot" ? FORGED
      : { subscribed: true });

    // A second document of the dead origin (a reloaded tab, a bookmark) reads
    // what the origin holds and hands it over.
    const second = await context.newPage();
    await second.goto(`http://proof.localhost:${port}/`);
    expect(await second.title()).toBe("hostile");
    const stolen = await second.evaluate(() => ({ token: localStorage.getItem("crew.web.session"), verifier: localStorage.getItem("crew.web.verifier") }));
    expect(stolen).toEqual({ token: "token-one", verifier: kept });
    replay = stolen.verifier!;
    // It also rewrites the storage; a document that started earlier must not care.
    await second.evaluate(() => { localStorage.setItem("crew.web.verifier", "attacker"); localStorage.setItem("crew.web.mode", "stored"); });

    // The tab that was open all along reconnects by itself.
    if (fooled) {
      await expect(trusted.locator(".node")).toHaveCount(1, { timeout: 15_000 });
      await expect(trusted.locator(".node")).toContainText("FORGED");
      await expect(trusted.locator("#conn")).toHaveText("live");
    } else {
      await expect(trusted.locator("#conn")).toContainText("stopped", { timeout: 15_000 });
      await expect(trusted.locator(".node")).toHaveCount(6); // what it last knew, nothing of the forgery
      await expect(trusted.locator(".node").filter({ hasText: "FORGED" })).toHaveCount(0);
      // … and it stays stopped: no further attempt reaches the squatter.
      await trusted.waitForTimeout(1500);
      await expect(trusted.locator("#conn")).toContainText("stopped");
    }
  });
}

// A document that is still open when its server is gone asks the origin for
// nothing it would run: whoever holds the port answers script requests too,
// and the policy (`script-src 'self'`) lets that answer run.
for (const [name, lazy] of [
  ["a script first requested after the server is gone is the squatter's script", true],
  ["a page that fetched its code while the server lived asks for none afterwards (the design)", false],
] as const) {
  test(name, async ({ context }) => {
    const port = BASE + (lazy ? 6 : 7);
    const secret = "another-secret";
    const server = await relay(port, "token-one", "code", `proof=${secret}`);
    const trusted = await context.newPage();
    await trusted.goto(`http://code.localhost:${port}/#k=token-one&v=${sha256(secret)}&m=hash${lazy ? "&lazy=1" : ""}`);
    await expect(trusted.locator("#conn")).toHaveText("live");
    const fetched = () => trusted.evaluate(() => performance.getEntriesByType("resource").filter((r) => r.name.endsWith(".js")).length);
    if (!lazy) await expect.poll(fetched).toBeGreaterThanOrEqual(4); // entry, Term, xterm, addon-fit
    else expect(await fetched()).toBe(1);

    server.kill("SIGKILL");
    await new Promise((gone) => server.once("exit", gone));
    scriptsAsked.length = 0;
    const squat = [await hostile(port, "127.0.0.1"), await hostile(port, "::1")];
    for (const s of squat) forgedSocket(s, () => ({ role: "operator", protocol: 4, version: "x", pid: 1 }));

    // The operator opens a terminal in the tab that was open all along.
    await trusted.getByRole("button", { name: /claude-3/ }).press("Enter");
    if (lazy) {
      await expect.poll(() => trusted.evaluate(() => (window as any).__owned)).toBe(true);
      expect(scriptsAsked.length).toBeGreaterThan(0);
    } else {
      await expect(trusted.locator(".xterm")).toBeVisible(); // its own code, from memory
      expect(await trusted.evaluate(() => (window as any).__owned)).toBeUndefined();
      expect(scriptsAsked).toEqual([]);
    }
  });
}
