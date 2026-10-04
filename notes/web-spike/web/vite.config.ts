import { defineConfig } from "vite";
import solid from "vite-plugin-solid";
export default defineConfig({
  plugins: [solid()],
  build: { target: "es2022", modulePreload: { polyfill: false }, cssCodeSplit: false, assetsInlineLimit: 0, reportCompressedSize: true },
});
