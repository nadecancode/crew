// The workspace switcher (notes/web-multi.md §multi-ui "The switcher"): the
// project name at the left of the status bar is a button, with the other
// workspaces' badges beside it, `? n` (questions waiting) and `! n` (agents
// needing attention). It opens a list:
//
//   ● payments   ~/work/payments   6 agents · 3 working   tasks 4/6         1
//   ○ billing    ~/work/billing    stopped                                   2
//   ─ running, not in this dashboard ─
//     docs       ~/work/docs       crew web -C ~/work/docs              [copy]
//
// A listbox: one tab stop, arrows move, enter switches, esc closes and gives
// focus back to the button, typing filters by project and path, 1–9 switch
// directly. The rows hold counts only: no text of one workspace is shown in
// another.

import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { otherCounts, shownName, useApp, workspaceName } from "../app";
import type { OtherSession, WorkspaceEntry } from "../protocol";

/** A directory as the operator reads it: their home as `~`. */
export function homeDir(path: string): string {
  return path.replace(/^(\/home\/[^/]+|\/Users\/[^/]+|\/root)(?=\/|$)/, "~");
}

/** One word for a POSIX shell: as it is when nothing in it is special, else
 * single-quoted. */
export function shellQuote(word: string): string {
  return /^[A-Za-z0-9_\/.,:@%+=-]+$/.test(word) ? word : `'${word.replace(/'/g, `'\\''`)}'`;
}

/** The command that adds a running session to the dashboard: its exact
 * directory, quoted, never abbreviated (`~` would be the reader's home). */
export function addCommand(workspace: string): string {
  return `crew web -C ${shellQuote(workspace)}`;
}

const DOT: Record<WorkspaceEntry["state"], string> = { running: "●", stopped: "○", unreachable: "○", incompatible: "⚠" };

/** What a row says of a workspace besides its name and directory. */
function rowState(e: WorkspaceEntry): string {
  if (e.state === "stopped") return "stopped";
  if (e.state === "incompatible") return e.detail || "started by another version of crew: restart the team";
  if (e.state === "unreachable") return e.detail || "not reachable";
  const s = e.summary;
  if (!s) return "running";
  return `${s.agents} agent${s.agents === 1 ? "" : "s"} · ${s.working} working`;
}

/** The status bar's button and the others' badges. */
export function SwitcherButton() {
  const app = useApp();
  const { page } = app;
  const name = () => shownName(app) ?? (app.link.id === null ? "no workspace" : "not in this dashboard");
  const questions = () => otherCounts(page).questions;
  const attention = () => otherCounts(page).attention;
  let button!: HTMLButtonElement;
  return (
    <span class="switcher">
      <button
        ref={button}
        class="project"
        data-switcher-button
        aria-haspopup="listbox"
        aria-expanded={page.switcher()}
        title={`${app.store.state.session?.workspace ?? page.hub.entry(app.link.id)?.workspace ?? ""} · switch workspace (w)`}
        onClick={() => page.setSwitcher(!page.switcher())}
      >
        {name()} ▾
      </button>
      <Show when={questions() > 0}><span class="badge q" title="questions waiting in other workspaces">? {questions()}</span></Show>
      <Show when={attention() > 0}><span class="badge att" title="agents needing attention in other workspaces">! {attention()}</span></Show>
      <Show when={page.switcher()}>
        <SwitcherList close={(refocus) => {
          page.setSwitcher(false);
          if (refocus) queueMicrotask(() => document.querySelector<HTMLElement>("[data-switcher-button]")?.focus());
        }} />
      </Show>
    </span>
  );
}

type Row = { kind: "joined"; entry: WorkspaceEntry; n: number } | { kind: "other"; other: OtherSession };

function SwitcherList(props: { close(refocus: boolean): void }) {
  const app = useApp();
  const { page } = app;
  const [filter, setFilter] = createSignal("");
  const [active, setActive] = createSignal(0);
  let list!: HTMLDivElement;

  const matches = (project: string, dir: string) => {
    const f = filter().toLowerCase();
    return !f || project.toLowerCase().includes(f) || dir.toLowerCase().includes(f) || homeDir(dir).toLowerCase().includes(f);
  };
  const joined = createMemo(() =>
    (page.hub.listing()?.joined ?? []).map((entry, i) => ({ kind: "joined" as const, entry, n: i + 1 })).filter((r) => matches(workspaceName(r.entry), r.entry.workspace)),
  );
  const others = createMemo(() =>
    (page.hub.listing()?.others ?? []).map((other) => ({ kind: "other" as const, other })).filter((r) => matches(r.other.project ?? "", r.other.workspace)),
  );
  /** Only joined rows can be chosen; arrows move over them. */
  const choosable = () => joined();
  const current = () => app.link.id;

  onMount(() => {
    const i = joined().findIndex((r) => r.entry.id === current());
    setActive(Math.max(0, i));
    list.focus();
    // A click anywhere else closes it.
    const outside = (e: MouseEvent) => {
      if (!(e.target as Element | null)?.closest?.(".switcher")) props.close(false);
    };
    document.addEventListener("mousedown", outside);
    onCleanup(() => document.removeEventListener("mousedown", outside));
  });

  function choose(entry: WorkspaceEntry): void {
    props.close(false);
    if (entry.id !== current()) page.switchTo(entry.id, { push: true });
    queueMicrotask(() => document.querySelector<HTMLElement>("[data-switcher-button]")?.focus());
  }

  function onKey(e: KeyboardEvent): void {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const rows = choosable();
    let handled = true;
    if (e.key === "Escape") props.close(true);
    else if (e.key === "ArrowDown") setActive((i) => Math.min(rows.length - 1, i + 1));
    else if (e.key === "ArrowUp") setActive((i) => Math.max(0, i - 1));
    else if (e.key === "Home") setActive(0);
    else if (e.key === "End") setActive(Math.max(0, rows.length - 1));
    else if (e.key === "Enter") {
      const r = rows[active()];
      if (r) choose(r.entry);
    } else if (/^[1-9]$/.test(e.key)) {
      const r = (page.hub.listing()?.joined ?? [])[Number(e.key) - 1];
      if (r) choose(r);
    } else if (e.key === "Backspace") {
      setFilter((f) => f.slice(0, -1));
      setActive(0);
    } else if (e.key.length === 1 && e.key !== " ") {
      setFilter((f) => f + e.key);
      setActive(0);
    } else if (e.key === "Tab") {
      props.close(false);
      handled = false;
    } else handled = false;
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  }

  const rowId = (r: Row) => `ws-${r.kind === "joined" ? r.entry.id : `other-${r.other.id}`}`;
  const activeId = () => {
    const r = choosable()[active()];
    return r ? rowId(r) : undefined;
  };

  return (
    <div class="switcher-pop" data-region="switcher">
      <Show when={filter()}><div class="switcher-filter muted">filter: <b>{filter()}</b></div></Show>
      <div ref={list} class="switcher-list" role="listbox" tabindex="0" aria-label="workspaces" aria-activedescendant={activeId()} onKeyDown={onKey}>
        <For each={joined()} fallback={<div class="switcher-none muted">{filter() ? "no workspace matches" : "No workspace is shown here. Run `crew web` in a workspace."}</div>}>
          {(r, i) => (
            <div
              id={rowId(r)}
              role="option"
              class="switcher-row"
              data-workspace={r.entry.id}
              data-state={r.entry.state}
              aria-selected={i() === active()}
              aria-current={r.entry.id === current() ? "true" : undefined}
              classList={{ active: i() === active(), dimmed: r.entry.state !== "running" }}
              onMouseEnter={() => setActive(i())}
              onClick={() => choose(r.entry)}
            >
              <span class={`ws-dot ws-${r.entry.state}`}>{DOT[r.entry.state]}</span>
              <span class="ws-name">{workspaceName(r.entry)}</span>
              <span class="ws-dir mono" title={r.entry.workspace}>{homeDir(r.entry.workspace)}</span>
              <span class="ws-state" title={rowState(r.entry)}>{rowState(r.entry)}</span>
              <span class="ws-tasks mono">
                <Show when={r.entry.state === "running" && r.entry.summary}>tasks {r.entry.summary!.tasks_done}/{r.entry.summary!.tasks}</Show>
              </span>
              <span class="ws-badges">
                <Show when={(r.entry.summary?.questions ?? 0) > 0}><span class="badge q">? {r.entry.summary!.questions}</span></Show>
                <Show when={(r.entry.summary?.attention ?? 0) > 0}><span class="badge att">! {r.entry.summary!.attention}</span></Show>
              </span>
              <kbd class="ws-key">{r.n <= 9 ? r.n : ""}</kbd>
            </div>
          )}
        </For>
      </div>
      <Show when={others().length > 0}>
        <div class="switcher-sep muted">─ running, not in this dashboard ─</div>
        <For each={others()}>
          {(r) => <OtherRow other={r.other} />}
        </For>
      </Show>
    </div>
  );
}

function OtherRow(props: { other: OtherSession }) {
  // Shown short, copied exact.
  const shown = () => `crew web -C ${homeDir(props.other.workspace)}`;
  const command = () => addCommand(props.other.workspace);
  const [copied, setCopied] = createSignal(false);
  return (
    <div class="switcher-row other" data-other={props.other.id}>
      <span class="ws-dot" />
      <span class="ws-name">{props.other.project ?? props.other.workspace.split("/").filter(Boolean).pop() ?? props.other.id}</span>
      <span class="ws-dir mono">{homeDir(props.other.workspace)}</span>
      <code class="ws-cmd" title={command()}>{shown()}</code>
      <button
        onClick={() => {
          void navigator.clipboard?.writeText(command()).then(() => setCopied(true), () => {});
        }}
        title="copy the command that adds it"
      >
        {copied() ? "copied" : "copy"}
      </button>
    </div>
  );
}
