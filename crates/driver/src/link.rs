//! Link a backend-emitted object into a runnable executable.
//!
//! A C/LLVM compiler drives the link (it supplies crt startup + the C runtime). The bet
//! program's runtime symbols come from the `rt-stub` static library, which cargo builds next
//! to the `bet` binary. On Unix that's `librt_stub.a` linked via the system `cc`; on Windows
//! it's `rt_stub.lib` linked via `clang` (from the LLVM toolchain), which pulls the MSVC CRT.
//! The archive's members are pulled on demand, so the object must precede the library.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Which runtime archive a compiled program is linked against. Both expose the identical
/// `rt-abi` symbol set, so this is a pure link-line choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Runtime {
    /// The bootstrap stub (`librt_stub.a`) — headless, deterministic, the CI/default.
    #[default]
    Stub,
    /// The real runtime (`libruntime.a`) — growable arenas, recovery, the per-OS layer.
    Real,
}

impl Runtime {
    /// The archive link name (`-l<name>` / `<name>.lib`) for this choice — the base name of the
    /// respective crate's staticlib (`librt_stub.a` / `libruntime.a`).
    ///
    /// These are string literals, *not* a dependency on the `rt-stub`/`runtime` crates: both
    /// export the same `#[no_mangle] bet_*` runtime symbols, so linking either as an rlib into
    /// the `bet` binary would collide on the symbol set (and linking *both* is a hard duplicate-
    /// symbol error on `rust-lld`). The compiler must not carry the runtime's symbols — those
    /// belong only in the *compiled program*, which links one archive by name at `bet build`.
    fn link_name(self) -> &'static str {
        match self {
            Runtime::Stub => "rt_stub",
            Runtime::Real => "runtime",
        }
    }
}

/// Link `obj` (native object bytes) into an executable at `out` (`.exe` is appended on Windows
/// when no extension is given), against the chosen `runtime`. Returns the path actually produced.
pub fn link_executable(obj: &[u8], out: &Path, runtime: Runtime) -> Result<PathBuf, String> {
    let out: PathBuf = if cfg!(windows) && out.extension().is_none() {
        out.with_extension("exe")
    } else {
        out.to_path_buf()
    };
    let obj_path = out.with_extension("o");
    std::fs::write(&obj_path, obj).map_err(|e| format!("writing object file: {e}"))?;

    // `rt_stub` (lib on Windows, `.a` on Unix) lives in the same directory as the running
    // `bet` binary (target/<profile>/); cargo co-locates the staticlib with the driver.
    let exe = std::env::current_exe().map_err(|e| format!("locating the bet binary: {e}"))?;
    let libdir = exe
        .parent()
        .ok_or("the bet binary has no parent directory")?;

    let result = if cfg!(windows) {
        link_msvc(&obj_path, libdir, &out, runtime)
    } else {
        link_unix(&obj_path, libdir, &out, runtime)
    };
    let _ = std::fs::remove_file(&obj_path); // best-effort cleanup
    result.map(|()| out)
}

fn link_unix(obj: &Path, libdir: &Path, out: &Path, runtime: Runtime) -> Result<(), String> {
    let mut cmd = Command::new("cc");
    cmd.arg(obj)
        .arg(format!("-L{}", libdir.display()))
        .arg(format!("-l{}", runtime.link_name()));
    for lib in unix_sys_libs() {
        cmd.arg(lib);
    }
    // The real runtime built with `gg-desktop` statically bundles minifb + cpal, whose macOS
    // framework dependencies are recorded as cargo link directives that do NOT survive into the
    // `.a` archive — so the final `cc` link must name them explicitly. Harmless when gg is
    // headless (the linker drops unreferenced frameworks).
    if cfg!(target_os = "macos") && runtime == Runtime::Real {
        for fw in MACOS_GG_FRAMEWORKS {
            cmd.arg("-framework").arg(fw);
        }
    }
    cmd.arg("-o").arg(out);
    run_linker(cmd, "cc")
}

/// macOS frameworks the `gg-desktop` runtime (minifb + cpal) references but a static archive does
/// not carry: window/graphics/input (Cocoa/Carbon/Metal/MetalKit), the relative-mouse cursor warp
/// (CoreGraphics), and audio (AudioUnit/CoreAudio/CoreFoundation). `AudioUnit`'s dylib re-exports
/// the AudioToolbox symbols.
const MACOS_GG_FRAMEWORKS: &[&str] = &[
    "Cocoa",
    "Carbon",
    "Metal",
    "MetalKit",
    "CoreGraphics",
    "AudioUnit",
    "CoreAudio",
    "CoreFoundation",
];

fn link_msvc(obj: &Path, libdir: &Path, out: &Path, runtime: Runtime) -> Result<(), String> {
    // clang (from the LLVM toolchain) drives the link, pulling the MSVC CRT. The runtime archive
    // bundles the Rust std the program needs; std references the Windows system libraries below,
    // passed as a safe superset (the linker only pulls what's actually referenced).
    let staticlib = libdir.join(format!("{}.lib", runtime.link_name()));
    let mut cmd = Command::new("clang");
    cmd.arg(obj).arg(&staticlib);
    for lib in WINDOWS_SYS_LIBS {
        cmd.arg(format!("-l{lib}"));
    }
    cmd.arg("-o").arg(out);
    run_linker(cmd, "clang")
}

fn run_linker(mut cmd: Command, name: &str) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("running the linker `{name}`: {e}"))?;
    if !status.success() {
        return Err(format!("linker `{name}` failed ({status})"));
    }
    Ok(())
}

/// Extra system libraries the Rust std bundled inside `librt_stub.a` needs (threads for
/// `bet_slide`, `dlopen`, libm). macOS's libSystem already provides them.
fn unix_sys_libs() -> &'static [&'static str] {
    if cfg!(target_os = "linux") {
        &["-lpthread", "-ldl", "-lm"]
    } else {
        &[]
    }
}

/// Windows system libraries referenced by the Rust std inside `rt_stub.lib`. A safe superset
/// of standard-SDK libraries; over-linking is harmless (unreferenced libs are ignored).
const WINDOWS_SYS_LIBS: &[&str] = &[
    "kernel32", "ntdll", "user32", "advapi32", "ws2_32", "userenv", "bcrypt", "dbghelp", "ole32",
    "oleaut32", "shell32", "psapi",
];
