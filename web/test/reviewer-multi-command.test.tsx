// The copy button must produce a command for the exact workspace path.
import { spawnSync } from "node:child_process";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { afterEach, beforeEach, expect, test } from "vitest";
import { AppContext, createPage, type Page } from "../src/app";
import { Session } from "../src/net/session";
import { SwitcherButton } from "../src/views/Switcher";
import { FakeSocket, fakeEnv, settle } from "./fakes";
import { A, B, entry, indexSocket, listing, serveIndex, stored } from "./multi";

let page: Page | undefined;
beforeEach(() => { FakeSocket.all = []; });
afterEach(() => {
  cleanup();
  page?.app().dispose();
  page?.hub.stop("gone");
});

for (const workspace of ["/srv/crew work/billing", "/srv/crew's work/billing", "/home/another-operator/project"]) {
  test(`the join command preserves ${workspace}`, async () => {
    page = createPage(fakeEnv(), new Session(stored(), null), new Promise(() => {}), {
      loc: { hash: `#/w/${A}/` },
      hist: { pushState() {}, replaceState() {} },
    });
    page.start();
    await serveIndex(indexSocket(), listing(1, [entry(A)], [{ id: B, project: "billing", workspace }]), settle);
    const view = render(() => <AppContext.Provider value={page!.app()}><SwitcherButton /></AppContext.Provider>);
    fireEvent.click(view.container.querySelector("[data-switcher-button]")!);
    const command = view.container.querySelector(".ws-cmd")!.textContent!;
    // A harmless shell function prints its arguments; no crew command runs.
    const parsed = spawnSync("/bin/sh", ["-c", 'crew() { printf "%s\\000" "$@"; }\n' + command], { encoding: "utf8" });
    expect(parsed.status, parsed.stderr).toBe(0);
    expect(parsed.stdout.split("\0").slice(0, -1)).toEqual(["web", "-C", workspace]);
  });
}
