import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
import { defineConfig } from "../../../web/node_modules/vitest/dist/config.js";
import solid from "../../../web/node_modules/vite-plugin-solid/dist/esm/index.mjs";

const root = fileURLToPath(new URL("../../../", import.meta.url));

export default defineConfig({
  root: root + "web",
  ...(process.env.CREW_NATIVE_FIXTURE_CACHE
    ? { cacheDir: resolve(root, process.env.CREW_NATIVE_FIXTURE_CACHE, "state/vite") }
    : {}),
  plugins: [solid()],
  test: {
    environment: "jsdom",
    include: ["../fixtures/native/state/check-web.test.ts"],
  },
  resolve: {
    conditions: ["development", "browser"],
    alias: { vitest: root + "web/node_modules/vitest/dist/index.js" },
  },
});
