// The Playwright projects (playwright.config.ts): the live specs against a
// real daemon (e2e/live/, among them a perf.spec.ts of their own) run only in
// the `live` project, never in `mock` or `perf`.
import { matchesGlob } from "node:path";
import { expect, test } from "vitest";
import config from "../playwright.config";

const globs = (v: unknown): string[] => (v == null ? [] : Array.isArray(v) ? (v as string[]) : [v as string]);
/** Whether a project runs `file` (relative to e2e/), as Playwright matches its patterns. */
function runs(name: string, file: string): boolean {
  const p = config.projects!.find((x) => x.name === name)!;
  const base = (p.testDir ?? "").replace(/^e2e\/?/, "");
  if (base && !file.startsWith(`${base}/`)) return false;
  const rel = base ? file.slice(base.length + 1) : file;
  const hit = (g: string) => matchesGlob(rel, g) || matchesGlob(rel, `**/${g}`);
  const match = globs(p.testMatch);
  if (match.length ? !match.some(hit) : !/\.(spec|test)\.[jt]sx?$/.test(rel)) return false;
  return !globs(p.testIgnore).some(hit);
}

test("each spec runs in its own project only", () => {
  expect(runs("perf", "perf.spec.ts")).toBe(true);
  expect(runs("perf", "live/perf.spec.ts")).toBe(false);
  expect(runs("mock", "perf.spec.ts")).toBe(false);
  expect(runs("mock", "live/perf.spec.ts")).toBe(false);
  expect(runs("mock", "live/vcs.spec.ts")).toBe(false);
  expect(runs("mock", "graph.spec.ts")).toBe(true);
  expect(runs("live", "live/perf.spec.ts")).toBe(true);
  expect(runs("live", "graph.spec.ts")).toBe(false);
});
