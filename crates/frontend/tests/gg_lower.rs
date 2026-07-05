//! Lowering tests for the `gg.*` platform-layer intrinsics (framebuffer / audio / input /
//! timing). `frontend::compile` parses, lowers, and **validates**, so a successful compile means
//! the emitted `midir` is well-formed; each test then asserts the expected `extern` symbol and
//! shape appear in the rendered `.mir`. The externs must match `rt-abi`'s frozen signatures:
//! `bet_gg_present(rawptr)`, `bet_gg_audio(rawptr, u64)`, `bet_gg_poll(rawptr) -> bool`,
//! `bet_gg_ticks() -> u64`.

fn mir(src: &str) -> String {
    let m = frontend::compile(src).unwrap_or_else(|e| panic!("should lower: {e}"));
    midir::print(&m)
}

#[test]
fn gg_blit_builds_framebuffer_and_presents() {
    let src = "finna main() { lowkey px: u32[4] = [0, 0, 0, 0]\n gg.blit(px, 2, 2) }";
    let text = mir(src);
    // A `FrameBuffer`-shaped struct is synthesized, filled, and its address handed to present.
    assert!(
        text.contains("bet_gg_present"),
        "present extern missing:\n{text}"
    );
    assert!(
        text.contains("addr_of"),
        "framebuffer address not taken:\n{text}"
    );
}

#[test]
fn gg_audio_passes_base_pointer_and_count() {
    let src = "finna main() { lowkey buf: i16[8] = [0, 0, 0, 0, 0, 0, 0, 0]\n gg.audio(buf, 4) }";
    let text = mir(src);
    assert!(
        text.contains("bet_gg_audio"),
        "audio extern missing:\n{text}"
    );
}

#[test]
fn gg_poll_returns_kind_code_tuple() {
    let src = "finna main() { lowkey k, c = gg.poll()\n spill.it(k)\n spill.it(c) }";
    let text = mir(src);
    assert!(text.contains("bet_gg_poll"), "poll extern missing:\n{text}");
    // Two `u32` event fields zero-extended into the `(int, int)` result.
    assert!(
        text.contains("tuple("),
        "poll should build a tuple:\n{text}"
    );
}

#[test]
fn gg_ticks_presents_u64_as_int() {
    let src = "finna main() { lowkey t = gg.ticks()\n spill.it(t) }";
    let text = mir(src);
    assert!(
        text.contains("bet_gg_ticks"),
        "ticks extern missing:\n{text}"
    );
}

/// The whole task snippet, exercising `blit`, `poll`, and a downstream `spill.it` together.
#[test]
fn gg_full_snippet_lowers() {
    let src = "finna main() {\n lowkey px: u32[4] = [0, 0, 0, 0]\n gg.blit(px, 2, 2)\n \
               lowkey k, c = gg.poll()\n spill.it(k) }";
    let text = mir(src);
    assert!(text.contains("bet_gg_present"));
    assert!(text.contains("bet_gg_poll"));
    // Uncomment / run with `--nocapture` to inspect the lowered IR.
    println!("=== gg full snippet .mir ===\n{text}");
}
