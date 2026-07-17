//! Locating the pinned LLVM, without hardcoding anyone's machine.
//!
//! `backend --features llvm` builds against `llvm-sys 180.x`, which finds LLVM through the
//! `LLVM_SYS_180_PREFIX` environment variable. Historically every caller in this repo (two
//! `run.sh` scripts and three port READMEs) hardcoded `/opt/homebrew/opt/llvm@18` — a path that
//! exists only on Apple-Silicon Homebrew installs, so a Linux or Intel-Mac contributor could not
//! build a port without editing files.
//!
//! This module is the single place that answers "where is LLVM 18?". [`discover`] probes, in
//! order of decreasing authority: an explicit env override, the repo-local `.llvm` slot, a
//! `llvm-config` on `PATH`, Homebrew's cellar, and finally the well-known distro paths. Both
//! `cargo xtask setup-llvm` and `cargo xtask run <port>` go through it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The LLVM major version the workspace pins (`llvm-sys 180.x` -> LLVM 18).
pub const MAJOR: &str = "18";

/// The environment variable `llvm-sys 180.x` reads to find an LLVM install.
///
/// The digits track the *crate* version (`180`), not the LLVM release (`18.1.x`) — an easy thing
/// to get wrong, and `setup-llvm` printed `LLVM_SYS_181_PREFIX` for a while because of it.
pub const PREFIX_VAR: &str = "LLVM_SYS_180_PREFIX";

/// A validated LLVM 18 installation.
pub struct Llvm {
    /// The install prefix, to be exported as [`PREFIX_VAR`].
    pub prefix: PathBuf,
    /// The full version string `llvm-config --version` reported (e.g. `18.1.8`).
    pub version: String,
    /// How this install was located — surfaced in `setup-llvm` output so it is obvious whether
    /// the answer came from the environment or from a probe.
    pub source: &'static str,
    /// An extra directory the *linker* needs on `LIBRARY_PATH`, or `None`.
    ///
    /// LLVM's static libraries link against `zstd`; on macOS that lives in the Homebrew prefix
    /// rather than a default search path, so the link step fails without this. See
    /// `.github/workflows/backend-llvm.yml`, which does the same thing for CI.
    pub library_path: Option<PathBuf>,
}

/// Find a usable LLVM 18, or `None`.
///
/// `root` is the workspace root (for the repo-local `.llvm` slot).
pub fn discover(root: &Path) -> Option<Llvm> {
    // 1. An explicit override always wins — if a contributor set it, respect it. This is also
    //    what makes the historical `LLVM_SYS_180_PREFIX=... cargo xtask run doom` invocation
    //    keep working unchanged.
    if let Some(p) = std::env::var_os(PREFIX_VAR) {
        let prefix = PathBuf::from(p);
        // A bad override is a hard stop, not something to silently probe past: falling through to
        // a different LLVM than the one the user pointed at would be more confusing than failing.
        if let Some(version) = probe(&prefix) {
            return Some(finish(prefix, version, "$LLVM_SYS_180_PREFIX"));
        }
        return None;
    }

    // 2. The repo-local slot (`/.llvm` is gitignored for exactly this).
    let local = root.join(".llvm");
    if let Some(version) = probe(&local) {
        return Some(finish(local, version, "the repo-local .llvm/"));
    }

    // 3. A `llvm-config` on PATH. Try the versioned name first (Debian/Ubuntu ship
    //    `llvm-config-18`); the unversioned one might be any major, so `probe` checks.
    for exe in ["llvm-config-18", "llvm-config"] {
        if let Some(prefix) = llvm_config_prefix(exe)
            && let Some(version) = probe(&prefix)
        {
            return Some(finish(prefix, version, exe));
        }
    }

    // 4. Homebrew's cellar — asking `brew` rather than assuming `/opt/homebrew`, so this works on
    //    Intel Macs (`/usr/local`) and Linuxbrew too.
    if let Some(prefix) = brew_prefix(&format!("llvm@{MAJOR}"))
        && let Some(version) = probe(&prefix)
    {
        return Some(finish(prefix, version, "brew --prefix llvm@18"));
    }

    // 5. Well-known install paths, last. These are the fallbacks for machines with no `brew` and
    //    no `llvm-config` on PATH; each is still version-checked by `probe`.
    for cand in [
        "/usr/lib/llvm-18",          // Debian/Ubuntu apt
        "/opt/homebrew/opt/llvm@18", // Apple Silicon Homebrew (no `brew` on PATH)
        "/usr/local/opt/llvm@18",    // Intel Homebrew (no `brew` on PATH)
        "/usr/lib64/llvm18",         // Fedora/RHEL
    ] {
        let prefix = PathBuf::from(cand);
        if let Some(version) = probe(&prefix) {
            return Some(finish(prefix, version, "a well-known install path"));
        }
    }

    None
}

/// Attach the platform's linker-search addition to a located prefix.
fn finish(prefix: PathBuf, version: String, source: &'static str) -> Llvm {
    Llvm {
        prefix,
        version,
        source,
        library_path: zstd_library_path(),
    }
}

/// Does `prefix` hold an LLVM whose major version is the pinned one? Returns its version string.
///
/// The version check is the point: an unversioned `llvm-config` or a `/usr/lib/llvm-*` guess can
/// easily be LLVM 17 or 19, and `llvm-sys` would fail deep in a build script with a much worse
/// message than ours.
fn probe(prefix: &Path) -> Option<String> {
    let cfg = prefix.join("bin").join("llvm-config");
    if !cfg.is_file() {
        return None;
    }
    let out = Command::new(&cfg).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // `18.1.8` -> major `18`. Compare the split component, not a `starts_with` prefix, so `1.8.x`
    // and a hypothetical `188.x` don't both look like a match.
    (version.split('.').next() == Some(MAJOR)).then_some(version)
}

/// `<exe> --prefix`, if `<exe>` is on PATH.
fn llvm_config_prefix(exe: &str) -> Option<PathBuf> {
    let out = Command::new(exe).arg("--prefix").output().ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// `brew --prefix <formula>`, if Homebrew is installed and knows the formula.
fn brew_prefix(formula: &str) -> Option<PathBuf> {
    let out = Command::new("brew")
        .args(["--prefix", formula])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// The directory holding `libzstd`, which LLVM's static libs need at link time — or `None` when
/// the platform's default search path already covers it (Linux distros install zstd system-wide).
fn zstd_library_path() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let lib = brew_prefix("zstd")
        .map(|p| p.join("lib"))
        .filter(|p| p.is_dir())
        .or_else(|| {
            // Fall back to the umbrella prefix (`$(brew --prefix)/lib`), which is what
            // `.github/workflows/backend-llvm.yml` exports.
            Command::new("brew")
                .arg("--prefix")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()).join("lib"))
                .filter(|p| p.is_dir())
        })?;
    Some(lib)
}

/// The message shown when no LLVM 18 could be found. Rust-facing tooling: boring, specific prose
/// with the exact commands to run (the slang-diagnostics convention is for `bet`'s own compiler
/// errors, not for xtask).
pub fn not_found_message() -> String {
    let install = if cfg!(target_os = "macos") {
        format!("  brew install llvm@{MAJOR}")
    } else {
        format!(
            "  sudo apt-get install -y llvm-{MAJOR}-dev libpolly-{MAJOR}-dev   (Debian/Ubuntu)\n  \
             sudo dnf install -y llvm{MAJOR}-devel                            (Fedora/RHEL)"
        )
    };
    format!(
        "could not find LLVM {MAJOR}, which the `llvm` codegen feature needs.\n\n\
         Install it:\n{install}\n\n\
         Then re-run. If it is installed somewhere unusual, point at it explicitly:\n  \
         export {PREFIX_VAR}=/path/to/llvm-{MAJOR}\n\n\
         Checked, in order: ${PREFIX_VAR}, the repo-local .llvm/, llvm-config-{MAJOR} and \
         llvm-config on PATH, brew --prefix llvm@{MAJOR}, and the well-known system paths."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `llvm-sys` version locked in Cargo.lock, as its major component (e.g. `180`).
    fn locked_llvm_sys_major() -> String {
        let lock = std::fs::read_to_string(crate::workspace_root().join("Cargo.lock"))
            .expect("Cargo.lock is readable");
        let doc: toml::Value = lock.parse().expect("Cargo.lock is valid TOML");
        let version = doc["package"]
            .as_array()
            .expect("[[package]] array")
            .iter()
            .find(|p| p.get("name").and_then(toml::Value::as_str) == Some("llvm-sys"))
            .and_then(|p| p.get("version"))
            .and_then(toml::Value::as_str)
            .expect("llvm-sys is in Cargo.lock (backend's `llvm` feature depends on it)")
            .to_string();
        version
            .split('.')
            .next()
            .expect("a version has a major component")
            .to_string()
    }

    /// `PREFIX_VAR` must match what the *locked* `llvm-sys` actually reads.
    ///
    /// `llvm-sys` derives its prefix variable from its own crate version: `llvm-sys 180.x` reads
    /// `LLVM_SYS_180_PREFIX`, `181.x` reads `LLVM_SYS_181_PREFIX`. So the name is not a free
    /// choice — it is a fact about a dependency, and it silently rots when that dependency is
    /// bumped.
    ///
    /// This is not a hypothetical: `setup-llvm` shipped `LLVM_SYS_181_PREFIX` (an LLVM *release*
    /// number, 18.1) where `llvm-sys 180.0.0` wanted `LLVM_SYS_180_PREFIX`. Nothing caught it,
    /// because exporting a variable nobody reads fails silently — discovery looks implemented
    /// while never working. That typo is the reason this whole module exists, so pin it against
    /// the real source of truth rather than against a copy of the same guess.
    #[test]
    fn prefix_var_matches_the_locked_llvm_sys_version() {
        let major = locked_llvm_sys_major();
        assert_eq!(
            PREFIX_VAR,
            format!("LLVM_SYS_{major}_PREFIX"),
            "llvm-sys {major}.x reads LLVM_SYS_{major}_PREFIX, but PREFIX_VAR is {PREFIX_VAR:?}. \
             If llvm-sys was bumped, update PREFIX_VAR (and MAJOR) in this module — every \
             `cargo xtask run`/`setup-llvm` path exports that name, and a wrong one is ignored \
             silently rather than erroring."
        );
    }

    /// `MAJOR` (the LLVM release we tell people to install) must agree with the locked `llvm-sys`
    /// too: `llvm-sys 180.x` targets LLVM 18. Without this, `setup-llvm` could cheerfully tell a
    /// contributor to `brew install llvm@18` while the build wants a different LLVM entirely.
    #[test]
    fn major_matches_the_locked_llvm_sys_version() {
        let major = locked_llvm_sys_major();
        assert!(
            major.starts_with(MAJOR),
            "llvm-sys {major}.x targets LLVM {}, but MAJOR is {MAJOR:?} — the install guidance \
             and the version check in `probe` would both be wrong.",
            &major[..2]
        );
    }
}
