//! `gg-backend` — bet's native `gg` platform backend (the SDL-equivalent layer).
//!
//! One small API — [`present`], [`audio`], [`poll`], [`ticks`] — shared by BOTH the real
//! `runtime` (via the frozen `rt-abi` `bet_gg_*` entry points) and the tree-walking
//! `interp` (via its `gg.*` intrinsics). Keeping the platform code in one crate means the
//! compiled path and `bet run` present pixels, drain audio, and report input through the
//! exact same window/stream/queue.
//!
//! ## Two builds from one source
//!
//! * **`desktop` feature ON** — a real window (`minifb`), a real audio output stream
//!   (`cpal`) draining a shared sample ring, and a synthesized input-event queue. All of it
//!   hangs off a single process-global singleton created lazily on first use.
//! * **`desktop` feature OFF (default)** — headless no-ops with ZERO heavy dependencies:
//!   [`present`]/[`audio`] do nothing, [`poll`] always reports [`event_kind::NONE`], and
//!   [`ticks`] is a monotonic `Instant`. This is what the default `cargo build` compiles, so
//!   neither `minifb` nor `cpal` is ever pulled in unless a caller explicitly opts in.
//!
//! ## Keycode convention
//!
//! [`Event::code`] values are defined by [`mod@key`] and are STABLE — the ports depend on the
//! exact numbers (letters are ASCII-uppercase, arrows are 256..=259, etc.). The `desktop`
//! backend maps `minifb::Key` onto them.

// The desktop singleton stores a `minifb::Window` and a `cpal::Stream`, neither of which is
// `Send`. We keep them in a process-global `Mutex` and force `Send` on the wrapper: `gg` is a
// single-threaded platform layer — the game loop presents, polls, and pushes audio from one
// thread — so the invariant holds. This is the one place the crate needs `unsafe`.
#![allow(unsafe_code)]

use rt_abi::{Event, event_kind};

/// Stable [`Event::code`] keycodes. The ports reference these by value, so the numbers are
/// part of the contract, not an implementation detail. Letters map to ASCII uppercase, digits
/// to ASCII `'0'..='9'`, and the arrow keys to a private 256..=259 block above ASCII.
pub mod key {
    // Letters A..=Z → ASCII uppercase (so `W == 87`, `S == 83`, ...).
    pub const A: u32 = b'A' as u32;
    pub const B: u32 = b'B' as u32;
    pub const C: u32 = b'C' as u32;
    pub const D: u32 = b'D' as u32;
    pub const E: u32 = b'E' as u32;
    pub const F: u32 = b'F' as u32;
    pub const G: u32 = b'G' as u32;
    pub const H: u32 = b'H' as u32;
    pub const I: u32 = b'I' as u32;
    pub const J: u32 = b'J' as u32;
    pub const K: u32 = b'K' as u32;
    pub const L: u32 = b'L' as u32;
    pub const M: u32 = b'M' as u32;
    pub const N: u32 = b'N' as u32;
    pub const O: u32 = b'O' as u32;
    pub const P: u32 = b'P' as u32;
    pub const Q: u32 = b'Q' as u32;
    pub const R: u32 = b'R' as u32;
    pub const S: u32 = b'S' as u32;
    pub const T: u32 = b'T' as u32;
    pub const U: u32 = b'U' as u32;
    pub const V: u32 = b'V' as u32;
    pub const W: u32 = b'W' as u32;
    pub const X: u32 = b'X' as u32;
    pub const Y: u32 = b'Y' as u32;
    pub const Z: u32 = b'Z' as u32;

    // Digits '0'..='9' → ASCII.
    pub const D0: u32 = b'0' as u32;
    pub const D1: u32 = b'1' as u32;
    pub const D2: u32 = b'2' as u32;
    pub const D3: u32 = b'3' as u32;
    pub const D4: u32 = b'4' as u32;
    pub const D5: u32 = b'5' as u32;
    pub const D6: u32 = b'6' as u32;
    pub const D7: u32 = b'7' as u32;
    pub const D8: u32 = b'8' as u32;
    pub const D9: u32 = b'9' as u32;

    /// Space bar (ASCII 32).
    pub const SPACE: u32 = b' ' as u32;
    /// Enter / Return. `10` (`\n`), matching the ASCII line feed.
    pub const ENTER: u32 = 10;
    /// Escape (ASCII 27). The `desktop` backend also treats this as a QUIT request.
    pub const ESC: u32 = 27;
    /// Arrow keys — a private block just above ASCII so they never collide with letters/digits.
    pub const UP: u32 = 256;
    pub const DOWN: u32 = 257;
    pub const LEFT: u32 = 258;
    pub const RIGHT: u32 = 259;
}

/// The empty-queue sentinel returned by [`poll`].
const NONE_EVENT: Event = Event {
    kind: event_kind::NONE,
    code: 0,
    x: 0,
    y: 0,
};

/// The window size `size()` reports before any frame is presented (no window yet), and the size
/// a headless build always reports. Kept in sync with `rt-stub`'s `bet_gg_size` default so the
/// two headless paths stay byte-identical.
pub const DEFAULT_W: u32 = 960;
pub const DEFAULT_H: u32 = 640;

/// Pack a `(width, height)` into the `u64` the `bet_gg_size` ABI returns (`w << 32 | h`).
const fn pack_size(w: u32, h: u32) -> u64 {
    ((w as u64) << 32) | h as u64
}

// ===========================================================================
// Public API — the calls the runtime and interp share.
// ===========================================================================

/// Present a framebuffer. `pixels` points to at least `stride * height` `u32`s (row-major,
/// `0x00RR_GGBB`); each of the `height` rows is `stride` pixels wide but only the first
/// `width` are shown. With `desktop`, copies the pixels into the window and pumps input; the
/// headless build ignores everything.
pub fn present(pixels: *const u32, width: u32, height: u32, stride: u32) {
    imp::present(pixels, width, height, stride);
}

/// Submit interleaved audio samples, pushed onto the output ring for the `cpal` callback to
/// drain. Headless: a no-op.
pub fn audio(samples: &[i16]) {
    imp::audio(samples);
}

/// Pop the next queued input event, or an [`event_kind::NONE`] [`Event`] when the queue is
/// empty. Headless: always `NONE`.
pub fn poll() -> Event {
    imp::poll()
}

/// A monotonic high-resolution timer, in nanoseconds. Real in both builds (`std::time::Instant`).
pub fn ticks() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// The current window `(width, height)` packed as `w << 32 | h` — the size the framebuffer
/// should match for 1:1 dynamic resolution. Tracks live resizes; returns the default until the
/// first frame opens the window, and always the default in a headless build.
pub fn size() -> u64 {
    imp::size()
}

// ---------------------------------------------------------------------------
// gg compositor / mixer / mouse (amendment §SP0.4 raise). All share the singleton the four
// original primitives use; the audio mixer lives behind its OWN mutex so the cpal callback
// never touches the outer GG lock (the existing no-deadlock invariant).
// ---------------------------------------------------------------------------

/// Upload an RGBA8 texture (`w * h` pixels, 4 bytes `R,G,B,A` each) and return its 1-based id
/// (`0` = invalid). Headless: `0`.
pub fn tex(rgba: &[u8], w: u32, h: u32) -> u32 {
    imp::tex(rgba, w, h)
}

/// Begin a frame: (re)size the logical canvas to `w * h` and clear it to `clear_argb`
/// (`0x00RR_GGBB`). Headless: a no-op.
pub fn frame(w: u32, h: u32, clear_argb: u32) {
    imp::frame(w, h, clear_argb);
}

/// Blit texture `tex` onto the canvas at `(dx, dy)` with premultiplied src-over alpha, clipped.
/// Headless: a no-op.
pub fn sprite(tex: u32, dx: i32, dy: i32) {
    imp::sprite(tex, dx, dy);
}

/// Blit the source sub-rectangle `(sx, sy, sw, sh)` of texture `tex` to `(dx, dy)` with
/// premultiplied src-over alpha — the same blit math as [`sprite`], windowed to the source rect
/// and clipped to both the texture and the canvas. Headless: a no-op.
#[allow(clippy::too_many_arguments)]
pub fn sprite_sub(tex: u32, sx: i32, sy: i32, sw: u32, sh: u32, dx: i32, dy: i32) {
    imp::sprite_sub(tex, sx, sy, sw, sh, dx, dy);
}

/// Fill a `w * h` rectangle at `(dx, dy)` with `argb` (`0xAARR_GGBB`), src-over, clipped.
/// Headless: a no-op.
pub fn rect(dx: i32, dy: i32, w: u32, h: u32, argb: u32) {
    imp::rect(dx, dy, w, h, argb);
}

/// Present the composited canvas (aspect-fit letterbox) and pump input. Headless: a no-op.
pub fn flush() {
    imp::flush();
}

/// Register a PCM sound (`channels` interleaved little-endian `i16` channels at `rate` Hz) with
/// the mixer and return its 1-based id (`0` = invalid). Headless: `0`.
pub fn sound(pcm: &[u8], channels: u32, rate: u32) -> u32 {
    imp::sound(pcm, channels, rate)
}

/// Start playing `sound` on a voice at `vol_q8` (Q8; `256` = unity), looping if `loop_ != 0`, and
/// return the 1-based voice id (`0` = could not play). Headless: `0`.
pub fn play(sound: u32, loop_: u32, vol_q8: u32) -> u32 {
    imp::play(sound, loop_, vol_q8)
}

/// Stop voice `voice` (from [`play`]). Headless: a no-op.
pub fn stop(voice: u32) {
    imp::stop(voice);
}

/// The mouse position in logical-canvas coordinates, packed `x << 32 | y`. Headless: `0`.
pub fn mouse() -> u64 {
    imp::mouse()
}

// ===========================================================================
// Headless implementation (default: `desktop` OFF). Zero heavy dependencies.
// ===========================================================================

#[cfg(not(feature = "desktop"))]
mod imp {
    use super::{Event, NONE_EVENT};

    pub(super) fn present(_pixels: *const u32, _width: u32, _height: u32, _stride: u32) {}

    pub(super) fn audio(_samples: &[i16]) {}

    pub(super) fn poll() -> Event {
        NONE_EVENT
    }

    pub(super) fn size() -> u64 {
        super::pack_size(super::DEFAULT_W, super::DEFAULT_H)
    }

    pub(super) fn tex(_rgba: &[u8], _w: u32, _h: u32) -> u32 {
        0
    }

    pub(super) fn frame(_w: u32, _h: u32, _clear_argb: u32) {}

    pub(super) fn sprite(_tex: u32, _dx: i32, _dy: i32) {}

    #[allow(clippy::too_many_arguments)]
    pub(super) fn sprite_sub(
        _tex: u32,
        _sx: i32,
        _sy: i32,
        _sw: u32,
        _sh: u32,
        _dx: i32,
        _dy: i32,
    ) {
    }

    pub(super) fn rect(_dx: i32, _dy: i32, _w: u32, _h: u32, _argb: u32) {}

    pub(super) fn flush() {}

    pub(super) fn sound(_pcm: &[u8], _channels: u32, _rate: u32) -> u32 {
        0
    }

    pub(super) fn play(_sound: u32, _loop_: u32, _vol_q8: u32) -> u32 {
        0
    }

    pub(super) fn stop(_voice: u32) {}

    pub(super) fn mouse() -> u64 {
        0
    }
}

// ===========================================================================
// Desktop implementation (`desktop` ON): minifb window + cpal audio + input queue.
// ===========================================================================

#[cfg(feature = "desktop")]
mod imp {
    use super::{Event, NONE_EVENT, event_kind, key};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex, OnceLock};

    /// The window title (brief §1: "bet · pong").
    const WINDOW_TITLE: &str = "bet · pong";
    /// Cap the audio ring so a program that outruns the audio callback can't grow it forever.
    const RING_CAP: usize = 1 << 20;

    /// `minifb::Key` → keycode pairs used to diff the down-key set each frame. `Escape` is
    /// handled separately (it maps to QUIT), so it is intentionally absent here.
    const KEY_TABLE: &[(minifb::Key, u32)] = &[
        (minifb::Key::A, key::A),
        (minifb::Key::B, key::B),
        (minifb::Key::C, key::C),
        (minifb::Key::D, key::D),
        (minifb::Key::E, key::E),
        (minifb::Key::F, key::F),
        (minifb::Key::G, key::G),
        (minifb::Key::H, key::H),
        (minifb::Key::I, key::I),
        (minifb::Key::J, key::J),
        (minifb::Key::K, key::K),
        (minifb::Key::L, key::L),
        (minifb::Key::M, key::M),
        (minifb::Key::N, key::N),
        (minifb::Key::O, key::O),
        (minifb::Key::P, key::P),
        (minifb::Key::Q, key::Q),
        (minifb::Key::R, key::R),
        (minifb::Key::S, key::S),
        (minifb::Key::T, key::T),
        (minifb::Key::U, key::U),
        (minifb::Key::V, key::V),
        (minifb::Key::W, key::W),
        (minifb::Key::X, key::X),
        (minifb::Key::Y, key::Y),
        (minifb::Key::Z, key::Z),
        (minifb::Key::Key0, key::D0),
        (minifb::Key::Key1, key::D1),
        (minifb::Key::Key2, key::D2),
        (minifb::Key::Key3, key::D3),
        (minifb::Key::Key4, key::D4),
        (minifb::Key::Key5, key::D5),
        (minifb::Key::Key6, key::D6),
        (minifb::Key::Key7, key::D7),
        (minifb::Key::Key8, key::D8),
        (minifb::Key::Key9, key::D9),
        (minifb::Key::Space, key::SPACE),
        (minifb::Key::Enter, key::ENTER),
        (minifb::Key::Up, key::UP),
        (minifb::Key::Down, key::DOWN),
        (minifb::Key::Left, key::LEFT),
        (minifb::Key::Right, key::RIGHT),
    ];

    /// An uploaded texture: premultiplied `0xAARR_GGBB` pixels, row-major `w * h`.
    struct Texture {
        w: u32,
        h: u32,
        px: Vec<u32>,
    }

    /// A registered PCM sound. `pcm` is the raw interleaved `i16` at its native `channels`/`rate`;
    /// `resampled` is a lazily-built copy interleaved at the DEVICE channels/rate, so the mixer
    /// advances a voice's `pos` by one per output sample slot (matching the raw ring's 1:1 drain).
    struct Sound {
        pcm: Vec<i16>,
        channels: u32,
        rate: u32,
        resampled: Option<Vec<i16>>,
    }

    /// A playing voice: a cursor into `sounds[sound].resampled` plus its gain and loop flag.
    struct Voice {
        sound: usize,
        pos: usize,
        vol_q8: u32,
        looping: bool,
        active: bool,
    }

    /// The voice mixer. Lives behind its OWN `Arc<Mutex<Mixer>>` (see `GgState::mixer`); the cpal
    /// callback locks only this, never the outer GG mutex.
    struct Mixer {
        sounds: Vec<Sound>,
        voices: Vec<Voice>,
        device_rate: u32,
        device_channels: u16,
    }

    /// The process-global platform state. Created lazily on first [`present`]/[`audio`].
    struct GgState {
        /// The window, created on the first `present` (its size comes from that first frame).
        window: Option<minifb::Window>,
        /// Samples awaiting playback; shared with the `cpal` callback (which only ever locks
        /// THIS mutex, never the outer `GG` one, so the two can't deadlock).
        ring: Arc<Mutex<VecDeque<i16>>>,
        /// The live output stream. Kept alive to keep audio playing; dropped ⇒ silence. `None`
        /// until the first `audio` call (or if no output device could be opened).
        stream: Option<cpal::Stream>,
        /// Whether we already tried (and either succeeded or failed) to open the audio stream.
        audio_started: bool,
        /// The synthesized input-event queue drained by [`poll`].
        events: VecDeque<Event>,
        /// Keycodes that were down at the previous `present`, for edge detection.
        down: Vec<u32>,
        /// Set once a QUIT has been enqueued, so we don't flood the queue every frame.
        quit_sent: bool,
        /// The composited logical canvas (`canvas_w * canvas_h` XRGB pixels), rebuilt by `frame`.
        canvas: Vec<u32>,
        canvas_w: u32,
        canvas_h: u32,
        /// Uploaded textures (premultiplied), indexed by `id - 1`.
        textures: Vec<Texture>,
        /// The integer upscale factor and letterbox offsets recorded at the last `flush`, used to
        /// map window pixels back to logical-canvas coordinates for the mouse.
        scale: u32,
        off_x: i32,
        off_y: i32,
        /// The last known mouse position, in logical-canvas coordinates.
        mouse_x: i32,
        mouse_y: i32,
        /// Debounced mouse-button state, for MOUSE_DOWN / MOUSE_UP edge detection.
        mouse_left: bool,
        mouse_right: bool,
        /// The voice mixer, behind its OWN mutex (NOT the outer GG one) so the cpal callback can
        /// lock it without ever touching GG — preserving the no-deadlock invariant.
        mixer: Arc<Mutex<Mixer>>,
    }

    // SAFETY: `minifb::Window` and `cpal::Stream` are `!Send`, but `gg` is a single-threaded
    // platform layer — every call funnels through the one game-loop thread — so the singleton
    // is only ever touched from that thread. The `Send` claim is what lets us hold the state
    // in a `static Mutex`; the single-thread discipline is what makes it sound.
    unsafe impl Send for GgState {}

    impl GgState {
        fn new() -> Self {
            GgState {
                window: None,
                ring: Arc::new(Mutex::new(VecDeque::new())),
                stream: None,
                audio_started: false,
                events: VecDeque::new(),
                down: Vec::new(),
                quit_sent: false,
                canvas: Vec::new(),
                canvas_w: 0,
                canvas_h: 0,
                textures: Vec::new(),
                scale: 1,
                off_x: 0,
                off_y: 0,
                mouse_x: 0,
                mouse_y: 0,
                mouse_left: false,
                mouse_right: false,
                mixer: Arc::new(Mutex::new(Mixer {
                    sounds: Vec::new(),
                    voices: Vec::new(),
                    device_rate: 0,
                    device_channels: 0,
                })),
            }
        }

        /// Copy `buf` (a tightly packed `w * h` frame) into the window, creating the window on
        /// the first call, then synthesize input events from the new key state.
        fn present(&mut self, buf: &[u32], w: usize, h: usize) {
            let window = self.window.get_or_insert_with(|| {
                // `X1` maps the framebuffer 1:1 to the window (no upscaling), so the program can
                // drive true dynamic resolution by sizing the framebuffer to `size()`. `resize`
                // lets the user maximize; `Stretch` only covers the single transient frame after
                // a resize, before the program reallocates its framebuffer to the new size.
                let opts = minifb::WindowOptions {
                    resize: true,
                    scale: minifb::Scale::X1,
                    scale_mode: minifb::ScaleMode::Stretch,
                    ..minifb::WindowOptions::default()
                };
                minifb::Window::new(WINDOW_TITLE, w, h, opts)
                    .expect("gg: failed to open the minifb window")
            });
            // `update_with_buffer` wants exactly `w * h` pixels; `present()` guarantees `buf`
            // is that long.
            let _ = window.update_with_buffer(buf, w, h);
            self.pump_input();
        }

        /// Diff the current key state against the previous frame, enqueueing KEY_DOWN/KEY_UP
        /// edges, and enqueue a single QUIT when the window is closed or Esc is pressed.
        fn pump_input(&mut self) {
            let Some(window) = self.window.as_ref() else {
                return;
            };

            if !self.quit_sent && (!window.is_open() || window.is_key_down(minifb::Key::Escape)) {
                self.events.push_back(Event {
                    kind: event_kind::QUIT,
                    code: 0,
                    x: 0,
                    y: 0,
                });
                self.quit_sent = true;
            }

            let mut now_down: Vec<u32> = Vec::new();
            for &(mk, code) in KEY_TABLE {
                if window.is_key_down(mk) {
                    now_down.push(code);
                    if !self.down.contains(&code) {
                        self.events.push_back(Event {
                            kind: event_kind::KEY_DOWN,
                            code,
                            x: 0,
                            y: 0,
                        });
                    }
                }
            }
            for &code in &self.down {
                if !now_down.contains(&code) {
                    self.events.push_back(Event {
                        kind: event_kind::KEY_UP,
                        code,
                        x: 0,
                        y: 0,
                    });
                }
            }
            self.down = now_down;

            // Mouse: track the cursor in logical-canvas coords, and synthesize MOUSE_DOWN/UP
            // edges for the two buttons (code 0 = left, 1 = right). Position via `gg.mouse()`.
            if let Some((mx, my)) = window.get_mouse_pos(minifb::MouseMode::Clamp) {
                let scale = self.scale.max(1) as i32;
                let lx = (mx as i32 - self.off_x) / scale;
                let ly = (my as i32 - self.off_y) / scale;
                let max_x = (self.canvas_w as i32 - 1).max(0);
                let max_y = (self.canvas_h as i32 - 1).max(0);
                self.mouse_x = lx.clamp(0, max_x);
                self.mouse_y = ly.clamp(0, max_y);
            }
            let left = window.get_mouse_down(minifb::MouseButton::Left);
            if left != self.mouse_left {
                self.events.push_back(Event {
                    kind: if left {
                        event_kind::MOUSE_DOWN
                    } else {
                        event_kind::MOUSE_UP
                    },
                    code: 0,
                    x: self.mouse_x,
                    y: self.mouse_y,
                });
                self.mouse_left = left;
            }
            let right = window.get_mouse_down(minifb::MouseButton::Right);
            if right != self.mouse_right {
                self.events.push_back(Event {
                    kind: if right {
                        event_kind::MOUSE_DOWN
                    } else {
                        event_kind::MOUSE_UP
                    },
                    code: 1,
                    x: self.mouse_x,
                    y: self.mouse_y,
                });
                self.mouse_right = right;
            }
        }

        /// Push samples onto the ring, opening the output stream on first use. Old samples are
        /// dropped rather than exceeding [`RING_CAP`].
        fn push_audio(&mut self, samples: &[i16]) {
            self.ensure_stream();
            // No stream (no device) ⇒ nothing to feed; skip.
            if self.stream.is_none() {
                return;
            }
            let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
            for &s in samples {
                if ring.len() >= RING_CAP {
                    ring.pop_front();
                }
                ring.push_back(s);
            }
        }

        /// Ensure the audio output stream exists (opened lazily on first audio use). Best-effort:
        /// if no device/stream can be opened, `stream` stays `None` and audio is silently dropped.
        fn ensure_stream(&mut self) {
            if self.audio_started {
                return;
            }
            self.audio_started = true;
            self.stream = build_stream(Arc::clone(&self.ring), Arc::clone(&self.mixer));
        }

        /// Upload an RGBA8 texture (premultiplying each pixel) and return its 1-based id.
        fn tex(&mut self, rgba: &[u8], w: u32, h: u32) -> u32 {
            if w == 0 || h == 0 {
                return 0;
            }
            let n = (w as usize) * (h as usize);
            let mut px = vec![0u32; n];
            for (i, out) in px.iter_mut().enumerate() {
                let base = i * 4;
                // Bounds-guard short input by zero-filling any missing channels.
                let r = u32::from(rgba.get(base).copied().unwrap_or(0));
                let g = u32::from(rgba.get(base + 1).copied().unwrap_or(0));
                let b = u32::from(rgba.get(base + 2).copied().unwrap_or(0));
                let a = u32::from(rgba.get(base + 3).copied().unwrap_or(0));
                // Premultiply so the src-over blit is a straight `src + dst*(255-a)/255`.
                let pr = r * a / 255;
                let pg = g * a / 255;
                let pb = b * a / 255;
                *out = (a << 24) | (pr << 16) | (pg << 8) | pb;
            }
            self.textures.push(Texture { w, h, px });
            self.textures.len() as u32
        }

        /// Begin a frame: (re)size the canvas if needed and clear it to `clear_argb`'s RGB.
        fn frame(&mut self, w: u32, h: u32, clear_argb: u32) {
            let n = (w as usize) * (h as usize);
            if self.canvas_w != w || self.canvas_h != h || self.canvas.len() != n {
                self.canvas = vec![0u32; n];
                self.canvas_w = w;
                self.canvas_h = h;
            }
            let clear = clear_argb & 0x00FF_FFFF;
            for p in self.canvas.iter_mut() {
                *p = clear;
            }
        }

        /// Premultiplied src-over blit of texture `tex` at `(dx, dy)`, clipped to the canvas.
        fn sprite(&mut self, tex: u32, dx: i32, dy: i32) {
            let (tw, th) = match self.textures.get((tex.wrapping_sub(1)) as usize) {
                Some(t) => (t.w as i32, t.h as i32),
                None => return, // `tex == 0` wraps to u32::MAX; both miss and no-op.
            };
            self.blit_sub(tex, 0, 0, tw, th, dx, dy);
        }

        /// Premultiplied src-over blit of the source sub-rectangle `(sx, sy, sw, sh)` of `tex` to
        /// `(dx, dy)`, clipped to both the texture and the canvas. The shared blit core: `sprite`
        /// is this over the whole texture, `sprite_sub` over an arbitrary source window.
        #[allow(clippy::too_many_arguments)]
        fn blit_sub(&mut self, tex: u32, sx: i32, sy: i32, sw: i32, sh: i32, dx: i32, dy: i32) {
            if tex == 0 || tex as usize > self.textures.len() {
                return;
            }
            let cw = self.canvas_w as i32;
            let ch = self.canvas_h as i32;
            let cwu = self.canvas_w as usize;
            // Split the borrow: read the texture, write the canvas (distinct fields of `self`).
            let GgState {
                textures, canvas, ..
            } = self;
            let t = &textures[(tex - 1) as usize];
            let tw = t.w as i32;
            let th = t.h as i32;
            // Clip the source window to the texture; each source pixel maps to a dest offset.
            let sx0 = sx.max(0);
            let sy0 = sy.max(0);
            let sx1 = sx.saturating_add(sw).min(tw);
            let sy1 = sy.saturating_add(sh).min(th);
            for syi in sy0..sy1 {
                let y = dy + (syi - sy);
                if y < 0 || y >= ch {
                    continue;
                }
                for sxi in sx0..sx1 {
                    let x = dx + (sxi - sx);
                    if x < 0 || x >= cw {
                        continue;
                    }
                    let src = t.px[(syi as usize) * (t.w as usize) + sxi as usize];
                    let a = src >> 24;
                    if a == 0 {
                        continue;
                    }
                    let di = (y as usize) * cwu + x as usize;
                    let dst = canvas[di];
                    let inv = 255 - a;
                    let sr = (src >> 16) & 0xFF;
                    let sg = (src >> 8) & 0xFF;
                    let sb = src & 0xFF;
                    let dr = (dst >> 16) & 0xFF;
                    let dg = (dst >> 8) & 0xFF;
                    let db = dst & 0xFF;
                    // `src` is premultiplied: out = src + dst*(255-a)/255.
                    let or = sr + dr * inv / 255;
                    let og = sg + dg * inv / 255;
                    let ob = sb + db * inv / 255;
                    canvas[di] = (or << 16) | (og << 8) | ob;
                }
            }
        }

        /// Premultiplied src-over blit of the source sub-rectangle `(sx, sy, sw, sh)` of `tex`.
        #[allow(clippy::too_many_arguments)]
        fn sprite_sub(&mut self, tex: u32, sx: i32, sy: i32, sw: u32, sh: u32, dx: i32, dy: i32) {
            self.blit_sub(tex, sx, sy, sw as i32, sh as i32, dx, dy);
        }

        /// Src-over fill of a rectangle with a straight (non-premultiplied) `0xAARR_GGBB` color.
        fn rect(&mut self, dx: i32, dy: i32, w: u32, h: u32, argb: u32) {
            let a = argb >> 24;
            if a == 0 {
                return;
            }
            let cw = self.canvas_w as i32;
            let ch = self.canvas_h as i32;
            let cwu = self.canvas_w as usize;
            let sr = (argb >> 16) & 0xFF;
            let sg = (argb >> 8) & 0xFF;
            let sb = argb & 0xFF;
            let inv = 255 - a;
            for ry in 0..h as i32 {
                let y = dy + ry;
                if y < 0 || y >= ch {
                    continue;
                }
                for rx in 0..w as i32 {
                    let x = dx + rx;
                    if x < 0 || x >= cw {
                        continue;
                    }
                    let di = (y as usize) * cwu + x as usize;
                    if a == 255 {
                        self.canvas[di] = argb & 0x00FF_FFFF;
                        continue;
                    }
                    let dst = self.canvas[di];
                    let dr = (dst >> 16) & 0xFF;
                    let dg = (dst >> 8) & 0xFF;
                    let db = dst & 0xFF;
                    let or = (sr * a + dr * inv) / 255;
                    let og = (sg * a + dg * inv) / 255;
                    let ob = (sb * a + db * inv) / 255;
                    self.canvas[di] = (or << 16) | (og << 8) | ob;
                }
            }
        }

        /// Present the composited canvas: aspect-fit (integer nearest-neighbor upscale, centered
        /// with black letterbox bars) into the window, then pump input.
        fn flush(&mut self) {
            let cw = self.canvas_w as usize;
            let ch = self.canvas_h as usize;
            if cw == 0 || ch == 0 {
                // Nothing to present yet, but still pump input so quit/keys are observed.
                self.pump_input();
                return;
            }
            // 1. Ensure the window (default ~2x the canvas), read its live size, end that borrow.
            let window = self.window.get_or_insert_with(|| {
                let opts = minifb::WindowOptions {
                    resize: true,
                    scale: minifb::Scale::X1,
                    scale_mode: minifb::ScaleMode::Stretch,
                    ..minifb::WindowOptions::default()
                };
                minifb::Window::new(WINDOW_TITLE, cw * 2, ch * 2, opts)
                    .expect("gg: failed to open the minifb window")
            });
            let (win_w, win_h) = window.get_size();
            // 2. Integer aspect-fit scale + centered offsets; nearest-neighbor upscale into `present`.
            let scale = (win_w / cw).min(win_h / ch).max(1);
            let out_w = cw * scale;
            let out_h = ch * scale;
            let off_x = (win_w as i32 - out_w as i32) / 2;
            let off_y = (win_h as i32 - out_h as i32) / 2;
            let mut present = vec![0u32; win_w * win_h];
            for y in 0..out_h {
                let py = off_y + y as i32;
                if py < 0 || py >= win_h as i32 {
                    continue;
                }
                let sy = y / scale;
                let row = (py as usize) * win_w;
                for x in 0..out_w {
                    let px_out = off_x + x as i32;
                    if px_out < 0 || px_out >= win_w as i32 {
                        continue;
                    }
                    present[row + px_out as usize] = self.canvas[sy * cw + x / scale];
                }
            }
            // 3. Re-borrow the window and present the upscaled buffer.
            let _ = self
                .window
                .as_mut()
                .unwrap()
                .update_with_buffer(&present, win_w, win_h);
            // 4. Record the mapping for the mouse, then pump input.
            self.scale = scale as u32;
            self.off_x = off_x;
            self.off_y = off_y;
            self.pump_input();
        }

        /// Register a PCM sound (interleaved little-endian `i16`) with the mixer; returns its
        /// 1-based id.
        fn sound(&mut self, pcm: &[u8], channels: u32, rate: u32) -> u32 {
            let samples: Vec<i16> = pcm
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            let channels = channels.max(1);
            let rate = if rate == 0 { 44_100 } else { rate };
            let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
            mixer.sounds.push(Sound {
                pcm: samples,
                channels,
                rate,
                resampled: None,
            });
            mixer.sounds.len() as u32
        }

        /// Start a voice playing `sound`, resampling it to the device format once. Returns the
        /// 1-based voice id, or `0` if there is no audio device or the sound id is invalid.
        fn play(&mut self, sound: u32, loop_: u32, vol_q8: u32) -> u32 {
            self.ensure_stream();
            if self.stream.is_none() {
                return 0;
            }
            let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
            if sound == 0 || sound as usize > mixer.sounds.len() {
                return 0;
            }
            let idx = (sound - 1) as usize;
            if mixer.sounds[idx].resampled.is_none() {
                let dst_ch = mixer.device_channels as u32;
                let dst_rate = mixer.device_rate;
                let s = &mixer.sounds[idx];
                let dst_rate = if dst_rate == 0 { s.rate } else { dst_rate };
                let res = resample(&s.pcm, s.channels, s.rate, dst_ch, dst_rate);
                mixer.sounds[idx].resampled = Some(res);
            }
            mixer.voices.push(Voice {
                sound: idx,
                pos: 0,
                vol_q8,
                looping: loop_ != 0,
                active: true,
            });
            mixer.voices.len() as u32
        }

        /// Stop the voice `voice` (1-based); unknown or finished ids are ignored.
        fn stop(&mut self, voice: u32) {
            if voice == 0 {
                return;
            }
            let mut mixer = self.mixer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(v) = mixer.voices.get_mut((voice - 1) as usize) {
                v.active = false;
            }
        }

        /// The mouse position in logical-canvas coordinates, packed `x << 32 | y`.
        fn mouse(&self) -> u64 {
            ((self.mouse_x as u32 as u64) << 32) | (self.mouse_y as u32 as u64)
        }
    }

    /// The process-global singleton. `Mutex<GgState>: Sync` holds because we force `GgState:
    /// Send`; the single-thread discipline (see the `unsafe impl`) makes that sound.
    static GG: OnceLock<Mutex<GgState>> = OnceLock::new();

    fn with_state<R>(f: impl FnOnce(&mut GgState) -> R) -> R {
        let cell = GG.get_or_init(|| Mutex::new(GgState::new()));
        let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    /// Open the default output device and start a stream whose callback drains `ring`. Returns
    /// `None` (silently) if no device/config/stream could be set up — audio is best-effort.
    fn build_stream(
        ring: Arc<Mutex<VecDeque<i16>>>,
        mixer: Arc<Mutex<Mixer>>,
    ) -> Option<cpal::Stream> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let device = cpal::default_host().default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        // Record the device format so `play` resamples each sound to it exactly once.
        {
            let mut m = mixer.lock().unwrap_or_else(|e| e.into_inner());
            m.device_rate = config.sample_rate.0;
            m.device_channels = config.channels;
        }
        let err_fn = |_err| { /* best-effort audio: ignore stream errors */ };

        let stream = match sample_format {
            cpal::SampleFormat::I16 => {
                let ring = Arc::clone(&ring);
                let mixer = Arc::clone(&mixer);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
                            let mut mixer = mixer.lock().unwrap_or_else(|e| e.into_inner());
                            for s in data.iter_mut() {
                                *s = mix_sample(&mut ring, &mut mixer);
                            }
                        },
                        err_fn,
                        None,
                    )
                    .ok()?
            }
            cpal::SampleFormat::F32 => {
                let ring = Arc::clone(&ring);
                let mixer = Arc::clone(&mixer);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
                            let mut mixer = mixer.lock().unwrap_or_else(|e| e.into_inner());
                            for s in data.iter_mut() {
                                let v = mix_sample(&mut ring, &mut mixer);
                                *s = f32::from(v) / 32768.0;
                            }
                        },
                        err_fn,
                        None,
                    )
                    .ok()?
            }
            // Some other native format — skip audio rather than guess.
            _ => return None,
        };
        stream.play().ok()?;
        Some(stream)
    }

    /// Resample `pcm` (interleaved `src_ch` channels at `src_rate` Hz) to interleaved `dst_ch`
    /// channels at `dst_rate` Hz, nearest-neighbor. The result is interleaved at the DEVICE channel
    /// count so the mixer advances a voice's `pos` by one per output sample slot. Channel mapping:
    /// mono -> duplicate, multi -> mono average, else copy shared channels.
    fn resample(pcm: &[i16], src_ch: u32, src_rate: u32, dst_ch: u32, dst_rate: u32) -> Vec<i16> {
        let src_ch = src_ch.max(1) as usize;
        let dst_ch = dst_ch.max(1) as usize;
        let src_rate = src_rate.max(1) as u64;
        let dst_rate = dst_rate.max(1) as u64;
        let src_frames = pcm.len() / src_ch;
        if src_frames == 0 {
            return Vec::new();
        }
        let dst_frames = (src_frames as u64 * dst_rate / src_rate) as usize;
        let mut out = Vec::with_capacity(dst_frames * dst_ch);
        for f in 0..dst_frames {
            let sf = ((f as u64 * src_rate / dst_rate) as usize).min(src_frames - 1);
            let base = sf * src_ch;
            if dst_ch == src_ch {
                out.extend_from_slice(&pcm[base..base + src_ch]);
            } else if src_ch == 1 {
                let s = pcm[base];
                for _ in 0..dst_ch {
                    out.push(s);
                }
            } else if dst_ch == 1 {
                let mut acc = 0i32;
                for c in 0..src_ch {
                    acc += i32::from(pcm[base + c]);
                }
                out.push((acc / src_ch as i32) as i16);
            } else {
                for c in 0..dst_ch {
                    out.push(if c < src_ch { pcm[base + c] } else { 0 });
                }
            }
        }
        out
    }

    /// Produce one output sample: the raw ring (kept 1:1 for `gg.audio` / Pong) plus every active
    /// mixer voice, summed and clipped to `i16`. Advances each voice's `pos`; a one-shot voice
    /// deactivates at its end, a looping voice wraps.
    fn mix_sample(ring: &mut VecDeque<i16>, mixer: &mut Mixer) -> i16 {
        let mut acc = i32::from(ring.pop_front().unwrap_or(0));
        let Mixer { sounds, voices, .. } = mixer;
        for v in voices.iter_mut() {
            if !v.active {
                continue;
            }
            let res = match sounds[v.sound].resampled.as_ref() {
                Some(r) if !r.is_empty() => r,
                _ => {
                    v.active = false;
                    continue;
                }
            };
            acc += (i32::from(res[v.pos]) * v.vol_q8 as i32) >> 8;
            v.pos += 1;
            if v.pos >= res.len() {
                if v.looping {
                    v.pos = 0;
                } else {
                    v.active = false;
                }
            }
        }
        acc.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    }

    pub(super) fn present(pixels: *const u32, width: u32, height: u32, stride: u32) {
        let (w, h, stride) = (width as usize, height as usize, stride as usize);
        if w == 0 || h == 0 {
            return;
        }
        // Copy the (possibly strided) source into a tightly packed `w * h` buffer up front, so
        // the raw-pointer read is confined to here and the window only ever sees a packed frame.
        let mut buf = vec![0u32; w * h];
        if !pixels.is_null() {
            // SAFETY: the caller guarantees `pixels` addresses at least `stride * height`
            // readable `u32`s (the frozen `FrameBuffer` contract); we read `w <= stride` per row.
            unsafe {
                for y in 0..h {
                    let src = pixels.add(y * stride);
                    std::ptr::copy_nonoverlapping(src, buf[y * w..].as_mut_ptr(), w);
                }
            }
        }
        with_state(|st| st.present(&buf, w, h));
    }

    pub(super) fn audio(samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        with_state(|st| st.push_audio(samples));
    }

    pub(super) fn poll() -> Event {
        with_state(|st| st.events.pop_front().unwrap_or(NONE_EVENT))
    }

    pub(super) fn size() -> u64 {
        with_state(|st| match st.window.as_ref() {
            // `get_size` is the live window size — it tracks user resizes, driving dynamic
            // resolution. Before the first `present` there's no window yet: report the default.
            Some(win) => {
                let (w, h) = win.get_size();
                super::pack_size(w as u32, h as u32)
            }
            None => super::pack_size(super::DEFAULT_W, super::DEFAULT_H),
        })
    }

    pub(super) fn tex(rgba: &[u8], w: u32, h: u32) -> u32 {
        with_state(|st| st.tex(rgba, w, h))
    }

    pub(super) fn frame(w: u32, h: u32, clear_argb: u32) {
        with_state(|st| st.frame(w, h, clear_argb));
    }

    pub(super) fn sprite(tex: u32, dx: i32, dy: i32) {
        with_state(|st| st.sprite(tex, dx, dy));
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn sprite_sub(tex: u32, sx: i32, sy: i32, sw: u32, sh: u32, dx: i32, dy: i32) {
        with_state(|st| st.sprite_sub(tex, sx, sy, sw, sh, dx, dy));
    }

    pub(super) fn rect(dx: i32, dy: i32, w: u32, h: u32, argb: u32) {
        with_state(|st| st.rect(dx, dy, w, h, argb));
    }

    pub(super) fn flush() {
        with_state(|st| st.flush());
    }

    pub(super) fn sound(pcm: &[u8], channels: u32, rate: u32) -> u32 {
        with_state(|st| st.sound(pcm, channels, rate))
    }

    pub(super) fn play(sound: u32, loop_: u32, vol_q8: u32) -> u32 {
        with_state(|st| st.play(sound, loop_, vol_q8))
    }

    pub(super) fn stop(voice: u32) {
        with_state(|st| st.stop(voice));
    }

    pub(super) fn mouse() -> u64 {
        with_state(|st| st.mouse())
    }
}
