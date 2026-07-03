//! Link a backend-emitted object into a runnable executable.
//!
//! The system C compiler (`cc`) is used as the link *driver* — it supplies the crt startup
//! files and libc for free across platforms. The bet program's runtime symbols
//! (`bet_print`, `bet_rt_init`, …) come from `librt_stub.a`, which cargo builds next to the
//! `bet` binary (a normal `rt-stub` dependency forces it). The archive's members are pulled
//! in on demand to satisfy undefined symbols, so the object must precede `-lrt_stub`.

use std::path::Path;
use std::process::Command;

/// Link `obj` (native object bytes) into an executable at `out`.
pub fn link_executable(obj: &[u8], out: &Path) -> Result<(), String> {
    // Write the object beside the output so the linker has a real file to consume.
    let obj_path = out.with_extension("o");
    std::fs::write(&obj_path, obj).map_err(|e| format!("writing object file: {e}"))?;

    // `librt_stub.a` lives in the same directory as the running `bet` binary
    // (target/<profile>/); cargo co-locates the staticlib with the driver.
    let exe = std::env::current_exe().map_err(|e| format!("locating the bet binary: {e}"))?;
    let libdir = exe
        .parent()
        .ok_or("the bet binary has no parent directory")?;

    let mut cmd = Command::new("cc");
    cmd.arg(&obj_path)
        .arg(format!("-L{}", libdir.display()))
        .arg(format!("-l{}", rt_stub::staticlib_link_name()));
    for lib in system_libs() {
        cmd.arg(lib);
    }
    cmd.arg("-o").arg(out);

    let result = cmd.status();
    let _ = std::fs::remove_file(&obj_path); // best-effort cleanup regardless of outcome
    let status = result.map_err(|e| format!("running the linker `cc`: {e}"))?;
    if !status.success() {
        return Err(format!("linker `cc` failed ({status})"));
    }
    Ok(())
}

/// Extra system libraries the Rust std bundled inside `librt_stub.a` needs (threads for
/// `bet_slide`, `dlopen`, libm). macOS's libSystem already provides them.
fn system_libs() -> &'static [&'static str] {
    #[cfg(target_os = "linux")]
    {
        &["-lpthread", "-ldl", "-lm"]
    }
    #[cfg(not(target_os = "linux"))]
    {
        &[]
    }
}
