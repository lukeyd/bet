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
        }

        /// Push samples onto the ring, opening the output stream on first use. Old samples are
        /// dropped rather than exceeding [`RING_CAP`].
        fn push_audio(&mut self, samples: &[i16]) {
            if !self.audio_started {
                self.audio_started = true;
                self.stream = build_stream(Arc::clone(&self.ring));
            }
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
    fn build_stream(ring: Arc<Mutex<VecDeque<i16>>>) -> Option<cpal::Stream> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let device = cpal::default_host().default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let err_fn = |_err| { /* best-effort audio: ignore stream errors */ };

        let stream = match sample_format {
            cpal::SampleFormat::I16 => {
                let ring = Arc::clone(&ring);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
                            for s in data.iter_mut() {
                                *s = ring.pop_front().unwrap_or(0);
                            }
                        },
                        err_fn,
                        None,
                    )
                    .ok()?
            }
            cpal::SampleFormat::F32 => {
                let ring = Arc::clone(&ring);
                device
                    .build_output_stream(
                        &config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            let mut ring = ring.lock().unwrap_or_else(|e| e.into_inner());
                            for s in data.iter_mut() {
                                let v = ring.pop_front().unwrap_or(0);
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
}
