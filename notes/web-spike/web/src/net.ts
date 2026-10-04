// One WebSocket = one daemon connection, speaking crew_core::rpc frames.
export type Frame = { id?: number; result?: unknown; error?: { code: string; message: string }; push?: { kind: string; data?: any } };
export type Handlers = { snapshot(s: any): void; push(p: { kind: string; data?: any }, t0: number): void; state(s: string): void };

export const PROTOCOL = Number((window as any).__PROTOCOL ?? 4);

export class Conn {
  private ws!: WebSocket;
  private next = 1;
  private waiting = new Map<number, { ok(v: any): void; err(e: any): void }>();
  private delay = 250;
  constructor(private token: string, private h: Handlers) { this.open(); }

  call<T = any>(method: string, params?: unknown): Promise<T> {
    const id = this.next++;
    return new Promise((ok, err) => {
      this.waiting.set(id, { ok, err });
      this.ws.send(JSON.stringify(params === undefined ? { id, method } : { id, method, params }));
    });
  }

  private open() {
    this.h.state("connecting");
    const ws = (this.ws = new WebSocket(`ws://${location.host}/ws`));
    ws.onopen = async () => {
      try {
        await this.call("hello", { role: "operator", token: this.token, client: "web", protocol: PROTOCOL });
        // Subscribe first, then the snapshot: nothing falls between the two.
        await this.call("subscribe");
        this.h.snapshot(await this.call("snapshot", { messages: 300, events: 2000 }));
        this.delay = 250;
        this.h.state("live");
      } catch (e: any) {
        this.h.state("error: " + (e?.message ?? e));
      }
    };
    ws.onmessage = (ev) => {
      const t0 = performance.now();
      const f: Frame = JSON.parse(ev.data);
      if (f.push) {
        if (f.push.kind === "resync") this.call("snapshot", { messages: 300, events: 2000 }).then((s) => this.h.snapshot(s));
        else this.h.push(f.push, t0);
        return;
      }
      const w = f.id !== undefined ? this.waiting.get(f.id) : undefined;
      if (!w) return;
      this.waiting.delete(f.id!);
      f.error ? w.err(f.error) : w.ok(f.result);
    };
    ws.onclose = () => {
      for (const w of this.waiting.values()) w.err({ code: "closed", message: "connection closed" });
      this.waiting.clear();
      this.h.state("disconnected");
      setTimeout(() => this.open(), this.delay);
      this.delay = Math.min(this.delay * 2, 5000);
    };
  }
}
