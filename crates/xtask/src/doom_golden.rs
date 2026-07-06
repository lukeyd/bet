//! `cargo xtask doom-golden-gen` — build and run the committed C golden generators
//! (`ports/doom/goldens/gen/gen_{fixed,random,angle,tables}.c`) against the reference
//! linuxdoom-1.10 source, writing their stdout to `ports/doom/goldens/<name>.golden`.
//!
//! The generators #include the reference .c files directly (m_fixed.c, m_random.c, tables.c),
//! so every value in the goldens is produced by id's own code paths. Output is fixed-width
//! hex text; the bet twin (`ports/doom/tools/goldens.bet`, W1) must reproduce it byte-for-byte
//! (checked by `doom-verify --goldens`).
//!
//! CRC-32 constants (used by gen_tables.c and the whole pipeline): IEEE 802.3 reflected,
//! poly 0xEDB88320, init 0xFFFFFFFF, final XOR 0xFFFFFFFF — see crate::doom::crc32.

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::doom::doom_ref_from;

const GENERATORS: [&str; 4] = ["gen_fixed", "gen_random", "gen_angle", "gen_tables"];

pub fn run(args: &[String], root: &Path) -> Result<()> {
    let doom_ref = doom_ref_from(args);
    let gen_dir = root.join("ports").join("doom").join("goldens").join("gen");
    let out_dir = root.join("ports").join("doom").join("goldens");
    let tmp = std::env::temp_dir().join(format!("bet-doom-golden-{}", std::process::id()));
    fs::create_dir_all(&tmp).with_context(|| format!("creating {}", tmp.display()))?;

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut result = Ok(());
    for name in GENERATORS {
        let src = gen_dir.join(format!("{name}.c"));
        let bin = tmp.join(name);
        let st = Command::new(&cc)
            .arg("-std=gnu89") // 1993 C: K&R-tolerant, `long long` as a GNU extension
            .arg("-w")
            .arg("-O1")
            .arg("-I")
            .arg(&doom_ref)
            .arg("-o")
            .arg(&bin)
            .arg(&src)
            .arg("-lm")
            .status()
            .with_context(|| format!("running `{cc}` on {}", src.display()))?;
        if !st.success() {
            result = Err(anyhow::anyhow!("compiling {} failed", src.display()));
            break;
        }
        let out = Command::new(&bin)
            .output()
            .with_context(|| format!("running {}", bin.display()))?;
        if !out.status.success() {
            result = Err(anyhow::anyhow!(
                "{name} exited {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr)
            ));
            break;
        }
        let golden = out_dir.join(format!("{}.golden", name.trim_start_matches("gen_")));
        fs::write(&golden, &out.stdout).with_context(|| format!("writing {}", golden.display()))?;
        println!(
            "doom-golden-gen: wrote {} ({} lines)",
            golden.display(),
            out.stdout.iter().filter(|&&b| b == b'\n').count()
        );
    }

    let _ = fs::remove_dir_all(&tmp); // best-effort cleanup
    if result.is_ok() {
        println!("doom-golden-gen OK ({} goldens)", GENERATORS.len());
    }
    result.map_err(|e| e.context("doom-golden-gen"))
}
