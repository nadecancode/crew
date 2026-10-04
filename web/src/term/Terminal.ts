// The terminal, a chunk of its own (notes/web-design.md §build, §security
// "No code after the start"): the entry module requests it when the document
// starts, and a terminal is made from it when one first opens.
//
// xterm.js with the `fit` addon only: no clipboard addon, so OSC 52 cannot
// write the operator's clipboard; title changes are ignored; hyperlinks open
// only when they are http(s). Each terminal is a WebSocket of its own:
// `hello`, then `attach`; output arrives as `pty` pushes, keys go out as
// `attach_input`, size changes as `attach_resize`.

import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { Conn, type ConnState, type Env } from "../net/conn";
import type { Session } from "../net/session";
import { safeHref } from "../text";

/** The pane never asks the agent's terminal for less than this; it scrolls instead. */
export const MIN_COLS = 80;
export const MIN_ROWS = 24;

export type TerminalOptions = {
  agent: string;
  env: Env;
  session: Session;
  /** The socket's state: `live` once attached; `stopped` when it closed. */
  state(s: ConnState, reason?: string): void;
  /** The server failed its proof on this socket: the whole page stops. */
  proofFailed(): void;
  /** The process ended. */
  ended(): void;
};

export type TerminalHandle = {
  focus(): void;
  /** Keys go nowhere while the page cannot act. */
  setReadOnly(on: boolean): void;
  /** Close the socket for good; what the terminal shows stays. */
  detach(): void;
  dispose(): void;
};

const css = (name: string, fallback: string) => getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;

function utf8ToB64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

function b64ToBytes(b64: string): Uint8Array {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

export function mountTerminal(host: HTMLElement, o: TerminalOptions): TerminalHandle {
  const term = new Terminal({
    fontFamily: css("--mono", "ui-monospace, monospace"),
    fontSize: 12,
    scrollback: 2000,
    cursorBlink: false,
    allowProposedApi: false,
    theme: {
      background: css("--bg-term", "#111217"),
      foreground: css("--text", "#ccd0d8"),
      cursor: css("--text", "#ccd0d8"),
      selectionBackground: css("--faint", "#343740"),
    },
    linkHandler: {
      activate(_e, uri) {
        const href = safeHref(uri);
        if (href) window.open(href, "_blank", "noopener,noreferrer");
      },
    },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(host);
  // ctrl-] gives the keyboard back to the page.
  term.attachCustomKeyEventHandler((e) => {
    if (e.type === "keydown" && e.ctrlKey && (e.key === "]" || e.code === "BracketRight")) {
      term.blur();
      (host.closest("[data-terminal]")?.parentElement?.querySelector("[role=tab][aria-selected=true]") as HTMLElement | null)?.focus();
      return false;
    }
    return true;
  });

  let readOnly = false;
  let disposed = false;
  const size = () => {
    const p = fit.proposeDimensions();
    return { cols: Math.max(MIN_COLS, p?.cols ?? MIN_COLS), rows: Math.max(MIN_ROWS, p?.rows ?? MIN_ROWS) };
  };
  const resize = () => {
    if (disposed) return;
    const { cols, rows } = size();
    if (cols !== term.cols || rows !== term.rows) term.resize(cols, rows);
  };
  resize();

  // For the perf spec: bytes and frames written, on the element.
  const stats = { bytes: 0, frames: 0, term };
  (host as HTMLElement & { crewTerminal?: unknown }).crewTerminal = stats;

  const conn = new Conn({
    env: o.env,
    session: o.session,
    reconnect: false,
    async ready(c) {
      await c.call("attach", { agent: o.agent, rows: term.rows, cols: term.cols });
    },
    push(p) {
      if (p.kind === "pty") {
        const bytes = b64ToBytes(p.data.data);
        stats.bytes += bytes.length;
        stats.frames++;
        term.write(bytes);
      } else if (p.kind === "pty_closed") {
        term.write("\r\n\x1b[2m[process ended]\x1b[0m\r\n");
        o.ended();
      }
    },
    state(s, reason) {
      if (reason === "proof") o.proofFailed();
      o.state(s, reason);
    },
  });
  conn.start();

  const send = (data: string) => {
    if (!readOnly) conn.notify("attach_input", { data: utf8ToB64(data) });
  };
  term.onData(send);
  term.onBinary((data) => {
    if (readOnly) return;
    const bytes = new Uint8Array(data.length);
    for (let i = 0; i < data.length; i++) bytes[i] = data.charCodeAt(i) & 0xff;
    let s = "";
    bytes.forEach((b) => (s += String.fromCharCode(b)));
    conn.notify("attach_input", { data: btoa(s) });
  });
  term.onResize(({ rows, cols }) => conn.notify("attach_resize", { rows, cols }));
  const ro = new ResizeObserver(() => resize());
  ro.observe(host);

  return {
    focus: () => term.focus(),
    setReadOnly(on) {
      readOnly = on;
      term.options.disableStdin = on;
    },
    detach() {
      conn.close();
    },
    dispose() {
      disposed = true;
      ro.disconnect();
      conn.close();
      term.dispose();
    },
  };
}
