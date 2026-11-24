//! Screen mirror: the keyboard as an ambient light for the screen. The whole keyboard shows one smooth gradient
//! of the screen's colours, all keys updated together (the owner, 2026-11-24: per-key copies flickered, bits
//! changing on their own distracted, a 2 s ease felt delayed; "the whole keyboard map at the same time", the
//! colours "relative" to the screen's average, "a gradient").
//!
//! - The screen is read small and split into [`ZONES_X`] x [`ZONES_Y`] areas ([`zones`]); nothing is stored.
//!   [`dxgi::DxgiScreen`] (Desktop Duplication) reads it on the graphics card, shrunk there by mipmaps, and only
//!   when the screen changed: a still screen costs next to nothing. When that can't start, [`win::WinScreen`]
//!   reads it through GDI (point-sampled to [`GRAB_W`] x [`GRAB_H`]). [`open`] picks.
//! - `spread` (0..1) pulls each area toward the whole screen's average: 0 is one colour for the whole keyboard,
//!   1 the areas as they are.
//! - The keys take a gradient between the areas' centres ([`gradient`], bilinear), so colours flow across the
//!   keyboard instead of changing key by key.
//! - `saturation` multiplies the areas' colour saturation (HSV, brightness kept): averages lean grey, and the
//!   owner found them pale; 2x was right. Greys stay grey.
//! - `smooth_ms` eases the whole keyboard toward the screen (short: it should follow, not lag). When any key
//!   has moved more than [`THRESHOLD`], every key is written at once.
//! - Colours go through the LED balance and palette (`LightingSettings::led`), so white reads white.
//! - AWCC is paused around each write only (docs/lighting-control.md). On stop the static colours come back.

use crate::clock::send;
use crate::lightctl::{self, Applied, LightIo, LightingSettings, Mode, PowerState, Shown};
use crate::reactive::{diff, frame_steps, full_plan, key_centre, Clock, FrameKind};
use alienfx::device::{check_steps, Pauser};
use alienfx::v5::Key;
use alienfx::{x17, Api, Rgb};
use serde::Serialize;

/// The shrunk screen: 4 pixels per key across (16 keys), 6 per row (6 rows).
pub const GRAB_W: usize = 128;
pub const GRAB_H: usize = 72;
/// The keyboard is written again (all of it) when a key moved more than this on any channel.
pub const THRESHOLD: u8 = 6;
/// Writes a second, the range accepted.
pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 1..=30;
pub const DEFAULT_FPS: u32 = 8;
/// The ease's time constant, ms, and its range.
pub const DEFAULT_SMOOTH_MS: u64 = 300;
pub const SMOOTH_RANGE: std::ops::RangeInclusive<u64> = 0..=10_000;
/// The screen's areas: columns and rows.
pub const ZONES_X: usize = 4;
pub const ZONES_Y: usize = 2;
/// How far the areas keep their own colour from the screen's average, 0..1 (the owner tried 0.5, 0.8 and 1.0 and
/// picked 1.0, 2026-11-24: 0.5 was "too much of the same").
pub const DEFAULT_SPREAD: f64 = 1.0;
pub const DEFAULT_SATURATION: f64 = 2.0;
/// The saturation boost's range.
pub const SATURATION_RANGE: std::ops::RangeInclusive<f64> = 1.0..=4.0;

/// The mirror's look.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ambient {
    pub fps: u32,
    pub smooth_ms: u64,
    pub spread: f64,
    pub saturation: f64,
}

impl Default for Ambient {
    fn default() -> Self {
        Ambient { fps: DEFAULT_FPS, smooth_ms: DEFAULT_SMOOTH_MS, spread: DEFAULT_SPREAD, saturation: DEFAULT_SATURATION }
    }
}

fn to_rgb(c: [f64; 3]) -> Rgb {
    let ch = |x: f64| x.round().clamp(0.0, 255.0) as u8;
    Rgb::new(ch(c[0]), ch(c[1]), ch(c[2]))
}

fn to_f(c: Rgb) -> [f64; 3] {
    [c.r as f64, c.g as f64, c.b as f64]
}

/// The screen's [`ZONES_X`] x [`ZONES_Y`] area averages, row by row from the top-left.
pub fn zones(g: &Grab) -> Vec<[f64; 3]> {
    let mut out = Vec::with_capacity(ZONES_X * ZONES_Y);
    for j in 0..ZONES_Y {
        for i in 0..ZONES_X {
            let (x0, x1) = (i * g.w / ZONES_X, ((i + 1) * g.w / ZONES_X).max(i * g.w / ZONES_X + 1));
            let (y0, y1) = (j * g.h / ZONES_Y, ((j + 1) * g.h / ZONES_Y).max(j * g.h / ZONES_Y + 1));
            let mut sum = [0.0; 3];
            let mut n = 0.0;
            for y in y0..y1.min(g.h) {
                for x in x0..x1.min(g.w) {
                    let c = g.px[y * g.w + x];
                    sum[0] += c.r as f64;
                    sum[1] += c.g as f64;
                    sum[2] += c.b as f64;
                    n += 1.0;
                }
            }
            out.push(if n > 0.0 { [sum[0] / n, sum[1] / n, sum[2] / n] } else { [0.0; 3] });
        }
    }
    out
}

/// Every lit key's colour from the areas: bilinear between the areas' centres at the key's centre (keys beyond
/// the outer centres take the edge areas' colours).
pub fn gradient(zones: &[[f64; 3]]) -> Vec<(u8, [f64; 3])> {
    let (zw, zh) = (16.0 / ZONES_X as f64, 6.0 / ZONES_Y as f64);
    x17::KEYS
        .iter()
        .filter_map(|k| key_centre(k).map(|c| (k.id, c)))
        .map(|(id, (x, y))| {
            let fx = (x / zw - 0.5).clamp(0.0, (ZONES_X - 1) as f64);
            let fy = (y / zh - 0.5).clamp(0.0, (ZONES_Y - 1) as f64);
            let (i0, j0) = (fx.floor() as usize, fy.floor() as usize);
            let (i1, j1) = ((i0 + 1).min(ZONES_X - 1), (j0 + 1).min(ZONES_Y - 1));
            let (tx, ty) = (fx - i0 as f64, fy - j0 as f64);
            let z = |i: usize, j: usize| zones[j * ZONES_X + i];
            let mut c = [0.0; 3];
            for ch in 0..3 {
                let top = z(i0, j0)[ch] * (1.0 - tx) + z(i1, j0)[ch] * tx;
                let bottom = z(i0, j1)[ch] * (1.0 - tx) + z(i1, j1)[ch] * tx;
                c[ch] = top * (1.0 - ty) + bottom * ty;
            }
            (id, c)
        })
        .collect()
}

/// What the keys aim for from one screen read: the areas pulled toward the screen's average by `spread`,
/// saturated, spread as a gradient over the keys, LED-corrected.
pub fn aim(s: &LightingSettings, g: &Grab, amb: &Ambient) -> Vec<(u8, [f64; 3])> {
    let z = zones(g);
    let n = z.len() as f64;
    let avg = [0, 1, 2].map(|ch| z.iter().map(|c| c[ch]).sum::<f64>() / n);
    let z: Vec<[f64; 3]> = z.iter().map(|c| to_f(saturate(to_rgb([0, 1, 2].map(|ch| avg[ch] + (c[ch] - avg[ch]) * amb.spread)), amb.saturation))).collect();
    gradient(&z).into_iter().map(|(id, c)| (id, to_f(s.led(to_rgb(c))))).collect()
}

/// `c` with its HSV saturation times `f` (at most full), hue and brightness kept.
pub fn saturate(c: Rgb, f: f64) -> Rgb {
    let (r, g, b) = (c.r as f64, c.g as f64, c.b as f64);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max <= 0.0 || max == min {
        return c;
    }
    let s = (max - min) / max;
    let s2 = (s * f).min(1.0);
    // Each channel keeps its place between min and max; the new min is max * (1 - s2).
    let new_min = max * (1.0 - s2);
    let ch = |x: f64| (new_min + (x - min) / (max - min) * (max - new_min)).round().clamp(0.0, 255.0) as u8;
    Rgb::new(ch(r), ch(g), ch(b))
}

/// A shrunk screen: `w` x `h` pixels, row by row from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grab {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Rgb>,
}

/// Where the screen comes from: Windows, or a script in tests.
pub trait ScreenSource {
    /// The screen's size in pixels (for the message).
    fn size(&mut self) -> (i32, i32);
    /// The screen shrunk to [`GRAB_W`] x [`GRAB_H`]; `None` when it can't be read (a secure desktop, say).
    fn grab(&mut self) -> Option<Grab>;
    /// How the screen is read (for the message).
    fn kind(&self) -> &'static str {
        "simulated"
    }
}

/// Each lit key's average colour of its part of `g` (screen colours, before the LED correction).
pub fn key_colours(g: &Grab) -> Vec<(u8, Rgb)> {
    let (sx, sy) = (g.w as f64 / 64.0, g.h as f64 / 6.0); // pixels per quarter key, per row
    x17::KEYS
        .iter()
        .filter(|k| k.present)
        .map(|k| {
            let x0 = ((k.col as f64 * sx).floor() as usize).min(g.w - 1);
            let x1 = (((k.col + k.width) as f64 * sx).ceil() as usize).clamp(x0 + 1, g.w);
            let y0 = ((k.row as f64 * sy).floor() as usize).min(g.h - 1);
            let y1 = (((k.row + 1) as f64 * sy).ceil() as usize).clamp(y0 + 1, g.h);
            let (mut r, mut gg, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let c = g.px[y * g.w + x];
                    r += c.r as u32;
                    gg += c.g as u32;
                    b += c.b as u32;
                    n += 1;
                }
            }
            (k.id, Rgb::new((r / n) as u8, (gg / n) as u8, (b / n) as u8))
        })
        .collect()
}

pub fn eligible(s: &LightingSettings, p: &PowerState, a: &Ambient) -> Result<(), String> {
    s.validate()?;
    if !(a.saturation.is_finite() && SATURATION_RANGE.contains(&a.saturation)) {
        return Err(format!("saturation {} is out of range: 1-4", a.saturation));
    }
    if !FPS_RANGE.contains(&a.fps) {
        return Err(format!("{} writes a second is out of range: 1-30", a.fps));
    }
    if !SMOOTH_RANGE.contains(&a.smooth_ms) {
        return Err(format!("an ease of {} ms is out of range: 0-10000", a.smooth_ms));
    }
    if !(a.spread.is_finite() && (0.0..=1.0).contains(&a.spread)) {
        return Err(format!("spread {} is out of range: 0-1", a.spread));
    }
    if s.mode != Mode::Custom {
        return Err("lighting is AWCC's (mode awcc), so the app writes nothing: switch to custom for the screen mirror".into());
    }
    if lightctl::level(s, p) == 0 {
        return Err("the keyboard is dark (off, brightness 0, or a sleep / lid / display rule)".into());
    }
    Ok(())
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorSummary {
    pub screen: (i32, i32),
    pub first: usize,
    pub frames: usize,
    pub reports: usize,
    pub keys: usize,
    /// Screen reads.
    pub grabs: usize,
    /// The slowest screen read, ms.
    pub max_grab_ms: u64,
    pub ran_ms: u64,
    pub restore: Applied,
}

/// Runs the mirror on `io` until `stop(now)` or the clock is stopped. Stops cleanly: the static settings
/// written back, also after a failed write.
#[allow(clippy::too_many_arguments)]
pub fn run(io: &mut dyn LightIo, s: &LightingSettings, p: &PowerState, prev: Shown, pauser: Option<&dyn Pauser>, amb: &Ambient, clock: &mut dyn Clock, src: &mut dyn ScreenSource, stop: &mut dyn FnMut(u64) -> bool) -> Result<MirrorSummary, String> {
    eligible(s, p, amb)?;
    let t0 = clock.now_ms();
    let size = src.size();
    let first_grab = src.grab().ok_or("the screen can't be read")?;
    // What each key is easing through (exact, so slow eases don't stall on rounding), and what was sent.
    let mut now_c = aim(s, &first_grab, amb);
    let mut shown: Vec<(u8, Rgb)> = now_c.iter().map(|(id, c)| (*id, to_rgb(*c))).collect();
    let steps = full_plan(prev, &shown, s, p)?;
    check_steps(Api::V5, &steps)?;
    let mut dev = io.keyboard()?;
    let during = Shown { effect: false, level: lightctl::level(s, p) };
    let put_back = |io: &mut dyn LightIo| lightctl::write_from(io, during, s, p, pauser);
    let first = match send(dev.as_mut(), pauser, &steps) {
        Ok(n) => n,
        Err(e) => {
            drop(dev);
            let _ = put_back(io);
            return Err(format!("the screen mirror couldn't start: {e}"));
        }
    };
    let mut sum = MirrorSummary { screen: size, first, grabs: 1, ..Default::default() };
    let interval = 1_000 / amb.fps as u64;
    let mut last = t0;
    let mut failed = None;
    loop {
        let start = clock.now_ms();
        if clock.stopped() || stop(start) {
            break;
        }
        if let Some(g) = src.grab() {
            sum.grabs += 1;
            let after = clock.now_ms();
            sum.max_grab_ms = sum.max_grab_ms.max(after - start);
            let k = if amb.smooth_ms == 0 { 1.0 } else { 1.0 - (-((after - last) as f64) / amb.smooth_ms as f64).exp() };
            last = after;
            for ((_, c), (_, t)) in now_c.iter_mut().zip(aim(s, &g, amb)) {
                for ch in 0..3 {
                    c[ch] += (t[ch] - c[ch]) * k;
                }
            }
            let next: Vec<(u8, Rgb)> = now_c.iter().map(|(id, c)| (*id, to_rgb(*c))).collect();
            // The whole keyboard at once, never bits of it.
            let moved = next.iter().zip(&shown).any(|((_, a), (_, b))| diff(*a, *b) > THRESHOLD);
            let changed: Vec<(u8, Rgb)> = if moved { next } else { Vec::new() };
            if !changed.is_empty() {
                let keys: Vec<Key> = changed.iter().map(|(id, c)| Key::new(*id, *c)).collect();
                match frame_steps(FrameKind::Full, &keys).and_then(|st| send(dev.as_mut(), pauser, &st)) {
                    Ok(n) => {
                        sum.frames += 1;
                        sum.reports += n;
                        sum.keys += changed.len();
                        for (id, c) in changed {
                            if let Some(slot) = shown.iter_mut().find(|(k, _)| *k == id) {
                                slot.1 = c;
                            }
                        }
                    }
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                }
            }
        }
        let spent = clock.now_ms() - start;
        clock.sleep_ms(interval.saturating_sub(spent).max(1));
    }
    sum.ran_ms = clock.now_ms() - t0;
    drop(dev);
    let restore = put_back(io);
    match (failed, restore) {
        (None, Ok(a)) => {
            sum.restore = a;
            Ok(sum)
        }
        (Some(e), Ok(a)) => Err(format!("The screen mirror stopped: {e}. The static colours were written back: {}", a.text())),
        (Some(e), Err(r)) => Err(format!("The screen mirror stopped: {e}. Writing the static colours back failed too: {r}")),
        (None, Err(r)) => Err(format!("the screen mirror stopped, but writing the static colours back failed: {r}")),
    }
}

/// The real screen (Windows): GDI shrinks the whole virtual screen into a small bitmap (halftone, so each small
/// pixel is an average), read back with `GetDIBits`.
#[cfg(windows)]
pub mod win {
    use super::{Grab, ScreenSource, GRAB_H, GRAB_W};
    use alienfx::Rgb;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};

    pub struct WinScreen {
        screen: HDC,
        mem: HDC,
        bmp: HBITMAP,
        old: HGDIOBJ,
    }

    impl WinScreen {
        pub fn new() -> Result<WinScreen, String> {
            // SAFETY: plain GDI calls; every handle is released in Drop.
            unsafe {
                let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                let screen = GetDC(None);
                if screen.is_invalid() {
                    return Err("the screen can't be opened".into());
                }
                let mem = CreateCompatibleDC(Some(screen));
                let bmp = CreateCompatibleBitmap(screen, GRAB_W as i32, GRAB_H as i32);
                let old = SelectObject(mem, bmp.into());
                SetStretchBltMode(mem, COLORONCOLOR);
                Ok(WinScreen { screen, mem, bmp, old })
            }
        }

        fn virtual_screen() -> (i32, i32, i32, i32) {
            // SAFETY: no pointers.
            unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) }
        }
    }

    impl Drop for WinScreen {
        fn drop(&mut self) {
            // SAFETY: the handles made in `new`.
            unsafe {
                SelectObject(self.mem, self.old);
                let _ = DeleteObject(self.bmp.into());
                let _ = DeleteDC(self.mem);
                ReleaseDC(None, self.screen);
            }
        }
    }

    impl ScreenSource for WinScreen {
        fn kind(&self) -> &'static str {
            "GDI (processor)"
        }

        fn size(&mut self) -> (i32, i32) {
            let (_, _, w, h) = Self::virtual_screen();
            (w, h)
        }

        fn grab(&mut self) -> Option<Grab> {
            let (x, y, w, h) = Self::virtual_screen();
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: GRAB_W as i32,
                    biHeight: -(GRAB_H as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buf = vec![0u8; GRAB_W * GRAB_H * 4];
            // SAFETY: `buf` holds GRAB_W x GRAB_H 32-bit pixels, as `info` describes.
            let ok = unsafe {
                StretchBlt(self.mem, 0, 0, GRAB_W as i32, GRAB_H as i32, Some(self.screen), x, y, w, h, SRCCOPY).as_bool()
                    && GetDIBits(self.mem, self.bmp, 0, GRAB_H as u32, Some(buf.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS) == GRAB_H as i32
            };
            ok.then(|| Grab { w: GRAB_W, h: GRAB_H, px: buf.chunks_exact(4).map(|p| Rgb::new(p[2], p[1], p[0])).collect() })
        }
    }
}

/// A screen reader: `gpu` is Desktop Duplication (falling back to GDI when it can't start), `cpu` is GDI.
/// Measured on the owner's laptop, 2026-11-24 (4K screen, 30 s): GDI about 13 % of one core with 2 MB of
/// private memory; Desktop Duplication about 3 % of one core, with about 215 MB of private memory committed
/// by the graphics driver (39 MB in RAM).
#[cfg(windows)]
pub fn open(gpu: bool) -> Result<Box<dyn ScreenSource>, String> {
    if !gpu {
        return Ok(Box::new(win::WinScreen::new()?));
    }
    match dxgi::DxgiScreen::new() {
        Ok(d) => Ok(Box::new(d)),
        Err(e) => {
            eprintln!("Desktop Duplication isn't available ({e}); reading the screen through GDI instead.");
            Ok(Box::new(win::WinScreen::new()?))
        }
    }
}

/// The screen through DXGI Desktop Duplication: the desktop image stays on the graphics card, is copied into a
/// texture whose mipmaps the card generates (each level the average of the one above), and only a small level
/// (at least 64 pixels wide: 120 x 67 for a 4K screen) is copied back. When the screen hasn't changed since the
/// last read, Windows says so and the last picture is reused: no copy at all.
///
/// The first screen of the graphics adapter that shows the desktop (on a laptop with two GPUs, whichever drives
/// the panel). When the duplication is lost (a resolution change, a full-screen game, the sign-in screen) it is
/// made again on the next read; until then the last picture stays.
#[cfg(windows)]
pub mod dxgi {
    use super::{Grab, ScreenSource};
    use alienfx::Rgb;
    use windows::core::Interface;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::*;

    /// The small level is the smallest one at least this wide.
    const MIN_W: u32 = 64;

    struct Mips {
        tex: ID3D11Texture2D,
        srv: ID3D11ShaderResourceView,
        staging: ID3D11Texture2D,
        level: u32,
        w: u32,
        h: u32,
        /// The desktop's size and format these were made for.
        desk: (u32, u32, i32),
    }

    pub struct DxgiScreen {
        device: ID3D11Device,
        ctx: ID3D11DeviceContext,
        output: IDXGIOutput1,
        dup: Option<IDXGIOutputDuplication>,
        mips: Option<Mips>,
        last: Option<Grab>,
        size: (i32, i32),
    }

    impl DxgiScreen {
        pub fn new() -> Result<DxgiScreen, String> {
            // SAFETY: COM calls on interfaces this function owns; out-pointers point at locals.
            unsafe {
                // Real pixels (a 4K screen at 200 % is 3840 x 2160); harmless when already set.
                let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("no DXGI factory: {e}"))?;
                let mut i = 0;
                loop {
                    let adapter = factory.EnumAdapters1(i).map_err(|_| "no graphics adapter shows the desktop".to_string())?;
                    i += 1;
                    let Ok(out) = adapter.EnumOutputs(0) else { continue };
                    let Ok(desc) = out.GetDesc() else { continue };
                    if !desc.AttachedToDesktop.as_bool() {
                        continue;
                    }
                    let r = desc.DesktopCoordinates;
                    let mut device = None;
                    let mut ctx = None;
                    let a: IDXGIAdapter = adapter.cast().map_err(|e| e.to_string())?;
                    D3D11CreateDevice(&a, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx))
                        .map_err(|e| format!("no Direct3D 11 device: {e}"))?;
                    let (device, ctx) = (device.ok_or("no Direct3D 11 device")?, ctx.ok_or("no Direct3D 11 context")?);
                    let output: IDXGIOutput1 = out.cast().map_err(|e| e.to_string())?;
                    let mut me = DxgiScreen { device, ctx, output, dup: None, mips: None, last: None, size: (r.right - r.left, r.bottom - r.top) };
                    me.duplicate()?;
                    return Ok(me);
                }
            }
        }

        fn duplicate(&mut self) -> Result<(), String> {
            // SAFETY: COM call on owned interfaces.
            self.dup = Some(unsafe { self.output.DuplicateOutput(&self.device) }.map_err(|e| format!("Desktop Duplication refused: {e}"))?);
            Ok(())
        }

        /// The mip chain and the staging copy for a desktop of this size and format, made once.
        fn mips(&mut self, desk: &D3D11_TEXTURE2D_DESC) -> Result<&Mips, String> {
            let key = (desk.Width, desk.Height, desk.Format.0);
            if self.mips.as_ref().is_none_or(|m| m.desk != key) {
                let mut level = 0;
                while (desk.Width >> (level + 1)) >= MIN_W {
                    level += 1;
                }
                let (w, h) = ((desk.Width >> level).max(1), (desk.Height >> level).max(1));
                let one = DXGI_SAMPLE_DESC { Count: 1, Quality: 0 };
                let td = D3D11_TEXTURE2D_DESC {
                    Width: desk.Width,
                    Height: desk.Height,
                    MipLevels: level + 1,
                    ArraySize: 1,
                    Format: desk.Format,
                    SampleDesc: one,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
                };
                let sd = D3D11_TEXTURE2D_DESC { Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: desk.Format, SampleDesc: one, Usage: D3D11_USAGE_STAGING, BindFlags: 0, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, MiscFlags: 0 };
                // SAFETY: descriptors are locals; out-pointers point at locals.
                unsafe {
                    let mut tex = None;
                    self.device.CreateTexture2D(&td, None, Some(&mut tex)).map_err(|e| format!("mip texture: {e}"))?;
                    let tex = tex.ok_or("mip texture")?;
                    let mut srv = None;
                    self.device.CreateShaderResourceView(&tex, None, Some(&mut srv)).map_err(|e| format!("mip view: {e}"))?;
                    let mut staging = None;
                    self.device.CreateTexture2D(&sd, None, Some(&mut staging)).map_err(|e| format!("staging texture: {e}"))?;
                    self.mips = Some(Mips { tex, srv: srv.ok_or("mip view")?, staging: staging.ok_or("staging texture")?, level, w, h, desk: key });
                }
            }
            Ok(self.mips.as_ref().expect("made above"))
        }

        /// One read, waiting up to `wait_ms` for a picture: `Ok(None)` when the screen hasn't changed.
        fn read(&mut self, wait_ms: u32) -> Result<Option<Grab>, windows::core::Error> {
            if self.dup.is_none() {
                self.duplicate().map_err(|_| windows::core::Error::from(DXGI_ERROR_ACCESS_LOST))?;
            }
            let dup = self.dup.clone().expect("made above");
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            // SAFETY: out-pointers point at locals; the frame is released below on every path.
            unsafe { dup.AcquireNextFrame(wait_ms, &mut info, &mut res) }?;
            let first = self.last.is_none();
            let out = (|| -> Result<Option<Grab>, windows::core::Error> {
                if !first && (info.LastPresentTime == 0 || info.AccumulatedFrames == 0) {
                    return Ok(None); // only the pointer moved
                }
                let desk: ID3D11Texture2D = res.as_ref().ok_or_else(|| windows::core::Error::from(DXGI_ERROR_ACCESS_LOST))?.cast()?;
                let mut dd = D3D11_TEXTURE2D_DESC::default();
                // SAFETY: `dd` is a local.
                unsafe { desk.GetDesc(&mut dd) };
                let m = self.mips(&dd).map_err(|_| windows::core::Error::from(DXGI_ERROR_ACCESS_LOST))?;
                let (tex, srv, staging, level, w, h) = (m.tex.clone(), m.srv.clone(), m.staging.clone(), m.level, m.w, m.h);
                // SAFETY: resources made on this device; the mapped rows are read within the mapping's size.
                unsafe {
                    self.ctx.CopySubresourceRegion(&tex, 0, 0, 0, 0, &desk, 0, None);
                    self.ctx.GenerateMips(&srv);
                    self.ctx.CopySubresourceRegion(&staging, 0, 0, 0, 0, &tex, level, None);
                    let mut map = D3D11_MAPPED_SUBRESOURCE::default();
                    self.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
                    let mut px = Vec::with_capacity((w * h) as usize);
                    for y in 0..h as usize {
                        let row = std::slice::from_raw_parts((map.pData as *const u8).add(y * map.RowPitch as usize), w as usize * 4);
                        px.extend(row.chunks_exact(4).map(|p| Rgb::new(p[2], p[1], p[0])));
                    }
                    self.ctx.Unmap(&staging, 0);
                    Ok(Some(Grab { w: w as usize, h: h as usize, px }))
                }
            })();
            // SAFETY: the frame acquired above.
            let _ = unsafe { dup.ReleaseFrame() };
            out
        }
    }

    impl ScreenSource for DxgiScreen {
        fn size(&mut self) -> (i32, i32) {
            self.size
        }

        fn kind(&self) -> &'static str {
            "Desktop Duplication (graphics card)"
        }

        fn grab(&mut self) -> Option<Grab> {
            // The first read waits for a picture; after that a read never waits.
            let wait = if self.last.is_none() { 1_000 } else { 0 };
            match self.read(wait) {
                Ok(Some(g)) => {
                    self.last = Some(g.clone());
                    Some(g)
                }
                Ok(None) => self.last.clone(),
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => self.last.clone(),
                Err(_) => {
                    // Lost (mode change, full-screen app, secure desktop): made again on the next read.
                    self.dup = None;
                    self.last.clone()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lightctl::FakeIo;
    use alienfx::device::FakePauser;

    fn id(name: &str) -> u8 {
        x17::by_name(name).unwrap().id
    }

    fn halves(left: Rgb, right: Rgb) -> Grab {
        Grab { w: GRAB_W, h: GRAB_H, px: (0..GRAB_W * GRAB_H).map(|i| if i % GRAB_W < GRAB_W / 2 { left } else { right }).collect() }
    }

    #[test]
    fn each_key_shows_its_part_of_the_screen() {
        let (red, blue) = (Rgb::new(255, 0, 0), Rgb::new(0, 0, 255));
        let k = key_colours(&halves(red, blue));
        let of = |n: &str| k.iter().find(|(i, _)| *i == id(n)).unwrap().1;
        assert_eq!((of("ESC"), of("Delete"), of("Left Ctrl"), of("Right Arrow")), (red, blue, red, blue));
        assert_eq!(k.len(), x17::KEYS.iter().filter(|k| k.present).count(), "every lit key");
        // A key across the middle averages both halves: on the home row J spans quarter units 31-35 of 64.
        let j = of("J");
        assert!(j.r > 0 && j.b > 0, "J straddles the middle: {j:?}");
    }

    #[test]
    fn saturation_deepens_colours_and_keeps_greys() {
        assert_eq!(saturate(Rgb::new(120, 120, 120), 2.0), Rgb::new(120, 120, 120), "grey stays grey");
        assert_eq!(saturate(Rgb::new(200, 150, 150), 1.0), Rgb::new(200, 150, 150), "1 changes nothing");
        assert_eq!(saturate(Rgb::new(200, 150, 150), 2.0), Rgb::new(200, 100, 100), "a pale red deepens, brightness kept");
        assert_eq!(saturate(Rgb::new(200, 150, 150), 4.0), Rgb::new(200, 0, 0), "at most full");
        assert_eq!(saturate(Rgb::new(0, 0, 0), 2.0), Rgb::new(0, 0, 0));
    }

    struct Still(Grab);
    impl ScreenSource for Still {
        fn size(&mut self) -> (i32, i32) {
            (3840, 2160)
        }
        fn grab(&mut self) -> Option<Grab> {
            Some(self.0.clone())
        }
    }

    struct Sim(u64, FakePauser);
    impl Clock for Sim {
        fn now_ms(&mut self) -> u64 {
            self.0
        }
        fn sleep_ms(&mut self, ms: u64) {
            assert_eq!(self.1.paused(), self.1.resumed(), "AWCC resumed before every wait");
            self.0 += ms;
        }
    }

    #[test]
    fn a_still_screen_settles_and_then_sends_nothing() {
        let s = LightingSettings { mode: Mode::Custom, ..Default::default() };
        let p = PowerState::default();
        let pz = FakePauser::default();
        let sum = run(&mut FakeIo::default(), &s, &p, Shown::of(&s, &p), Some(&pz), &Ambient { fps: 10, smooth_ms: 0, spread: 1.0, saturation: 1.0 }, &mut Sim(0, pz.clone()), &mut Still(halves(Rgb::new(255, 0, 0), Rgb::new(0, 0, 255))), &mut |now| now >= 5_000).unwrap();
        assert_eq!(sum.frames, 0, "the first picture is the screen; nothing changes after");
        assert!(sum.grabs >= 45, "about 10 reads a second: {}", sum.grabs);
        assert_eq!(pz.paused(), pz.resumed());
    }

    #[test]
    fn the_keyboard_is_one_gradient_relative_to_the_average() {
        let s = LightingSettings { mode: Mode::Custom, ..Default::default() };
        let g = halves(Rgb::new(255, 0, 0), Rgb::new(0, 0, 255));
        let at = |amb: &Ambient, n: &str| to_rgb(aim(&s, &g, amb).into_iter().find(|(i, _)| *i == id(n)).unwrap().1);
        let full = Ambient { spread: 1.0, saturation: 1.0, ..Default::default() };
        let (esc, del) = (at(&full, "ESC"), at(&full, "Delete"));
        assert!(esc.r > esc.b && del.b > del.r, "red on the left, blue on the right: {esc:?} {del:?}");
        // Across the middle the colour moves step by step: red falls and blue rises along the home row.
        let row: Vec<Rgb> = ["A", "S", "D", "F", "G", "H", "J", "K", "L", ";"].iter().map(|n| at(&full, n)).collect();
        assert!(row.windows(2).all(|w| w[1].r <= w[0].r && w[1].b >= w[0].b), "a gradient: {row:?}");
        assert!(row.iter().any(|c| c.r > 0 && c.b > 0), "mixed in between");
        // Spread 0: the whole keyboard is the screen's average.
        let one = Ambient { spread: 0.0, saturation: 1.0, ..Default::default() };
        let all: Vec<Rgb> = aim(&s, &g, &one).into_iter().map(|(_, c)| to_rgb(c)).collect();
        assert!(all.windows(2).all(|w| w[0] == w[1]), "one colour");
    }

    /// The screen switches from red to blue at 1 s.
    struct Switch(u64);
    impl ScreenSource for Switch {
        fn size(&mut self) -> (i32, i32) {
            (1920, 1080)
        }
        fn grab(&mut self) -> Option<Grab> {
            self.0 += 1;
            let c = if self.0 > 10 { Rgb::new(0, 0, 255) } else { Rgb::new(255, 0, 0) };
            Some(halves(c, c))
        }
    }

    #[test]
    fn a_change_is_followed_smoothly_and_capped() {
        let s = LightingSettings { mode: Mode::Custom, ..Default::default() };
        let p = PowerState::default();
        let pz = FakePauser::default();
        let sum = run(&mut FakeIo::default(), &s, &p, Shown::of(&s, &p), None, &Ambient::default(), &mut Sim(0, pz), &mut Switch(0), &mut |now| now >= 4_000).unwrap();
        // Red to blue at 1 s, eased over 0.3 s at up to 8 writes a second: a few whole-keyboard steps.
        assert!(sum.frames >= 2 && sum.frames <= 12, "eased steps after the switch: {}", sum.frames);
        assert_eq!(sum.keys, sum.frames * x17::KEYS.iter().filter(|k| k.present).count(), "every write is the whole keyboard");
    }
}
