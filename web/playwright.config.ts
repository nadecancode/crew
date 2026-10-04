import { defineConfig } from "@playwright/test";

// Browser tests (notes/web-design.md §tests). `mock`: the built bundle against
// dev/mock.ts, which each spec starts itself (e2e/fixtures.ts), so there is no
// webServer here. `live`: a real daemon, driven by scripts/web-e2e.
//
// Playwright's own Chromium does not start on this host (NixOS);
// PLAYWRIGHT_EXECUTABLE_PATH names a Chromium-based browser that does.
const executablePath = process.env.PLAYWRIGHT_EXECUTABLE_PATH || undefined;

export default defineConfig({
  reporter: "line",
  use: {
    browserName: "chromium",
    headless: true,
    launchOptions: { executablePath },
    trace: "retain-on-failure",
  },
  projects: [
    { name: "mock", testDir: "e2e", testIgnore: "live/**" },
    { name: "live", testDir: "e2e/live" },
  ],
});
