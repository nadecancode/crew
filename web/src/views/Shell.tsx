// The page (notes/web-design.md §ui "Layout"):
//
//   status bar (with the workspace switcher) · usage strip · question banner
//   main area (graph, tasks, logs, usage) | side panel
//   message box · footer
//
// A page that is reconnecting or stopped retains the last known workspace
// and disables mutations beneath a visible availability notice.
// So is a workspace that is not live while the page is (notes/web-multi.md
// §multi-ui "Not running, incompatible"): what it last showed, dimmed, under
// a bar that says why.

import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup, onMount, type JSX } from "solid-js";
import { AppContext, pageTitle, useApp, workspaceName, type Page } from "../app";
import { useSecond } from "../clock";
import { clockTime, fmtElapsed, runtimeGlyph } from "../format";
import { AgentGraph } from "../graph/AgentGraph";
import { TaskDag } from "../graph/TaskDag";
import { handleKey, keyboardFree } from "../keys";
import type { Theme } from "../prefs";
import { operations } from "../operate";
import { asker } from "../protocol";
import { ContextMenu } from "./ContextMenu";
import { Dialogs } from "./dialogs";
import { Logs } from "./Logs";
import { MessageBox } from "./MessageBox";
import { SidePanel } from "./SidePanel";
import { UsageView } from "./UsageView";
import { levelTone, pausedText, until, usageRows, windowLabel, STALE_MS } from "./usage";
import { OpsContext, useOps } from "./ops";
import { SwitcherButton, homeDir } from "./Switcher";

/** The workspace on screen, made anew at each switch: nothing of the one
 * left is drawn under the name of the next. */
export function Root(props: { page: Page }) {
  return (
    <Show when={props.page.app()} keyed>
      {(app) => (
        <AppContext.Provider value={app}>
          <Shell />
        </AppContext.Provider>
      )}
    </Show>
  );
}

export function Shell() {
  const app = useApp();
  const ops = operations(app);
  const { ui, link } = app;
  const media = typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: dark)") : null;
  const [dark, setDark] = createSignal(media?.matches ?? false);
  createEffect(() => {
    const theme = app.prefs.prefs.theme;
    document.documentElement.dataset.theme = theme === "system" ? (dark() ? "dark" : "light") : theme;
  });

  onMount(() => {
    const onKey = (e: KeyboardEvent) => handleKey(app, ops, e);
    const onHash = () => app.page.fromLocation();
    const appearance = () => setDark(media?.matches ?? false);
    media?.addEventListener("change", appearance);
    document.addEventListener("keydown", onKey);
    window.addEventListener("hashchange", onHash);
    onCleanup(() => {
      media?.removeEventListener("change", appearance);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("hashchange", onHash);
    });
  });

  // The page title: `<project> · crew`, with the questions waiting in any
  // joined workspace in front: `(3) payments · crew`.
  createEffect(() => (document.title = pageTitle(app)));

  // A dialog does not outlive the workspace it was opened for.
  createEffect(on(link.shown, (s) => s === "removed" && ui.closeDialog()));

  // A question opens its dialog by itself once nothing else holds the
  // keyboard (a dialog, a focused text field, the terminal), as
  // `App::pop_question` decides.
  const [focusTick, setFocusTick] = createSignal(0);
  onMount(() => {
    const bump = () => setFocusTick((n) => n + 1);
    document.addEventListener("focusin", bump);
    document.addEventListener("focusout", bump);
    onCleanup(() => {
      document.removeEventListener("focusin", bump);
      document.removeEventListener("focusout", bump);
    });
  });
  createEffect(() => {
    focusTick();
    if (!app.store.state.loaded || !link.live() || ui.dialog()) return;
    const q = app.store.openQuestions().find((q) => !ui.shownQuestions.has(q.id));
    if (!q) return;
    // The focus moves in a microtask after a click; check once it settled.
    queueMicrotask(() => {
      if (!ui.dialog() && keyboardFree(app) && !ui.shownQuestions.has(q.id)) ui.openDialog({ kind: "question", id: q.id });
    });
  });

  const state = () => (link.conn() === "connecting" ? "reconnecting" : link.conn());
  const readOnly = () => !link.live();

  return (
    <OpsContext.Provider value={ops}>
      <div class="page" classList={{ readonly: readOnly(), loaded: app.store.state.loaded, team: ui.view() === "graph" }} data-conn-state={state()} data-workspace-state={link.shown()}>
        <Show when={app.store.state.loaded || link.conn() === "live"} fallback={<Entry />}>
          <StatusBar />
          <WorkspaceSummary />
          <ViewNav />
          <Warnings />
          <ConnBar />
          <WorkspaceBar />
          <UsageStrip />
          <QuestionBanner />
          <div class="page-body" classList={{ full: ui.panelFull(), replacement: ui.viewport() < 768 && ui.panelOpen() }}>
            <main id="main" class="main" data-region="main" aria-label="main area" inert={ui.panelFull() || (ui.viewport() < 768 && ui.panelOpen())}>
              <Switch>
                <Match when={!app.store.state.loaded}><Empty><div class="ws-text"><WorkspaceText /></div></Empty></Match>
                <Match when={ui.view() === "graph"}><AgentGraph /></Match>
                <Match when={ui.view() === "tasks"}><TaskDag /></Match>
                <Match when={ui.view() === "logs"}><Logs /></Match>
                <Match when={ui.view() === "usage"}><UsageView /></Match>
              </Switch>
            </main>
            <SidePanel />
          </div>
          <MessageBox />
          <Footer />
        </Show>
        <Toasts />
        <ContextMenu />
        <Dialogs />
      </div>
    </OpsContext.Provider>
  );
}

/** Before anything was loaded: connecting, or signed out, or stopped. */
function Entry() {
  const app = useApp();
  const { link } = app;
  return (
    <div class="entry">
      <div class="entry-box">
        <h1><span class="orch">◆</span> crew</h1>
        <div data-conn={link.conn() === "connecting" ? "reconnecting" : link.conn()} class="conn-text">
          <Switch fallback={<p>connecting…</p>}>
            <Match when={link.conn() === "signed-out"}>
              <p>Signed out: run <code>crew web</code> in the workspace and open the address it prints.</p>
              <Show when={link.stopDetail()}><p class="muted">{link.stopDetail()}</p></Show>
              <p class="muted">Paste that address nowhere else: it signs in whoever opens it first.</p>
            </Match>
            <Match when={link.conn() === "stopped"}>
              <StopText />
            </Match>
            <Match when={link.conn() === "reconnecting"}><p>connecting… the dashboard's server does not answer yet.</p></Match>
          </Switch>
        </div>
      </div>
    </div>
  );
}

function StopText() {
  const { link } = useApp();
  const at = () => (link.stoppedAt() ? clockTime(link.stoppedAt()!) : "");
  return (
    <Switch>
      <Match when={link.stopReason() === "shutdown"}><p>The dashboard's server was stopped at {at()}. Run <code>crew web</code> for the new address. This tab cannot come back; reloading it asks whoever holds the port now.</p></Match>
      <Match when={link.stopReason() === "proof"}><p>The dashboard was stopped at {at()}: whoever answers at this address could not prove it is this dashboard's server. Run <code>crew web</code> for the new address. This tab cannot come back; reloading it asks whoever holds the port now.</p></Match>
      <Match when={link.stopReason() === "protocol"}><p>This page and the dashboard's server speak different protocol versions ({link.stopDetail()}). Run <code>crew web</code> after upgrading crew.</p></Match>
      <Match when={link.stopReason() === "unauthorized" || link.stopReason() === "no-session"}><p>Signed out: run <code>crew web</code>.</p></Match>
      <Match when={true}><p>The dashboard was stopped at {at()}. Run <code>crew web</code> for the new address. This tab cannot come back; reloading it asks whoever holds the port now.</p></Match>
    </Switch>
  );
}

/** What the workspace on screen is, when it is not live and the page is. */
function WorkspaceText() {
  const { link, page, store } = useApp();
  const entry = () => page.hub.entry(link.id);
  const name = () => (entry() ? workspaceName(entry()!) : store.state.session?.project ?? link.id ?? "");
  const dir = () => homeDir(entry()?.workspace ?? "");
  return (
    <Switch>
      <Match when={link.shown() === "not-running"}>
        <p><b>{name()}</b> is not running. Start it (<code>crew run</code> in {dir()}) and it comes back here.</p>
        <Show when={entry()?.state === "unreachable" && link.detail()}><p class="muted">{link.detail()}</p></Show>
      </Match>
      <Match when={link.shown() === "incompatible"}>
        <p><b>{name()}</b> was started by another version of crew. Restart the team to see it here.</p>
        <Show when={link.detail()}><p class="muted">{link.detail()}</p></Show>
      </Match>
      <Match when={link.shown() === "removed" && link.seen()}><p><b>{name()}</b> was taken out of this dashboard. Pick another workspace (<kbd>w</kbd>), or run <code>crew web</code> in it to bring it back.</p></Match>
      <Match when={link.shown() === "removed"}><p>That workspace is not in this dashboard: run <code>crew web</code> in it. The others are in the switcher (<kbd>w</kbd>).</p></Match>
      <Match when={link.shown() === "none"}><p>No workspace is shown here. Run <code>crew web</code> in a workspace.</p></Match>
      <Match when={link.shown() === "reconnecting"}><p>reconnecting to <b>{name()}</b>… what you see is what it last showed; nothing can be sent until it answers.</p></Match>
      <Match when={true}><p>connecting to <b>{name()}</b>…</p></Match>
    </Switch>
  );
}

/** The workspace on screen is not live, the page is: why, over what it last showed. */
function WorkspaceBar() {
  const { link, store } = useApp();
  return (
    <Show when={store.state.loaded && link.conn() === "live" && link.shown() !== "live" && link.shown() !== "connecting"}>
      <div class="conn-bar ws-bar" classList={{ stopped: link.shown() !== "reconnecting" }} role="status" data-workspace-bar={link.shown()}>
        <WorkspaceText />
      </div>
    </Show>
  );
}

/** Reconnecting or stopped: what the page shows is what it last knew. */
function ConnBar() {
  const { link } = useApp();
  return (
    <Show when={link.conn() !== "live"}>
      <div class="conn-bar" classList={{ stopped: link.conn() === "stopped" || link.conn() === "signed-out" }} role="status">
        <Show when={link.conn() === "stopped" || link.conn() === "signed-out"} fallback={<p>reconnecting… what you see is what the dashboard last knew; nothing can be sent until the server answers.</p>}>
          <StopText />
        </Show>
      </div>
    </Show>
  );
}

function StatusBar() {
  const app = useApp();
  const ops = useOps();
  const { ui, link, prefs } = app;
  const conn = () => link.conn() === "connecting" ? "reconnecting" : link.conn();
  return (
    <header class="status-bar" data-region="status">
      <span class="brand"><span class="gauge" aria-hidden="true">◆</span> crew</span>
      <SwitcherButton />
      <span class="spacer" />
      <span class="conn" data-conn={conn()} role="status" aria-live="polite"><span class="dot" aria-hidden="true">●</span> Dashboard {conn() === "signed-out" ? "signed out" : conn()}</span>
      <button disabled={!link.live()} onClick={() => ops.command()} title="command line (Ctrl K)">Commands <kbd aria-hidden="true">Ctrl K</kbd></button>
      <label class="appearance">Appearance <select aria-label="Appearance" value={prefs.prefs.theme} onChange={(e) => prefs.setTheme(e.currentTarget.value as Theme)}><option value="system">System</option><option value="light">Light</option><option value="dark">Dark</option></select></label>
      <button onClick={() => ui.openDialog({ kind: "help" })} title="keys and commands (?)" aria-label="keys and commands">?</button>
      <button disabled={!link.live() || app.isPending("shutdown")} onClick={() => ops.stop()} title="Workspace actions: stop the team (S)">stop team</button>
    </header>
  );
}

function WorkspaceSummary() {
  const { store } = useApp();
  const second = useSecond();
  const s = () => store.state.session;
  const elapsed = () => {
    second();
    const start = s()?.team_started_at;
    return start ? fmtElapsed((s()?.completed_at ?? store.serverNow(Date.now())) - start) : null;
  };
  const counts = createMemo(() => [
    [store.state.agents.length, "Agents"],
    [store.state.agents.filter((a) => a.status === "working" && !a.paused).length, "Working"],
    [`${store.state.tasks.filter((t) => t.status === "done").length}/${store.state.tasks.length}`, "Tasks complete"],
    [store.openQuestions().length, "Questions"],
  ].map(([value, label]) => [store.state.loaded ? value : "—", label]));
  return <section class="workspace-summary" aria-label="workspace summary">
    <div class="objective-block"><details class="objective"><summary title="Expand objective"><h1>{s()?.objective || s()?.project || "Workspace"}</h1></summary></details>
      <p class="workspace-meta"><span title={s()?.workspace}>{homeDir(s()?.workspace ?? "")}</span> · {elapsed()} <span class="vcs" title={s()?.vcs_note || `agents work with ${s()?.vcs}`}>{s()?.vcs}</span><Show when={s()?.sandbox}> · {s()?.sandbox === "required" ? "sandbox" : s()?.sandbox === "off" ? "no sandbox" : "sandbox unconfirmed"}</Show></p>
      <Show when={s()?.summary || s()?.completed_at != null}><details class="completion"><summary>Team completed <Show when={s()?.completed_at != null}>{clockTime(s()!.completed_at!)}</Show></summary><p>{s()?.summary}</p></details></Show>
    </div>
    <dl class="summary-counts"><For each={counts()}>{([value, label]) => <div><dd>{value}</dd><dt>{label}</dt></div>}</For></dl>
  </section>;
}

function ViewNav() {
  const { ui, link } = useApp();
  const ops = useOps();
  const views = [["graph", "Team", ""], ["tasks", "Tasks", "t"], ["logs", "Events", "l"], ["usage", "Usage", "v"]] as const;
  return <nav class="views" data-region="status" aria-label="views"><For each={views}>{([v, label, key]) => <button classList={{ on: ui.view() === v }} aria-pressed={ui.view() === v} onClick={() => ui.setView(v)} title={key ? `${label} (${key})` : label}>{label}</button>}</For>
    <span class="spacer" /><button class="bus-toggle" onClick={() => { ui.setBusOpen(true); ui.select(null); ui.setPanelFull(false); }}>Message bus</button><button disabled={!link.live()} onClick={() => ops.compose()}>Message <kbd aria-hidden="true">m</kbd></button>
  </nav>;
}

/** The TUI's banners: no sandbox, a socket in /tmp. Fixed text, never the raw wire value. */
function Warnings() {
  const { store } = useApp();
  const sandbox = () => {
    const m = store.state.session?.sandbox;
    if (!store.state.session || m === "required") return null;
    return m === "off" ? "NO SANDBOX · agents can access the host" : "SANDBOX UNCONFIRMED · host access may be unrestricted";
  };
  return (
    <>
      <Show when={sandbox()}><div class="warning" role="note">{sandbox()}</div></Show>
      <Show when={store.state.session?.socket_warning}><div class="warning" role="note">SOCKET IN /tmp · using a private temporary directory</div></Show>
      <Show when={store.state.session?.vcs_note}><div class="warning note" role="note">{store.state.session!.vcs_note}</div></Show>
    </>
  );
}

function UsageStrip() {
  const app = useApp();
  const ops = useOps();
  const { store, link } = app;
  const second = useSecond();
  const now = () => (second(), store.serverNow(Date.now()));
  const rows = createMemo(() => usageRows(store.state.session?.quotas ?? [], store.state.agents));
  return (
    <Show when={rows().length > 0}>
      <section class="usage-strip" data-region="usage" aria-label="usage">
        <For each={rows()}>
          {(row) => (
            <>
              <Show when={row.paused.length > 0}>
                <div class="paused-row warn">
                  <span>{pausedText(row.paused, now())}</span>
                  <button disabled={!link.live()} onClick={() => ops.resumePaused()} title="u">continue now</button>
                  <button disabled={!link.live()} onClick={() => ops.restartPaused()} title="U">restart on the current login</button>
                </div>
              </Show>
              <div class="quota-row">
                <span class={`rt rt-${row.runtime}`}>{runtimeGlyph(row.runtime)} {row.runtime}</span>
                <For each={row.quotas}>
                  {(q) => (
                    <span class="quota" classList={{ stale: now() - q.updated_at > STALE_MS }} title={`${windowLabel(q.window)}: ${q.percent_used.toFixed(1)}% used${q.resets_at ? `, resets in ${until(q.resets_at - now())}` : ""}`}>
                      <span class="muted">{windowLabel(q.window)}</span>
                      <span class="bar gauge"><span class={`fill ${levelTone(q.percent_used)}`} style={{ width: `${Math.min(100, Math.max(q.percent_used > 0 ? 1 : 0, q.percent_used))}%` }} /></span>
                      <b class="pct">{Math.round(Math.max(0, q.percent_used))}%</b>
                      <Show when={q.resets_at && q.resets_at > now()}><span class="dim">↻ {until(q.resets_at! - now())}</span></Show>
                    </span>
                  )}
                </For>
              </div>
            </>
          )}
        </For>
      </section>
    </Show>
  );
}

function QuestionBanner() {
  const app = useApp();
  const { store, ui } = app;
  const qs = () => store.openQuestions();
  return (
    <Show when={qs().length > 0}>
      <section class="question-banner" data-region="questions" role="alert">
        <span class="q-mark">?</span>
        <span class="q-text"><b>{asker(qs()[0])}</b> asks: {qs()[0].body.split("\n", 1)[0]}</span>
        <Show when={qs().length > 1}><span class="muted">and {qs().length - 1} more</span></Show>
        <button class="primary" disabled={!app.link.live()} onClick={() => ui.openDialog({ kind: "question", id: qs()[0].id })} title="a">answer</button>
      </section>
    </Show>
  );
}

const HINTS: Record<string, string> = {
  main: "arrows walk the agents · enter inspector · m message · / command · t tasks · l logs · v usage · w workspaces · + - 0 zoom · ? keys",
  panel: "esc closes · tab moves through the panel · f fills the main area · d diff · i prompt · r refresh",
  compose: "enter sends · shift-enter new line · tab completes a command · esc leaves the box",
  terminal: "keys go to the agent · ctrl-] gives the keyboard back",
  status: "tab moves on to the graph · w switches workspace · ? keys and commands",
  switcher: "arrows move · enter switches · 1–9 directly · typing filters · esc closes",
  usage: "u continue now · U restart on the current login",
  questions: "a answers · 1–6 pick a suggested answer in the dialog",
};

function Footer() {
  const app = useApp();
  const [region, setRegion] = createSignal("main");
  onMount(() => {
    const f = (e: FocusEvent) => {
      const el = e.target as Element | null;
      setRegion(el?.closest?.("[data-terminal]") ? "terminal" : el?.closest?.("[data-region]")?.getAttribute("data-region") ?? "main");
    };
    document.addEventListener("focusin", f);
    onCleanup(() => document.removeEventListener("focusin", f));
  });
  return (
    <footer class="footer" data-region="footer">
      <Show when={app.ui.status()} fallback={<span class="hints">{app.prefs.prefs.keys || region() === "compose" ? HINTS[region()] ?? HINTS.main : "single-key shortcuts are off (/keys on) · ctrl-k command line · tab moves between regions"}</span>}>
        <span class="status-line" classList={{ err: app.ui.status()!.error }} role="status">{app.ui.status()!.text}</span>
      </Show>
    </footer>
  );
}

function Toasts() {
  const { ui } = useApp();
  return (
    <div class="toasts" aria-live="assertive">
      <For each={ui.notices()}>
        {(n) => (
          <div class="toast" classList={{ err: !n.action }} role="alert">
            <span>{n.text}</span>
            <Show when={n.action}>
              <button class="act" onClick={() => (ui.dismissNotice(n.id), n.action!.run())}>{n.action!.label}</button>
            </Show>
            <button aria-label="dismiss" onClick={() => ui.dismissNotice(n.id)}>×</button>
          </div>
        )}
      </For>
    </div>
  );
}

export function Empty(props: { children: JSX.Element }) {
  return <div class="empty">{props.children}</div>;
}
