#!/usr/bin/env node
/**
 * Set the app version in the three files that have to agree.
 *
 * The duplication is forced by the toolchain: `package.json` is what the JS
 * tooling reads, `src-tauri/Cargo.toml` is what cargo stamps into the binary,
 * and `src-tauri/tauri.conf.json` is what the bundle reports to the updater.
 * The updater compares the running version against the manifest, so a release
 * where these disagree either never offers an update or offers one forever.
 *
 * Usage:
 *   node scripts/bump-version.mjs 0.2.0
 *   node scripts/bump-version.mjs --check    # verify they already agree
 */

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const APP = join(ROOT, "apps", "hush-tauri");

const PACKAGE_JSON = join(APP, "package.json");
const TAURI_CONF = join(APP, "src-tauri", "tauri.conf.json");
const CARGO_TOML = join(APP, "src-tauri", "Cargo.toml");

const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

/** Read the `version` of the `[package]` table, and only that one. */
function cargoPackageVersion(text) {
  const lines = text.split("\n");
  const start = lines.findIndex((l) => l.trim() === "[package]");
  if (start === -1) throw new Error(`${CARGO_TOML}: no [package] table`);
  for (let i = start + 1; i < lines.length; i++) {
    const line = lines[i].trim();
    if (line.startsWith("[")) break; // the next table; [package] had no version
    const m = /^version\s*=\s*"([^"]*)"/.exec(line);
    if (m) return { value: m[1], index: i };
  }
  throw new Error(`${CARGO_TOML}: [package] has no version`);
}

/** Read the version without rewriting anything. */
function readVersions() {
  const pkg = JSON.parse(readFileSync(PACKAGE_JSON, "utf8"));
  const conf = JSON.parse(readFileSync(TAURI_CONF, "utf8"));
  const cargo = cargoPackageVersion(readFileSync(CARGO_TOML, "utf8"));
  return { pkg: pkg.version, conf: conf.version, cargo: cargo.value };
}

function check() {
  const { pkg, conf, cargo } = readVersions();
  const all = [pkg, conf, cargo];
  const agree = all.every((v) => v === pkg);
  console.log(`package.json        ${pkg}`);
  console.log(`tauri.conf.json     ${conf}`);
  console.log(`src-tauri/Cargo.toml ${cargo}`);
  if (!agree) {
    console.error("\nThese must match before a release.");
    process.exit(1);
  }
  console.log("\nIn sync.");
}

function bump(version) {
  if (!SEMVER.test(version)) {
    console.error(`Not a version: ${version} (expected MAJOR.MINOR.PATCH)`);
    process.exit(2);
  }

  // JSON files: re-serialised rather than patched by text, so the result is
  // valid JSON whatever the previous formatting was. The trailing newline is
  // restored because these files are edited by hand and by tooling.
  for (const path of [PACKAGE_JSON, TAURI_CONF]) {
    const data = JSON.parse(readFileSync(path, "utf8"));
    data.version = version;
    writeFileSync(path, JSON.stringify(data, null, 2) + "\n");
    console.log(`set ${path} -> ${version}`);
  }

  // Cargo.toml: patched in place. Re-serialising TOML would rewrite the whole
  // file and lose every comment in it.
  const lines = readFileSync(CARGO_TOML, "utf8").split("\n");
  const { index } = cargoPackageVersion(lines.join("\n"));
  lines[index] = lines[index].replace(/"([^"]*)"/, `"${version}"`);
  writeFileSync(CARGO_TOML, lines.join("\n"));
  console.log(`set ${CARGO_TOML} -> ${version}`);

  console.log(
    "\nCargo.lock still records the old version. Run a cargo command (or the\n" +
      "next build) to refresh it, then commit the result with the rest.",
  );
}

const arg = process.argv[2];
if (!arg || arg === "--help" || arg === "-h") {
  console.log("Usage: node scripts/bump-version.mjs <version>");
  console.log("       node scripts/bump-version.mjs --check");
  process.exit(arg ? 0 : 2);
} else if (arg === "--check") {
  check();
} else {
  bump(arg);
}
