//! Shared helpers for the `doom-*` xtask family (data-table codegen + verification oracle).
//!
//! The DOOM reference source (id's GPL linuxdoom-1.10 release) lives OUTSIDE the repo (it is
//! gitignored) but is resolved relative to the repo root by default — see [`default_doom_ref`];
//! it is read-only input. Everything generated from it lands under `ports/doom/` (GPL, isolated
//! there — see ports/doom/README.md).
//!
//! None of these paths are hardcoded to a developer's machine: each resolves from an env-var
//! override (for CI / alternate checkouts) and otherwise from a repo-relative default rooted at
//! the workspace, so `cargo xtask-doom …` works from any clone regardless of the current dir.

use std::path::{Path, PathBuf};

/// The repo (workspace) root, resolved at compile time from this crate's manifest dir
/// (`crates/xtask/` -> two parents up). Independent of the current working directory, so the
/// repo-relative defaults below point at the right place no matter where xtask is invoked from.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Read a path from an env var, treating unset/empty as "not provided".
fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Where the id linuxdoom-1.10 reference tree lives (outside the repo, gitignored there).
///
/// Resolution order: the `BET_DOOM_REF` env var if set, else the repo-relative default
/// `doom-reference/linuxdoom-1.10`. The `--doom-ref <path>` flag (see [`doom_ref_from`])
/// overrides both on every doom-* subcommand.
pub fn default_doom_ref() -> PathBuf {
    env_path("BET_DOOM_REF").unwrap_or_else(|| repo_root().join("doom-reference/linuxdoom-1.10"))
}

/// The shareware IWAD used by the oracle run (v1.9, 1264 lumps).
///
/// Resolution order: the `BET_DOOM_IWAD` env var if set, else the repo-relative default
/// `doom-reference/doom1.wad`.
pub fn default_iwad() -> PathBuf {
    env_path("BET_DOOM_IWAD").unwrap_or_else(|| repo_root().join("doom-reference/doom1.wad"))
}

/// Where `doom-oracle --setup` clones doomgeneric (outside the repo; `/doom-oracle/` is
/// in the repo root .gitignore).
///
/// Resolution order: the `BET_DOOM_ORACLE` env var if set, else the repo-relative default
/// `doom-oracle`.
pub fn oracle_dir() -> PathBuf {
    env_path("BET_DOOM_ORACLE").unwrap_or_else(|| repo_root().join("doom-oracle"))
}

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

/// Resolve the doom reference tree: the `--doom-ref <path>` flag if present, otherwise
/// [`default_doom_ref`] (env `BET_DOOM_REF` -> repo-relative default).
pub fn doom_ref_from(args: &[String]) -> PathBuf {
    args.iter()
        .position(|a| a == "--doom-ref")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(default_doom_ref)
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

    // The defaults must be machine-independent: rooted under the repo (never a developer's
    // home dir). The env-override branch (BET_DOOM_*) can't be unit-tested here because the
    // crate forbids `unsafe`, which `std::env::set_var` now requires — it is exercised at
    // runtime instead (`BET_DOOM_ORACLE=... cargo xtask-doom doom-oracle --setup`).
    #[test]
    fn defaults_are_repo_relative_not_absolute_author_paths() {
        let root = repo_root();
        assert!(root.is_absolute());
        // Each assertion holds only when the corresponding override is absent (the common
        // case); if a BET_DOOM_* var is set in the test env, that resolver is env-driven and
        // we skip the repo-relative check for it.
        let cases: [(&str, PathBuf, &str); 3] = [
            (
                "BET_DOOM_REF",
                default_doom_ref(),
                "doom-reference/linuxdoom-1.10",
            ),
            ("BET_DOOM_IWAD", default_iwad(), "doom-reference/doom1.wad"),
            ("BET_DOOM_ORACLE", oracle_dir(), "doom-oracle"),
        ];
        for (var, path, suffix) in cases {
            if std::env::var_os(var).is_some() {
                continue;
            }
            assert!(
                path.starts_with(&root),
                "{} not under repo root",
                path.display()
            );
            assert!(
                path.ends_with(suffix),
                "{} lacks suffix {suffix}",
                path.display()
            );
        }
    }

    // The CLI flag takes precedence over the default (and, layered under it, the env var).
    #[test]
    fn doom_ref_flag_overrides_default() {
        let args = vec!["--doom-ref".to_string(), "/tmp/flag-ref".to_string()];
        assert_eq!(doom_ref_from(&args), PathBuf::from("/tmp/flag-ref"));
    }
}
