//! `cargo xtask run <port> [-- args]` — build and run a `ports/` program with one command.
//!
//! Every port needs the same four things: an LLVM-enabled compiler (`driver --features llvm`), a
//! windowed runtime (`runtime --features gg-desktop`), a `bet build` of its entry module, and the
//! right environment. Before this command that knowledge lived in two copy-pasted `run.sh`
//! scripts (doom, fireworks) and four READMEs, all of which hardcoded one developer's Homebrew
//! path. The table below is now the single source of truth, and LLVM is *discovered* (see
//! [`crate::llvm`]) rather than assumed.
//!
//! Assets are deliberately asymmetric: `pong`/`fireworks`/`gg-demo`/`oregon-trail` need nothing,
//! `doom` auto-fetches the BSD-licensed Freedoom IWAD when no WAD is present, and
//! `frozen-bubble` cannot be automated (its assets are GPL-2 and come from a separate checkout),
//! so it explains the two commands instead.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::llvm;

/// Which runtime archive a port links against.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Runtime {
    /// The bootstrap `rt-stub` — enough for stdout/stdin ports. No extra build step.
    Stub,
    /// The real `runtime`, built `--features gg-desktop` for a window + audio device.
    Real,
}

/// What a port needs beyond a compiler before it can run.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Assets {
    /// Nothing — clone and run.
    None,
    /// An IWAD. Auto-fetched (Freedoom) when absent.
    DoomWad,
    /// A local bake of a GPL-2 upstream checkout. Cannot be auto-fetched.
    FrozenBubble,
}

struct Port {
    /// The `cargo xtask run <name>` name (also the `ports/<name>/` directory).
    name: &'static str,
    /// The entry module, relative to the workspace root.
    entry: &'static str,
    runtime: Runtime,
    /// `Some(false)` pins `--overflow-checks off`; `None` leaves the driver's default (trap at
    /// -O0). Only DOOM needs this — see the table entry.
    overflow_checks: Option<bool>,
    /// Environment the port's *runtime* wants (not the build).
    env: &'static [(&'static str, &'static str)],
    assets: Assets,
    /// One line for `cargo xtask run` with no argument.
    blurb: &'static str,
}

const PORTS: &[Port] = &[
    Port {
        name: "pong",
        entry: "ports/pong/pong.bet",
        runtime: Runtime::Real,
        overflow_checks: None,
        env: &[],
        assets: Assets::None,
        blurb: "PONG on the gg platform layer — no assets, start here",
    },
    Port {
        name: "fireworks",
        entry: "ports/fireworks/fireworks.bet",
        runtime: Runtime::Real,
        overflow_checks: None,
        // The demo renders a small logical frame; 2x makes it a sensible window size. This is
        // what ports/fireworks/run.sh defaulted to.
        env: &[("GG_SCALE", "2")],
        assets: Assets::None,
        blurb: "per-frame scratch-arena demo (mem.scratch cycling)",
    },
    Port {
        name: "gg-demo",
        entry: "ports/gg-demo/gg-demo.bet",
        runtime: Runtime::Real,
        overflow_checks: None,
        env: &[],
        assets: Assets::None,
        blurb: "gg platform-layer shakeout — self-terminates after 600 frames",
    },
    Port {
        name: "doom",
        entry: "ports/doom/doom.bet",
        runtime: Runtime::Real,
        // LOAD-BEARING. DOOM's 16.16 fixed-point render/physics math is a direct translation of
        // C, and relies on `int` 2's-complement wraparound (FixedMul intermediates, angle
        // arithmetic, overflowing wall geometry). `bet` traps signed overflow at -O0 like Rust,
        // so without this the renderer aborts as soon as play starts. See the driver's
        // `--overflow-checks` docs: "needed by faithful ports whose fixed-point math relies on
        // 2's-complement wraparound".
        overflow_checks: Some(false),
        // Open at the largest square that fits the live display; gg detects the monitor at
        // runtime and aspect-fits DOOM inside it.
        env: &[("GG_FULLSQUARE", "1")],
        assets: Assets::DoomWad,
        blurb: "real DOOM, byte-exact sim parity (auto-fetches freedoom1.wad)",
    },
    Port {
        name: "frozen-bubble",
        entry: "ports/frozen-bubble/frozen-bubble.bet",
        runtime: Runtime::Real,
        overflow_checks: None,
        env: &[],
        assets: Assets::FrozenBubble,
        blurb: "Frozen Bubble — needs a local GPL-2 asset bake first (see below)",
    },
    Port {
        name: "oregon-trail",
        entry: "ports/oregon-trail/oregon.bet",
        // Text/stdin only: no window, no audio, so the bootstrap runtime is enough and we skip
        // building `runtime` entirely.
        runtime: Runtime::Stub,
        overflow_checks: None,
        env: &[],
        assets: Assets::None,
        blurb: "the 1971 MECC original — text only, no window",
    },
];

/// Freedoom 0.13.0. BSD-licensed and freely redistributable, so DOOM has a zero-manual-step
/// default path (id's `doom1.wad` is shareware — we never fetch or ship that).
///
/// The SHA-256 is upstream's own published value from the release's PGP-signed
/// `freedoom-0.13.0-CHECKSUM` asset — not a hash observed from our own download, which would
/// only pin whatever a MITM served us the first time. Same discipline as `doom-oracle`'s pinned
/// doomgeneric SHA.
const FREEDOOM_VERSION: &str = "0.13.0";
const FREEDOOM_URL: &str =
    "https://github.com/freedoom/freedoom/releases/download/v0.13.0/freedoom-0.13.0.zip";
const FREEDOOM_SHA256: &str = "3f9b264f3e3ce503b4fb7f6bdcb1f419d93c7b546f4df3e874dd878db9688f59";

pub fn run(args: &[String], root: &Path) -> Result<()> {
    // `cargo xtask run doom -- -warp 1 1` and `cargo xtask run doom -warp 1 1` both work: split
    // at the first `--` if present, else the first argument is the port and the rest pass
    // through. The `--` form is what you need for args that look like xtask flags.
    let Some((name, rest)) = args.split_first() else {
        bail!("{}", usage());
    };
    let passthrough: Vec<String> = rest
        .iter()
        .skip_while(|a| a.as_str() == "--")
        .cloned()
        .collect();

    let Some(port) = PORTS.iter().find(|p| p.name == name.as_str()) else {
        bail!("unknown port {name:?}\n{}", usage());
    };

    // Resolve everything that can fail *before* spending minutes on a build.
    let llvm =
        llvm::discover(root).ok_or_else(|| anyhow::anyhow!("{}", llvm::not_found_message()))?;
    println!("==> LLVM {} at {}", llvm.version, llvm.prefix.display());
    let extra_args = prepare_assets(port, root)?;

    build_toolchain(port, root, &llvm)?;
    let bin = compile_port(port, root, &llvm)?;
    launch(port, &bin, &extra_args, &passthrough)
}

/// Check/fetch a port's assets, returning any arguments they imply (DOOM's `-iwad <path>`).
fn prepare_assets(port: &Port, root: &Path) -> Result<Vec<String>> {
    match port.assets {
        Assets::None => Ok(Vec::new()),
        Assets::DoomWad => {
            let wad = resolve_wad(root)?;
            Ok(vec!["-iwad".into(), wad.display().to_string()])
        }
        Assets::FrozenBubble => {
            let dat = root.join("ports/frozen-bubble/assets.dat");
            if !dat.is_file() {
                // Not auto-fetchable, and pretending otherwise would be worse than saying so.
                bail!(
                    "frozen-bubble has no baked assets at {}.\n\n\
                     Its graphics and audio are GPL-2, so this repo ships only the baker, never \
                     the baked data. Bake them from an upstream checkout (~170 MB of decoded \
                     RGBA + PCM):\n  \
                     git clone --depth 1 https://github.com/kthakore/frozen-bubble /tmp/frozen-bubble\n  \
                     cargo xtask bake-frozen-bubble --src /tmp/frozen-bubble --out {}\n\n\
                     Then re-run `cargo xtask run frozen-bubble`.",
                    dat.display(),
                    dat.display(),
                );
            }
            Ok(Vec::new())
        }
    }
}

/// Find an IWAD, fetching Freedoom if there is none.
///
/// Order: `$DOOM_WAD`, then id's shareware `doom1.wad` if the contributor already has one (it is
/// the demo-parity oracle, so prefer it), then a previously fetched `freedoom1.wad`, then fetch.
fn resolve_wad(root: &Path) -> Result<PathBuf> {
    if let Some(w) = std::env::var_os("DOOM_WAD") {
        let wad = PathBuf::from(w);
        if !wad.is_file() {
            bail!("$DOOM_WAD is set to {}, which is not a file", wad.display());
        }
        return Ok(wad);
    }
    let refdir = root.join("doom-reference");
    for cand in ["doom1.wad", "freedoom1.wad"] {
        let wad = refdir.join(cand);
        if wad.is_file() {
            println!("==> WAD: {}", wad.display());
            return Ok(wad);
        }
    }
    fetch_freedoom(&refdir)
}

/// Download + verify + unpack Freedoom's `freedoom1.wad` into `dir`.
fn fetch_freedoom(dir: &Path) -> Result<PathBuf> {
    let wad = dir.join("freedoom1.wad");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    println!(
        "==> no WAD found — fetching Freedoom {FREEDOOM_VERSION} (BSD-licensed, ~24 MB)\n    \
         {FREEDOOM_URL}"
    );
    let zip = dir.join(format!("freedoom-{FREEDOOM_VERSION}.zip"));
    let curl = Command::new("curl")
        .args(["-fSL", "--retry", "3", "-o"])
        .arg(&zip)
        .arg(FREEDOOM_URL)
        .status()
        .context("running `curl` (is it installed / is the network up?)")?;
    if !curl.success() {
        bail!("downloading {FREEDOOM_URL} failed");
    }

    // Verify before unpacking: a corrupt or substituted archive must never reach `unzip`.
    let got = sha256(&zip)?;
    if got != FREEDOOM_SHA256 {
        let _ = std::fs::remove_file(&zip);
        bail!(
            "checksum mismatch for freedoom-{FREEDOOM_VERSION}.zip — the download was discarded.\n  \
             expected {FREEDOOM_SHA256}\n  got      {got}\n\
             Either the download was corrupted (re-run) or the artifact changed; do not use it."
        );
    }
    println!("==> sha256 verified");

    // `-j` flattens the `freedoom-<ver>/` directory, `-o` overwrites a partial previous run.
    let unzip = Command::new("unzip")
        .args(["-j", "-o", "-q"])
        .arg(&zip)
        .arg("*/freedoom1.wad")
        .arg("-d")
        .arg(dir)
        .status()
        .context("running `unzip` (install it: `brew install unzip` / `apt-get install unzip`)")?;
    if !unzip.success() {
        bail!("unpacking freedoom1.wad from {} failed", zip.display());
    }
    let _ = std::fs::remove_file(&zip);
    if !wad.is_file() {
        bail!("{} did not contain freedoom1.wad", FREEDOOM_URL);
    }
    println!("==> WAD: {}", wad.display());
    Ok(wad)
}

/// SHA-256 of a file, as lowercase hex.
///
/// Shells out rather than taking a `sha2` dependency — the same "use the system tool" call the
/// rest of xtask makes for `git`, `curl`, `gzip`, and `brotli`. `sha256sum` is the coreutils
/// name; `shasum -a 256` is what macOS ships.
fn sha256(path: &Path) -> Result<String> {
    let attempts: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];
    for (exe, args) in attempts {
        let Ok(out) = Command::new(exe).args(args).arg(path).output() else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        // Both tools print `<hex>  <filename>`.
        if let Some(hex) = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
        {
            return Ok(hex.to_ascii_lowercase());
        }
    }
    bail!(
        "no SHA-256 tool found — need `sha256sum` or `shasum` on PATH to verify the download. \
         Refusing to use an unverified archive."
    )
}

/// Apply the discovered LLVM to a command's environment.
fn with_llvm_env(cmd: &mut Command, llvm: &llvm::Llvm) {
    cmd.env(llvm::PREFIX_VAR, &llvm.prefix);
    if let Some(extra) = &llvm.library_path {
        // Prepend, preserving any inherited value.
        let joined = match std::env::var_os("LIBRARY_PATH") {
            Some(existing) if !existing.is_empty() => {
                let mut v = extra.clone().into_os_string();
                v.push(":");
                v.push(existing);
                v
            }
            _ => extra.clone().into_os_string(),
        };
        cmd.env("LIBRARY_PATH", joined);
    }
}

/// Build the compiler, and the windowed runtime if the port links against it.
fn build_toolchain(port: &Port, root: &Path, llvm: &llvm::Llvm) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    println!("==> building the bet compiler (driver --features llvm)");
    let mut cmd = Command::new(&cargo);
    cmd.args(["build", "-p", "driver", "--features", "llvm"])
        .current_dir(root);
    with_llvm_env(&mut cmd, llvm);
    let st = cmd
        .status()
        .context("running `cargo build -p driver --features llvm`")?;
    if !st.success() {
        bail!(
            "`cargo build -p driver --features llvm` failed (LLVM {} at {}).\n\
             If the failure is in `llvm-sys`'s build script, the install is likely incomplete — \
             it needs the LLVM *development* files (headers + static libs), not just the tools.",
            llvm.version,
            llvm.prefix.display()
        );
    }

    // Either way the driver links against a runtime *archive* that must already be built — the
    // link step just looks for `libruntime.a` / `librt_stub.a` in `target/debug` and fails with a
    // bare `ld: library 'rt_stub' not found` if it isn't there. `-p driver` alone does not pull
    // either one in, so build the one this port links against.
    let (pkg, args, label) = match port.runtime {
        Runtime::Real => (
            "runtime",
            &["build", "-p", "runtime", "--features", "gg-desktop"][..],
            "the windowed runtime (runtime --features gg-desktop)",
        ),
        Runtime::Stub => (
            "rt-stub",
            &["build", "-p", "rt-stub"][..],
            "the bootstrap runtime (rt-stub)",
        ),
    };
    println!("==> building {label}");
    let st = Command::new(&cargo)
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("running `cargo build -p {pkg}`"))?;
    if !st.success() {
        bail!("`cargo build -p {pkg}` failed");
    }
    Ok(())
}

/// `bet build` the port's entry module; returns the executable's path.
fn compile_port(port: &Port, root: &Path, llvm: &llvm::Llvm) -> Result<PathBuf> {
    let bet = root.join("target/debug/bet");
    // Build into `target/` so the binaries are gitignored and `cargo clean` sweeps them (the old
    // run.sh scripts dropped them in /tmp, and the READMEs in the repo root).
    let out = root.join("target/ports").join(port.name);
    std::fs::create_dir_all(out.parent().expect("target/ports has a parent"))
        .context("creating target/ports")?;

    let mut cmd = Command::new(&bet);
    cmd.arg("build").arg(root.join(port.entry));
    if port.runtime == Runtime::Real {
        cmd.args(["--runtime", "real"]);
    }
    if let Some(checks) = port.overflow_checks {
        cmd.args(["--overflow-checks", if checks { "on" } else { "off" }]);
    }
    cmd.arg("-o").arg(&out).current_dir(root);
    with_llvm_env(&mut cmd, llvm);

    println!("==> compiling {} -> {}", port.entry, out.display());
    let st = cmd
        .status()
        .with_context(|| format!("running {} build", bet.display()))?;
    if !st.success() {
        bail!("`bet build {}` failed", port.entry);
    }
    Ok(out)
}

/// Exec the built port.
fn launch(port: &Port, bin: &Path, extra: &[String], passthrough: &[String]) -> Result<()> {
    let mut cmd = Command::new(bin);
    cmd.args(extra).args(passthrough);
    for (k, v) in port.env {
        // Never clobber a deliberate override (`GG_SCALE=4 cargo xtask run fireworks`).
        if std::env::var_os(k).is_none() {
            cmd.env(k, v);
        }
    }

    let shown: Vec<&str> = extra
        .iter()
        .chain(passthrough.iter())
        .map(String::as_str)
        .collect();
    println!("==> running: {} {}", bin.display(), shown.join(" "));

    let st = cmd
        .status()
        .with_context(|| format!("running {}", bin.display()))?;
    if !st.success() {
        bail!("{} exited with {}", port.name, st);
    }
    Ok(())
}

fn usage() -> String {
    let mut s = String::from("usage: cargo xtask run <port> [-- args]\n\nports:\n");
    for p in PORTS {
        s.push_str(&format!("  {:<14} {}\n", p.name, p.blurb));
    }
    s.push_str(
        "\nLLVM is discovered automatically (llvm-config / brew / the well-known paths); run \
         `cargo xtask setup-llvm` to see what was found.",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every port's entry module must exist — this table is the thing that rots when a port is
    /// renamed, and a stale entry would only surface as a confusing `bet build` failure.
    #[test]
    fn port_entries_exist() {
        let root = crate::workspace_root();
        for p in PORTS {
            assert!(
                root.join(p.entry).is_file(),
                "port {:?} entry {:?} does not exist",
                p.name,
                p.entry
            );
        }
    }

    /// The table must cover `ports/` exactly: a new port directory should appear in `xtask run`,
    /// which is the whole discoverability point of the command.
    #[test]
    fn every_port_directory_is_runnable() {
        let root = crate::workspace_root();
        for entry in std::fs::read_dir(root.join("ports")).expect("ports/ exists") {
            let entry = entry.expect("readable dir entry");
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(
                PORTS.iter().any(|p| p.name == name),
                "ports/{name}/ has no entry in xtask's PORTS table — add one so \
                 `cargo xtask run {name}` works"
            );
        }
    }

    /// DOOM's fixed-point math needs C wraparound. If this ever flips back to the default, the
    /// renderer aborts on overflowing geometry the moment play starts — a subtle, late failure,
    /// so pin it with a test.
    #[test]
    fn doom_disables_overflow_checks() {
        let doom = PORTS
            .iter()
            .find(|p| p.name == "doom")
            .expect("doom is a port");
        assert_eq!(doom.overflow_checks, Some(false));
    }
}
