// The background: a dot-matrix screen computed on the CPU into one pixel buffer. Every cell is one
// square dot, on or off, chosen by ordered (Bayer) dithering of a brightness map: the current service
// logo shaded like a halftone print, slow clouds drifting behind it, a vignette at the edges.
// Logos change with a burst: the old one flies apart in a spiral, a shock wave runs through the
// clouds, and the new one gathers from the scatter, each dot flashing as it lands. The logo's dots
// move out of the pointer's way and glow, then ease back; a click sends a ring through the logo.

use std::collections::HashMap;
use std::rc::Rc;

use windows_numerics::Matrix3x2;

use crate::gfx::Gfx;
use crate::theme::{Rgb, Theme};

const PITCH: f32 = 4.0;
const NOISE: usize = 64;
const LEVELS: usize = 32;
const BAYER: [f32; 16] = [0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0];
/// Where the logo sits, as a share of the height: the free band between the title and the buttons.
const LOGO_Y: f32 = 0.71;
/// How long a click's ring runs through the logo, in ms.
pub const RING_MS: f64 = 1100.0;
/// How fast a dot follows where it is pushed, in ms: the inertia that keeps the motion smooth.
const SETTLE_MS: f32 = 90.0;

/// A rectangle (device pixels) around text: dots that touch it stay dark.
#[derive(Clone, Copy)]
pub struct Hole {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// A click on the background, in device pixels and field time.
#[derive(Clone, Copy)]
pub struct Ripple {
    pub x: f32,
    pub y: f32,
    pub at: f64,
}

/// What pushes the dots this frame: where from, how far it reaches, how hard.
struct Force {
    x: f32,
    y: f32,
    reach: f32,
    push: f32,
    /// Radius of a ring that runs outwards; None for a round push from the point itself.
    ring: Option<f32>,
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

pub struct Frame<'a> {
    pub now: f64,
    /// The pointer over the window, in device pixels.
    pub pointer: Option<(f32, f32)>,
    pub ripples: &'a [Ripple],
    pub shapes: &'a [&'static str],
    pub hot: bool,
    pub busy: bool,
    pub quiet: bool,
    pub holes: &'a [Hole],
}

pub struct Field {
    pub w: usize,
    pub h: usize,
    pitch: usize,
    dot: usize,
    cols: usize,
    rows: usize,
    noise: Vec<f32>,
    seed: Vec<f32>,
    fade: Vec<f32>,
    /// Where each dot is pushed from its place, device pixels; eases towards the push of this frame.
    offset: Vec<(f32, f32)>,
    last: f64,
    /// Something moves (dots on their way, a logo changing): the caller gives frames at full rate.
    pub stirred: bool,
    hidden: Vec<bool>,
    masks: HashMap<usize, Rc<Vec<f32>>>,
    from: Rc<Vec<f32>>,
    to: Rc<Vec<f32>>,
    index: usize,
    since: f64,
    started: bool,
    base: Vec<u32>,
    pub pixels: Vec<u32>,
    theme: Theme,
    hot: bool,
    ramp: [u32; LEVELS],
    bg: u32,
}

fn pack(c: Rgb) -> u32 {
    0xff00_0000 | u32::from(c.0) << 16 | u32::from(c.1) << 8 | u32::from(c.2)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Tileable value noise: random values blurred a few times with wrap-around.
fn clouds(rng: &mut Rng) -> Vec<f32> {
    let n = NOISE;
    let mut a: Vec<f32> = (0..n * n).map(|_| rng.next()).collect();
    for _ in 0..4 {
        let mut b = vec![0.0; n * n];
        for y in 0..n {
            for x in 0..n {
                let mut s = 0.0;
                for (dx, dy) in [(0, 0), (1, 0), (n - 1, 0), (0, 1), (0, n - 1)] {
                    s += a[((y + dy) % n) * n + (x + dx) % n];
                }
                b[y * n + x] = s / 5.0;
            }
        }
        a = b;
    }
    let (lo, hi) = a.iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    a.iter().map(|v| (v - lo) / (hi - lo).max(1e-6)).collect()
}

impl Field {
    pub fn new(_gfx: &Gfx, w: usize, h: usize, scale: f32, theme: Theme) -> Self {
        let pitch = (PITCH * scale).round().max(3.0) as usize;
        let dot = pitch - scale.round().max(1.0) as usize;
        let cols = w.div_ceil(pitch);
        let rows = h.div_ceil(pitch);
        let n = cols * rows;
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15u64 ^ (w as u64) << 20 ^ h as u64);
        let noise = clouds(&mut rng);
        let seed = (0..n).map(|_| rng.next()).collect();
        // Edges fade like a radial vignette, so the screen never ends in a hard border.
        let fade = (0..n)
            .map(|i| {
                let x = ((i % cols) as f32 + 0.5) / cols as f32 - 0.5;
                let y = ((i / cols) as f32 + 0.5) / rows as f32 - 0.5;
                let d = ((x / 0.95).powi(2) + (y / 0.8).powi(2)).sqrt() * 2.0;
                if d < 0.5 { 1.0 } else { 1.0 - 0.75 * ((d - 0.5) / 0.5).min(1.0) }
            })
            .collect();
        let empty = Rc::new(vec![0.0; n]);
        let mut field = Self {
            w,
            h,
            pitch,
            dot,
            cols,
            rows,
            noise,
            seed,
            fade,
            offset: vec![(0.0, 0.0); n],
            last: 0.0,
            stirred: false,
            hidden: vec![false; n],
            masks: HashMap::new(),
            from: empty.clone(),
            to: empty,
            index: 0,
            since: 0.0,
            started: false,
            base: Vec::new(),
            pixels: vec![0; w * h],
            theme,
            hot: false,
            ramp: [0; LEVELS],
            bg: 0,
        };
        field.paint();
        field
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.paint();
    }

    // Lit dots go from a dim grey to the text colour; with the bypass off they stop at a soft grey.
    // The unlit grid is barely there, like the pixels of a switched-off LCD.
    fn paint(&mut self) {
        let t = self.theme;
        let top = if self.hot { t.fg } else { t.soft };
        for (i, slot) in self.ramp.iter_mut().enumerate() {
            let k = i as f32 / (LEVELS - 1) as f32;
            let c = if k < 0.5 { t.faint.mix(t.mute, k * 2.0) } else { t.mute.mix(top, (k - 0.5) * 2.0) };
            *slot = pack(c);
        }
        self.bg = pack(t.bg);
        let off = pack(t.bg.mix(t.line, if t.dark { 0.6 } else { 0.5 }));
        self.base = vec![self.bg; self.w * self.h];
        for r in 0..self.rows {
            for c in 0..self.cols {
                self.fill(r, c, off, true);
            }
        }
    }

    fn fill(&mut self, r: usize, c: usize, color: u32, base: bool) {
        let (x0, y0) = (c * self.pitch, r * self.pitch);
        if x0 >= self.w {
            return;
        }
        let x1 = (x0 + self.dot).min(self.w);
        let buf = if base { &mut self.base } else { &mut self.pixels };
        for y in y0..(y0 + self.dot).min(self.h) {
            buf[y * self.w + x0..y * self.w + x1].fill(color);
        }
    }

    fn sample(&self, x: f32, y: f32) -> f32 {
        let n = NOISE as f32;
        let (x, y) = (x.rem_euclid(n), y.rem_euclid(n));
        let (x0, y0) = (x as usize % NOISE, y as usize % NOISE);
        let (x1, y1) = ((x0 + 1) % NOISE, (y0 + 1) % NOISE);
        let (fx, fy) = (x.fract(), y.fract());
        let (fx, fy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
        let at = |x: usize, y: usize| self.noise[y * NOISE + x];
        let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
        let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
        top + (bottom - top) * fy
    }

    // Rasterises a logo onto the dot grid: contour 1, inside 0.5, a thin halo just outside.
    fn mask(&mut self, gfx: &Gfx, path: &'static str) -> Rc<Vec<f32>> {
        let key = path.as_ptr() as usize;
        if let Some(m) = self.masks.get(&key) {
            return m.clone();
        }
        let (cols, rows) = (self.cols, self.rows);
        let (w, h) = (self.w as f32, self.h as f32);
        // The logo fills the free band between the title and the buttons: up top it would run under
        // the dial and the title and come out cut in pieces.
        let size = (w * 0.56).min(h * 0.34);
        let (cx, cy) = (w / 2.0, h * LOGO_Y);
        let p = self.pitch as f32;
        let geometry = gfx.path(path);
        let core = gfx.raster(cols as u32, rows as u32, |rt, brush| unsafe {
            let s = size / 24.0 / p;
            let m = Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: (cx - size / 2.0) / p, M32: (cy - size / 2.0) / p };
            rt.SetTransform(&m);
            rt.FillGeometry(&geometry, brush, None);
        });
        let inside = |c: isize, r: isize| c >= 0 && r >= 0 && (c as usize) < cols && (r as usize) < rows && core[r as usize * cols + c as usize] > 127;
        let mut out = vec![0.0f32; cols * rows];
        for r in 0..rows as isize {
            for c in 0..cols as isize {
                let i = r as usize * cols + c as usize;
                out[i] = if inside(c, r) {
                    let edge = !inside(c - 1, r) || !inside(c + 1, r) || !inside(c, r - 1) || !inside(c, r + 1);
                    if edge { 1.0 } else { 0.5 }
                } else {
                    let near = [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().filter(|(dx, dy)| inside(c + dx, r + dy)).count();
                    near as f32 * 0.08
                };
            }
        }
        let out = Rc::new(out);
        self.masks.insert(key, out.clone());
        out
    }

    pub fn step(&mut self, gfx: &Gfx, f: &Frame) {
        if self.hot != f.hot {
            self.hot = f.hot;
            self.paint();
        }
        let hold = if f.busy { 2600.0 } else { 6000.0 };
        let morph = if f.busy { 1300.0 } else { 1900.0 };
        if !f.shapes.is_empty() && (!self.started || f.now - self.since > hold + morph) {
            self.index = if self.started { (self.index + 1) % f.shapes.len() } else { 0 };
            self.from = self.to.clone();
            self.to = self.mask(gfx, f.shapes[self.index % f.shapes.len()]);
            self.since = f.now;
            if !self.started {
                self.from = self.to.clone();
                self.started = true;
            }
        }
        let raw = ((f.now - self.since) / morph) as f32;
        let moving = self.started && raw < 1.0;
        let p = raw.clamp(0.0, 1.0);
        let secs = (f.now / 1000.0) as f32 * if f.busy { 1.8 } else { 1.0 };
        let (shape_gain, cloud_gain) = if f.quiet { (0.4, 0.55) } else { (1.0, 1.0) };

        self.pixels.copy_from_slice(&self.base);
        self.hidden.fill(false);
        let p_px = self.pitch as f32;
        for hole in f.holes {
            let c0 = (hole.x0 / p_px).floor().max(0.0) as usize;
            let r0 = (hole.y0 / p_px).floor().max(0.0) as usize;
            let c1 = ((hole.x1 / p_px).ceil() as usize).min(self.cols);
            let r1 = ((hole.y1 / p_px).ceil() as usize).min(self.rows);
            for r in r0..r1 {
                self.hidden[r * self.cols + c0.min(c1)..r * self.cols + c1].fill(true);
            }
        }

        // Sizes in dips grow with the dots.
        let u = p_px / PITCH;
        let mut forces: Vec<Force> = Vec::new();
        if let Some((x, y)) = f.pointer {
            forces.push(Force { x, y, reach: 34.0 * u, push: 9.0 * u, ring: None });
        }
        for ripple in f.ripples {
            let age = (f.now - ripple.at) as f32;
            let k = 1.0 - age / RING_MS as f32;
            if k > 0.0 {
                forces.push(Force { x: ripple.x, y: ripple.y, reach: 14.0 * u, push: 8.0 * u * k, ring: Some(age * 0.3 * u) });
            }
        }
        let dt = (f.now - self.last).clamp(0.0, 100.0) as f32;
        self.last = f.now;
        let follow = 1.0 - (-dt / SETTLE_MS).exp();
        let mut stirred = moving;

        let (lcx, lcy) = (self.w as f32 / 2.0, self.h as f32 * LOGO_Y);
        let wave = ease_out(p) * (self.w as f32).hypot(self.h as f32) * 0.55;
        let swirl = 1.1;
        let (from, to) = (self.from.clone(), self.to.clone());
        for r in 0..self.rows {
            let y = r as f32 / self.rows as f32;
            for c in 0..self.cols {
                let i = r * self.cols + c;
                if self.hidden[i] {
                    self.fill(r, c, self.bg, false);
                    continue;
                }
                let (x0, y0) = (c as f32 * p_px, r as f32 * p_px);
                let (mx, my) = (x0 + p_px / 2.0, y0 + p_px / 2.0);
                let s = self.seed[i];
                let cloud = self.sample(c as f32 * 0.085 + secs * 0.35, r as f32 * 0.085 + secs * 0.12) * 0.62
                    + self.sample(c as f32 * 0.2 - secs * 0.55, r as f32 * 0.2 + secs * 0.3) * 0.38;
                let mut ambient = ((cloud - 0.42) * 1.7).clamp(0.0, 1.0) * 0.36 * cloud_gain;
                // Inside a logo the shade falls off from the top and breathes with the clouds: a halftone print.
                let grain = self.sample(c as f32 * 0.45 + secs * 0.2, r as f32 * 0.45 - secs * 0.15);
                let ink = |shape: f32| {
                    shape_gain
                        * match shape {
                            s if s >= 0.99 => 1.0,
                            s if s >= 0.49 => (0.08 + 0.3 * (1.0 - y) + 0.6 * (cloud * 0.5 + grain * 0.5)).min(0.95),
                            s => s * 2.0,
                        }
                };
                let (old, new) = (ink(from[i]), ink(to[i]));
                let threshold = (BAYER[(r % 4) * 4 + c % 4] + 0.5) / 16.0;

                // The shock wave of a change lights up the clouds as it passes.
                let (ox, oy) = (mx - lcx, my - lcy);
                let dist = ox.hypot(oy).max(1.0);
                if moving {
                    let band = (dist - wave) / (7.0 * p_px);
                    ambient += 0.55 * (1.0 - p) * (-band * band).exp() * cloud_gain;
                }

                // Only the logo answers the pointer and click rings: its dots step aside, glow, and
                // ease back. The clouds behind stay where they are.
                let logo = old.max(new) > 0.05;
                let (mut tx, mut ty, mut glow) = (0.0f32, 0.0f32, 0.0f32);
                if logo {
                    for force in &forces {
                        let (fx, fy) = (mx - force.x, my - force.y);
                        let d = fx.hypot(fy).max(0.5);
                        let k = match force.ring {
                            Some(radius) => (1.0 - (d - radius).abs() / force.reach).max(0.0),
                            None => {
                                let k = (1.0 - d / force.reach).max(0.0);
                                k * k * (3.0 - 2.0 * k)
                            }
                        };
                        if k > 0.0 {
                            tx += fx / d * force.push * k;
                            ty += fy / d * force.push * k;
                            glow += k;
                        }
                    }
                }
                let (ex, ey) = self.offset[i];
                let (dx, dy) = (ex + (tx - ex) * follow, ey + (ty - ey) * follow);
                self.offset[i] = if dx.abs() + dy.abs() < 0.05 { (0.0, 0.0) } else { (dx, dy) };
                stirred |= (dx - tx).abs() + (dy - ty).abs() > 0.2;
                let glow = (glow.min(1.0) * 0.5).max((dx.hypot(dy) / (9.0 * u)).min(1.0) * 0.35);

                // Dots both logos share stay put during a change; the rest fly.
                let stay = if moving { old.min(new) } else { new };
                let lit = if logo { stay + glow * stay.max(0.4) } else { 0.0 };
                if logo && lit > ambient {
                    let v = (lit * self.fade[i]).min(1.0);
                    if v > threshold {
                        self.put(x0 + dx, y0 + dy, self.ramp[(v * (LEVELS - 1) as f32) as usize]);
                    }
                } else {
                    let v = (ambient.max(stay) * self.fade[i]).min(1.0);
                    if v > threshold {
                        self.fill(r, c, self.ramp[(v * (LEVELS - 1) as f32) as usize], false);
                    }
                }
                if !moving {
                    continue;
                }
                let (ux, uy) = (ox / dist, oy / dist);
                let travel = (16.0 + 30.0 * s) * p_px;
                let fly = |out: f32| {
                    let a = swirl * out;
                    let (rx, ry) = (ux * a.cos() - uy * a.sin(), ux * a.sin() + uy * a.cos());
                    (x0 + rx * travel * out + dx, y0 + ry * travel * out + dy)
                };
                // The old logo bursts outwards in a spiral and fades on the way.
                if old > stay + 0.05 {
                    let t = ((p * 1.7 - s * 0.45) / 0.85).clamp(0.0, 1.0);
                    if t < 1.0 {
                        let flash = if t > 0.0 && t < 0.12 { 0.6 * shape_gain } else { 0.0 };
                        let b = (old * (1.0 - t) * self.fade[i]).max(flash);
                        if b > threshold {
                            let (px, py) = fly(t * t);
                            self.put(px, py, self.ramp[(b.min(1.0) * (LEVELS - 1) as f32) as usize]);
                        }
                    }
                }
                // The new one gathers from the scatter and flashes as each dot lands.
                if new > stay + 0.05 {
                    let t = ((p - 0.22 - s * 0.38) / 0.4).clamp(0.0, 1.0);
                    if t > 0.0 {
                        let e = ease_out(t);
                        let flash = if t > 0.82 && t < 1.0 { shape_gain } else { 0.0 };
                        let b = (new * (0.35 + 0.65 * e) * self.fade[i]).max(flash);
                        if b > threshold {
                            let (px, py) = fly(1.0 - e);
                            self.put(px, py, self.ramp[(b.min(1.0) * (LEVELS - 1) as f32) as usize]);
                        }
                    }
                }
            }
        }
        self.stirred = stirred;
    }

    /// A dot anywhere, not only on the grid: for dots on the move. Text holes stay clean.
    fn put(&mut self, x: f32, y: f32, color: u32) {
        let (x, y) = (x.round(), y.round());
        if x < 0.0 || y < 0.0 {
            return;
        }
        let (x0, y0) = (x as usize, y as usize);
        if x0 >= self.w || y0 >= self.h {
            return;
        }
        let (c, r) = ((x0 + self.dot / 2) / self.pitch, (y0 + self.dot / 2) / self.pitch);
        if c < self.cols && r < self.rows && self.hidden[r * self.cols + c] {
            return;
        }
        let x1 = (x0 + self.dot).min(self.w);
        for y in y0..(y0 + self.dot).min(self.h) {
            self.pixels[y * self.w + x0..y * self.w + x1].fill(color);
        }
    }
}
