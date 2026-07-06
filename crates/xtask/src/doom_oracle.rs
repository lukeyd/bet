//! `cargo xtask doom-oracle` — the doomgeneric-based reference oracle.
//!
//! `--setup` clones https://github.com/ozkl/doomgeneric (pinned to [`DOOMGENERIC_SHA`]) into
//! `/Users/lukebaggett/Documents/bet/doom-oracle` (outside the repo; `/doom-oracle/` is in the
//! repo root .gitignore) and applies `ports/doom/goldens/oracle.patch`, which adds the headless
//! dump platform `doomgeneric/doomgeneric_dump.c` (no window, uncapped time) plus two one-line
//! hooks (after `G_Ticker()` in d_net.c's RunTic; after `P_SetupLevel(..)` in g_game.c).
//!
//! `--run --demo <name>` builds the patched tree with the system C compiler and runs
//! `-iwad <shareware wad> -timedemo <name>` with `DOOM_SYNC_OUT` pointed at
//! `ports/doom/goldens/<name>.oracle.sync`, producing the reference sync stream the bet port
//! (W2/W3) will be diffed against (`doom-verify --ours .. --theirs ..`).
//!
//! Sync line / SETUP block format: see the header of `crates/xtask/src/doom_verify.rs` (the
//! emitting C code lives in the patch's doomgeneric_dump.c; crc32 constants in doom.rs).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::doom::{DEFAULT_IWAD, ORACLE_DIR};

/// The pinned doomgeneric commit (master as cloned 2026-07-06).
pub const DOOMGENERIC_SHA: &str = "dcb7a8dbc7a16ce3dda29382ac9aae9d77d21284";
const DOOMGENERIC_URL: &str = "https://github.com/ozkl/doomgeneric";

/// The default Makefile's source list, minus the X11 platform file (`doomgeneric_xlib.c`),
/// plus our headless dump platform from the patch.
const SOURCES: [&str; 82] = [
    "dummy.c",
    "am_map.c",
    "doomdef.c",
    "doomstat.c",
    "dstrings.c",
    "d_event.c",
    "d_items.c",
    "d_iwad.c",
    "d_loop.c",
    "d_main.c",
    "d_mode.c",
    "d_net.c",
    "f_finale.c",
    "f_wipe.c",
    "g_game.c",
    "hu_lib.c",
    "hu_stuff.c",
    "info.c",
    "i_cdmus.c",
    "i_endoom.c",
    "i_joystick.c",
    "i_scale.c",
    "i_sound.c",
    "i_system.c",
    "i_timer.c",
    "memio.c",
    "m_argv.c",
    "m_bbox.c",
    "m_cheat.c",
    "m_config.c",
    "m_controls.c",
    "m_fixed.c",
    "m_menu.c",
    "m_misc.c",
    "m_random.c",
    "p_ceilng.c",
    "p_doors.c",
    "p_enemy.c",
    "p_floor.c",
    "p_inter.c",
    "p_lights.c",
    "p_map.c",
    "p_maputl.c",
    "p_mobj.c",
    "p_plats.c",
    "p_pspr.c",
    "p_saveg.c",
    "p_setup.c",
    "p_sight.c",
    "p_spec.c",
    "p_switch.c",
    "p_telept.c",
    "p_tick.c",
    "p_user.c",
    "r_bsp.c",
    "r_data.c",
    "r_draw.c",
    "r_main.c",
    "r_plane.c",
    "r_segs.c",
    "r_sky.c",
    "r_things.c",
    "sha1.c",
    "sounds.c",
    "statdump.c",
    "st_lib.c",
    "st_stuff.c",
    "s_sound.c",
    "tables.c",
    "v_video.c",
    "wi_stuff.c",
    "w_checksum.c",
    "w_file.c",
    "w_main.c",
    "w_wad.c",
    "z_zone.c",
    "w_file_stdc.c",
    "i_input.c",
    "i_video.c",
    "doomgeneric.c",
    "doomgeneric_dump.c",
    "icon.c",
];

pub fn run(args: &[String], root: &Path) -> Result<()> {
    if args.iter().any(|a| a == "--setup") {
        setup(root)?;
        return Ok(());
    }
    if args.iter().any(|a| a == "--run") {
        let demo = args
            .iter()
            .position(|a| a == "--demo")
            .and_then(|i| args.get(i + 1))
            .context("usage: doom-oracle --run --demo <name>")?;
        let out = produce_sync(args, root, demo)?;
        println!("doom-oracle: sync stream written to {}", out.display());
        return Ok(());
    }
    bail!("usage: doom-oracle --setup | doom-oracle --run --demo <name>");
}

/// Clone (if needed), pin, and patch the oracle checkout. Idempotent.
fn setup(root: &Path) -> Result<PathBuf> {
    let dir = PathBuf::from(ORACLE_DIR);
    if !dir.join(".git").exists() {
        println!(
            "doom-oracle: cloning {DOOMGENERIC_URL} -> {}",
            dir.display()
        );
        let st = Command::new("git")
            .args(["clone", DOOMGENERIC_URL])
            .arg(&dir)
            .status()
            .context("running `git clone` (is git installed / network up?)")?;
        if !st.success() {
            bail!("`git clone {DOOMGENERIC_URL}` failed");
        }
    }

    // Pin to the recorded SHA.
    let head = git_out(&dir, &["rev-parse", "HEAD"])?;
    if head.trim() != DOOMGENERIC_SHA {
        println!(
            "doom-oracle: checkout at {}, pinning to {DOOMGENERIC_SHA}",
            head.trim()
        );
        let st = Command::new("git")
            .args(["-C"])
            .arg(&dir)
            .args(["checkout", DOOMGENERIC_SHA])
            .status()
            .context("running `git checkout <pinned sha>`")?;
        if !st.success() {
            bail!("could not check out pinned doomgeneric SHA {DOOMGENERIC_SHA}");
        }
    }

    // Apply the dump-platform patch (skip when already applied).
    let dump_c = dir.join("doomgeneric").join("doomgeneric_dump.c");
    if dump_c.exists() {
        println!(
            "doom-oracle: oracle.patch already applied ({} exists)",
            dump_c.display()
        );
        return Ok(dir);
    }
    let patch = root
        .join("ports")
        .join("doom")
        .join("goldens")
        .join("oracle.patch");
    if !patch.exists() {
        bail!("{} not found (it is committed with W0)", patch.display());
    }
    let st = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .arg("apply")
        .arg(&patch)
        .status()
        .context("running `git apply oracle.patch`")?;
    if !st.success() {
        bail!("`git apply {}` failed", patch.display());
    }
    println!(
        "doom-oracle: setup complete at {} ({DOOMGENERIC_SHA})",
        dir.display()
    );
    Ok(dir)
}

fn git_out(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("running `git {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Ensure setup, build the headless oracle, run `-timedemo <demo>` and return the path of the
/// committed-into-the-repo sync stream (`ports/doom/goldens/<demo>.oracle.sync`).
pub fn produce_sync(args: &[String], root: &Path, demo: &str) -> Result<PathBuf> {
    let dir = setup(root)?;
    let src_dir = dir.join("doomgeneric");
    let bin = dir.join("doomgeneric_dump");

    let rebuild = args.iter().any(|a| a == "--rebuild") || !bin.exists();
    if rebuild {
        println!(
            "doom-oracle: building headless oracle ({} sources)...",
            SOURCES.len()
        );
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
        let mut cmd = Command::new(&cc);
        cmd.arg("-O1")
            .arg("-w")
            .args(["-DNORMALUNIX", "-D_DEFAULT_SOURCE"])
            .arg("-o")
            .arg(&bin);
        for s in SOURCES {
            cmd.arg(src_dir.join(s));
        }
        cmd.arg("-lm");
        let st = cmd
            .status()
            .with_context(|| format!("running `{cc}` on the oracle"))?;
        if !st.success() {
            bail!("oracle build failed (see compiler output above)");
        }
    }

    let out_path = root
        .join("ports")
        .join("doom")
        .join("goldens")
        .join(format!("{demo}.oracle.sync"));
    let _ = fs::remove_file(&out_path); // the dump appends; start clean
    fs::create_dir_all(out_path.parent().expect("has parent"))?;

    println!("doom-oracle: running -timedemo {demo} (iwad: {DEFAULT_IWAD})...");
    let out = Command::new(&bin)
        .current_dir(&dir)
        .env("DOOM_SYNC_OUT", &out_path)
        .args(["-iwad", DEFAULT_IWAD, "-timedemo", demo])
        .output()
        .context("running the headless oracle")?;
    // Vanilla-family timedemo quits through I_Error("timed %i gametics...") — a nonzero exit
    // is normal. The real success signal is a non-empty sync stream.
    let produced = fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    if produced == 0 {
        bail!(
            "oracle produced no sync output (exit {}):\nstdout:\n{}\nstderr:\n{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    println!(
        "doom-oracle: {} lines of sync stream ({} bytes)",
        fs::read_to_string(&out_path)
            .map(|s| s.lines().count())
            .unwrap_or(0),
        produced
    );
    Ok(out_path)
}
