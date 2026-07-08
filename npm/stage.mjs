#!/usr/bin/env node
// stage.mjs — assemble the publishable npm packages from repo sources.
//
//   node npm/stage.mjs --demos
//       Copy the bundled demos (ports/* + npm/demos-extra/*) into
//       npm/betlang/demos/ and write demos/manifest.json. The staged copies are
//       gitignored; ports/ stays the single source of truth.
//
//   node npm/stage.mjs --platform <platform-arch> --binary <path-to-bet>
//       Create npm/dist/betlang-<platform-arch>/ — the @betlang/<platform-arch>
//       package holding one prebuilt binary — with its version taken from
//       npm/betlang/package.json (the single version source).
//
// CI runs --demos once and --platform once per matrix leg, then publishes every
// npm/dist/* package followed by npm/betlang itself.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const NPM_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.dirname(NPM_DIR);
const MAIN_PKG_DIR = path.join(NPM_DIR, "betlang");
const MAIN_PKG = JSON.parse(fs.readFileSync(path.join(MAIN_PKG_DIR, "package.json"), "utf8"));

// Must match SUPPORTED in npm/betlang/bin/bet.js.
const PLATFORMS = {
  "darwin-arm64": { os: "darwin", cpu: "arm64" },
  "darwin-x64": { os: "darwin", cpu: "x64" },
  "linux-x64": { os: "linux", cpu: "x64" },
  "linux-arm64": { os: "linux", cpu: "arm64" },
  "win32-x64": { os: "win32", cpu: "x64" },
};

// name -> where its .bet sources live, the entry module, and a one-line blurb.
const DEMOS = {
  hello: {
    src: "npm/demos-extra/hello",
    entry: "hello.bet",
    blurb: "a two-minute tour of the syntax",
    window: false,
  },
  "oregon-trail": {
    src: "ports/oregon-trail",
    entry: "oregon.bet",
    blurb: "the 1978 MECC classic, faithfully ported",
    window: false,
  },
  pong: {
    src: "ports/pong",
    entry: "pong.bet",
    blurb: "PONG with dynamic resolution and sound",
    window: true,
  },
  "gg-demo": {
    src: "ports/gg-demo",
    entry: "gg-demo.bet",
    blurb: "the gg platform layer's five primitives",
    window: true,
  },
};

function stageDemos() {
  const demosDir = path.join(MAIN_PKG_DIR, "demos");
  fs.rmSync(demosDir, { recursive: true, force: true });
  const manifest = {};
  for (const [name, d] of Object.entries(DEMOS)) {
    const srcDir = path.join(REPO, d.src);
    const dstDir = path.join(demosDir, name);
    fs.mkdirSync(dstDir, { recursive: true });
    const bets = fs.readdirSync(srcDir).filter((f) => f.endsWith(".bet"));
    if (!bets.includes(d.entry)) {
      throw new Error(`demo "${name}": entry ${d.entry} not found in ${d.src}`);
    }
    for (const f of bets) fs.copyFileSync(path.join(srcDir, f), path.join(dstDir, f));
    manifest[name] = { entry: d.entry, blurb: d.blurb, window: d.window };
    console.log(`staged demo ${name} (${bets.length} file${bets.length === 1 ? "" : "s"})`);
  }
  fs.writeFileSync(path.join(demosDir, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  console.log(`wrote ${path.relative(REPO, path.join(demosDir, "manifest.json"))}`);
}

function stagePlatform(platform, binary) {
  const spec = PLATFORMS[platform];
  if (!spec) {
    throw new Error(`unknown platform "${platform}" (known: ${Object.keys(PLATFORMS).join(", ")})`);
  }
  if (!fs.existsSync(binary)) throw new Error(`binary not found: ${binary}`);

  const pkgDir = path.join(NPM_DIR, "dist", `betlang-${platform}`);
  fs.rmSync(pkgDir, { recursive: true, force: true });
  fs.mkdirSync(path.join(pkgDir, "bin"), { recursive: true });

  const exe = spec.os === "win32" ? "bet.exe" : "bet";
  fs.copyFileSync(binary, path.join(pkgDir, "bin", exe));
  fs.chmodSync(path.join(pkgDir, "bin", exe), 0o755);

  const pkg = {
    name: `@betlang/${platform}`,
    version: MAIN_PKG.version,
    description: `Prebuilt \`bet\` binary for ${platform}. Install betlang instead of this package.`,
    license: MAIN_PKG.license,
    repository: MAIN_PKG.repository,
    os: [spec.os],
    cpu: [spec.cpu],
    files: ["bin/"],
  };
  fs.writeFileSync(path.join(pkgDir, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
  console.log(`staged ${pkg.name}@${pkg.version} -> ${path.relative(REPO, pkgDir)}`);
}

const argv = process.argv.slice(2);
const has = (f) => argv.includes(f);
const val = (f) => {
  const i = argv.indexOf(f);
  return i >= 0 ? argv[i + 1] : undefined;
};

// Every platform package pins the main package's version; refuse to stage if the
// optionalDependencies drifted from it.
for (const [dep, v] of Object.entries(MAIN_PKG.optionalDependencies ?? {})) {
  if (v !== MAIN_PKG.version) {
    throw new Error(
      `betlang/package.json: optionalDependencies["${dep}"] is ${v} but version is ${MAIN_PKG.version}; keep them in lockstep`
    );
  }
}

let did = false;
if (has("--demos")) {
  stageDemos();
  did = true;
}
if (has("--platform")) {
  stagePlatform(val("--platform"), val("--binary"));
  did = true;
}
if (!did) {
  console.error("usage: node npm/stage.mjs --demos | --platform <platform-arch> --binary <path>");
  process.exit(2);
}
