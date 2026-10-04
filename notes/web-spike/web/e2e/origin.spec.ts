// Who owns the page's origin: the checks behind §security "One origin per server start".
import { spawn, type ChildProcess } from "node:child_process";
import http from "node:http";
import { expect, test } from "@playwright/test";

const { RELAY, SOCK, DIST } = process.env;
const BASE = Number(process.env.PORT ?? 47615) + 10;
test.skip(!RELAY || !SOCK || !DIST, "needs RELAY, SOCK and DIST (run.sh sets them)");

const children: ChildProcess[] = [];
const servers: http.Server[] = [];
test.afterEach(async () => {
  for (const c of children.splice(0)) c.kill();
  await Promise.all(servers.splice(0).map((s) => new Promise((r) => s.close(r))));
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
function hostile(port: number, address: string): Promise<http.Server> {
  const server = http.createServer((_, res) => res.writeHead(200, { "content-type": "text/html" }).end(HOSTILE));
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
