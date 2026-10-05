// Budgets P1–P4 (notes/web-design.md §perf) from the committed bundle's
// manifest, and the runtime dependencies.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, test } from "vitest";

const DIST = join(process.cwd(), "..", "crates", "crew", "web-dist");
const KB = 1000;

describe.runIf(existsSync(join(DIST, "manifest.json")))("bundle", () => {
  const manifest = JSON.parse(readFileSync(join(DIST, "manifest.json"), "utf8")) as { files: { path: string; bytes: number }[] };
  const sum = (f: (p: string) => boolean) => manifest.files.filter((x) => f(x.path)).reduce((n, x) => n + x.bytes, 0);
  const js = (p: string) => p.endsWith(".js");
  const terminal = (p: string) => /assets\/(Terminal|xterm)-/.test(p);

  test("P1: script for the dashboard (all but the terminal chunk) ≤ 228 KB", () => {
    expect(sum((p) => js(p) && !terminal(p))).toBeLessThanOrEqual(228 * KB);
  });
  test("P2: stylesheet ≤ 34 KB", () => {
    expect(sum((p) => p.endsWith(".css"))).toBeLessThanOrEqual(34 * KB);
  });
  test("P3: terminal chunk ≤ 350 KB", () => {
    expect(sum((p) => js(p) && terminal(p))).toBeLessThanOrEqual(350 * KB);
  });
  test("P4: the whole of web-dist ≤ 600 KB", () => {
    expect(sum(() => true)).toBeLessThanOrEqual(600 * KB);
  });
});

test("runtime dependencies are exactly the three of §build", () => {
  const pkg = JSON.parse(readFileSync(join(process.cwd(), "package.json"), "utf8"));
  expect(Object.keys(pkg.dependencies).sort()).toEqual(["@xterm/addon-fit", "@xterm/xterm", "solid-js"]);
});
