// Writes crates/crew/web-dist/manifest.json, the last step of `bun run build`
// (notes/web-design.md §build "manifest.json"): every file of the bundle with
// its size and FNV-1a 64 hash, and `source`, a fingerprint of the inputs of
// the build, which `crates/crew/tests/web_bundle.rs` recomputes to fail
// `cargo test` when the bundle is older than the sources. A fingerprint
// against forgetfulness, not a security boundary.

import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { join, relative, sep } from "node:path";

const web = join(import.meta.dirname, "..");
const dist = join(web, "..", "crates", "crew", "web-dist");

const PRIME = 0x100000001b3n;
const MASK = 0xffffffffffffffffn;

/** FNV-1a 64, as `crew_core::util::fnv1a64`, fed in pieces. */
class Fnv {
  h = 0xcbf29ce484222325n;
  update(bytes: Uint8Array): this {
    let h = this.h;
    for (const b of bytes) h = ((h ^ BigInt(b)) * PRIME) & MASK;
    this.h = h;
    return this;
  }
  hex(): string {
    return `fnv1a64:${this.h.toString(16).padStart(16, "0")}`;
  }
}

/** Every file under `dir`, as paths relative to `base` with `/`, sorted bytewise. */
function walk(dir: string, base: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) out.push(...walk(p, base));
    else out.push(relative(base, p).split(sep).join("/"));
  }
  return out.sort((a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b)));
}

const enc = new TextEncoder();
const source = new Fnv();
for (const path of ["index.html", "package.json", "bun.lock", "tsconfig.json", "vite.config.ts", ...walk(join(web, "src"), web)]) {
  const bytes = readFileSync(join(web, path));
  source.update(enc.encode(path)).update(new Uint8Array([0])).update(enc.encode(String(bytes.length))).update(new Uint8Array([0])).update(bytes);
}

const files = walk(dist, dist)
  .filter((path) => path !== "manifest.json")
  .map((path) => {
    const bytes = readFileSync(join(dist, path));
    return { path, bytes: bytes.length, hash: new Fnv().update(bytes).hex() };
  });

const manifest = { version: 1, source: source.hex(), files };
writeFileSync(join(dist, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
const total = files.reduce((n, f) => n + f.bytes, 0);
console.log(`manifest: ${files.length} files, ${total} bytes, source ${manifest.source}`);
