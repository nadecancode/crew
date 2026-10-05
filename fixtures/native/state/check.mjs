// Build existing libraries, then check the same corpus with both existing
// clients. No Cargo manifests, generated wire fixtures, or product files change.
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const directory = fileURLToPath(new URL("./", import.meta.url));
const root = fileURLToPath(new URL("../../../", import.meta.url));
const files = readdirSync(directory).filter((name) => name.endsWith(".case.json")).sort().map((name) => join(directory, name));
const artifacts = process.env.CREW_NATIVE_FIXTURE_CACHE
  ? resolve(root, process.env.CREW_NATIVE_FIXTURE_CACHE, "state")
  : join(root, "target/native-validation");
mkdirSync(artifacts, { recursive: true });
const scratch = mkdtempSync(join(artifacts, "state-"));

function safeIntegers(value, path) {
  if (typeof value === "number" && Number.isInteger(value) && !Number.isSafeInteger(value)) {
    throw new Error(path + ": integer exceeds JavaScript's exact range; do not round IDs, counters, or milliseconds");
  }
  if (value !== null && typeof value === "object") {
    for (const [key, field] of Object.entries(value)) safeIntegers(field, path + "." + key);
  }
}

function run(command, args, capture = false) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(command + " failed (exit " + result.status + ")");
  return result.stdout;
}

try {
  for (const file of files) safeIntegers(JSON.parse(readFileSync(file, "utf8")), file);
  const output = run("./scripts/build-env", ["cargo", "build", "--locked", "-p", "crew-tui", "--message-format=json"], true);
  const libraries = new Map();
  for (const line of output.trim().split("\n")) {
    let message;
    try { message = JSON.parse(line); } catch { continue; }
    if (message.reason !== "compiler-artifact") continue;
    const library = message.filenames.find((name) => name.endsWith(".rlib"));
    if (library) libraries.set(message.target.name, library);
  }
  const args = ["rustc", "--edition=2021", join(directory, "check.rs"), "-o", join(scratch, "check")];
  for (const name of ["crew_core", "crew_tui", "serde_json"]) {
    const library = libraries.get(name);
    if (!library) throw new Error("Missing Cargo artifact: " + name);
    args.push("--extern", name + "=" + library, "-L", "dependency=" + dirname(library));
  }
  run("./scripts/build-env", args);
  run(join(scratch, "check"), files);
  if (!process.argv.includes("--rust-only")) {
    run(process.execPath, [join(root, "web/node_modules/vitest/vitest.mjs"), "run", "--config", join(directory, "check-vitest.config.mjs")]);
  }
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
