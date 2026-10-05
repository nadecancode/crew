// @vitest-environment node
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { expect, test } from "vitest";

const root = resolve(process.cwd(), "..");
function capture(out: string) {
  return spawnSync(process.execPath, ["dev/gallery.ts", "--out", out], { cwd: process.cwd(), encoding: "utf8", timeout: 5000 });
}

test("gallery refuses repository output, including names starting with two dots, before creating it", () => {
  const out = join(root, `..gallery-${process.pid}`);
  const result = capture(out);
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain("Gallery output must be outside the repository");
  expect(existsSync(out)).toBe(false);
});

test("gallery refuses an external symlink resolving into the repository before creating output", () => {
  const dir = mkdtempSync(join(tmpdir(), "crew-gallery-boundary-"));
  const name = `gallery-test-${process.pid}`;
  try {
    symlinkSync(root, join(dir, "repo"), "dir");
    const result = capture(join(dir, "repo", name));
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("Gallery output resolves inside the repository");
    expect(existsSync(join(root, name))).toBe(false);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
