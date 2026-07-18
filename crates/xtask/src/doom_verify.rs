//! `cargo xtask doom-verify` — the DOOM port's verification harness.
//!
//! Three modes:
//!   --goldens             run the bet twin (ports/doom/tools/goldens.bet, W1) and diff its
//!                         output against ports/doom/goldens/*.golden
//!   --ours F --theirs F   sync-stream diff: first divergent line + context + per-field diff
//!                         + triage (sim / renderer / loader)
//!   --demo NAME           produce the oracle stream for a demo, then (W2/W3) diff the bet
//!                         port's stream against it
//!
//! Sync-stream format (produced by the doomgeneric oracle patch and, later, the bet port):
//!   per-gametic fingerprint lines, all values %08x fixed-width hex:
//!     T=<gametic> R=<prndindex> X=<mo.x> Y=<mo.y> Z=<mo.z> A=<mo.angle> MX=<momx> MY=<momy>
//!     H=<health> S=<sector idx> LT=<leveltime> C=<crc32 of the 8-bit framebuffer>
//!   and per-level-load SETUP blocks:
//!     SETUP sectors=<n> lines=<n> things=<n>
//!     SETUP T i=<n> type=<mobjtype> x=<fixed> y=<fixed> a=<angle>
//!
//! The bet-twin golden protocol (`--goldens`): the twin prints, for each golden in order,
//! a marker line `#GOLDEN <name>` followed by that golden's exact lines — so its stdout is
//! diffable section-by-section against fixed.golden/random.golden/angle.golden/tables.golden.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

const GOLDEN_NAMES: [&str; 4] = ["fixed", "random", "angle", "tables"];

pub fn run(args: &[String], root: &Path) -> Result<()> {
    if args.iter().any(|a| a == "--goldens") {
        return goldens(root);
    }
    if let Some(demo) = flag_value(args, "--demo") {
        return demo_mode(args, root, demo);
    }
    let (Some(ours), Some(theirs)) = (flag_value(args, "--ours"), flag_value(args, "--theirs"))
    else {
        bail!(
            "usage: doom-verify --goldens | --ours <file> --theirs <file> [--sim-only] | --demo <name>"
        );
    };
    let sim_only = args.iter().any(|a| a == "--sim-only");
    let ours_text = fs::read_to_string(ours).with_context(|| format!("reading --ours {ours}"))?;
    let theirs_text =
        fs::read_to_string(theirs).with_context(|| format!("reading --theirs {theirs}"))?;
    match sync_diff_opt(&ours_text, &theirs_text, sim_only) {
        None => {
            let scope = if sim_only { " (sim fields only)" } else { "" };
            println!(
                "doom-verify: streams match ({} lines){scope}",
                ours_text.lines().count()
            );
            Ok(())
        }
        Some(d) => {
            print!("{}", d.render());
            bail!("sync streams diverge at line {}", d.line_no)
        }
    }
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

// ---------------------------------------------------------------------------
// --goldens: the bet twin vs the C-generated goldens
// ---------------------------------------------------------------------------

fn goldens(root: &Path) -> Result<()> {
    let twin = root
        .join("ports")
        .join("doom")
        .join("tools")
        .join("goldens.bet");
    if !twin.exists() {
        println!(
            "doom-verify --goldens: bet twin not written yet (W1) — expected {}.\n\
             The diff engine is ready; nothing to check.",
            twin.display()
        );
        return Ok(());
    }

    // Build the driver once, then run the twin on the interpreter.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = std::process::Command::new(&cargo)
        .args(["build", "-p", "driver"])
        .current_dir(root)
        .status()
        .context("running `cargo build -p driver`")?;
    if !status.success() {
        bail!("`cargo build -p driver` failed; cannot run the bet twin");
    }
    let bin = root
        .join("target")
        .join("debug")
        .join(format!("bet{}", std::env::consts::EXE_SUFFIX));
    let out = std::process::Command::new(&bin)
        .arg("run")
        .arg(&twin)
        .output()
        .with_context(|| format!("running `bet run {}`", twin.display()))?;
    if !out.status.success() {
        bail!(
            "bet twin errored (exit {}):\n{}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let sections = split_golden_sections(&stdout)?;

    let dir = root.join("ports").join("doom").join("goldens");
    let mut problems = Vec::new();
    for name in GOLDEN_NAMES {
        let Some(twin_text) = sections.iter().find(|(n, _)| n == name).map(|(_, t)| t) else {
            problems.push(format!("  twin printed no `#GOLDEN {name}` section"));
            continue;
        };
        let path = dir.join(format!("{name}.golden"));
        if !path.exists() {
            bail!(
                "golden not found: {}\n\
                 The `{name}.golden` tables are gitignored (id-derived output), so a fresh checkout\n\
                 has none. Regenerate them from the reference source with:\n    \
                 cargo xtask-doom doom-golden-gen\n\
                 (making --goldens runnable in CI without that step is tracked in #114).",
                path.display()
            );
        }
        let golden =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        if let Some(d) = first_line_diff(twin_text, &golden) {
            problems.push(format!("  {name}.golden: {d}"));
        }
    }
    if problems.is_empty() {
        println!(
            "doom-verify --goldens OK: all {} sections match",
            GOLDEN_NAMES.len()
        );
        Ok(())
    } else {
        bail!("golden mismatches:\n{}", problems.join("\n"));
    }
}

/// Split twin stdout on `#GOLDEN <name>` marker lines into (name, section text) pairs.
fn split_golden_sections(stdout: &str) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in stdout.lines() {
        if let Some(name) = line.strip_prefix("#GOLDEN ") {
            out.push((name.trim().to_string(), String::new()));
        } else if let Some((_, text)) = out.last_mut() {
            text.push_str(line);
            text.push('\n');
        } else if !line.trim().is_empty() {
            bail!("twin output before the first #GOLDEN marker: {line:?}");
        }
    }
    Ok(out)
}

/// First differing line between two texts, as a short human message.
fn first_line_diff(ours: &str, theirs: &str) -> Option<String> {
    let a: Vec<&str> = ours.lines().collect();
    let b: Vec<&str> = theirs.lines().collect();
    for i in 0..a.len().max(b.len()) {
        match (a.get(i), b.get(i)) {
            (Some(x), Some(y)) if x == y => continue,
            (Some(x), Some(y)) => {
                return Some(format!("line {}: twin {x:?} != golden {y:?}", i + 1));
            }
            (Some(x), None) => {
                return Some(format!("line {}: twin has extra {x:?}", i + 1));
            }
            (None, Some(y)) => {
                return Some(format!(
                    "line {}: twin ends early (golden has {y:?})",
                    i + 1
                ));
            }
            (None, None) => unreachable!(),
        }
    }
    None
}

// ---------------------------------------------------------------------------
// --demo: oracle side runs; bet side is W2/W3
// ---------------------------------------------------------------------------

fn demo_mode(args: &[String], root: &Path, demo: &str) -> Result<()> {
    let theirs = crate::doom_oracle::produce_sync(args, root, demo)?;
    println!(
        "doom-verify --demo {demo}: oracle stream ready at {}.\n\
         bet port not built yet (W2/W3) — nothing to diff against; once the port emits its\n\
         own stream, run `cargo xtask doom-verify --ours <port.sync> --theirs {}`.",
        theirs.display(),
        theirs.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// the sync-stream diff engine
// ---------------------------------------------------------------------------

/// Where a divergence points: the subsystem to debug first.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Triage {
    /// A `SETUP` line differs — the map loader / spawn pipeline diverged.
    Loader,
    /// Only the framebuffer crc (`C=`) differs — game state agrees; suspect the renderer.
    Renderer,
    /// Any sim-state field (R/X/Y/A first; also Z/MX/MY/H/S/LT/T) differs — the simulation.
    Sim,
}

#[derive(Debug)]
pub struct Divergence {
    /// 1-based line number of the first divergent line.
    pub line_no: usize,
    /// Up to 3 preceding (matching) lines, for context.
    pub context: Vec<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
    /// (field, ours value, theirs value) for each differing `K=V` field.
    pub fields: Vec<(String, String, String)>,
    pub triage: Triage,
}

impl Divergence {
    pub fn render(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "sync-stream divergence at line {}:", self.line_no);
        for c in &self.context {
            let _ = writeln!(s, "        {c}");
        }
        match (&self.ours, &self.theirs) {
            (Some(o), Some(t)) => {
                let _ = writeln!(s, "  ours: {o}");
                let _ = writeln!(s, "theirs: {t}");
            }
            (Some(o), None) => {
                let _ = writeln!(s, "  ours: {o}");
                let _ = writeln!(s, "theirs: <stream ended>");
            }
            (None, Some(t)) => {
                let _ = writeln!(s, "  ours: <stream ended>");
                let _ = writeln!(s, "theirs: {t}");
            }
            (None, None) => {}
        }
        if !self.fields.is_empty() {
            let _ = writeln!(s, "differing fields:");
            for (k, o, t) in &self.fields {
                let _ = writeln!(s, "  {k}: ours={o} theirs={t}");
            }
        }
        let hint = match self.triage {
            Triage::Loader => "SETUP block differs -> map loader / spawn order (P_SetupLevel)",
            Triage::Renderer => "framebuffer crc only -> renderer (game state agrees at this tic)",
            Triage::Sim => "sim state moved first -> game simulation (P_Ticker path)",
        };
        let _ = writeln!(s, "triage: {hint}");
        s
    }
}

/// Compare two sync streams line-by-line; `None` when byte-identical (modulo a trailing
/// newline). Reports the FIRST divergent line — everything after the first divergence is
/// noise, since the sim is a chaotic system.
/// When `sim_only` is set the framebuffer crc field (`C=`) is ignored for both the
/// line-equality test and the field diff — so the first reported divergence is always
/// among the sim fields R/X/Y/Z/A/MX/MY/H/S/LT (the renderer's `C` is the sibling's
/// problem). With `sim_only = false` this is a strict, crc-sensitive line diff.
pub fn sync_diff_opt(ours: &str, theirs: &str, sim_only: bool) -> Option<Divergence> {
    let a: Vec<&str> = ours.lines().collect();
    let b: Vec<&str> = theirs.lines().collect();
    let n = a.len().max(b.len());
    for i in 0..n {
        let (x, y) = (a.get(i).copied(), b.get(i).copied());
        let equal = if sim_only {
            match (x, y) {
                (Some(x), Some(y)) => strip_c(x) == strip_c(y),
                (None, None) => true,
                _ => false,
            }
        } else {
            x == y
        };
        if equal {
            continue;
        }
        let start = i.saturating_sub(3);
        let context = a[start..i].iter().map(|s| s.to_string()).collect();
        let fields = match (x, y) {
            (Some(x), Some(y)) => field_diff(x, y, sim_only),
            _ => Vec::new(),
        };
        let is_setup = x.map(|l| l.starts_with("SETUP")).unwrap_or(false)
            || y.map(|l| l.starts_with("SETUP")).unwrap_or(false);
        let triage = if is_setup {
            Triage::Loader
        } else if !fields.is_empty() && fields.iter().all(|(k, _, _)| k == "C") {
            Triage::Renderer
        } else {
            Triage::Sim
        };
        return Some(Divergence {
            line_no: i + 1,
            context,
            ours: x.map(str::to_string),
            theirs: y.map(str::to_string),
            fields,
            triage,
        });
    }
    None
}

/// Drop the framebuffer-crc token (`C=...`) from a fingerprint line, so the remaining
/// text is the sim-only fingerprint. Used by `--sim-only`.
fn strip_c(line: &str) -> String {
    line.split_whitespace()
        .filter(|tok| !tok.starts_with("C="))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse `K=V` tokens out of both lines and list the fields whose values differ (fields
/// missing on one side diff against `<absent>`). When `sim_only`, the `C` field is
/// omitted from the report.
fn field_diff(ours: &str, theirs: &str, sim_only: bool) -> Vec<(String, String, String)> {
    let pa = parse_fields(ours);
    let pb = parse_fields(theirs);
    let mut keys: Vec<&String> = pa.keys().collect();
    for k in pb.keys() {
        if !pa.contains_key(k) {
            keys.push(k);
        }
    }
    let absent = "<absent>".to_string();
    let mut out = Vec::new();
    for k in keys {
        if sim_only && k == "C" {
            continue;
        }
        let va = pa.get(k).unwrap_or(&absent);
        let vb = pb.get(k).unwrap_or(&absent);
        if va != vb {
            out.push((k.clone(), va.clone(), vb.clone()));
        }
    }
    out
}

fn parse_fields(line: &str) -> indexmap_lite::OrderedMap {
    let mut m = indexmap_lite::OrderedMap::new();
    for tok in line.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            m.insert(k.to_string(), v.to_string());
        }
    }
    m
}

/// A tiny insertion-ordered map (avoids an external crate for one use).
mod indexmap_lite {
    pub struct OrderedMap {
        pairs: Vec<(String, String)>,
    }
    impl OrderedMap {
        pub fn new() -> Self {
            OrderedMap { pairs: Vec::new() }
        }
        pub fn insert(&mut self, k: String, v: String) {
            self.pairs.push((k, v));
        }
        pub fn get(&self, k: &str) -> Option<&String> {
            self.pairs.iter().find(|(pk, _)| pk == k).map(|(_, v)| v)
        }
        pub fn contains_key(&self, k: &str) -> bool {
            self.get(k).is_some()
        }
        pub fn keys(&self) -> impl Iterator<Item = &String> {
            self.pairs.iter().map(|(k, _)| k)
        }
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const L1: &str = "T=00000001 R=00000000 X=00100000 Y=00200000 Z=00000000 A=40000000 MX=00000000 MY=00000000 H=00000064 S=00000005 LT=00000001 C=deadbeef";
    const L2: &str = "T=00000002 R=00000003 X=00110000 Y=00200000 Z=00000000 A=40000000 MX=00010000 MY=00000000 H=00000064 S=00000005 LT=00000002 C=cafef00d";

    #[test]
    fn identical_streams_match() {
        let s = format!("{L1}\n{L2}\n");
        assert!(sync_diff_opt(&s, &s, false).is_none());
    }

    #[test]
    fn sim_divergence_triaged_with_context_and_fields() {
        let ours = format!("{L1}\n{L2}\nT=00000003 R=00000007 X=00120000 Y=00200000 C=11111111\n");
        let theirs =
            format!("{L1}\n{L2}\nT=00000003 R=00000009 X=00121000 Y=00200000 C=22222222\n");
        let d = sync_diff_opt(&ours, &theirs, false).expect("diverges");
        assert_eq!(d.line_no, 3);
        assert_eq!(d.context.len(), 2); // only two preceding lines exist
        assert_eq!(d.triage, Triage::Sim);
        let keys: Vec<&str> = d.fields.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["R", "X", "C"]);
        assert_eq!(d.fields[0].1, "00000007");
        assert_eq!(d.fields[0].2, "00000009");
        let rendered = d.render();
        assert!(rendered.contains("triage: sim state moved first"));
    }

    #[test]
    fn sim_only_ignores_crc_and_finds_first_sim_field() {
        // Two tics that agree on every sim field but differ in C: sim-only sees no
        // divergence there, and reports the later tic where X actually diverges.
        let l2b = L2.replace("C=cafef00d", "C=deadbeef");
        let ours = format!("{L1}\n{L2}\nT=00000003 R=00000007 X=00120000 Y=00200000 C=11111111\n");
        let theirs =
            format!("{L1}\n{l2b}\nT=00000003 R=00000007 X=00121000 Y=00200000 C=22222222\n");
        let d = sync_diff_opt(&ours, &theirs, true).expect("diverges on X");
        assert_eq!(d.line_no, 3);
        assert_eq!(d.triage, Triage::Sim);
        // C must not appear in the reported fields under --sim-only.
        let keys: Vec<&str> = d.fields.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["X"]);
    }

    #[test]
    fn sim_only_crc_only_divergence_matches() {
        // If ONLY C differs across the whole stream, sim-only reports a full match.
        let ours = format!("{L1}\n{L2}\n");
        let theirs = format!("{L1}\n{}\n", L2.replace("C=cafef00d", "C=00000000"));
        assert!(sync_diff_opt(&ours, &theirs, true).is_none());
        // ...but the default (crc-sensitive) diff still catches it.
        assert!(sync_diff_opt(&ours, &theirs, false).is_some());
    }

    #[test]
    fn crc_only_divergence_is_renderer() {
        let ours = format!("{L1}\n{L2}\n");
        let theirs = format!("{L1}\n{}\n", L2.replace("C=cafef00d", "C=00000000"));
        let d = sync_diff_opt(&ours, &theirs, false).expect("diverges");
        assert_eq!(d.triage, Triage::Renderer);
        assert_eq!(d.fields.len(), 1);
        assert_eq!(d.fields[0].0, "C");
    }

    #[test]
    fn setup_divergence_is_loader() {
        let ours = "SETUP sectors=00000010 lines=00000040 things=00000005\nSETUP T i=00000000 type=00000001 x=00100000 y=00100000 a=00000000\n";
        let theirs = "SETUP sectors=00000010 lines=00000040 things=00000005\nSETUP T i=00000000 type=00000002 x=00100000 y=00100000 a=00000000\n";
        let d = sync_diff_opt(ours, theirs, false).expect("diverges");
        assert_eq!(d.line_no, 2);
        assert_eq!(d.triage, Triage::Loader);
        assert_eq!(d.fields[0].0, "type");
    }

    #[test]
    fn truncated_stream_reports_end() {
        let ours = format!("{L1}\n{L2}\n");
        let theirs = format!("{L1}\n");
        let d = sync_diff_opt(&ours, &theirs, false).expect("diverges");
        assert_eq!(d.line_no, 2);
        assert!(d.theirs.is_none());
        assert!(d.render().contains("<stream ended>"));
    }

    #[test]
    fn golden_section_split() {
        let out = "#GOLDEN fixed\nFIXEDMUL a=00000001 b=00000001 r=00000000\n#GOLDEN random\nRNDTABLE i=00000000 v=00000000\n";
        let sections = split_golden_sections(out).unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "fixed");
        assert_eq!(sections[0].1, "FIXEDMUL a=00000001 b=00000001 r=00000000\n");
        assert_eq!(sections[1].0, "random");
    }
}
