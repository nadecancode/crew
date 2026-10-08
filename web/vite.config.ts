import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// The bundle is committed and embedded in the crew binary (notes/web-design.md
// §build): reproducible names, no inlined assets, vendor code apart from the
// app so that an ordinary change rewrites one chunk. terser (pinned) minifies:
// smaller than esbuild's output, which keeps the bundle within its budgets
// (§Budgets). Only its safe transforms: `pure_getters` and the `unsafe_*`
// options would drop reads Solid's stores track.
//
// `bun run dev` proxies /ws to the mock daemon (`bun run mock`), or to a real
// daemon when CREW_WEB is the address `crew web --no-open` printed; the proxy
// then presents that address as Host and Origin.
const target = process.env.CREW_WEB?.replace(/\/$/, "");
export default defineConfig({
  plugins: [solid()],
  build: {
    outDir: "../crates/crew/web-dist",
    emptyOutDir: true,
    target: "es2022",
    minify: "terser",
    terserOptions: { compress: { passes: 3 } },
    sourcemap: false,
    cssCodeSplit: false,
    assetsInlineLimit: 0,
    modulePreload: { polyfill: false },
    reportCompressedSize: false,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("node_modules/@xterm")) return "xterm";
          if (id.includes("node_modules/solid-js")) return "solid";
          return undefined;
        },
      },
    },
  },
  server: {
    proxy: {
      "/ws": target
        ? {
            target: target.replace(/^http/, "ws"),
            ws: true,
            changeOrigin: true,
            headers: { Origin: target },
          }
        : { target: "ws://127.0.0.1:47700", ws: true },
    },
  },
});
