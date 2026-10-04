// No markup is ever made from strings (notes/web-design.md §security "Agent
// text in the page", rule 1; the policy refuses string styles).
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "vitest";

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((n) => (statSync(join(dir, n)).isDirectory() ? files(join(dir, n)) : [join(dir, n)]));
}

test("no file under src/ has an HTML sink, eval, or a string style", () => {
  for (const path of files(join(process.cwd(), "src"))) {
    const text = readFileSync(path, "utf8");
    for (const sink of [/innerHTML/, /outerHTML/, /insertAdjacentHTML/, /document\.write/, /\beval\s*\(/, /new Function/, /setTimeout\(\s*["'`]/, /setInterval\(\s*["'`]/]) expect(text, `${path}: ${sink}`).not.toMatch(sink);
    // `style={{…}}` is set through the CSSOM; a string style would be blocked.
    expect(text, `${path}: style string`).not.toMatch(/\bstyle=["'`]/);
    expect(text, `${path}: style string`).not.toMatch(/\bstyle=\{\s*["'`]/);
  }
});
