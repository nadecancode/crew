// A WebSocket client in plain node, written by hand so a test sets Host and
// Origin itself, and so the same source can run inside an agent's sandbox
// (harness.ts hands it to `node -e`). No imports beyond node's own, no
// top-level effects, nothing the page's own code is.
import { randomBytes } from "node:crypto";
import { connect } from "node:net";

/** Open `ws://127.0.0.1:<port>/ws` presenting `host` and `origin`. Resolves with
 * { upgraded: false } when the server refuses the upgrade, else a client:
 * `call(method, params)` answers the reply frame, `send(method, params)` does
 * not wait, `replies` counts the reply frames seen, `close()`. */
export function rawWs(port, host, origin, { timeoutMs = 8000 } = {}) {
  return new Promise((resolve) => {
    const sock = connect(port, "127.0.0.1");
    sock.setNoDelay(true);
    const key = randomBytes(16).toString("base64");
    let buf = Buffer.alloc(0);
    let upgraded = false;
    let next = 1;
    const waiting = new Map();
    const client = {
      upgraded: true,
      replies: 0,
      errors: 0,
      send(method, params = {}) {
        const id = next++;
        const payload = Buffer.from(JSON.stringify({ id, method, params }));
        const mask = randomBytes(4);
        const head = payload.length < 126 ? Buffer.from([0x81, 0x80 | payload.length]) : payload.length < 65536 ? Buffer.from([0x81, 0x80 | 126, payload.length >> 8, payload.length & 255]) : (() => { const h = Buffer.alloc(10); h[0] = 0x81; h[1] = 0x80 | 127; h.writeBigUInt64BE(BigInt(payload.length), 2); return h; })();
        const body = Buffer.from(payload);
        for (let i = 0; i < body.length; i++) body[i] ^= mask[i % 4];
        sock.write(Buffer.concat([head, mask, body]));
        return id;
      },
      call(method, params = {}) {
        const id = client.send(method, params);
        return new Promise((ok, no) => {
          const t = setTimeout(() => no(new Error(`no answer to ${method}`)), timeoutMs);
          waiting.set(id, (frame) => (clearTimeout(t), ok(frame)));
        });
      },
      close() {
        sock.destroy();
      },
    };
    const refuse = () => resolve({ upgraded: false });
    sock.setTimeout(timeoutMs * 4, () => (upgraded ? undefined : (sock.destroy(), refuse())));
    sock.on("error", () => (upgraded ? undefined : refuse()));
    sock.on("close", () => (upgraded ? undefined : refuse()));
    sock.on("connect", () => sock.write(`GET /ws HTTP/1.1\r\nHost: ${host}\r\nOrigin: ${origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\n\r\n`));
    sock.on("data", (d) => {
      buf = Buffer.concat([buf, d]);
      if (!upgraded) {
        const end = buf.indexOf("\r\n\r\n");
        if (end < 0) return;
        if (!buf.subarray(0, 12).toString().includes("101")) return (sock.destroy(), refuse());
        upgraded = true;
        buf = buf.subarray(end + 4);
        resolve(client);
      }
      for (;;) {
        if (buf.length < 2) return;
        let len = buf[1] & 0x7f, off = 2;
        if (len === 126) {
          if (buf.length < 4) return;
          len = buf.readUInt16BE(2);
          off = 4;
        } else if (len === 127) {
          if (buf.length < 10) return;
          len = Number(buf.readBigUInt64BE(2));
          off = 10;
        }
        if (buf.length < off + len) return;
        const opcode = buf[0] & 0x0f;
        const body = buf.subarray(off, off + len);
        buf = buf.subarray(off + len);
        if (opcode !== 1) continue;
        let frame;
        try {
          frame = JSON.parse(body.toString());
        } catch {
          continue;
        }
        if (frame.id !== undefined && frame.id !== null) {
          client.replies++;
          if (frame.error) client.errors++;
          const w = waiting.get(frame.id);
          if (w) (waiting.delete(frame.id), w(frame));
        }
      }
    });
  });
}

/** `web_pair` with the code of a pairing address, then `hello`: a signed-in client. */
export async function signedIn(port, host, origin, code, protocol) {
  const ws = await rawWs(port, host, origin);
  if (!ws.upgraded) throw new Error("upgrade refused");
  const paired = await ws.call("web_pair", { code });
  if (!paired.result) throw new Error(`web_pair: ${JSON.stringify(paired)}`);
  const hello = await ws.call("hello", { role: "operator", token: paired.result.token, client: "web", protocol });
  if (!hello.result) throw new Error(`hello: ${JSON.stringify(hello)}`);
  return ws;
}
