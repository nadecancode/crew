// The inspector's terminal tab (notes/web-design.md §transport "No polling").
//
// pty backend: xterm.js attached over a socket of its own; output is pushed
// as the process writes it, keys go in as they are typed. A terminal socket
// that closes shows "disconnected" and attaches again when the state socket
// is live again.
//
// herdr backend: there is no attachment. The tab shows the last screen and a
// "jump to pane" button, and reads the screen (`agent_output`) when the tab
// opens, on the refresh key, after input the operator sent, and on an `agent`
// push whose `last_activity_at` changed, at least 1 s apart. Keys typed into
// it go to the agent with `agent_input`; keys that pile up while a call is
// out go in the next one.

import { Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";
import { useApp } from "../app";
import type { ConnState } from "../net/conn";
import { decodeOutput, type Agent } from "../protocol";
import type { TerminalHandle } from "../term/Terminal";
import { Sgr } from "../text";
import { useOps } from "./ops";

export function TerminalTab(props: { agent: Agent }) {
  return (
    <Show when={props.agent.backend === "herdr"} fallback={<PtyTerminal id={props.agent.id} />}>
      <HerdrScreen agent={props.agent} />
    </Show>
  );
}

function PtyTerminal(props: { id: string }) {
  const app = useApp();
  const { link } = app;
  let host!: HTMLDivElement;
  const [state, setState] = createSignal<ConnState | "loading" | "unavailable">("loading");
  const [ended, setEnded] = createSignal(false);
  let handle: TerminalHandle | null = null;
  let disposed = false;

  const attach = async () => {
    const mod = await app.terminal.catch(() => null);
    if (disposed) return;
    if (!mod) return setState("unavailable");
    handle?.dispose();
    host.replaceChildren();
    setEnded(false);
    handle = mod.mountTerminal(host, {
      agent: props.id,
      env: link.env,
      session: link.session,
      state: (s) => setState(s),
      proofFailed: () => link.stop("proof"),
      ended: () => setEnded(true),
    });
    handle.setReadOnly(!link.live());
  };
  onMount(() => {
    if (link.live()) void attach();
    // Attached again once the state socket is live again.
    const off = link.onLive(() => {
      if (state() !== "live") void attach();
    });
    onCleanup(off);
  });
  createEffect(on(link.live, (live) => handle?.setReadOnly(!live)));
  createEffect(on(() => props.id, () => void (state() !== "loading" && attach()), { defer: true }));
  onCleanup(() => {
    disposed = true;
    handle?.dispose();
    handle = null;
  });

  return (
    <div class="terminal-tab" data-terminal={props.id}>
      <Show when={state() === "unavailable"}>
        <p class="pad warn">The terminal's code was not loaded when this page started; this page cannot load it now. Open a new tab from <code>crew web</code>.</p>
      </Show>
      <Show when={state() === "stopped" && !ended()}>
        <div class="term-note warn">disconnected · it attaches again when the dashboard is live</div>
      </Show>
      <Show when={state() === "loading" || state() === "connecting"}>
        <div class="term-note muted">attaching…</div>
      </Show>
      <div ref={host} class="xterm-host" onClick={() => handle?.focus()} />
    </div>
  );
}

const READ_GAP_MS = 1000;

/** Keys as a terminal sends them. */
function keyText(e: KeyboardEvent): string | null {
  if (e.ctrlKey && !e.altKey && !e.metaKey && e.key.length === 1) {
    const c = e.key.toUpperCase().charCodeAt(0);
    if (c >= 64 && c <= 95) return String.fromCharCode(c - 64);
    return null;
  }
  if (e.metaKey) return null;
  const named: Record<string, string> = {
    Enter: "\r", Backspace: "\x7f", Tab: "\t", Escape: "\x1b",
    ArrowUp: "\x1b[A", ArrowDown: "\x1b[B", ArrowRight: "\x1b[C", ArrowLeft: "\x1b[D",
    Home: "\x1b[H", End: "\x1b[F", Delete: "\x1b[3~", PageUp: "\x1b[5~", PageDown: "\x1b[6~",
  };
  if (named[e.key]) return (e.altKey ? "\x1b" : "") + named[e.key];
  if (e.key.length === 1) return (e.altKey ? "\x1b" : "") + e.key;
  return null;
}

function HerdrScreen(props: { agent: Agent }) {
  const app = useApp();
  const ops = useOps();
  const { link, ui } = app;
  const [screen, setScreen] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  let lastRead = 0;
  let inFlight = false;
  let again: number | null = null;
  const read = () => {
    if (!link.live()) return;
    const wait = lastRead + READ_GAP_MS - Date.now();
    if (inFlight || wait > 0) {
      if (again === null) again = window.setTimeout(() => ((again = null), read()), Math.max(wait, 50));
      return;
    }
    inFlight = true;
    lastRead = Date.now();
    link.call("agent_output", { id: props.agent.id, lines: 300, screen: true, ansi: true }, decodeOutput)
      .then((r) => (setScreen(r.text), setError(null)), (e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => (inFlight = false));
  };
  onCleanup(() => again !== null && window.clearTimeout(again));
  // The tab opening, the refresh key, activity of the agent.
  createEffect(on(() => [props.agent.id, props.agent.last_activity_at, ui.refreshTick()], read));

  // Keys typed here go to the agent; what piles up while a call is out goes in the next.
  let pending = "";
  let sending = false;
  const flush = () => {
    if (sending || !pending || !link.live()) return;
    const text = pending;
    pending = "";
    sending = true;
    link.call("agent_input", { id: props.agent.id, text, submit: false })
      .catch((e) => ui.setStatus(`keys not delivered: ${e instanceof Error ? e.message : e}`, true))
      .finally(() => {
        sending = false;
        read();
        flush();
      });
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.ctrlKey && (e.key === "]" || e.code === "BracketRight")) {
      e.preventDefault();
      (e.currentTarget as HTMLElement).blur();
      return;
    }
    const t = keyText(e);
    if (t === null || !link.live()) return;
    e.preventDefault();
    e.stopPropagation();
    pending += t;
    flush();
  };
  const onPaste = (e: ClipboardEvent) => {
    const text = e.clipboardData?.getData("text/plain");
    if (!text || !link.live()) return;
    e.preventDefault();
    pending += text;
    flush();
  };

  return (
    <div class="terminal-tab herdr" data-terminal={props.agent.id}>
      <div class="term-bar">
        <span class="muted">herdr pane {props.agent.backend_ref ?? ""} · the last screen; click it to type into the agent, ctrl-] gives the keyboard back</span>
        <span class="spacer" />
        <button disabled={!link.live()} onClick={() => ops.focusPane(props.agent.id)}>jump to pane</button>
        <button disabled={!link.live()} onClick={() => ui.refresh()} title="read the screen again (r)">refresh</button>
      </div>
      <Show when={screen() !== null} fallback={<p class="pad muted">{error() ?? "reading the screen…"}</p>}>
        <pre class="screen" tabindex="0" aria-label={`the terminal of ${props.agent.id}`} onKeyDown={onKey} onPaste={onPaste}><Sgr text={screen()!} /></pre>
      </Show>
    </div>
  );
}
