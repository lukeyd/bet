//! `bake-frozen-bubble` — bake a local Frozen Bubble (GPL-2) checkout into `bet` assets.
//!
//! Usage: `cargo xtask bake-frozen-bubble --src <fb-checkout> --out <path/assets.dat>`
//!
//! The user supplies a *local* Frozen Bubble source checkout via `--src`. Frozen Bubble's
//! media (sprites, sounds, level maps) is GPL-2 and is **NOT** shipped with `bet` — this
//! command ships the *baker*, not the baked data. It reads the checkout's `share/` tree
//! (falling back to the whole `--src` if there is no `share/`) and produces two files:
//!
//!   1. `<--out>`               a single packed, little-endian binary blob (`.dat`), and
//!   2. `<--out dir>/assets_gen.bet`  a generated `facts` index mapping human names -> entry index.
//!
//! All media decoding lives here in xtask (external `image`/`lewton` crates), keeping it out
//! of the bet toolchain proper. The decode step and the byte-writer are decoupled so the
//! writer/reader are unit-testable on synthetic in-memory inputs with no real assets (see the
//! inline `#[cfg(test)] mod tests`).
//!
//! ## `.dat` format (all integers little-endian)
//! ```text
//! header (20 bytes):
//!   magic     : [u8;4] = b"FBD1"
//!   version   : u32 = 1
//!   count     : u32                     // number of entries
//!   str_off   : u32                     // ABSOLUTE file offset of the string table
//!   blob_off  : u32                     // ABSOLUTE file offset of the blob region
//! entry table (at offset 20): count x 32 bytes, each:
//!   name_off  : u32   // offset of the name, RELATIVE to str_off
//!   kind      : u32   // 0=image RGBA8888, 1=audio i16 PCM LE, 2=levels, 3=trig
//!   offset    : u32   // offset of the payload, RELATIVE to blob_off
//!   len       : u32   // payload length in bytes
//!   a, b, c   : u32   // kind-specific metadata (below)
//!   reserved  : u32 = 0
//! string table (at str_off): each name = u16 LE length prefix + UTF-8 bytes (name_off points at the u16)
//! blob region (at blob_off): concatenated payloads
//! ```
//! Per-kind `a`/`b`/`c` and payload:
//!   - image  (0): a=width, b=height, c=0 — payload = w*h*4 RGBA8888 bytes.
//!   - audio  (1): a=sample_rate, b=channels, c=frame_count — payload = interleaved i16 LE PCM, len = frames*channels*2 (NOT resampled).
//!   - levels (2): a=count, b=cols, c=max_rows — payload = count*max_rows*cols bytes, row-major, 255 = empty, each level padded.
//!   - trig   (3): a=entries, b=0, c=0 — payload = `entries` interleaved (cos, sin) Q16 i32 LE pairs, len = entries*8.
//!
//! ## Documented format decisions
//!   - `entry.offset` is RELATIVE to `blob_off`; `name_off` is RELATIVE to `str_off`;
//!     `str_off`/`blob_off` themselves are ABSOLUTE from the start of the file.
//!   - The trig table is interleaved `(cos, sin)` Q16 pairs over a full turn `[0, 2*pi)`:
//!     `cos(2*pi*i/N)`, `sin(2*pi*i/N)` each stored as `round(f * 65536)`. This matches the
//!     repo's 16.16 fixed-point convention (`FRACUNIT = 65536`) and hands the runtime both
//!     functions directly.
//!   - The levels blob is a fixed-stride `count x max_rows x cols` cube (each level padded with
//!     255) so the runtime can index `blob[L*max_rows*cols + R*cols + C]` with no per-level table.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const HEADER_SIZE: usize = 20;
const ENTRY_SIZE: usize = 32;
const MAGIC: &[u8; 4] = b"FBD1";
const VERSION: u32 = 1;

const KIND_IMAGE: u32 = 0;
const KIND_AUDIO: u32 = 1;
const KIND_LEVELS: u32 = 2;
const KIND_TRIG: u32 = 3;

/// Real Frozen Bubble `gfx` mixes `.gif` core sprites, `.png` menu art, and one `.bmp`.
const IMAGE_EXTS: &[&str] = &["png", "gif", "bmp"];
/// Frozen Bubble sound effects are all Ogg Vorbis.
const AUDIO_EXTS: &[&str] = &["ogg"];
/// Resolution of the generated trig table (a full turn). 1024 samples ~= 0.35 deg steps.
const TRIG_ENTRIES: u32 = 1024;

/// One packed asset, generic and fully decoupled from any decoder so the writer can be
/// exercised on hand-built inputs.
struct Asset {
    /// Human name (relative path for media; `levels`/`trig` for the synthesized tables).
    name: String,
    kind: u32,
    a: u32,
    b: u32,
    c: u32,
    blob: Vec<u8>,
}

/// One entry recovered by `read_dat` — the deliberate reader counterpart to `write_dat`,
/// exercised by the round-trip self-test and available to future runtime/tooling consumers.
#[allow(dead_code)]
#[derive(Debug)]
struct ParsedEntry {
    name: String,
    kind: u32,
    a: u32,
    b: u32,
    c: u32,
    payload: Vec<u8>,
}

/// `cargo xtask bake-frozen-bubble --src <fb-checkout> --out <path/assets.dat>`.
pub fn run(args: &[String]) -> Result<()> {
    let out = match flag_value(args, "--out") {
        Some(o) => PathBuf::from(o),
        None => bail!("bake-frozen-bubble: --out <path/assets.dat> is required"),
    };
    let src = match flag_value(args, "--src") {
        Some(s) => PathBuf::from(s),
        None => bail!(
            "bake-frozen-bubble: supply a local Frozen Bubble checkout via --src <path>; \
             its GPL-2 assets are not shipped with `bet`"
        ),
    };
    if !src.exists() {
        bail!(
            "bake-frozen-bubble: --src path {} does not exist; supply a local Frozen Bubble \
             checkout (its GPL-2 assets are not shipped with `bet`)",
            src.display()
        );
    }
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating output directory {}", dir.display()))?;
    }
    bake_real(&src, &out)
}

/// Deterministically walk the checkout, decode every asset, and write both output files.
fn bake_real(src: &Path, out: &Path) -> Result<()> {
    let share = src.join("share");
    let root = if share.is_dir() {
        share
    } else {
        src.to_path_buf()
    };

    let mut files: Vec<PathBuf> = Vec::new();
    walk(&root, &mut files)?;
    files.sort(); // stable indices, independent of readdir order

    let mut assets: Vec<Asset> = Vec::new();

    // Images first (kind 0), in relative-path order.
    for p in files.iter().filter(|p| ext_is(p, IMAGE_EXTS)) {
        let bytes = std::fs::read(p).with_context(|| format!("reading image {}", p.display()))?;
        let (w, h, rgba) =
            decode_image(&bytes).with_context(|| format!("decoding image {}", p.display()))?;
        assets.push(Asset {
            name: rel_name(&root, p),
            kind: KIND_IMAGE,
            a: w,
            b: h,
            c: 0,
            blob: rgba,
        });
    }

    // Then audio (kind 1).
    for p in files.iter().filter(|p| ext_is(p, AUDIO_EXTS)) {
        let bytes = std::fs::read(p).with_context(|| format!("reading audio {}", p.display()))?;
        let (rate, channels, frames, pcm) =
            decode_ogg(&bytes).with_context(|| format!("decoding ogg {}", p.display()))?;
        assets.push(Asset {
            name: rel_name(&root, p),
            kind: KIND_AUDIO,
            a: rate,
            b: channels,
            c: frames,
            blob: pcm,
        });
    }

    // Levels (kind 2): a single file literally named `levels`, if present.
    if let Some(lp) = files
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("levels"))
    {
        let text =
            std::fs::read_to_string(lp).with_context(|| format!("reading {}", lp.display()))?;
        let (count, cols, max_rows, blob) = parse_levels(&text);
        assets.push(Asset {
            name: "levels".to_string(),
            kind: KIND_LEVELS,
            a: count,
            b: cols,
            c: max_rows,
            blob,
        });
    }

    // Trig (kind 3): always synthesized, never read from disk.
    let (entries, blob) = build_trig(TRIG_ENTRIES);
    assets.push(Asset {
        name: "trig".to_string(),
        kind: KIND_TRIG,
        a: entries,
        b: 0,
        c: 0,
        blob,
    });

    write_outputs(&assets, out)
}

/// Value following `name` in `args`, if present.
fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// Recursively collect every file (not directory) under `dir`.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

/// True if `p`'s (lowercased) extension is in `exts`.
fn ext_is(p: &Path, exts: &[&str]) -> bool {
    match p.extension().and_then(|e| e.to_str()) {
        Some(e) => exts.contains(&e.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// `p` relative to `root`, `/`-separated, as the asset's human name.
fn rel_name(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

// ---------------------------------------------------------------------------
// decoders
// ---------------------------------------------------------------------------

/// Decode any supported image (PNG/GIF/BMP) to RGBA8888. Format is sniffed from the bytes,
/// so the mixed `.gif`/`.png` sprite tree is handled by one path with no per-format branch.
fn decode_image(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).context("decoding image (png/gif/bmp)")?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((w, h, rgba.into_raw()))
}

/// Decode an Ogg Vorbis stream to interleaved i16 LE PCM. Not resampled — the runtime mixer
/// resamples at play time — so the source sample rate/channel count travel in the entry.
fn decode_ogg(bytes: &[u8]) -> Result<(u32, u32, u32, Vec<u8>)> {
    use lewton::inside_ogg::OggStreamReader;

    let mut reader =
        OggStreamReader::new(std::io::Cursor::new(bytes)).context("opening ogg stream")?;
    let rate = reader.ident_hdr.audio_sample_rate;
    let channels = u32::from(reader.ident_hdr.audio_channels);

    let mut samples: Vec<i16> = Vec::new();
    while let Some(packet) = reader
        .read_dec_packet_itl()
        .context("decoding ogg vorbis packet")?
    {
        samples.extend(packet);
    }

    let frames = (samples.len() as u32).checked_div(channels).unwrap_or(0);

    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        pcm.extend_from_slice(&s.to_le_bytes());
    }
    Ok((rate, channels, frames, pcm))
}

/// Parse the Frozen Bubble `levels` file. Levels are separated by blank lines; within a level
/// each non-empty line is a row and each non-space glyph is a cell: `-` -> 255 (empty), an ASCII
/// digit -> its value (real FB uses 0-7), a letter -> `10 + (A..)` defensively. Every level is
/// padded to a global `count x max_rows x cols` cube with 255. (Char-based skip-spaces naturally
/// yields the hex-grid's 8-then-7 offset rows.)
fn parse_levels(text: &str) -> (u32, u32, u32, Vec<u8>) {
    let mut levels: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut cur: Vec<Vec<u8>> = Vec::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                levels.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let row: Vec<u8> = line
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| match c {
                '-' => 255u8,
                d if d.is_ascii_digit() => (d as u8) - b'0',
                l if l.is_ascii_alphabetic() => 10 + (l.to_ascii_uppercase() as u8 - b'A'),
                _ => 255u8,
            })
            .collect();
        cur.push(row);
    }
    if !cur.is_empty() {
        levels.push(cur);
    }

    let cols = levels.iter().flatten().map(Vec::len).max().unwrap_or(0) as u32;
    let max_rows = levels.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let count = levels.len() as u32;

    let mut blob = Vec::with_capacity((count * max_rows * cols) as usize);
    for lvl in &levels {
        for r in 0..max_rows as usize {
            let row = lvl.get(r);
            for c in 0..cols as usize {
                let v = row.and_then(|row| row.get(c)).copied().unwrap_or(255);
                blob.push(v);
            }
        }
    }
    (count, cols, max_rows, blob)
}

/// Build an interleaved `(cos, sin)` Q16 table of `n` samples over a full turn `[0, 2*pi)`.
fn build_trig(n: u32) -> (u32, Vec<u8>) {
    let mut blob = Vec::with_capacity(n as usize * 8);
    for i in 0..n {
        let theta = std::f64::consts::TAU * f64::from(i) / f64::from(n);
        let cos = (theta.cos() * 65536.0).round() as i32;
        let sin = (theta.sin() * 65536.0).round() as i32;
        blob.extend_from_slice(&cos.to_le_bytes());
        blob.extend_from_slice(&sin.to_le_bytes());
    }
    (n, blob)
}

// ---------------------------------------------------------------------------
// .dat writer / reader (pure; the core unit-tested surface)
// ---------------------------------------------------------------------------

/// Build the entire `.dat` byte image (header -> entry table -> string table -> blobs).
fn write_dat(assets: &[Asset]) -> Vec<u8> {
    let count = assets.len() as u32;
    let entry_table_size = assets.len() * ENTRY_SIZE;
    let str_off = HEADER_SIZE + entry_table_size; // absolute

    // String table + each name's offset relative to str_off.
    let mut strtab: Vec<u8> = Vec::new();
    let mut name_offs: Vec<u32> = Vec::with_capacity(assets.len());
    for a in assets {
        name_offs.push(strtab.len() as u32);
        let bytes = a.name.as_bytes();
        strtab.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        strtab.extend_from_slice(bytes);
    }

    let blob_off = str_off + strtab.len(); // absolute

    // Blob region + each payload's offset relative to blob_off.
    let mut blob: Vec<u8> = Vec::new();
    let mut blob_offs: Vec<u32> = Vec::with_capacity(assets.len());
    for a in assets {
        blob_offs.push(blob.len() as u32);
        blob.extend_from_slice(&a.blob);
    }

    let mut out = Vec::with_capacity(blob_off + blob.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(str_off as u32).to_le_bytes());
    out.extend_from_slice(&(blob_off as u32).to_le_bytes());

    for ((a, &name_off), &boff) in assets.iter().zip(&name_offs).zip(&blob_offs) {
        out.extend_from_slice(&name_off.to_le_bytes());
        out.extend_from_slice(&a.kind.to_le_bytes());
        out.extend_from_slice(&boff.to_le_bytes());
        out.extend_from_slice(&(a.blob.len() as u32).to_le_bytes());
        out.extend_from_slice(&a.a.to_le_bytes());
        out.extend_from_slice(&a.b.to_le_bytes());
        out.extend_from_slice(&a.c.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    }
    debug_assert_eq!(out.len(), str_off);
    out.extend_from_slice(&strtab);
    debug_assert_eq!(out.len(), blob_off);
    out.extend_from_slice(&blob);
    out
}

/// Read a `u32` LE at byte offset `at`.
#[allow(dead_code)]
fn rd_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parse a `.dat` image back into entries (validates magic/version; slices names + payloads).
#[allow(dead_code)]
fn read_dat(bytes: &[u8]) -> Result<Vec<ParsedEntry>> {
    if bytes.len() < HEADER_SIZE {
        bail!("dat too short: {} bytes (< header)", bytes.len());
    }
    if bytes.get(..4) != Some(MAGIC.as_slice()) {
        bail!("dat has bad magic (expected FBD1)");
    }
    let version = rd_u32(bytes, 4);
    if version != VERSION {
        bail!("dat unsupported version {version} (expected {VERSION})");
    }
    let count = rd_u32(bytes, 8) as usize;
    let str_off = rd_u32(bytes, 12) as usize;
    let blob_off = rd_u32(bytes, 16) as usize;

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let base = HEADER_SIZE + i * ENTRY_SIZE;
        if base + ENTRY_SIZE > bytes.len() {
            bail!("dat entry {i} runs past end of file");
        }
        let name_off = rd_u32(bytes, base) as usize;
        let kind = rd_u32(bytes, base + 4);
        let offset = rd_u32(bytes, base + 8) as usize;
        let len = rd_u32(bytes, base + 12) as usize;
        let a = rd_u32(bytes, base + 16);
        let b = rd_u32(bytes, base + 20);
        let c = rd_u32(bytes, base + 24);

        // Name: u16 LE length prefix + UTF-8, at str_off + name_off.
        let nptr = str_off + name_off;
        if nptr + 2 > bytes.len() {
            bail!("dat entry {i} name length runs past end of file");
        }
        let nlen = u16::from_le_bytes([bytes[nptr], bytes[nptr + 1]]) as usize;
        let nstart = nptr + 2;
        if nstart + nlen > bytes.len() {
            bail!("dat entry {i} name runs past end of file");
        }
        let name = String::from_utf8(bytes[nstart..nstart + nlen].to_vec())
            .with_context(|| format!("dat entry {i} name is not valid UTF-8"))?;

        // Payload: at blob_off + offset.
        let pstart = blob_off + offset;
        if pstart + len > bytes.len() {
            bail!("dat entry {i} payload runs past end of file");
        }
        let payload = bytes[pstart..pstart + len].to_vec();

        out.push(ParsedEntry {
            name,
            kind,
            a,
            b,
            c,
            payload,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// assets_gen.bet index
// ---------------------------------------------------------------------------

/// The file-name stem (last path component, extension stripped) of an asset name.
fn asset_stem(name: &str) -> &str {
    let file = name.rsplit('/').next().unwrap_or(name);
    match file.rsplit_once('.') {
        Some((stem, _ext)) if !stem.is_empty() => stem,
        _ => file,
    }
}

/// Uppercase + non-alphanumeric collapsed to single `_`, trimmed.
fn sanitize(s: &str) -> String {
    let mut out = String::new();
    let mut prev_underscore = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// The bet constant name for an asset (before collision de-duplication).
fn const_name(a: &Asset) -> String {
    match a.kind {
        KIND_IMAGE => format!("TEX_{}", sanitize(asset_stem(&a.name))),
        KIND_AUDIO => format!("SND_{}", sanitize(asset_stem(&a.name))),
        KIND_LEVELS => "LEVELS_BLOB".to_string(),
        KIND_TRIG => "TRIG_BLOB".to_string(),
        _ => format!("ASSET_{}", sanitize(&a.name)),
    }
}

/// Generate the `assets_gen.bet` source: one `facts NAME: int = <index>` per entry.
fn gen_bet(assets: &[Asset]) -> String {
    let mut s = String::new();
    s.push_str("// generated by `cargo xtask bake-frozen-bubble` — do not edit\n");
    s.push_str(&format!("facts FB_ASSET_COUNT: int = {}\n", assets.len()));

    let mut used: HashSet<String> = HashSet::new();
    used.insert("FB_ASSET_COUNT".to_string());
    for (i, a) in assets.iter().enumerate() {
        let mut name = const_name(a);
        if !used.insert(name.clone()) {
            // Collision: suffix with the (unique) index.
            name = format!("{name}_{i}");
            used.insert(name.clone());
        }
        s.push_str(&format!("facts {name}: int = {i}\n"));
    }
    s
}

/// Write `<out>` (.dat) and `<out dir>/assets_gen.bet`.
fn write_outputs(assets: &[Asset], out: &Path) -> Result<()> {
    let dat = write_dat(assets);
    std::fs::write(out, &dat).with_context(|| format!("writing {}", out.display()))?;

    let bet = gen_bet(assets);
    let bet_path = out
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("assets_gen.bet");
    std::fs::write(&bet_path, bet).with_context(|| format!("writing {}", bet_path.display()))?;

    println!(
        "baked {} assets -> {} ({} bytes)",
        assets.len(),
        out.display(),
        dat.len()
    );
    println!("wrote index -> {}", bet_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// tests — synthetic in-memory inputs only; no real (GPL-2) assets required
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny 2x2 RGBA PNG encoded in memory via the `image` crate's PNG encoder.
    fn tiny_png() -> Vec<u8> {
        use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
        let mut buf = RgbaImage::new(2, 2);
        buf.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        buf.put_pixel(1, 0, Rgba([0, 255, 0, 255]));
        buf.put_pixel(0, 1, Rgba([0, 0, 255, 255]));
        buf.put_pixel(1, 1, Rgba([255, 255, 0, 128]));
        let mut cur = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(buf)
            .write_to(&mut cur, ImageFormat::Png)
            .unwrap();
        cur.into_inner()
    }

    #[test]
    fn decode_image_sniffs_and_roundtrips_png() {
        let png = tiny_png();
        let (w, h, rgba) = decode_image(&png).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(rgba.len(), 2 * 2 * 4);
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]); // top-left red
    }

    #[test]
    fn parse_levels_pads_to_cube() {
        // Two levels. Level 0 has two rows of differing widths; level 1 has one row.
        let text = "1 2 3\n4 5\n\n6 7\n";
        let (count, cols, max_rows, blob) = parse_levels(text);
        assert_eq!(count, 2);
        assert_eq!(cols, 3); // widest row "1 2 3"
        assert_eq!(max_rows, 2); // level 0 has two rows
        assert_eq!(blob.len() as u32, count * cols * max_rows);
        assert_eq!(&blob[0..3], &[1, 2, 3]); // L0 R0
        assert_eq!(&blob[3..6], &[4, 5, 255]); // L0 R1 (padded)
        assert_eq!(&blob[6..9], &[6, 7, 255]); // L1 R0 (padded)
        assert_eq!(&blob[9..12], &[255, 255, 255]); // L1 R1 (all pad)

        // `-` glyphs decode to empty (255).
        let (_, _, _, dashes) = parse_levels("- 1 -\n");
        assert_eq!(&dashes[0..3], &[255, 1, 255]);
    }

    #[test]
    fn build_trig_is_q16() {
        let (n, blob) = build_trig(4);
        assert_eq!(n, 4);
        assert_eq!(blob.len(), 4 * 8);
        let cos0 = i32::from_le_bytes(blob[0..4].try_into().unwrap());
        let sin0 = i32::from_le_bytes(blob[4..8].try_into().unwrap());
        assert_eq!(cos0, 65536); // cos(0) = 1.0 in Q16
        assert_eq!(sin0, 0); // sin(0) = 0
    }

    #[test]
    fn dat_roundtrips_byte_for_byte() {
        // A PNG-decoded image, hand-built PCM (no real Ogg needed), a level cube, and trig.
        let png = tiny_png();
        let (w, h, rgba) = decode_image(&png).unwrap();
        let img = Asset {
            name: "gfx/test.png".to_string(),
            kind: KIND_IMAGE,
            a: w,
            b: h,
            c: 0,
            blob: rgba,
        };

        let pcm_frames: [i16; 3] = [0, 1000, -1000];
        let mut pcm = Vec::new();
        for frame in pcm_frames {
            pcm.extend_from_slice(&frame.to_le_bytes());
        }
        let snd = Asset {
            name: "snd/test.ogg".to_string(),
            kind: KIND_AUDIO,
            a: 22050,
            b: 1,
            c: 3,
            blob: pcm,
        };

        let (lc, lcols, lrows, lblob) = parse_levels("1 2\n3 4\n");
        let lvl = Asset {
            name: "levels".to_string(),
            kind: KIND_LEVELS,
            a: lc,
            b: lcols,
            c: lrows,
            blob: lblob,
        };

        let (tn, tblob) = build_trig(8);
        let trig = Asset {
            name: "trig".to_string(),
            kind: KIND_TRIG,
            a: tn,
            b: 0,
            c: 0,
            blob: tblob,
        };

        let assets = vec![img, snd, lvl, trig];
        let dat = write_dat(&assets);
        assert_eq!(&dat[0..4], MAGIC.as_slice());

        // Round-trip through a real file: pid-namespaced temp path, cleaned up after.
        let path = std::env::temp_dir().join(format!("bet-fb-bake-{}.dat", std::process::id()));
        std::fs::write(&path, &dat).unwrap();
        let from_disk = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let parsed = read_dat(&from_disk).unwrap();
        assert_eq!(parsed.len(), assets.len());
        for (p, a) in parsed.iter().zip(&assets) {
            assert_eq!(p.name, a.name);
            assert_eq!(p.kind, a.kind);
            assert_eq!((p.a, p.b, p.c), (a.a, a.b, a.c));
            assert_eq!(
                p.payload, a.blob,
                "payload for {} not byte-identical",
                a.name
            );
        }
    }

    #[test]
    fn gen_bet_has_header_and_facts() {
        let assets = vec![
            Asset {
                name: "gfx/balls/bubble-1.gif".to_string(),
                kind: KIND_IMAGE,
                a: 32,
                b: 32,
                c: 0,
                blob: vec![0; 16],
            },
            Asset {
                name: "snd/pop.ogg".to_string(),
                kind: KIND_AUDIO,
                a: 22050,
                b: 1,
                c: 1,
                blob: vec![0; 2],
            },
            Asset {
                name: "levels".to_string(),
                kind: KIND_LEVELS,
                a: 1,
                b: 1,
                c: 1,
                blob: vec![255],
            },
            Asset {
                name: "trig".to_string(),
                kind: KIND_TRIG,
                a: 4,
                b: 0,
                c: 0,
                blob: vec![0; 32],
            },
        ];
        let bet = gen_bet(&assets);
        assert!(bet.starts_with("// generated by"));
        assert!(bet.contains("facts FB_ASSET_COUNT: int = 4"));
        assert!(bet.contains("facts TEX_BUBBLE_1: int = 0"));
        assert!(bet.contains("facts SND_POP: int = 1"));
        assert!(bet.contains("facts LEVELS_BLOB: int = 2"));
        assert!(bet.contains("facts TRIG_BLOB: int = 3"));
    }

    #[test]
    fn gen_bet_dedupes_colliding_names() {
        // Two images whose stems sanitize to the same constant collide; the second is suffixed.
        let assets = vec![
            Asset {
                name: "a/icon.png".to_string(),
                kind: KIND_IMAGE,
                a: 1,
                b: 1,
                c: 0,
                blob: vec![0; 4],
            },
            Asset {
                name: "b/icon.png".to_string(),
                kind: KIND_IMAGE,
                a: 1,
                b: 1,
                c: 0,
                blob: vec![0; 4],
            },
        ];
        let bet = gen_bet(&assets);
        assert!(bet.contains("facts TEX_ICON: int = 0"));
        assert!(bet.contains("facts TEX_ICON_1: int = 1"));
    }
}
