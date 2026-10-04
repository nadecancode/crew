import { onCleanup, onMount } from "solid-js";
import { PROTOCOL } from "./net";

// A second WebSocket, turned into a raw terminal attachment (pty backend).
export default function Term(props: { agent: string; token: string }) {
  let el!: HTMLDivElement;
  onMount(async () => {
    const [{ Terminal }, { FitAddon }] = await Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit")]);
    await import("@xterm/xterm/css/xterm.css");
    const term = new Terminal({ fontSize: 12, scrollback: 2000, convertEol: false });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();
    const ws = new WebSocket(`ws://${location.host}/ws`);
    let id = 1;
    const send = (method: string, params?: unknown) => ws.send(JSON.stringify({ id: id++, method, params }));
    ws.onopen = () => {
      send("hello", { role: "operator", token: props.token, client: "web-term", protocol: PROTOCOL });
      send("attach", { agent: props.agent, rows: term.rows, cols: term.cols });
    };
    const dec = (b64: string) => Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
    (window as any).__term = { bytes: 0, frames: 0, errors: [] as string[], term };
    ws.onmessage = (ev) => {
      const f = JSON.parse(ev.data);
      if (f.error) (window as any).__term.errors.push(f.error.message);
      if (f.push?.kind === "pty") { const b = dec(f.push.data.data); (window as any).__term.bytes += b.length; (window as any).__term.frames++; term.write(b); }
      if (f.push?.kind === "pty_closed") term.write("\r\n[process ended]\r\n");
    };
    const enc = new TextEncoder();
    term.onData((d) => send("attach_input", { data: btoa(String.fromCharCode(...enc.encode(d))) }));
    term.onResize(({ rows, cols }) => send("attach_resize", { rows, cols }));
    onCleanup(() => { ws.close(); term.dispose(); });
  });
  return <div ref={el} class="term" />;
}
