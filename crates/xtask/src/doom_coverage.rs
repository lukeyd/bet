//! `cargo xtask doom-coverage` — port-progress accounting for the DOOM port.
//!
//! Reads the frozen function census `ports/doom/goldens/inventory.txt` (`<file> <fn> <line>`
//! per line, from `doom-gen --inventory`) and, for each function, greps the reference `.c`
//! for a marker comment within the 5 lines above the definition line:
//!     // PORTED: <note>            the function is fully ported to bet
//!     // PORTED-PARTIAL: <note>    partially ported (counted as ported, shown separately)
//!     // SKIPPED: <note>           deliberately not ported (platform code, dead code, ...)
//! Everything unmarked is todo. Prints per-file and total %ported / %skipped / %todo.
//!
//! NOTE: the markers live in the reference tree's local git checkout (outside this repo).
//! They are the one sanctioned annotation the porting workstreams add above function
//! definitions as they port; nothing else in the reference files may be touched. Right now
//! nothing is marked, so 100% todo is the correct baseline.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::doom::doom_ref_from;

#[derive(Default, Clone, Copy)]
struct Counts {
    ported: usize,
    partial: usize,
    skipped: usize,
    todo: usize,
}

impl Counts {
    fn total(&self) -> usize {
        self.ported + self.partial + self.skipped + self.todo
    }
}

pub fn run(args: &[String], root: &Path) -> Result<()> {
    let doom_ref = doom_ref_from(args);
    let inv_path = root
        .join("ports")
        .join("doom")
        .join("goldens")
        .join("inventory.txt");
    let inv = fs::read_to_string(&inv_path).with_context(|| {
        format!(
            "reading {} (run `cargo xtask doom-gen --inventory` first)",
            inv_path.display()
        )
    })?;

    let mut per_file: BTreeMap<String, Counts> = BTreeMap::new();
    let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (ln, entry) in inv.lines().enumerate() {
        if entry.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = entry.split_whitespace().collect();
        let [file, _fn_name, line] = parts.as_slice() else {
            bail!("inventory.txt line {}: malformed entry {entry:?}", ln + 1);
        };
        let line: usize = line
            .parse()
            .with_context(|| format!("inventory.txt line {}: bad line number", ln + 1))?;

        let src_lines = match sources.get(*file) {
            Some(v) => v,
            None => {
                let text = fs::read_to_string(doom_ref.join(file))
                    .with_context(|| format!("reading reference {file}"))?;
                sources.insert(file.to_string(), text.lines().map(str::to_string).collect());
                &sources[*file]
            }
        };

        // Look for a marker within the 5 lines above the definition line (1-based).
        let lo = line.saturating_sub(6); // 0-based index of (line - 5)
        let hi = line.saturating_sub(1).min(src_lines.len());
        let mut mark = MarkerKind::Todo;
        for l in &src_lines[lo..hi] {
            let t = l.trim();
            if let Some(rest) = t.strip_prefix("//") {
                let rest = rest.trim_start();
                if rest.starts_with("PORTED-PARTIAL:") {
                    mark = MarkerKind::Partial;
                } else if rest.starts_with("PORTED:") {
                    mark = MarkerKind::Ported;
                } else if rest.starts_with("SKIPPED:") {
                    mark = MarkerKind::Skipped;
                }
            }
        }
        let c = per_file.entry(file.to_string()).or_default();
        match mark {
            MarkerKind::Ported => c.ported += 1,
            MarkerKind::Partial => c.partial += 1,
            MarkerKind::Skipped => c.skipped += 1,
            MarkerKind::Todo => c.todo += 1,
        }
    }

    let mut total = Counts::default();
    println!(
        "{:<14} {:>5} {:>7} {:>8} {:>5} {:>8} {:>8} {:>7}",
        "file", "fns", "ported", "partial", "skip", "%ported", "%skipped", "%todo"
    );
    for (file, c) in &per_file {
        print_row(file, *c);
        total.ported += c.ported;
        total.partial += c.partial;
        total.skipped += c.skipped;
        total.todo += c.todo;
    }
    println!("{}", "-".repeat(72));
    print_row("TOTAL", total);
    Ok(())
}

enum MarkerKind {
    Ported,
    Partial,
    Skipped,
    Todo,
}

fn print_row(name: &str, c: Counts) {
    let n = c.total().max(1) as f64;
    println!(
        "{:<14} {:>5} {:>7} {:>8} {:>5} {:>7.1}% {:>7.1}% {:>6.1}%",
        name,
        c.total(),
        c.ported,
        c.partial,
        c.skipped,
        (c.ported + c.partial) as f64 / n * 100.0,
        c.skipped as f64 / n * 100.0,
        c.todo as f64 / n * 100.0,
    );
}
