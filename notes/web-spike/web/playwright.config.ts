import { defineConfig } from "@playwright/test";
// A host without Playwright's own browsers (NixOS) names one in the environment.
const executablePath = process.env.PLAYWRIGHT_EXECUTABLE_PATH || undefined;
export default defineConfig({ testDir: "e2e", reporter: "line", use: { browserName: "chromium", headless: true, launchOptions: { executablePath } } });
