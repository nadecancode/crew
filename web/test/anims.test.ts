// Every case of anim.json, generated from crew_tui::anim::Anims: the web
// animates what the TUI animates (notes/web-design.md W11).
import { describe, expect, test } from "vitest";
import * as A from "../src/anim/anims";
import type { Message, VcsEvent } from "../src/protocol";
import { fixture } from "./fixtures";

type Step = { at: number; message?: Message; vcs?: VcsEvent; snapshot?: { vcs: VcsEvent; age: number }[]; remove?: string };
type Check = { at: number; links: any[]; marks: any[]; shown: Record<string, string>; animating: boolean; running: boolean; next_expiry: number | null };
type Case = { name: string; workers: string[]; steps: Step[]; checks: Check[] };

const anim = fixture<{ constants: Record<string, number>; cases: Case[] }>("anim.json");
const r3 = (x: number | null) => (x === null ? null : Math.round(x * 1000) / 1000);
const vcs = (v: VcsEvent): VcsEvent => ({ files: [], ...v });

test("the constants are the TUI's", () => {
  for (const [name, value] of Object.entries(anim.constants)) expect((A as any)[name], name).toBe(value);
});

describe("anim.json", () => {
  for (const c of anim.cases) {
    test(c.name, () => {
      const a = new A.Anims();
      const times = [...new Set([...c.steps.map((s) => s.at), ...c.checks.map((k) => k.at)])].sort((x, y) => x - y);
      for (const at of times) {
        for (const s of c.steps.filter((s) => s.at === at)) {
          if (s.message) a.onMessage(s.message, c.workers, at);
          else if (s.vcs) a.onVcs(vcs(s.vcs), at);
          else if (s.snapshot) a.resume(s.snapshot.map((e) => [vcs(e.vcs), e.age] as const), at);
          else if (s.remove !== undefined) a.removeAgent(s.remove);
        }
        for (const k of c.checks.filter((k) => k.at === at)) {
          a.gc(at);
          const links = a.links.map((l) => ({ from: l.from, to: l.to, kind: l.kind, tone: l.tone, held: l.held !== null, pulse: r3(A.pulse(l, at)), live: A.linkLive(l, at) }));
          const marks = a.marks.map((m) => ({ agent: m.agent, kind: m.kind, tone: m.tone, label: m.label, started: m.started, live: A.markLive(m, at), sweep: r3(A.sweep(m, at)) }));
          const boxes = [...c.workers, "orchestrator", ...a.marks.map((m) => m.agent)];
          const shown: Record<string, string> = {};
          for (const id of boxes) {
            const m = a.markFor(id, at);
            if (m) shown[id] = m.label;
          }
          const sortKeys = (o: any) => Object.fromEntries(Object.entries(o).sort());
          expect({ links, marks: marks.map(sortKeys), shown, animating: a.animating(at), running: a.running(), next_expiry: a.nextExpiry(at) }, `${c.name} @${at}`).toEqual({
            links: k.links.map((l) => ({ from: l.from, to: l.to, kind: l.kind, tone: l.tone, held: l.held, pulse: l.pulse, live: l.live })),
            marks: k.marks.map(sortKeys),
            shown: k.shown,
            animating: k.animating,
            running: k.running,
            next_expiry: k.next_expiry,
          });
        }
      }
    });
  }
});
