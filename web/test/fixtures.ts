// The wire fixtures the Rust side generates (crates/crew/tests/fixtures/web/,
// notes/web-design.md §tests). Read, never written.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// vitest runs with web/ as the working directory.
export const FIXTURES = resolve(process.cwd(), "../crates/crew/tests/fixtures/web") + "/";

export function fixture<T = any>(name: string): T {
  return JSON.parse(readFileSync(FIXTURES + name, "utf8"));
}

export function fixtureLines(name: string): any[] {
  return readFileSync(FIXTURES + name, "utf8").split("\n").filter((l) => l.trim()).map((l) => JSON.parse(l));
}

export function fixtureText(name: string): string {
  return readFileSync(FIXTURES + name, "utf8");
}
