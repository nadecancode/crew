#!/usr/bin/env node
// Retrospective keep-alive simulation over the cache baseline timeline
// (notes/prompt-cache/baseline.timeline.json.gz). For each policy it prices,
// per runtime and model, what keep-alive turns would have cost and which
// expired-cache rewrites they would have avoided. Token volumes come from the
// timeline; prices from prices.tsv beside this script. Nothing is read from
// prompts; the output is aggregate numbers only.
//
// Usage: node simulate.mjs [timeline.json.gz] > results.json

import { readFileSync } from "node:fs";
import { gunzipSync } from "node:zlib";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const timelinePath = process.argv[2] ?? join(here, "..", "baseline.timeline.json.gz");
const timeline = JSON.parse(gunzipSync(readFileSync(timelinePath)).toString("utf8"));

// model -> { input, write1h, read, output } in USD per million tokens.
const prices = {};
for (const line of readFileSync(join(here, "prices.tsv"), "utf8").trim().split("\n").slice(1)) {
  const [model, input, write1h, read, output] = line.split("\t");
  prices[model] = { input: +input, write1h: +write1h, read: +read, output: +output };
}
const priceOf = (model) => {
  const key = Object.keys(prices).find((k) => model === k || model.startsWith(k));
  if (!key) throw new Error(`no price for ${model}`);
  return prices[key];
};

const MIN = 60_000;
// What one keep-alive turn adds: new input (crew's line, the CLI's own
// attachments, the reply) and output (the reply, any thinking). Low and high.
const KEEPALIVE = {
  low: { newInput: 200, output: 20 },
  high: { newInput: 2_000, output: 400 },
};
// New input the first request after a gap has anyway (the new message): not
// avoidable by any keep-alive. Low and high.
const NEW_TAIL = { low: 0, high: 10_000 };

// Retention assumed per runtime, and when the policy would fire: the first
// keep-alive `start` after the member's last request, then every `every`,
// at most `cap` times. Claude Code's one-hour bucket is measured; Codex's
// retention is not, so two assumptions bracket it.
const RUNTIMES = {
  claude: [{ label: "1h TTL", ttl: 60 * MIN, start: 55 * MIN, every: 55 * MIN }],
  codex: [
    { label: "30 min retention (assumed)", ttl: 30 * MIN, start: 25 * MIN, every: 25 * MIN },
    { label: "10 min retention (assumed)", ttl: 10 * MIN, start: 8 * MIN, every: 8 * MIN },
  ],
};
// A gap that ends with one of these crew events had a member that could not
// take a keep-alive (paused, out of usage, not answering) or a session the
// cache would not have served anyway (restarted): no keep-alive, no saving.
const INELIGIBLE = new Set(["agent.limit_lifted", "agent.unresponsive", "agent.paused", "agent.resumed", "agent.restart", "agent.respawned"]);
const ineligible = (r) => (r?.crew_events ?? []).some((e) => INELIGIBLE.has(e.type));

const CAPS = [1, 2, 3, 4, 6, 8, 12, 24];
const MIN_CONTEXT = [0, 50_000, 150_000];

// Each idle stretch: the member's request before it, and the request after it
// in the same session (null at the session's end: the stretch then runs to
// the member's next session, or to the end of the window).
function stretches() {
  const out = [];
  const byAgent = new Map();
  for (const s of timeline.sessions) {
    if (!s.requests?.length) continue;
    const key = s.agent_id ?? `session:${s.session_id}`;
    if (!byAgent.has(key)) byAgent.set(key, []);
    byAgent.get(key).push(s);
  }
  const windowEnd = timeline.window?.until ?? Math.max(...timeline.sessions.flatMap((s) => s.requests.map((r) => r.timestamp_ms)));
  for (const sessions of byAgent.values()) {
    sessions.sort((a, b) => a.requests[0].timestamp_ms - b.requests[0].timestamp_ms);
    sessions.forEach((s, i) => {
      const rs = s.requests;
      for (let j = 1; j < rs.length; j++) {
        // Codex records carry a turn id instead of a first-in-turn flag.
        const after = { ...rs[j], first_request_in_turn: rs[j].first_request_in_turn ?? (rs[j].turn_id !== rs[j - 1].turn_id) };
        out.push({ runtime: s.runtime, before: rs[j - 1], after, gap: rs[j].timestamp_ms - rs[j - 1].timestamp_ms });
      }
      const last = rs[rs.length - 1];
      const next = sessions[i + 1]?.requests[0].timestamp_ms ?? windowEnd;
      out.push({ runtime: s.runtime, before: last, after: null, gap: Math.max(0, next - last.timestamp_ms) });
    });
  }
  return out;
}

// What a first request after a gap would have saved had the cache been warm:
// its rewritten (Claude) or uncached (Codex) tokens beyond the new tail,
// priced at the difference between writing (or uncached input) and reading.
function avoidable(runtime, r, tail) {
  const p = priceOf(r.model);
  if (runtime === "claude") {
    const written = r.cache_creation_tokens ?? 0;
    const tokens = Math.max(0, written - tail);
    return { tokens, usd: (tokens * (p.write1h - p.read)) / 1e6 };
  }
  const tokens = Math.max(0, (r.uncached_input_tokens ?? 0) - tail);
  return { tokens, usd: (tokens * (p.input - p.read)) / 1e6 };
}

// One keep-alive turn on a context of `context` tokens.
function keepaliveCost(runtime, model, context, k) {
  const p = priceOf(model);
  const write = runtime === "claude" ? p.write1h : p.input;
  return (context * p.read + k.newInput * write + k.output * p.output) / 1e6;
}

// `bound` "conservative" pairs costly keep-alives with a large new tail (the
// least saving); "optimistic" pairs cheap ones with none.
function simulate(runtime, retention, cap, minContext, bound) {
  const k = bound === "conservative" ? KEEPALIVE.high : KEEPALIVE.low;
  const tail = bound === "conservative" ? NEW_TAIL.high : NEW_TAIL.low;
  const r = { keepalives: 0, keepaliveUsd: 0, covered: 0, coveredTokens: 0, savedUsd: 0, missedAfterCap: 0, wastedKeepalives: 0, wastedUsd: 0, ineligible: 0 };
  for (const s of stretches()) {
    if (s.runtime !== runtime) continue;
    const context = s.before.input_total_tokens + (s.before.output_tokens ?? 0);
    if (context < minContext) continue;
    // Only a member between turns gets a keep-alive: a gap inside a turn (a
    // long tool call) cannot be filled, and the turn ends with the request.
    if (s.after && !s.after.first_request_in_turn) continue;
    if (s.gap <= retention.start) continue;
    if (ineligible(s.after)) {
      r.ineligible += 1;
      continue;
    }
    const fired = Math.min(cap, Math.floor((s.gap - retention.start) / retention.every) + 1);
    let cost = 0;
    for (let i = 0; i < fired; i++) cost += keepaliveCost(runtime, s.before.model, context + i * k.newInput, k);
    r.keepalives += fired;
    r.keepaliveUsd += cost;
    const lastAt = retention.start + (fired - 1) * retention.every;
    const coveredByPolicy = s.gap <= lastAt + retention.ttl;
    if (!s.after) {
      r.wastedKeepalives += fired;
      r.wastedUsd += cost;
      continue;
    }
    if (s.gap <= retention.ttl) {
      // The cache would have lived anyway: the keep-alive bought nothing.
      r.wastedKeepalives += fired;
      r.wastedUsd += cost;
      continue;
    }
    if (!coveredByPolicy) {
      r.missedAfterCap += 1;
      r.wastedKeepalives += fired;
      r.wastedUsd += cost;
      continue;
    }
    const a = avoidable(runtime, s.after, tail);
    r.covered += 1;
    r.coveredTokens += a.tokens;
    r.savedUsd += a.usd;
  }
  r.netUsd = r.savedUsd - r.keepaliveUsd;
  return r;
}

const results = [];
for (const [runtime, retentions] of Object.entries(RUNTIMES)) {
  for (const retention of retentions) {
    for (const cap of CAPS) {
      for (const minContext of MIN_CONTEXT) {
        const conservative = simulate(runtime, retention, cap, minContext, "conservative");
        const optimistic = simulate(runtime, retention, cap, minContext, "optimistic");
        results.push({ runtime, retention: retention.label, cap, minContext, conservative, optimistic });
      }
    }
  }
}

// Exposure without any keep-alive, per runtime and model: first requests in a
// turn after a gap beyond the assumed retention.
const exposure = {};
for (const s of stretches()) {
  if (!s.after || !s.after.first_request_in_turn) continue;
  for (const retention of RUNTIMES[s.runtime] ?? []) {
    if (s.gap <= retention.ttl) continue;
    const key = `${s.runtime}|${retention.label}|${s.after.model}`;
    const e = (exposure[key] ??= { events: 0, rewriteTokens: 0, rewriteUsdAtWrite: 0, gapsSeconds: [] });
    const a = avoidable(s.runtime, s.after, 0);
    e.events += 1;
    e.rewriteTokens += a.tokens;
    e.rewriteUsdAtWrite += a.usd;
    e.gapsSeconds.push(Math.round(s.gap / 1000));
  }
}
for (const e of Object.values(exposure)) {
  const g = e.gapsSeconds.sort((a, b) => a - b);
  e.gapSecondsMedian = g[Math.floor(g.length / 2)];
  e.gapSecondsP90 = g[Math.floor(g.length * 0.9)];
  e.gapSecondsMax = g[g.length - 1];
  delete e.gapsSeconds;
}

console.log(JSON.stringify({
  source: { timeline: "notes/prompt-cache/baseline.timeline.json.gz", generated_at: timeline.generated_at, window: timeline.window },
  assumptions: { keepalive: KEEPALIVE, newTail: NEW_TAIL, runtimes: RUNTIMES, prices },
  exposure,
  results,
}, null, 2));
