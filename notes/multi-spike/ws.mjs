// A WebSocket client in plain node: the test sets Host and Origin itself and
// sees pushes, replies and the close frame with their arrival times. With
// `ignoreClose` it is a client that does not do as it is told: it keeps its
// socket and keeps writing after the server said goodbye.
import { randomBytes } from "node:crypto";
import { connect } from "node:net";

/** Resolves with null when the upgrade is refused, else a client. */
export function open(port, host, { origin = `http://${host}`, onPush = () => {}, timeoutMs = 8000, ignoreClose = false } = {}) {
  return new Promise((resolve) => {
    const sock = connect(port, "127.0.0.1");
    sock.setNoDelay(true);
    let buf = Buffer.alloc(0), upgraded = false, next = 1;
    const waiting = new Map();
    const client = {
      closed: null, // { code, reason } once the server closed
      onPush,
      sendRaw(text) {
        const payload = Buffer.from(text), mask = randomBytes(4);
        const head = payload.length < 126 ? Buffer.from([0x81, 0x80 | payload.length])
          : payload.length < 65536 ? Buffer.from([0x81, 0x80 | 126, payload.length >> 8, payload.length & 255])
          : (() => { const h = Buffer.alloc(10); h[0] = 0x81; h[1] = 0x80 | 127; h.writeBigUInt64BE(BigInt(payload.length), 2); return h; })();
        const body = Buffer.from(payload);
        for (let i = 0; i < body.length; i++) body[i] ^= mask[i % 4];
        try { sock.write(Buffer.concat([head, mask, body])); } catch { /* the other end is gone */ }
      },
      call(method, params) {
        const id = next++;
        client.sendRaw(JSON.stringify(params === undefined ? { id, method } : { id, method, params }));
        return new Promise((ok, no) => {
          const t = setTimeout(() => no(new Error(`no answer to ${method}`)), timeoutMs);
          waiting.set(id, (frame) => (clearTimeout(t), ok(frame)));
        });
      },
      /** Replies to ids nobody waits for (a smuggled request's). */
      stray: [],
      close() { sock.destroy(); },
      /** Stop reading: what the server writes piles up until it cannot write. */
      pause() { sock.pause(); },
      whenClosed: null,
    };
    let closedResolve;
    client.whenClosed = new Promise((r) => (closedResolve = r));
    const refuse = () => (upgraded ? closedResolve(client.closed) : resolve(null));
    sock.on("error", refuse);
    sock.on("close", refuse);
    sock.on("connect", () => sock.write(`GET /ws HTTP/1.1\r\nHost: ${host}\r\nOrigin: ${origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${randomBytes(16).toString("base64")}\r\nSec-WebSocket-Version: 13\r\n\r\n`));
    sock.on("data", (d) => {
      const at = process.hrtime.bigint();
      buf = buf.length ? Buffer.concat([buf, d]) : d;
      if (!upgraded) {
        const end = buf.indexOf("\r\n\r\n");
        if (end < 0) return;
        if (!buf.subarray(0, 12).toString().includes("101")) return (sock.destroy(), resolve(null));
        upgraded = true;
        buf = buf.subarray(end + 4);
        resolve(client);
      }
      for (;;) {
        if (buf.length < 2) return;
        let len = buf[1] & 0x7f, off = 2;
        if (len === 126) { if (buf.length < 4) return; len = buf.readUInt16BE(2); off = 4; }
        else if (len === 127) { if (buf.length < 10) return; len = Number(buf.readBigUInt64BE(2)); off = 10; }
        if (buf.length < off + len) return;
        const opcode = buf[0] & 0x0f, body = buf.subarray(off, off + len);
        buf = buf.subarray(off + len);
        if (opcode === 8) { client.closed = { code: body.length >= 2 ? body.readUInt16BE(0) : 1005, reason: body.subarray(2).toString() }; closedResolve(client.closed); if (!ignoreClose) sock.destroy(); continue; }
        if (opcode !== 1) continue;
        let frame;
        try { frame = JSON.parse(body.toString()); } catch { continue; }
        if (frame.push) client.onPush(frame.push, at);
        else if (frame.id !== undefined && frame.id !== null) {
          const w = waiting.get(frame.id);
          if (w) (waiting.delete(frame.id), w(frame)); else client.stray.push(frame);
        }
      }
    });
  });
}
