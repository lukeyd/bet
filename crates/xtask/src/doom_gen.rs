//! `cargo xtask doom-gen` — deterministic codegen: id's linuxdoom-1.10 data tables → `bet`
//! modules under `ports/doom/defs/`.
//!
//! Hand-rolled parsers over id's uniformly-formatted initializer lists (info.h/info.c,
//! sounds.h/sounds.c, tables.c, plus the `MF_*` flag enum in p_mobj.h). Numeric table
//! literals are copied VERBATIM — never recomputed — so id's exact 1993 rounding is
//! preserved bit-for-bit.
//!
//! Flags:
//!   --doom-ref <path>  reference tree (default: crate::doom::DEFAULT_DOOM_REF)
//!   --check            regenerate in memory and byte-diff against the committed files
//!   --inventory        scan every reference .c for function definitions →
//!                      ports/doom/goldens/inventory.txt (frozen once committed)

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::doom::{crc32, doom_ref_from};

/// ≤100 `v.stack(..)` calls per init-filler fn (states/sfx); mobjinfo rows are much wider, so
/// they chunk at 50; raw table stores chunk at 512 per fn.
const STATES_CHUNK: usize = 100;
const MOBJ_CHUNK: usize = 50;
const SFX_CHUNK: usize = 100;
const TABLE_CHUNK: usize = 512;

pub fn run(args: &[String], root: &Path) -> Result<()> {
    let doom_ref = doom_ref_from(args);
    if args.iter().any(|a| a == "--inventory") {
        return inventory(&doom_ref, root);
    }

    let generated = generate(&doom_ref)?;
    let out_dir = root.join("ports").join("doom").join("defs");

    if args.iter().any(|a| a == "--check") {
        let mut drift: Vec<String> = Vec::new();
        for (name, text) in &generated {
            let path = out_dir.join(name);
            match fs::read(&path) {
                Ok(bytes) if bytes == text.as_bytes() => {}
                Ok(_) => drift.push(format!("  {} is stale (differs from regeneration)", name)),
                Err(e) => drift.push(format!("  {} unreadable: {e}", name)),
            }
        }
        if drift.is_empty() {
            println!(
                "doom-gen --check OK ({} generated modules match)",
                generated.len()
            );
            Ok(())
        } else {
            bail!(
                "doom-gen --check found drift (run `cargo xtask doom-gen` and commit):\n{}",
                drift.join("\n")
            );
        }
    } else {
        fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
        for (name, text) in &generated {
            let path = out_dir.join(name);
            fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
            println!(
                "doom-gen: wrote {} ({} bytes, {} lines)",
                path.display(),
                text.len(),
                text.lines().count()
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// input model
// ---------------------------------------------------------------------------

/// One row of id's `states[]`: 7 fields + the trailing `// S_NAME` row comment.
struct StateRow {
    sprite: String,
    frame: i64,
    tics: i64,
    /// `Some("A_Light0")` for `{A_Light0}` entries, `None` for `{NULL}`.
    action: Option<String>,
    next: String,
    misc1: i64,
    misc2: i64,
}

/// One row of `S_sfx[]` (the trailing `data` field, always 0, is dropped).
struct SfxRow {
    name: String,
    singularity: i64,
    priority: i64,
    /// Index into S_sfx for `&S_sfx[sfx_x]` links; -1 for no link (id writes `0`).
    link: i64,
    pitch: i64,
    volume: i64,
}

struct ParsedRef {
    // enums, in ordinal order (count markers excluded)
    statenames: Vec<String>,
    spritenames: Vec<String>,
    mobjnames: Vec<String>,
    sfxnames: Vec<String>,
    musnames: Vec<String>,
    // data tables
    sprnames: Vec<String>,
    states: Vec<StateRow>,
    /// 23 evaluated i32 fields per mobj, in mobjinfo field order.
    mobjinfo: Vec<[i64; 23]>,
    sfx: Vec<SfxRow>,
    /// Verbatim numeric literals.
    finetangent: Vec<String>,
    finesine: Vec<String>,
    tantoangle: Vec<String>,
    /// Distinct `A_*` action names, sorted alphabetically → id 1.. (AID_NONE = 0).
    actions: Vec<String>,
    // header crc line fragments per output module
    crc_info: String,
    crc_tables: String,
    crc_sounds: String,
}

const MOBJ_FIELDS: [&str; 23] = [
    "doomednum",
    "spawnstate",
    "spawnhealth",
    "seestate",
    "seesound",
    "reactiontime",
    "attacksound",
    "painstate",
    "painchance",
    "painsound",
    "meleestate",
    "missilestate",
    "deathstate",
    "xdeathstate",
    "deathsound",
    "speed",
    "radius",
    "height",
    "mass",
    "damage",
    "activesound",
    "flags",
    "raisestate",
];

fn generate(doom_ref: &Path) -> Result<Vec<(String, String)>> {
    let read = |name: &str| -> Result<String> {
        let p = doom_ref.join(name);
        fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))
    };
    let info_h = read("info.h")?;
    let info_c = read("info.c")?;
    let sounds_h = read("sounds.h")?;
    let sounds_c = read("sounds.c")?;
    let tables_c = read("tables.c")?;
    let p_mobj_h = read("p_mobj.h")?;

    let parsed = parse_all(&info_h, &info_c, &sounds_h, &sounds_c, &tables_c, &p_mobj_h)?;

    Ok(vec![
        ("info.bet".to_string(), emit_info(&parsed)),
        ("tables.bet".to_string(), emit_tables(&parsed)),
        ("sounds.bet".to_string(), emit_sounds(&parsed)),
    ])
}

fn parse_all(
    info_h: &str,
    info_c: &str,
    sounds_h: &str,
    sounds_c: &str,
    tables_c: &str,
    p_mobj_h: &str,
) -> Result<ParsedRef> {
    // Comment-stripped views (1:1 byte offsets preserved; string literals kept).
    let info_h_s = strip_comments(info_h, false);
    let info_c_s = strip_comments(info_c, false);
    let sounds_h_s = strip_comments(sounds_h, false);
    let tables_c_s = strip_comments(tables_c, false);
    let p_mobj_h_s = strip_comments(p_mobj_h, false);

    let spritenames = parse_enum(&info_h_s, "} spritenum_t", "SPR_", "NUMSPRITES")?;
    let statenames = parse_enum(&info_h_s, "} statenum_t", "S_", "NUMSTATES")?;
    let mobjnames = parse_enum(&info_h_s, "} mobjtype_t", "MT_", "NUMMOBJTYPES")?;
    let sfxnames = parse_enum(&sounds_h_s, "} sfxenum_t", "sfx_", "NUMSFX")?;
    let musnames = parse_enum(&sounds_h_s, "} musicenum_t", "mus_", "NUMMUSIC")?;

    let sprite_ord = ordinals(&spritenames);
    let state_ord = ordinals(&statenames);
    let sfx_ord = ordinals(&sfxnames);
    let mf_flags = parse_mf_flags(&p_mobj_h_s)?;

    let sprnames = parse_sprnames(&info_c_s, spritenames.len())?;
    let (states, actions) = parse_states(info_c, &info_c_s, &statenames, &sprite_ord, &state_ord)?;
    let mobjinfo = parse_mobjinfo(
        info_c, &info_c_s, &mobjnames, &state_ord, &sfx_ord, &mf_flags,
    )?;
    let sfx = parse_sfx(sounds_c, &sfx_ord)?;
    if sfx.len() != sfxnames.len() {
        bail!(
            "S_sfx has {} rows but sfxenum_t has {} entries",
            sfx.len(),
            sfxnames.len()
        );
    }

    let finetangent = parse_table(&tables_c_s, "finetangent[", 4096)?;
    let finesine = parse_table(&tables_c_s, "finesine[", 5 * 8192 / 4)?;
    let tantoangle = parse_table(&tables_c_s, "tantoangle[", 2049)?;

    let crc = |name: &str, src: &str| format!("{name}={:08x}", crc32(src.as_bytes()));
    Ok(ParsedRef {
        statenames,
        spritenames,
        mobjnames,
        sfxnames,
        musnames,
        sprnames,
        states,
        mobjinfo,
        sfx,
        finetangent,
        finesine,
        tantoangle,
        actions,
        crc_info: format!(
            "{} {} {} {}",
            crc("info.h", info_h),
            crc("info.c", info_c),
            crc("sounds.h", sounds_h),
            crc("p_mobj.h", p_mobj_h)
        ),
        crc_tables: crc("tables.c", tables_c),
        crc_sounds: format!(
            "{} {}",
            crc("sounds.h", sounds_h),
            crc("sounds.c", sounds_c)
        ),
    })
}

fn ordinals(names: &[String]) -> BTreeMap<String, i64> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i as i64))
        .collect()
}

// ---------------------------------------------------------------------------
// C-source scanning primitives
// ---------------------------------------------------------------------------

/// Blank out C comments (`//` and `/* */`) with spaces, preserving every byte offset and
/// newline. With `blank_strings`, string/char literal *contents* are blanked too (the quotes
/// remain) — used by the function-inventory scanner so braces in literals can't confuse it.
pub(crate) fn strip_comments(src: &str, blank_strings: bool) -> String {
    #[derive(PartialEq)]
    enum St {
        Code,
        Line,
        Block,
        Str,
        Chr,
    }
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut st = St::Code;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match st {
            St::Code => match c {
                b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                    st = St::Line;
                    out.push(b' ');
                }
                b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                    st = St::Block;
                    out.push(b' ');
                }
                b'"' => {
                    st = St::Str;
                    out.push(c);
                }
                b'\'' => {
                    st = St::Chr;
                    out.push(c);
                }
                _ => out.push(c),
            },
            St::Line => {
                if c == b'\n' {
                    st = St::Code;
                    out.push(c);
                } else {
                    out.push(b' ');
                }
            }
            St::Block => {
                if c == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                    st = St::Code;
                    continue;
                }
                out.push(if c == b'\n' { b'\n' } else { b' ' });
            }
            St::Str => {
                if c == b'\\' && i + 1 < b.len() {
                    out.push(if blank_strings { b' ' } else { c });
                    out.push(if blank_strings { b' ' } else { b[i + 1] });
                    i += 2;
                    continue;
                }
                if c == b'"' {
                    st = St::Code;
                    out.push(c);
                } else if blank_strings {
                    out.push(if c == b'\n' { b'\n' } else { b' ' });
                } else {
                    out.push(c);
                }
            }
            St::Chr => {
                if c == b'\\' && i + 1 < b.len() {
                    out.push(if blank_strings { b' ' } else { c });
                    out.push(if blank_strings { b' ' } else { b[i + 1] });
                    i += 2;
                    continue;
                }
                if c == b'\'' {
                    st = St::Code;
                    out.push(c);
                } else if blank_strings {
                    out.push(b' ');
                } else {
                    out.push(c);
                }
            }
        }
        i += 1;
    }
    String::from_utf8(out).expect("strip_comments preserves UTF-8 structure")
}

/// Parse a C enum by its closing marker (e.g. `"} statenum_t"`): every identifier between the
/// nearest preceding `typedef enum` and the marker, in order. The final identifier must be the
/// count marker (`NUMSTATES`, excluded from the result); all others must carry `prefix`.
fn parse_enum(
    stripped: &str,
    end_marker: &str,
    prefix: &str,
    count_name: &str,
) -> Result<Vec<String>> {
    let end = stripped
        .find(end_marker)
        .with_context(|| format!("enum end marker {end_marker:?} not found"))?;
    let start = stripped[..end]
        .rfind("typedef enum")
        .with_context(|| format!("`typedef enum` before {end_marker:?} not found"))?;
    let body = &stripped[start + "typedef enum".len()..end];

    let mut names: Vec<String> = Vec::new();
    for tok in idents(body) {
        names.push(tok);
    }
    let last = names
        .pop()
        .with_context(|| format!("enum {end_marker:?} is empty"))?;
    if last != count_name {
        bail!("enum {end_marker:?}: expected final count marker {count_name}, got {last}");
    }
    for n in &names {
        if !n.starts_with(prefix) {
            bail!("enum {end_marker:?}: entry {n} lacks prefix {prefix}");
        }
    }
    Ok(names)
}

/// All C identifiers in `s`, in order.
fn idents(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(s[start..i].to_string());
        } else {
            i += 1;
        }
    }
    out
}

/// Slice out the `{ ... }` initializer body following `marker` (offsets valid for both the
/// stripped view and the raw source, since stripping is 1:1).
fn init_body_span(stripped: &str, marker: &str) -> Result<(usize, usize)> {
    let m = stripped
        .find(marker)
        .with_context(|| format!("marker {marker:?} not found"))?;
    let open = stripped[m..]
        .find('{')
        .with_context(|| format!("no `{{` after {marker:?}"))?
        + m;
    // These are flat (states/sfx rows contain one nesting level; mobjinfo two), so track depth.
    let b = stripped.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok((open + 1, i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    bail!("unterminated initializer after {marker:?}");
}

fn parse_int(s: &str) -> Result<i64> {
    let t = s.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest.trim()),
        None => (false, t),
    };
    let v = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16)
    } else {
        t.parse::<i64>()
    }
    .with_context(|| format!("bad integer literal {s:?}"))?;
    Ok(if neg { -v } else { v })
}

// ---------------------------------------------------------------------------
// info.c parsers
// ---------------------------------------------------------------------------

fn parse_sprnames(info_c_stripped: &str, expected: usize) -> Result<Vec<String>> {
    let (a, b) = init_body_span(info_c_stripped, "sprnames[NUMSPRITES]")?;
    let body = &info_c_stripped[a..b];
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(q) = rest.find('"') {
        let after = &rest[q + 1..];
        let close = after.find('"').context("unterminated sprite name string")?;
        out.push(after[..close].to_string());
        rest = &after[close + 1..];
    }
    if out.len() != expected {
        bail!("sprnames: {} entries, expected {expected}", out.len());
    }
    for (i, n) in out.iter().enumerate() {
        if n.len() != 4 {
            bail!("sprnames[{i}] = {n:?} is not 4 chars");
        }
    }
    Ok(out)
}

/// Parse `states[NUMSTATES]`. Every row sits on one line:
/// `{SPR_TROO,0,-1,{NULL},S_NULL,0,0},\t// S_NULL` — 7 fields, the 4th a braced action
/// (`{NULL}` or `{A_Name}`), then the row's statenum name as a trailing `//` comment (validated
/// against info.h's enum order). Returns the rows plus the distinct action names sorted
/// alphabetically (the AID_* universe).
fn parse_states(
    info_c_raw: &str,
    info_c_stripped: &str,
    statenames: &[String],
    sprite_ord: &BTreeMap<String, i64>,
    state_ord: &BTreeMap<String, i64>,
) -> Result<(Vec<StateRow>, Vec<String>)> {
    let (a, b) = init_body_span(info_c_stripped, "states[NUMSTATES]")?;
    let raw_body = &info_c_raw[a..b];

    let mut rows: Vec<StateRow> = Vec::new();
    let mut action_set: std::collections::BTreeSet<String> = Default::default();

    for line in raw_body.lines() {
        let t = line.trim_start();
        if !t.starts_with('{') {
            continue;
        }
        let idx = rows.len();
        let row = parse_state_row(t)
            .with_context(|| format!("states[{idx}] (line: {})", t.trim_end()))?;
        // Cross-validate the row comment against the enum ordinal from info.h.
        let (row, comment) = row;
        if let Some(name) = &comment {
            let want = &statenames[idx];
            if name != want {
                bail!("states[{idx}] row comment says {name}, info.h enum says {want}");
            }
        } else {
            bail!("states[{idx}] has no trailing // S_* comment");
        }
        if !sprite_ord.contains_key(&row.sprite) {
            bail!("states[{idx}]: unknown sprite {}", row.sprite);
        }
        if !state_ord.contains_key(&row.next) {
            bail!("states[{idx}]: unknown nextstate {}", row.next);
        }
        if let Some(act) = &row.action {
            if !act.starts_with("A_") {
                bail!("states[{idx}]: action {act} lacks A_ prefix");
            }
            action_set.insert(act.clone());
        }
        rows.push(row);
    }
    if rows.len() != statenames.len() {
        bail!(
            "states[]: {} rows, expected {} (statenum_t)",
            rows.len(),
            statenames.len()
        );
    }
    Ok((rows, action_set.into_iter().collect()))
}

/// `{SPR_X,frame,tics,{ACTION},S_NEXT,misc1,misc2},  // S_NAME` → row + comment name.
fn parse_state_row(line: &str) -> Result<(StateRow, Option<String>)> {
    let mut c = Cursor::new(line);
    c.expect('{')?;
    let sprite = c.ident()?;
    c.expect(',')?;
    let frame = c.int()?;
    c.expect(',')?;
    let tics = c.int()?;
    c.expect(',')?;
    c.expect('{')?;
    let action_name = c.ident()?;
    let action = if action_name == "NULL" {
        None
    } else {
        Some(action_name)
    };
    c.expect('}')?;
    c.expect(',')?;
    let next = c.ident()?;
    c.expect(',')?;
    let misc1 = c.int()?;
    c.expect(',')?;
    let misc2 = c.int()?;
    c.expect('}')?;
    let comment = c.trailing_comment_ident();
    Ok((
        StateRow {
            sprite,
            frame,
            tics,
            action,
            next,
            misc1,
            misc2,
        },
        comment,
    ))
}

/// Parse `mobjinfo[NUMMOBJTYPES]`: 137 blocks of `{  // MT_NAME` + 23 `value,  // field` lines.
/// Field comments are validated against [`MOBJ_FIELDS`] order; block comments against the
/// mobjtype_t enum order; value expressions are evaluated (ints, `N*FRACUNIT`, `S_*`/`sfx_*`
/// ordinals, `MF_*|...` flag ORs).
fn parse_mobjinfo(
    info_c_raw: &str,
    info_c_stripped: &str,
    mobjnames: &[String],
    state_ord: &BTreeMap<String, i64>,
    sfx_ord: &BTreeMap<String, i64>,
    mf_flags: &BTreeMap<String, i64>,
) -> Result<Vec<[i64; 23]>> {
    let (a, b) = init_body_span(info_c_stripped, "mobjinfo[NUMMOBJTYPES]")?;
    let raw_body = &info_c_raw[a..b];

    let mut out: Vec<[i64; 23]> = Vec::new();
    let mut cur: Vec<i64> = Vec::new();
    let mut in_block = false;

    for line in raw_body.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let (code, comment) = split_line_comment(t);
        let code = code.trim().trim_end_matches(',').trim();
        if !in_block {
            if code == "{" {
                in_block = true;
                cur.clear();
                let bi = out.len();
                let name = comment
                    .with_context(|| format!("mobjinfo[{bi}]: `{{` line lacks // MT_* comment"))?
                    .trim()
                    .to_string();
                if bi >= mobjnames.len() || name != mobjnames[bi] {
                    bail!(
                        "mobjinfo[{bi}] block comment {name} does not match enum {}",
                        mobjnames.get(bi).map(String::as_str).unwrap_or("<none>")
                    );
                }
            } else if !code.is_empty() {
                bail!("unexpected line between mobjinfo blocks: {t:?}");
            }
            continue;
        }
        // in a block
        if code == "}" {
            let bi = out.len();
            if cur.len() != 23 {
                bail!("mobjinfo[{bi}]: {} fields, expected 23", cur.len());
            }
            let mut arr = [0i64; 23];
            arr.copy_from_slice(&cur);
            out.push(arr);
            in_block = false;
            continue;
        }
        if code.is_empty() {
            continue;
        }
        let fi = cur.len();
        let bi = out.len();
        let field = comment
            .with_context(|| format!("mobjinfo[{bi}] field {fi}: no // fieldname comment"))?
            .trim()
            .to_string();
        if fi >= 23 || field != MOBJ_FIELDS[fi] {
            bail!(
                "mobjinfo[{bi}] field {fi}: comment says {field}, expected {}",
                MOBJ_FIELDS.get(fi).copied().unwrap_or("<none>")
            );
        }
        let v = eval_expr(code, state_ord, sfx_ord, mf_flags)
            .with_context(|| format!("mobjinfo[{bi}].{field} = {code:?}"))?;
        if v < i32::MIN as i64 || v > u32::MAX as i64 {
            bail!("mobjinfo[{bi}].{field} = {v} out of 32-bit range");
        }
        cur.push(v);
    }
    if out.len() != mobjnames.len() {
        bail!(
            "mobjinfo[]: {} blocks, expected {}",
            out.len(),
            mobjnames.len()
        );
    }
    Ok(out)
}

/// Split a line at a `//` comment (the raw source line): `(code, Some(comment))`.
fn split_line_comment(line: &str) -> (&str, Option<&str>) {
    match line.find("//") {
        Some(p) => (&line[..p], Some(line[p + 2..].trim())),
        None => (line, None),
    }
}

/// Evaluate a mobjinfo value expression: `A|B|...` of atoms; atom = int literal, `N*FRACUNIT`,
/// or an identifier (`S_*` state ordinal, `sfx_*` sfx ordinal, `MF_*` flag bit).
fn eval_expr(
    expr: &str,
    state_ord: &BTreeMap<String, i64>,
    sfx_ord: &BTreeMap<String, i64>,
    mf_flags: &BTreeMap<String, i64>,
) -> Result<i64> {
    let mut acc: i64 = 0;
    for part in expr.split('|') {
        acc |= eval_atom(part.trim(), state_ord, sfx_ord, mf_flags)?;
    }
    Ok(acc)
}

fn eval_atom(
    atom: &str,
    state_ord: &BTreeMap<String, i64>,
    sfx_ord: &BTreeMap<String, i64>,
    mf_flags: &BTreeMap<String, i64>,
) -> Result<i64> {
    if let Some((l, r)) = atom.split_once('*') {
        let l = eval_atom(l.trim(), state_ord, sfx_ord, mf_flags)?;
        let r = eval_atom(r.trim(), state_ord, sfx_ord, mf_flags)?;
        return Ok(l * r);
    }
    let first = atom.chars().next().context("empty value expression")?;
    if first.is_ascii_digit() || first == '-' {
        return parse_int(atom);
    }
    if atom == "FRACUNIT" {
        return Ok(65536);
    }
    if let Some(v) = state_ord.get(atom) {
        return Ok(*v);
    }
    if let Some(v) = sfx_ord.get(atom) {
        return Ok(*v);
    }
    if let Some(v) = mf_flags.get(atom) {
        return Ok(*v);
    }
    bail!("unknown identifier {atom:?} in mobjinfo value")
}

/// `MF_NAME = value,` lines from p_mobj.h's mobjflag_t enum (decimal or hex values).
fn parse_mf_flags(p_mobj_h_stripped: &str) -> Result<BTreeMap<String, i64>> {
    let mut out = BTreeMap::new();
    for line in p_mobj_h_stripped.lines() {
        let t = line.trim();
        if !t.starts_with("MF_") {
            continue;
        }
        let Some((name, rest)) = t.split_once('=') else {
            continue;
        };
        let value = rest.trim().trim_end_matches(',').trim();
        out.insert(name.trim().to_string(), parse_int(value)?);
    }
    if out.is_empty() {
        bail!("no MF_* flags found in p_mobj.h");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// sounds.c / tables.c parsers
// ---------------------------------------------------------------------------

/// `S_sfx[]` rows: `{ "name", false, 64, 0, -1, -1, 0 },` — name, singularity, priority,
/// link (`0` → -1, `&S_sfx[sfx_x]` → x's ordinal), pitch, volume, data (must be 0; dropped).
fn parse_sfx(sounds_c_raw: &str, sfx_ord: &BTreeMap<String, i64>) -> Result<Vec<SfxRow>> {
    let stripped = strip_comments(sounds_c_raw, false);
    let (a, b) = init_body_span(&stripped, "S_sfx[] =")?;
    let raw_body = &sounds_c_raw[a..b];

    let mut out = Vec::new();
    for line in raw_body.lines() {
        let t = line.trim();
        if !t.starts_with('{') {
            continue;
        }
        let i = out.len();
        out.push(parse_sfx_row(t, sfx_ord).with_context(|| format!("S_sfx[{i}]: {t}"))?);
    }
    Ok(out)
}

fn parse_sfx_row(line: &str, sfx_ord: &BTreeMap<String, i64>) -> Result<SfxRow> {
    let mut c = Cursor::new(line);
    c.expect('{')?;
    let name = c.quoted()?;
    c.expect(',')?;
    let singularity = match c.ident()?.as_str() {
        "false" => 0,
        "true" => 1,
        other => bail!("bad singularity {other:?}"),
    };
    c.expect(',')?;
    let priority = c.int()?;
    c.expect(',')?;
    // link: `0` or `&S_sfx[sfx_x]`
    c.skip_ws();
    let link = if c.peek() == Some('&') {
        c.expect('&')?;
        let arr = c.ident()?;
        if arr != "S_sfx" {
            bail!("bad link array {arr:?}");
        }
        c.expect('[')?;
        let target = c.ident()?;
        c.expect(']')?;
        *sfx_ord
            .get(&target)
            .with_context(|| format!("unknown link target {target:?}"))?
    } else {
        let z = c.int()?;
        if z != 0 {
            bail!("non-zero non-pointer link {z}");
        }
        -1
    };
    c.expect(',')?;
    let pitch = c.int()?;
    c.expect(',')?;
    let volume = c.int()?;
    c.expect(',')?;
    let data = c.int()?;
    if data != 0 {
        bail!("S_sfx data field is {data}, expected 0");
    }
    c.expect('}')?;
    Ok(SfxRow {
        name,
        singularity,
        priority,
        link,
        pitch,
        volume,
    })
}

/// Extract a numeric table's literals VERBATIM (validated as integers, emitted untouched —
/// id's exact rounding is the contract).
fn parse_table(tables_c_stripped: &str, marker: &str, expected: usize) -> Result<Vec<String>> {
    let (a, b) = init_body_span(tables_c_stripped, marker)?;
    let body = &tables_c_stripped[a..b];
    let mut out = Vec::new();
    for tok in body.split(',') {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        parse_int(t).with_context(|| format!("table {marker:?}"))?; // validate only
        out.push(t.to_string());
    }
    if out.len() != expected {
        bail!(
            "table {marker:?}: {} entries, expected {expected}",
            out.len()
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// tiny single-line cursor
// ---------------------------------------------------------------------------

struct Cursor<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(s: &'a str) -> Self {
        Cursor { s, i: 0 }
    }
    fn skip_ws(&mut self) {
        let b = self.s.as_bytes();
        while self.i < b.len() && (b[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<char> {
        self.skip_ws();
        self.s[self.i..].chars().next()
    }
    fn expect(&mut self, ch: char) -> Result<()> {
        self.skip_ws();
        if self.s[self.i..].starts_with(ch) {
            self.i += ch.len_utf8();
            Ok(())
        } else {
            bail!("expected {ch:?} at column {} of {:?}", self.i, self.s)
        }
    }
    fn ident(&mut self) -> Result<String> {
        self.skip_ws();
        let b = self.s.as_bytes();
        let start = self.i;
        while self.i < b.len() && (b[self.i].is_ascii_alphanumeric() || b[self.i] == b'_') {
            self.i += 1;
        }
        if self.i == start {
            bail!("expected identifier at column {} of {:?}", self.i, self.s);
        }
        Ok(self.s[start..self.i].to_string())
    }
    fn int(&mut self) -> Result<i64> {
        self.skip_ws();
        let b = self.s.as_bytes();
        let start = self.i;
        if self.i < b.len() && (b[self.i] == b'-' || b[self.i] == b'+') {
            self.i += 1;
        }
        while self.i < b.len() && (b[self.i].is_ascii_alphanumeric()) {
            self.i += 1; // alnum: allows 0x... hex
        }
        parse_int(&self.s[start..self.i])
    }
    fn quoted(&mut self) -> Result<String> {
        self.expect('"')?;
        let rest = &self.s[self.i..];
        let close = rest.find('"').context("unterminated string")?;
        let out = rest[..close].to_string();
        self.i += close + 1;
        Ok(out)
    }
    /// The identifier inside a trailing `// NAME` comment, if present.
    fn trailing_comment_ident(&mut self) -> Option<String> {
        let rest = &self.s[self.i..];
        let p = rest.find("//")?;
        let after = rest[p + 2..].trim();
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        if end == 0 {
            None
        } else {
            Some(after[..end].to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// emitters
// ---------------------------------------------------------------------------

fn header(module: &str, crcs: &str, what: &str) -> String {
    format!(
        "// GENERATED by cargo xtask doom-gen — DO NOT EDIT. input-crc32: {crcs}\n\
         // {module} — {what}\n\
         // Derived from id Software's linuxdoom-1.10 (GPL). See ports/doom/README.md.\n"
    )
}

/// AID_* name for an action fn: `A_WeaponReady` → `AID_WEAPONREADY`.
fn aid_name(action: &str) -> String {
    format!("AID_{}", action.trim_start_matches("A_").to_uppercase())
}

fn emit_info(p: &ParsedRef) -> String {
    let mut s = header(
        "info.bet",
        &p.crc_info,
        "states[] / mobjinfo[] / sprite names as pure data (no pulls; a leaf module)",
    );
    s.push_str(
        "//\n\
         // Action fn pointers become dense action ids: AID_NONE = 0, then the distinct A_*\n\
         // names sorted alphabetically, numbered from 1. The engine dispatches on actionId.\n\
         // `vec[T]` is a shared handle on both the interpreter and compiled backends, so the\n\
         // init fillers mutate the caller's vec in place (call initStatesAll / initMobjinfoAll\n\
         // exactly once on an empty vec).\n\n",
    );

    // ---- drips ----
    s.push_str("flex drip State {\n");
    for f in [
        "spriteIdx",
        "frame",
        "tics",
        "actionId",
        "nextstate",
        "misc1",
        "misc2",
    ] {
        let _ = writeln!(s, "    flex {f}: i32");
    }
    s.push_str("}\n\n");

    s.push_str("flex drip MobjInfo {\n");
    for f in MOBJ_FIELDS {
        let _ = writeln!(s, "    flex {f}: i32");
    }
    s.push_str("}\n\n");

    // ---- enum ordinal facts ----
    s.push_str("// statenum_t ordinals\n");
    for (i, n) in p.statenames.iter().enumerate() {
        let _ = writeln!(s, "flex facts {n}: int = {i}");
    }
    let _ = writeln!(s, "flex facts NUMSTATES: int = {}\n", p.statenames.len());

    s.push_str("// mobjtype_t ordinals\n");
    for (i, n) in p.mobjnames.iter().enumerate() {
        let _ = writeln!(s, "flex facts {n}: int = {i}");
    }
    let _ = writeln!(s, "flex facts NUMMOBJTYPES: int = {}\n", p.mobjnames.len());

    s.push_str("// spritenum_t ordinals\n");
    for (i, n) in p.spritenames.iter().enumerate() {
        let _ = writeln!(s, "flex facts {n}: int = {i}");
    }
    let _ = writeln!(s, "flex facts NUMSPRITES: int = {}\n", p.spritenames.len());

    s.push_str("// action ids (AID_NONE = 0; distinct A_* names sorted alphabetically from 1)\n");
    s.push_str("flex facts AID_NONE: int = 0\n");
    for (i, a) in p.actions.iter().enumerate() {
        let _ = writeln!(s, "flex facts {}: int = {}  // {a}", aid_name(a), i + 1);
    }
    let _ = writeln!(s, "flex facts NUMACTIONS: int = {}\n", p.actions.len() + 1);

    // ---- states fillers ----
    let aid_of = |action: &Option<String>| -> usize {
        match action {
            None => 0,
            Some(a) => 1 + p.actions.iter().position(|x| x == a).expect("action known"),
        }
    };
    let sprite_ord = ordinals(&p.spritenames);
    let state_ord = ordinals(&p.statenames);

    let n_chunks = p.states.len().div_ceil(STATES_CHUNK);
    for chunk in 0..n_chunks {
        let _ = writeln!(s, "flex finna initStates{chunk}(v: vec[State]) {{");
        let lo = chunk * STATES_CHUNK;
        let hi = (lo + STATES_CHUNK).min(p.states.len());
        for i in lo..hi {
            let r = &p.states[i];
            let _ = writeln!(
                s,
                "    v.stack(State{{ spriteIdx: {}, frame: {}, tics: {}, actionId: {}, nextstate: {}, misc1: {}, misc2: {} }})  // {}",
                sprite_ord[&r.sprite],
                r.frame,
                r.tics,
                aid_of(&r.action),
                state_ord[&r.next],
                r.misc1,
                r.misc2,
                p.statenames[i],
            );
        }
        s.push_str("}\n\n");
    }
    s.push_str("// fill an empty vec with all NUMSTATES states, in statenum_t order\n");
    s.push_str("flex finna initStatesAll(v: vec[State]) {\n");
    for chunk in 0..n_chunks {
        let _ = writeln!(s, "    initStates{chunk}(v)");
    }
    s.push_str("}\n\n");

    // ---- mobjinfo fillers ----
    let n_chunks = p.mobjinfo.len().div_ceil(MOBJ_CHUNK);
    for chunk in 0..n_chunks {
        let _ = writeln!(s, "flex finna initMobjinfo{chunk}(m: vec[MobjInfo]) {{");
        let lo = chunk * MOBJ_CHUNK;
        let hi = (lo + MOBJ_CHUNK).min(p.mobjinfo.len());
        for i in lo..hi {
            let vals = &p.mobjinfo[i];
            let fields: Vec<String> = MOBJ_FIELDS
                .iter()
                .zip(vals.iter())
                .map(|(f, v)| {
                    // `flags` is a bit set that can use bit 31 in principle; everything in
                    // DOOM fits in i32 as written, but keep the emitted literal in i32 range.
                    let vv = if *v > i32::MAX as i64 {
                        *v - (1i64 << 32)
                    } else {
                        *v
                    };
                    format!("{f}: {vv}")
                })
                .collect();
            let _ = writeln!(
                s,
                "    m.stack(MobjInfo{{ {} }})  // {}",
                fields.join(", "),
                p.mobjnames[i],
            );
        }
        s.push_str("}\n\n");
    }
    s.push_str("// fill an empty vec with all NUMMOBJTYPES entries, in mobjtype_t order\n");
    s.push_str("flex finna initMobjinfoAll(m: vec[MobjInfo]) {\n");
    for chunk in 0..n_chunks {
        let _ = writeln!(s, "    initMobjinfo{chunk}(m)");
    }
    s.push_str("}\n\n");

    // ---- sprite names ----
    let concat: String = p.sprnames.concat();
    s.push_str("// sprnames[i] — 4 chars per sprite, one concatenated string\n");
    s.push_str("flex finna spriteName(i: int) -> str {\n");
    let _ = writeln!(s, "    facts NAMES: str = \"{concat}\"");
    s.push_str("    bet str.sub(NAMES, i * 4, i * 4 + 4)\n");
    s.push_str("}\n");
    s
}

fn emit_tables(p: &ParsedRef) -> String {
    let mut s = header(
        "tables.bet",
        &p.crc_tables,
        "finesine / finetangent / tantoangle + SlopeDiv (no pulls; a leaf module)",
    );
    s.push_str(
        "//\n\
         // Table literals are copied VERBATIM from tables.c — id's exact rounding, never\n\
         // recomputed. `[]T` slabs are value-copied by the interpreter when passed to a fn,\n\
         // so every filler takes the slab, mutates, and `bet`s it back: call as\n\
         //     t = tables.initFinesineAll(t)\n\
         // (compiled slabs share the backing store, so the rebind is free there.)\n\
         //\n\
         // finecosine is NOT emitted: finecosine[i] == finesine[i + 2048] (cosine is sine\n\
         // phase-shifted a quarter turn, FINEANGLES/4 = 2048), exactly like id's\n\
         // `finecosine = &finesine[FINEANGLES/4]` alias.\n\n",
    );

    s.push_str("flex facts FINEANGLES: int = 8192\n");
    s.push_str("flex facts FINEMASK: int = 8191\n");
    s.push_str("// 0x100000000 to 0x2000\n");
    s.push_str("flex facts ANGLETOFINESHIFT: int = 19\n");
    s.push_str("flex facts SLOPERANGE: int = 2048\n");
    s.push_str("flex facts SLOPEBITS: int = 11\n");
    s.push_str("// DBITS = FRACBITS(16) - SLOPEBITS(11)\n");
    s.push_str("flex facts DBITS: int = 5\n");
    s.push_str("flex facts ANG45: u32 = 0x20000000\n");
    s.push_str("flex facts ANG90: u32 = 0x40000000\n");
    s.push_str("flex facts ANG180: u32 = 0x80000000\n");
    s.push_str("flex facts ANG270: u32 = 0xC0000000\n");
    let _ = writeln!(s, "flex facts FINESINE_LEN: int = {}", p.finesine.len());
    let _ = writeln!(
        s,
        "flex facts FINETANGENT_LEN: int = {}",
        p.finetangent.len()
    );
    let _ = writeln!(
        s,
        "flex facts TANTOANGLE_LEN: int = {}\n",
        p.tantoangle.len()
    );

    s.push_str(
        "// SlopeDiv from tables.c: u32 in, u32 out, clamped to SLOPERANGE.\n\
         flex finna slopeDiv(num: u32, den: u32) -> u32 {\n\
         \x20   fr den < 512 {\n\
         \x20       bet 2048\n\
         \x20   }\n\
         \x20   lowkey ans: u32 = (num << 3) / (den >> 8)\n\
         \x20   fr ans <= 2048 {\n\
         \x20       bet ans\n\
         \x20   }\n\
         \x20   bet 2048\n\
         }\n\n",
    );

    emit_table_fillers(&mut s, "Finesine", "i32", &p.finesine);
    emit_table_fillers(&mut s, "Finetangent", "i32", &p.finetangent);
    emit_table_fillers(&mut s, "Tantoangle", "u32", &p.tantoangle);
    s
}

/// Chunked filler fns for one numeric table: `initName<k>(t: []ty) -> []ty` with ≤512 indexed
/// stores each, plus `initNameAll` chaining them (value-threading — see module header).
fn emit_table_fillers(s: &mut String, name: &str, ty: &str, values: &[String]) {
    let n_chunks = values.len().div_ceil(TABLE_CHUNK);
    for chunk in 0..n_chunks {
        let _ = writeln!(s, "flex finna init{name}{chunk}(t: []{ty}) -> []{ty} {{");
        let lo = chunk * TABLE_CHUNK;
        let hi = (lo + TABLE_CHUNK).min(values.len());
        for (i, v) in values.iter().enumerate().take(hi).skip(lo) {
            let _ = writeln!(s, "    t[{i}] = {v}");
        }
        s.push_str("    bet t\n}\n\n");
    }
    let _ = writeln!(
        s,
        "// fill a mem.slab[{ty}]({}) — call as `t = init{name}All(t)`",
        values.len()
    );
    let _ = writeln!(s, "flex finna init{name}All(t: []{ty}) -> []{ty} {{");
    for chunk in 0..n_chunks {
        let _ = writeln!(s, "    t = init{name}{chunk}(t)");
    }
    s.push_str("    bet t\n}\n\n");
}

fn emit_sounds(p: &ParsedRef) -> String {
    let mut s = header(
        "sounds.bet",
        &p.crc_sounds,
        "S_sfx[] rows + sfx/music enum ordinals (no pulls; a leaf module)",
    );
    s.push_str(
        "//\n\
         // SfxInfo.link is the S_sfx INDEX of the linked sound (id stores a pointer,\n\
         // `&S_sfx[sfx_x]`), or -1 for none. The always-zero `data` pointer is dropped.\n\
         // Fact names: sfx_x → SFX_X, mus_x → MUS_X (ordinals; original names in comments).\n\n",
    );

    s.push_str("flex drip SfxInfo {\n");
    s.push_str("    flex name: str\n");
    s.push_str("    flex singularity: i32\n");
    s.push_str("    flex priority: i32\n");
    s.push_str("    flex link: i32\n");
    s.push_str("    flex pitch: i32\n");
    s.push_str("    flex volume: i32\n");
    s.push_str("}\n\n");

    s.push_str("// sfxenum_t ordinals\n");
    for (i, n) in p.sfxnames.iter().enumerate() {
        let fact = format!("SFX_{}", n.trim_start_matches("sfx_").to_uppercase());
        let _ = writeln!(s, "flex facts {fact}: int = {i}  // {n}");
    }
    let _ = writeln!(s, "flex facts NUMSFX: int = {}\n", p.sfxnames.len());

    s.push_str("// musicenum_t ordinals\n");
    for (i, n) in p.musnames.iter().enumerate() {
        let fact = format!("MUS_{}", n.trim_start_matches("mus_").to_uppercase());
        let _ = writeln!(s, "flex facts {fact}: int = {i}  // {n}");
    }
    let _ = writeln!(s, "flex facts NUMMUSIC: int = {}\n", p.musnames.len());

    let n_chunks = p.sfx.len().div_ceil(SFX_CHUNK);
    for chunk in 0..n_chunks {
        let _ = writeln!(s, "flex finna initSfx{chunk}(v: vec[SfxInfo]) {{");
        let lo = chunk * SFX_CHUNK;
        let hi = (lo + SFX_CHUNK).min(p.sfx.len());
        for (i, r) in p.sfx.iter().enumerate().take(hi).skip(lo) {
            let _ = writeln!(
                s,
                "    v.stack(SfxInfo{{ name: \"{}\", singularity: {}, priority: {}, link: {}, pitch: {}, volume: {} }})  // {}",
                r.name, r.singularity, r.priority, r.link, r.pitch, r.volume, p.sfxnames[i],
            );
        }
        s.push_str("}\n\n");
    }
    s.push_str("// fill an empty vec with all NUMSFX rows, in sfxenum_t order\n");
    s.push_str("flex finna initSfxAll(v: vec[SfxInfo]) {\n");
    for chunk in 0..n_chunks {
        let _ = writeln!(s, "    initSfx{chunk}(v)");
    }
    s.push_str("}\n");
    s
}

// ---------------------------------------------------------------------------
// --inventory: function-definition census of the reference tree
// ---------------------------------------------------------------------------

/// Scan every `*.c` in the reference tree for function definitions and write
/// `ports/doom/goldens/inventory.txt` as `<file> <fn> <line>` lines. Once committed the file
/// is FROZEN: a re-run that produces different bytes warns and leaves it untouched.
fn inventory(doom_ref: &Path, root: &Path) -> Result<()> {
    let mut files: Vec<String> = fs::read_dir(doom_ref)
        .with_context(|| format!("reading {}", doom_ref.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".c"))
        .collect();
    files.sort();

    let mut out = String::new();
    let mut total = 0usize;
    for f in &files {
        let src = fs::read_to_string(doom_ref.join(f)).with_context(|| format!("reading {f}"))?;
        let stripped = strip_comments(&src, true);
        let fns = scan_functions(&stripped);
        total += fns.len();
        for (name, line) in fns {
            let _ = writeln!(out, "{f} {name} {line}");
        }
    }

    let path = root
        .join("ports")
        .join("doom")
        .join("goldens")
        .join("inventory.txt");
    if let Ok(existing) = fs::read(&path) {
        if existing == out.as_bytes() {
            println!(
                "doom-gen --inventory: {} unchanged ({} functions across {} files)",
                path.display(),
                total,
                files.len()
            );
        } else {
            eprintln!(
                "warning: doom-gen --inventory produced different output than the committed\n\
                 {} — the inventory is FROZEN; leaving the committed file untouched.\n\
                 (delete it and re-run to intentionally re-baseline)",
                path.display()
            );
        }
        return Ok(());
    }
    fs::create_dir_all(path.parent().expect("has parent"))?;
    fs::write(&path, &out).with_context(|| format!("writing {}", path.display()))?;
    println!(
        "doom-gen --inventory: wrote {} ({} functions across {} files)",
        path.display(),
        total,
        files.len()
    );
    Ok(())
}

/// C words that can never be a function name — filters `(*fnptr)`-style declarators where the
/// token before `(` is a type keyword.
const C_NON_NAMES: [&str; 16] = [
    "if", "while", "for", "switch", "return", "sizeof", "void", "int", "char", "long", "short",
    "unsigned", "signed", "float", "double", "else",
];

/// Find function definitions in comment-and-string-stripped C source. A definition is an
/// identifier followed by a balanced `(...)` and then (optionally after K&R parameter
/// declarations) a `{`, starting from a column-0 identifier line at brace depth 0. Returns
/// `(name, 1-based line of the name)`. Handles id's three styles:
/// name-alone-at-col-0 with `(` on the next line, `type name(args)` one-liners, and
/// K&R `name(a, b) int a; char* b; {` definitions.
pub(crate) fn scan_functions(stripped: &str) -> Vec<(String, usize)> {
    let lines: Vec<&str> = stripped.lines().collect();
    // Brace depth at the start of each line.
    let mut depth_at = vec![0i32; lines.len()];
    let mut d = 0i32;
    for (i, l) in lines.iter().enumerate() {
        depth_at[i] = d;
        for c in l.chars() {
            match c {
                '{' => d += 1,
                '}' => d -= 1,
                _ => {}
            }
        }
    }

    let mut out: Vec<(String, usize)> = Vec::new();
    let mut seen_lines: std::collections::BTreeSet<usize> = Default::default();
    for i in 0..lines.len() {
        if depth_at[i] != 0 {
            continue;
        }
        let Some(c0) = lines[i].chars().next() else {
            continue;
        };
        if !(c0.is_ascii_alphabetic() || c0 == '_') {
            continue;
        }
        if let Some((name, name_line)) = try_fn_at(&lines, i)
            && seen_lines.insert(name_line)
        {
            out.push((name, name_line + 1));
        }
    }
    out.sort_by_key(|(_, l)| *l);
    out
}

/// Try to parse a function definition whose first line is `lines[start]` (see
/// [`scan_functions`] for the accepted shapes). Returns `(name, 0-based name line)`.
fn try_fn_at(lines: &[&str], start: usize) -> Option<(String, usize)> {
    // Flatten a bounded window into (char, line) pairs.
    let end = (start + 80).min(lines.len());
    let mut chars: Vec<(char, usize)> = Vec::new();
    for (li, l) in lines.iter().enumerate().take(end).skip(start) {
        for c in l.chars() {
            chars.push((c, li));
        }
        chars.push(('\n', li));
    }

    let mut i = 0usize;
    let mut last_ident: Option<(String, usize)> = None;
    // Phase 1: find the parameter-list `(`, tracking the identifier right before it.
    loop {
        if i >= chars.len() {
            return None;
        }
        let (c, li) = chars[i];
        if c.is_ascii_alphabetic() || c == '_' {
            let s = i;
            while i < chars.len() && (chars[i].0.is_ascii_alphanumeric() || chars[i].0 == '_') {
                i += 1;
            }
            let word: String = chars[s..i].iter().map(|(c, _)| *c).collect();
            last_ident = Some((word, li));
            continue;
        }
        match c {
            '(' => break,
            ';' | '=' | '{' | '}' => return None, // declaration / initializer / block
            _ => {}
        }
        i += 1;
    }
    let (name, name_line) = last_ident?;
    if C_NON_NAMES.contains(&name.as_str()) {
        return None;
    }

    // Phase 2: skip the balanced parameter list.
    let mut pdepth = 0i32;
    while i < chars.len() {
        match chars[i].0 {
            '(' => pdepth += 1,
            ')' => {
                pdepth -= 1;
                if pdepth == 0 {
                    i += 1;
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if pdepth != 0 {
        return None;
    }

    // Phase 3: after `)` — `{` (definition), `;`/`,`/`=` (prototype/declarator), or K&R
    // parameter declarations (identifiers and `;`s) followed by `{`.
    let mut saw_knr = false;
    while i < chars.len() {
        let c = chars[i].0;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '{' => return Some((name, name_line)),
            ';' if !saw_knr => return None, // plain prototype
            ',' | '=' => return None,       // declarator list / initializer
            ';' => {
                i += 1;
            }
            '(' | ')' => return None,
            _ => {
                saw_knr = true; // K&R parameter declaration text
                i += 1;
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_comments_preserves_offsets() {
        let src = "a /* x\ny */ b // c\nd \"s//t\" e";
        let out = strip_comments(src, false);
        assert_eq!(out.len(), src.len());
        assert_eq!(out.lines().count(), src.lines().count());
        assert!(out.contains('a') && out.contains('b') && out.contains('d'));
        assert!(!out.contains("y */"));
        assert!(out.contains("\"s//t\""), "string contents kept: {out}");
        let blanked = strip_comments(src, true);
        assert!(!blanked.contains("s//t"), "string contents blanked");
    }

    #[test]
    fn parse_enum_orders_and_validates() {
        let h = "typedef enum\n{\n    SPR_A,\n    SPR_B,\n    NUMX\n\n} x_t;\n";
        let names = parse_enum(h, "} x_t", "SPR_", "NUMX").unwrap();
        assert_eq!(names, vec!["SPR_A".to_string(), "SPR_B".to_string()]);
        assert!(parse_enum(h, "} x_t", "SPR_", "WRONG").is_err());
    }

    #[test]
    fn state_row_parses() {
        let (r, c) =
            parse_state_row("{SPR_SHTG,4,0,{A_Light0},S_NULL,0,0},\t// S_LIGHTDONE").unwrap();
        assert_eq!(r.sprite, "SPR_SHTG");
        assert_eq!(r.frame, 4);
        assert_eq!(r.tics, 0);
        assert_eq!(r.action.as_deref(), Some("A_Light0"));
        assert_eq!(r.next, "S_NULL");
        assert_eq!(c.as_deref(), Some("S_LIGHTDONE"));

        let (r, _) = parse_state_row("{SPR_TROO,0,-1,{NULL},S_NULL,0,0},\t// S_NULL").unwrap();
        assert_eq!(r.tics, -1);
        assert!(r.action.is_none());
    }

    #[test]
    fn sfx_row_parses_links() {
        let mut ord = BTreeMap::new();
        ord.insert("sfx_pistol".to_string(), 1i64);
        let r = parse_sfx_row(
            "{ \"chgun\", false, 64, &S_sfx[sfx_pistol], 150, 0, 0 },",
            &ord,
        )
        .unwrap();
        assert_eq!(r.name, "chgun");
        assert_eq!(r.link, 1);
        assert_eq!(r.pitch, 150);
        let r = parse_sfx_row("{ \"none\", false,  0, 0, -1, -1, 0 },", &ord).unwrap();
        assert_eq!(r.link, -1);
        assert_eq!(r.singularity, 0);
    }

    #[test]
    fn expr_eval_covers_mobjinfo_shapes() {
        let states: BTreeMap<String, i64> = [("S_PLAY".to_string(), 149i64)].into();
        let sfx: BTreeMap<String, i64> = [("sfx_pldeth".to_string(), 59i64)].into();
        let mf: BTreeMap<String, i64> = [
            ("MF_SOLID".to_string(), 2i64),
            ("MF_SHOOTABLE".to_string(), 4i64),
        ]
        .into();
        let ev = |e: &str| eval_expr(e, &states, &sfx, &mf).unwrap();
        assert_eq!(ev("-1"), -1);
        assert_eq!(ev("16*FRACUNIT"), 16 * 65536);
        assert_eq!(ev("S_PLAY"), 149);
        assert_eq!(ev("sfx_pldeth"), 59);
        assert_eq!(ev("MF_SOLID|MF_SHOOTABLE"), 6);
        assert_eq!(ev("0x400"), 1024);
    }

    #[test]
    fn fn_scanner_handles_id_styles() {
        let src = "\
int\n\
FixedMul\n\
( fixed_t a,\n\
  fixed_t b )\n\
{\n\
    return a;\n\
}\n\
\n\
int P_Random (void)\n\
{\n\
    return 0;\n\
}\n\
\n\
void D_DoomMain (void);\n\
int	rndindex = 0;\n\
state_t states[3] = {\n\
    {1},\n\
};\n\
main (argc, argv)\n\
int argc;\n\
char** argv;\n\
{\n\
    return 0;\n\
}\n";
        let fns = scan_functions(&strip_comments(src, true));
        let names: Vec<&str> = fns.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["FixedMul", "P_Random", "main"]);
        assert_eq!(fns[0].1, 2); // FixedMul's name line
        assert_eq!(fns[1].1, 9);
    }
}
