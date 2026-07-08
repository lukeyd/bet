#!/usr/bin/env node
// betlang npm shim — resolves the platform-specific `bet` binary (shipped in an
// @betlang/<platform>-<arch> optionalDependency) and execs it, plus two npm-only
// niceties the native CLI doesn't have: `bet demo [name]` (run a bundled example)
// and `bet --version`.
"use strict";

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const PKG = require("../package.json");

// platform-arch pairs a prebuilt binary exists for; must match npm/stage.mjs PLATFORMS.
const SUPPORTED = ["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64", "win32-x64"];

function fail(msg) {
  process.stderr.write(msg.endsWith("\n") ? msg : msg + "\n");
  process.exit(1);
}

function resolveBinary() {
  // Escape hatch for development and for platforms we don't prebuild: point
  // BET_BIN at any locally built `bet` (e.g. target/release/bet).
  if (process.env.BET_BIN) return process.env.BET_BIN;

  const key = `${process.platform}-${process.arch}`;
  if (!SUPPORTED.includes(key)) {
    fail(
      `betlang: no prebuilt binary for ${key} (supported: ${SUPPORTED.join(", ")}).\n` +
        `You can build from source (https://github.com/lukeyd/bet) and set BET_BIN=/path/to/bet.`
    );
  }
  const exe = process.platform === "win32" ? "bet.exe" : "bet";
  try {
    return require.resolve(`@betlang/${key}/bin/${exe}`);
  } catch {
    fail(
      `betlang: the platform package @betlang/${key} is not installed.\n` +
        `This usually means optional dependencies were skipped (--no-optional / --omit=optional)\n` +
        `or the lockfile was created on a different platform. Try:\n` +
        `    npm install --force betlang\n` +
        `or set BET_BIN=/path/to/bet to use a locally built binary.`
    );
  }
}

function run(bin, args) {
  const r = spawnSync(bin, args, { stdio: "inherit" });
  if (r.error) fail(`betlang: failed to run ${bin}: ${r.error.message}`);
  if (r.signal) fail(`betlang: bet was killed by signal ${r.signal}`);
  process.exit(r.status ?? 1);
}

function loadDemoManifest() {
  const manifestPath = path.join(__dirname, "..", "demos", "manifest.json");
  if (!fs.existsSync(manifestPath)) {
    fail(
      "betlang: no demos are bundled in this install (demos/manifest.json missing).\n" +
        "If you are running from the repo, stage them first: node npm/stage.mjs --demos"
    );
  }
  return JSON.parse(fs.readFileSync(manifestPath, "utf8"));
}

function demo(args) {
  const demos = loadDemoManifest();
  const name = args[0];
  if (!name) {
    process.stdout.write("bundled demos (run with `bet demo <name>`):\n\n");
    for (const [n, d] of Object.entries(demos)) {
      const where = d.window ? "opens a window" : "runs in the terminal";
      process.stdout.write(`    ${n.padEnd(14)} ${d.blurb} (${where})\n`);
    }
    process.exit(0);
  }
  const d = demos[name];
  if (!d) {
    fail(`betlang: unknown demo "${name}". Run \`bet demo\` to list the bundled ones.`);
  }
  const entry = path.join(__dirname, "..", "demos", name, d.entry);
  run(resolveBinary(), ["run", entry]);
}

const argv = process.argv.slice(2);

if (argv[0] === "--version" || argv[0] === "-V") {
  process.stdout.write(`betlang ${PKG.version}\n`);
  process.exit(0);
}
if (argv[0] === "demo") {
  demo(argv.slice(1));
}
run(resolveBinary(), argv);
