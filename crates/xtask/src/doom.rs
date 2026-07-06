//! Shared helpers for the `doom-*` xtask family (data-table codegen + verification oracle).
//!
//! The DOOM reference source (id's GPL linuxdoom-1.10 release) lives OUTSIDE the repo at
//! [`DEFAULT_DOOM_REF`]; it is read-only input. Everything generated from it lands under
//! `ports/doom/` (GPL, isolated there — see ports/doom/README.md).

use std::path::PathBuf;

/// Where the id linuxdoom-1.10 reference tree lives on this machine (outside the repo,
/// gitignored there). Overridable with `--doom-ref <path>` on every doom-* subcommand.
pub const DEFAULT_DOOM_REF: &str = "/Users/lukebaggett/Documents/bet/doom-reference/linuxdoom-1.10";

/// The shareware IWAD used by the oracle run (v1.9, 1264 lumps).
pub const DEFAULT_IWAD: &str = "/Users/lukebaggett/Documents/bet/doom-reference/doom1.wad";

/// Where `doom-oracle --setup` clones doomgeneric (outside the repo; `/doom-oracle/` is
/// in the repo root .gitignore).
pub const ORACLE_DIR: &str = "/Users/lukebaggett/Documents/bet/doom-oracle";

/// CRC-32 (IEEE 802.3): reflected, polynomial 0xEDB88320, init 0xFFFFFFFF, final XOR
/// 0xFFFFFFFF. This exact variant is used EVERYWHERE in the doom verification pipeline —
/// the C golden generators (`ports/doom/goldens/gen/*.c`), the oracle framebuffer
/// fingerprint (`oracle.patch`), this Rust diff code, and the bet twin must all match it
/// bit-for-bit. Do not swap in a different table/variant.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// Resolve `--doom-ref` (defaulting to [`DEFAULT_DOOM_REF`]) from a flag list.
pub fn doom_ref_from(args: &[String]) -> PathBuf {
    args.iter()
        .position(|a| a == "--doom-ref")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DOOM_REF))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_vectors() {
        // The canonical check value for CRC-32/ISO-HDLC ("IEEE 802.3").
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
    }
}
