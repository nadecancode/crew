// Fixtures of the `mock` project (notes/web-design.md §tests): a mock daemon
// per test serving the built bundle, the page's Content-Security-Policy
// violations, opening the page paired and live, and a guard on what a page
// asks of its origin once it started.
//
//   import { expect, noRequestsAfterStart, test } from "./fixtures.ts";
//   test.use({ mockOptions: { fixture: "team10" } });
//   test("…", async ({ page, mock, csp, openPage }) => {
//     const guard = noRequestsAfterStart(page, mock);
//     await openPage();
//     …
//     expect(guard.requests).toEqual([]);
//     expect(csp).toEqual([]);
//   });

import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { test as base, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { DEFAULT_DIST, startMock } from "../dev/mock.ts";
import type { Mock, MockOptions } from "../dev/mock.ts";

export { expect };
export { NO_REPLY } from "../dev/mock.ts";
export type { HttpRecord, Mock, MockCall, MockConn, MockOptions, Push, PushFrame } from "../dev/mock.ts";

type Fixtures = {
  /** The test's mock: `test.use({ mockOptions: { fixture: "subagents" } })`. */
  mockOptions: MockOptions;
  /** A mock daemon serving the built bundle, started for the test and stopped after it. */
  mock: Mock;
  /** Content-Security-Policy violations in any page of the test, as they
   * happen: console errors and `securitypolicyviolation` events. */
  csp: string[];
  /** Open the mock's pairing address (in `page` unless another is given) and
   * wait until the page is live. */
  openPage: (target?: Page) => Promise<Page>;
};

export const test = base.extend<Fixtures>({
  mockOptions: [{}, { option: true }],

  mock: async ({ mockOptions }, use) => {
    const dist = mockOptions.dist ?? DEFAULT_DIST;
    if (!existsSync(join(dist, "index.html"))) throw new Error(`no bundle in ${dist}: run \`bun run build\` first`);
    const mock = await startMock(mockOptions);
    await use(mock);
    await mock.stop();
  },

  csp: async ({ context }, use) => {
    const seen: string[] = [];
    context.on("console", (m) => {
      if (m.type() === "error" && /Content Security Policy/i.test(m.text())) seen.push(`console: ${m.text()}`);
    });
    // The event reaches the test through a binding: the page's policy allows
    // no inline script, and an init script is not subject to it.
    await context.exposeBinding("__crewCspViolation", (_source, text: string) => {
      seen.push(`event: ${text}`);
    });
    await context.addInitScript(() => {
      document.addEventListener("securitypolicyviolation", (e) => {
        const report = (window as unknown as { __crewCspViolation(text: string): void }).__crewCspViolation;
        report(`${e.effectiveDirective} blocked ${e.blockedURI || "inline"} at ${e.sourceFile || "?"}:${e.lineNumber}`);
      });
    });
    await use(seen);
  },

  openPage: async ({ page, mock }, use) => {
    await use(async (target = page) => {
      await target.goto(mock.pairUrl());
      await target.locator('[data-conn="live"]').waitFor({ state: "attached" });
      return target;
    });
  },
});

/** What `page` requests after its document started (its `load` event),
 * WebSocket upgrades aside: after the start the page asks its origin for
 * nothing (§security, "A document whose server is gone"). A navigation of the
 * page is a new document, which loads again. Call it before the page opens.
 *
 * With `mock`, the requests the mock received while a document of the page
 * was started count as well: upgrades, the document itself and the icons the
 * browser fetches on its own (after `load`, unseen by the page's request
 * events) aside. The mock cannot tell pages apart: give it only in a test
 * with one page. */
export function noRequestsAfterStart(page: Page, mock?: Mock): { readonly requests: string[] } {
  const ignored = new Set(["/", "/index.html", ...(mock ? icons(mock.dist) : [])]);
  const seen: string[] = [];
  const started: { from: number; to: number }[] = [];
  let current: { from: number; to: number } | null = null;
  page.on("request", (r) => {
    if (r.isNavigationRequest() && r.frame() === page.mainFrame()) {
      if (current) current.to = Date.now();
      current = null;
      return;
    }
    if (current) seen.push(`page: ${r.method()} ${r.url()}`);
  });
  page.on("load", () => {
    current = { from: Date.now(), to: Infinity };
    started.push(current);
  });
  return {
    get requests() {
      const received = (mock?.requests ?? [])
        .filter((q) => !q.upgrade && !ignored.has(q.path.split("?")[0]))
        .filter((q) => started.some((w) => q.time >= w.from && q.time < w.to))
        .map((q) => `mock: ${q.method} ${q.path} (${q.status})`);
      return [...seen, ...received];
    },
  };
}

/** What the browser fetches by itself for a document: `/favicon.ico`, or the
 * icons `index.html` links. */
function icons(dist: string): string[] {
  const paths = ["/favicon.ico"];
  let html = "";
  try {
    html = readFileSync(join(dist, "index.html"), "utf8");
  } catch {
    return paths;
  }
  for (const tag of html.match(/<link\b[^>]*>/gi) ?? []) {
    if (!/\brel\s*=\s*["']?[^"'>]*icon/i.test(tag)) continue;
    const href = /\bhref\s*=\s*["']?([^"'\s>]+)/i.exec(tag)?.[1];
    if (href && !href.includes(":")) paths.push(href.startsWith("/") ? href : `/${href.replace(/^\.\//, "")}`);
  }
  return paths;
}
