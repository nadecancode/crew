import { defineConfig } from "vitest/config";
import solid from "vite-plugin-solid";
// `include` keeps vitest away from the Playwright specs in e2e/.
export default defineConfig({ plugins: [solid()], test: { environment: "jsdom", include: ["test/**/*.test.{ts,tsx}"] }, resolve: { conditions: ["development", "browser"] } });
