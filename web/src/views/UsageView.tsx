// The usage view (notes/web-design.md §graphs "Usage"), all from state the
// page already has: per agent its context window, tokens as one stacked bar,
// turns and compactions; per runtime the usage windows with their reset; and
// a line per agent of its context percentage since the page was opened (the
// daemon keeps no usage history). Hand-written SVG; hover gives the figures.

import { For, Show, createMemo, createSignal } from "solid-js";
import { useApp } from "../app";
import { useSecond } from "../clock";
import { clockTime, resetTime, runtimeGlyph, tokens, windowSize } from "../format";
import type { Agent } from "../protocol";
import { levelTone, until, usageRows, windowLabel, STALE_MS } from "./usage";

const SERIES = ["input", "output", "cache read", "cache written"] as const;
const total = (a: Agent) => (a.usage ? a.usage.input_tokens + a.usage.output_tokens + a.usage.cache_read_tokens + a.usage.cache_creation_tokens : 0);

export function UsageView() {
  const app = useApp();
  const { store, ui } = app;
  const second = useSecond();
  const now = () => (second(), store.serverNow(Date.now()));
  const agents = createMemo(() => [...store.state.agents].sort((a, b) => total(b) - total(a) || a.id.localeCompare(b.id)));
  const max = () => Math.max(1, ...agents().map(total));
  const rows = createMemo(() => usageRows(store.state.session?.quotas ?? [], store.state.agents));
  const [hover, setHover] = createSignal<string | null>(null);
  const open = (id: string) => {
    ui.setSelected(id);
    ui.select({ kind: "agent", id });
  };

  return (
    <div class="usage-view scroll" tabindex="0" aria-label="usage">
      <section>
        <h3>agents <span class="muted">by tokens</span></h3>
        <div class="legend">
          <For each={SERIES}>{(s, i) => <span><span class={`swatch s${i()}`} /> {s}</span>}</For>
        </div>
        <table class="usage-table">
          <thead><tr><th>agent</th><th>context</th><th>tokens</th><th class="num">turns</th><th class="num">compactions</th></tr></thead>
          <tbody>
            <For each={agents()}>
              {(a) => {
                const parts = () => (a.usage ? [a.usage.input_tokens, a.usage.output_tokens, a.usage.cache_read_tokens, a.usage.cache_creation_tokens] : []);
                return (
                  <tr classList={{ hover: hover() === a.id }} onPointerEnter={() => setHover(a.id)} onPointerLeave={() => setHover(null)}>
                    <td><button class={`link rt-${a.runtime}`} onClick={() => open(a.id)}>{runtimeGlyph(a.runtime)} {a.id}</button></td>
                    <td>
                      <Show when={a.context} fallback={<span class="dim">not reported</span>}>
                        <span class="ctx" tabindex="0" aria-label={`${a.context!.percent}% · ${a.context!.tokens} of ${a.context!.window} tokens`} title={`${a.context!.percent}% · ${tokens(a.context!.tokens)} of ${tokens(a.context!.window)}`}>
                          <svg class="gauge-svg" viewBox="0 0 100 8" preserveAspectRatio="none" aria-hidden="true">
                            <rect class="track" width="100" height="8" rx="2" />
                            <rect class={`fill ${levelTone(a.context!.percent)}`} width={a.context!.percent} height="8" rx="2" />
                          </svg>
                          {a.context!.percent}% <span class="muted">of {windowSize(a.context!.window)}</span>
                        </span>
                      </Show>
                    </td>
                    <td>
                      <Show when={a.usage} fallback={<span class="dim">not reported</span>}>
                        <span class="stack" tabindex="0" aria-label={SERIES.map((s, i) => `${s} ${parts()[i]}`).join(" · ")} title={SERIES.map((s, i) => `${s} ${tokens(parts()[i])}`).join(" · ")}>
                          <svg class="stack-svg" viewBox="0 0 200 10" preserveAspectRatio="none" aria-hidden="true">
                            {(() => {
                              let x = 0;
                              return parts().map((v, i) => {
                                const w = (v / max()) * 200;
                                const r = <rect class={`s${i}`} x={x} width={w} height="10" />;
                                x += w;
                                return r;
                              });
                            })()}
                          </svg>
                          {tokens(total(a))}
                        </span>
                      </Show>
                    </td>
                    <td class="num">{a.usage?.turns ?? "—"}</td>
                    <td class="num">{a.compactions}{a.compact_pending ? " (queued)" : ""}</td>
                  </tr>
                );
              }}
            </For>
          </tbody>
        </table>
      </section>

      <Show when={rows().length > 0}>
        <section>
          <h3>usage windows</h3>
          <For each={rows()}>
            {(row) => (
              <div class="window-row">
                <span class={`rt rt-${row.runtime}`}>{runtimeGlyph(row.runtime)} {row.runtime}</span>
                <For each={row.quotas}>
                  {(q) => (
                    <div class="window" classList={{ stale: now() - q.updated_at > STALE_MS }} title={`reported ${clockTime(q.updated_at)}`}>
                      <div class="muted">{windowLabel(q.window)}</div>
                      <svg class="window-svg" viewBox="0 0 100 12" preserveAspectRatio="none" aria-hidden="true">
                        <rect class="track" width="100" height="12" rx="3" />
                        <rect class={`fill ${levelTone(q.percent_used)}`} width={Math.min(100, Math.max(0, q.percent_used))} height="12" rx="3" />
                      </svg>
                      <div><b>{Math.round(Math.max(0, q.percent_used))}%</b> used
                        <Show when={q.resets_at}> · resets {resetTime(q.resets_at!, now())}<Show when={q.resets_at! > now()}> (in {until(q.resets_at! - now())})</Show></Show>
                      </div>
                    </div>
                  )}
                </For>
                <Show when={row.paused.length}><div class="warn">paused at a limit: {row.paused.map((a) => a.id).join(", ")}</div></Show>
              </div>
            )}
          </For>
        </section>
      </Show>

      <section>
        <h3>context since this page was opened</h3>
        <p class="muted">The daemon keeps no usage history: these lines start when the page did, and start again after a reload.</p>
        <ContextLines hover={hover} setHover={setHover} />
      </section>
    </div>
  );
}

const W = 640, H = 180, PAD = 28;

function ContextLines(props: { hover: () => string | null; setHover(id: string | null): void }) {
  const app = useApp();
  const { store } = app;
  const series = createMemo(() => {
    store.usageVersion();
    return [...store.usage.entries()].filter(([, ring]) => ring.length > 0).map(([id, ring]) => ({ id, points: ring.toArray() }));
  });
  const span = createMemo(() => {
    let t0 = Infinity, t1 = -Infinity;
    for (const s of series()) for (const p of s.points) (t0 = Math.min(t0, p.t)), (t1 = Math.max(t1, p.t));
    if (!Number.isFinite(t0)) return null;
    return { t0, t1: Math.max(t1, t0 + 60_000) };
  });
  const x = (t: number) => PAD + ((t - span()!.t0) / (span()!.t1 - span()!.t0)) * (W - 2 * PAD);
  const y = (p: number) => H - PAD + 8 - (p / 100) * (H - 2 * PAD);
  const path = (points: { t: number; percent: number }[]) =>
    points.map((p, i) => `${i ? "L" : "M"}${x(p.t).toFixed(1)} ${y(p.percent).toFixed(1)}`).join("");
  const runtimeOf = (id: string) => store.agent(id)?.runtime ?? "shell";
  return (
    <Show when={span()} fallback={<p class="dim">no agent has reported its context window yet</p>}>
      <svg class="lines-svg" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="context window use per agent over time">
        <For each={[0, 50, 100]}>{(p) => <><line class="grid" x1={PAD} x2={W - PAD} y1={y(p)} y2={y(p)} /><text class="axis" x={4} y={y(p) + 4}>{p}%</text></>}</For>
        <text class="axis" x={PAD} y={H - 4}>{clockTime(span()!.t0)}</text>
        <text class="axis" x={W - PAD} y={H - 4} text-anchor="end">{clockTime(span()!.t1)}</text>
        <For each={series()}>
          {(s) => (
            <g class={`rt-${runtimeOf(s.id)}`}>
            <Show when={s.points.length === 1}>
              <circle cx={x(s.points[0].t)} cy={y(s.points[0].percent)} r="3" fill="currentColor" />
            </Show>
            <path class="ctx-line" classList={{ hover: props.hover() === s.id, dim: !!props.hover() && props.hover() !== s.id }} d={path(s.points)}
              onPointerEnter={() => props.setHover(s.id)} onPointerLeave={() => props.setHover(null)}>
              <title>{s.id}: {s.points[s.points.length - 1].percent}% now</title>
            </path>
            </g>
          )}
        </For>
      </svg>
    </Show>
  );
}
