// The main window's content: three screens drawn with Direct2D over the dot-matrix field.
// Terminal look: monospace type, square corners with bracket ticks, inverted hover, a blinking cursor.

use std::collections::HashMap;
use std::time::Instant;

use rkn_core::config::Config;
use rkn_core::ipc::{Kind, LineKind, Link, Phase, SiteState, State, Status, UpdateState};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::DirectWrite::{DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING, IDWriteTextLayout};

use crate::field::{Field, Frame, Hole};
use crate::gfx::{Canvas, Font, Gfx, align, baseline, ink, metrics, rect};
use crate::icons;
use crate::text::{self, Tone};
use crate::theme::{Rgb, Theme};

pub const W: f32 = 380.0;
pub const H: f32 = 600.0;
pub const BAR: f32 = 36.0;
// The field redraws at 24 fps; the controls above it follow the display while they ease.
const FIELD_MS: f64 = 41.0;
const GLITCH: &[char] = &['#', '%', '&', '@', '$', '*', '+', '=', '/', '\\', '<', '>', '0', '1', '?', '!', '░', '▒', '▓'];

// Down arrow for the update badge in the title bar.
const ARROW: [&str; 7] = ["..X..", "..X..", "..X..", "XXXXX", ".XXX.", "..X..", "XXXXX"];

/// How often the window needs frames: never, at a calm 30 fps for the field and the cursor,
/// or at the display's rate while something eases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    Still,
    Ambient,
    Smooth,
}

// The power glyph, one character per dot.
const POWER: [&str; 10] = [
    ".....X.....",
    "..X..X..X..",
    ".X...X...X.",
    "X....X....X",
    "X....X....X",
    "X.........X",
    "X.........X",
    ".X.......X.",
    "..X.....X..",
    "...XXXXX...",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    Services,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hit {
    Min,
    Close,
    Power,
    Services,
    Settings,
    Back,
    Action,
    Relearn,
    Found,
    Update,
    Toggle(usize),
}

/// What a click asks the app to do; runs after the UI state is released.
pub enum Act {
    None,
    Minimize,
    Close,
    SetEnabled(bool),
    Relearn,
    StopLearning,
    Install,
    CheckUpdate,
    InstallUpdate,
    LoadConfig,
    Toggle(usize),
}

pub const TOGGLES: [(&str, &str); 2] = [
    ("Запускать с Windows", "Живёт в трее, окно не открывается"),
    ("Обновляться самому", "Новые версии ставятся сами, но не во время игры"),
];

pub fn toggle_value(config: &Config, autostart: bool, i: usize) -> bool {
    match i {
        0 => autostart,
        _ => config.auto_update,
    }
}

#[derive(Clone, Copy)]
struct Tween {
    from: f32,
    to: f32,
    at: Instant,
    dur: f32,
}

impl Tween {
    fn new(v: f32, dur: f32) -> Self {
        Self { from: v, to: v, at: Instant::now(), dur }
    }

    fn value(&self, now: Instant) -> f32 {
        let t = (now.duration_since(self.at).as_secs_f32() / self.dur).clamp(0.0, 1.0);
        let e = 1.0 - (1.0 - t).powi(4);
        self.from + (self.to - self.from) * e
    }

    fn set(&mut self, to: f32, now: Instant) {
        if (to - self.to).abs() > f32::EPSILON {
            self.from = self.value(now);
            self.to = to;
            self.at = now;
        }
    }

    fn jump(&mut self, to: f32) {
        self.from = to;
        self.to = to;
    }

    fn busy(&self, now: Instant) -> bool {
        now.duration_since(self.at).as_secs_f32() < self.dur
    }
}

pub struct Ui {
    pub screen: Screen,
    /// The service is not registered: the home screen offers to install it.
    pub missing: bool,
    found_open: bool,
    pub focused: bool,
    pub hover: Option<Hit>,
    pub press: Option<Hit>,
    hits: Vec<(D2D_RECT_F, Hit)>,
    enter: Tween,
    trace: Tween,
    learn: Tween,
    knobs: HashMap<usize, Tween>,
    hovers: HashMap<Hit, Tween>,
    voids: Vec<D2D_RECT_F>,
    voids_next: Vec<D2D_RECT_F>,
    title: (String, Instant),
    sub: (String, Instant),
    scroll: f32,
    max_scroll: f32,
    pub notice: Option<(String, Instant)>,
    clock: Instant,
    field_ms: f64,
    field_drawn: f64,
    field_dirty: bool,
    field: Option<Field>,
}

fn tone_color(t: &Theme, tone: Tone) -> Rgb {
    match tone {
        Tone::Ok => t.ok,
        Tone::Warn => t.warn,
        Tone::Bad => t.bad,
        Tone::Mute => t.mute,
    }
}

fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn hash(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^ (x >> 33)
}

/// The text as it decodes: characters settle left to right, the rest flicker through noise.
fn decode(s: &str, age: f32, span: f32) -> String {
    let n = s.chars().count().max(1) as f32;
    let tick = (age * 30.0) as u64;
    s.chars()
        .enumerate()
        .map(|(i, ch)| {
            let settle = 0.06 + span * i as f32 / n;
            if ch == ' ' || age >= settle {
                ch
            } else {
                GLITCH[(hash((i as u64) << 20 ^ tick) % GLITCH.len() as u64) as usize]
            }
        })
        .collect()
}

/// Cuts a line to `max` characters with an ellipsis.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

impl Ui {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            screen: Screen::Home,
            missing: false,
            found_open: false,
            focused: false,
            hover: None,
            press: None,
            hits: Vec::new(),
            enter: Tween::new(1.0, 0.35),
            trace: Tween::new(0.0, 0.9),
            learn: Tween::new(0.0, 0.9),
            knobs: HashMap::new(),
            hovers: HashMap::new(),
            voids: Vec::new(),
            voids_next: Vec::new(),
            title: (String::new(), now),
            sub: (String::new(), now),
            scroll: 0.0,
            max_scroll: 0.0,
            notice: None,
            clock: now,
            field_ms: 0.0,
            field_drawn: -1.0,
            field_dirty: true,
            field: None,
        }
    }

    pub fn drop_field(&mut self) {
        self.field = None;
    }

    pub fn theme_changed(&mut self, theme: Theme) {
        if let Some(f) = &mut self.field {
            f.set_theme(theme);
        }
        self.field_dirty = true;
    }

    pub fn go(&mut self, screen: Screen) -> Act {
        if self.screen == screen {
            return Act::None;
        }
        self.screen = screen;
        self.enter = Tween { from: 0.0, to: 1.0, at: Instant::now(), dur: 0.35 };
        self.scroll = 0.0;
        self.hover = None;
        self.field_dirty = true;
        if screen == Screen::Settings { Act::LoadConfig } else { Act::None }
    }

    pub fn say(&mut self, text: String) {
        self.notice = Some((text, Instant::now()));
    }

    /// Keeps tweens pointed at the current service state; cheap, runs before every paint.
    pub fn sync(&mut self, connected: bool, status: Option<&Status>, config: Option<&Config>, autostart: bool) {
        let now = Instant::now();
        let state = status.filter(|_| connected).map(|s| s.state);
        let learning = state == Some(State::Learning);
        let on = state.is_some_and(|s| s != State::Paused);
        self.trace.set(if on && !learning { 1.0 } else { 0.0 }, now);
        let percent = status.and_then(|s| s.learn.as_ref()).map_or(0.0, |l| l.percent as f32);
        if percent < self.learn.to {
            self.learn.jump(percent);
        } else {
            self.learn.set(percent, now);
        }
        let view = text::view(connected, self.missing, status);
        if view.title != self.title.0 {
            self.title = (view.title, now);
        }
        if view.sub != self.sub.0 {
            self.sub = (view.sub, now);
        }
        if let Some(config) = config {
            for i in 0..TOGGLES.len() {
                let v = if toggle_value(config, autostart, i) { 1.0 } else { 0.0 };
                let knob = self.knobs.entry(i).or_insert_with(|| Tween::new(v, 0.2));
                knob.set(v, now);
            }
        }
    }

    pub fn pace(&self, status: Option<&Status>) -> Pace {
        let now = Instant::now();
        let fresh = |at: Instant| now.duration_since(at).as_secs_f32() < 0.8;
        let state = status.map(|s| s.state);
        let easing = self.enter.busy(now)
            || self.trace.busy(now)
            || self.learn.busy(now)
            || self.knobs.values().any(|k| k.busy(now))
            || self.hovers.values().any(|k| k.busy(now))
            || fresh(self.title.1)
            || fresh(self.sub.1)
            || self.notice.as_ref().is_some_and(|(_, at)| at.elapsed().as_secs_f32() < 5.4);
        if easing {
            Pace::Smooth
        } else if self.focused || matches!(state, Some(State::Starting)) {
            Pace::Ambient
        } else {
            Pace::Still
        }
    }

    /// Whether a point of the title bar drags the window: anywhere but the controls on it.
    pub fn caption(&self, x: f32, y: f32) -> bool {
        y < BAR && self.hit(x, y).is_none()
    }

    /// 0..1, easing in and out as the pointer enters and leaves a control.
    fn hov(&mut self, hit: Hit, now: Instant) -> f32 {
        let target = if self.hover == Some(hit) { 1.0 } else { 0.0 };
        let t = self.hovers.entry(hit).or_insert_with(|| Tween::new(target, 0.12));
        t.set(target, now);
        t.value(now)
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| inside(r, x, y)).map(|(_, h)| *h)
    }

    pub fn wheel(&mut self, delta: f32) -> bool {
        if self.screen != Screen::Services {
            return false;
        }
        let next = (self.scroll - delta * 48.0).clamp(0.0, self.max_scroll);
        let moved = next != self.scroll;
        self.scroll = next;
        moved
    }

    pub fn click(&mut self, hit: Hit, status: Option<&Status>) -> Act {
        let learning = status.is_some_and(|s| s.state == State::Learning);
        match hit {
            Hit::Min => Act::Minimize,
            Hit::Close => Act::Close,
            // While learning this is a pause too: the service stops the search with the bypass.
            Hit::Power => Act::SetEnabled(status.is_some_and(|s| s.state == State::Paused)),
            Hit::Services => self.go(Screen::Services),
            Hit::Settings => self.go(Screen::Settings),
            Hit::Back => self.go(Screen::Home),
            Hit::Action if self.missing => Act::Install,
            Hit::Action | Hit::Relearn if learning => Act::StopLearning,
            Hit::Found => {
                self.found_open = !self.found_open;
                Act::None
            }
            Hit::Update => match status.map(|s| s.update.state) {
                Some(UpdateState::Available) => Act::InstallUpdate,
                _ => Act::CheckUpdate,
            },
            Hit::Action => Act::Relearn,
            Hit::Relearn => {
                self.go(Screen::Home);
                Act::Relearn
            }
            Hit::Toggle(i) => Act::Toggle(i),
        }
    }

    pub fn paint(
        &mut self,
        c: &mut Canvas,
        g: &Gfx,
        t: &Theme,
        scale: f32,
        connected: bool,
        status: Option<&Status>,
        config: Option<&Config>,
    ) {
        let now = Instant::now();
        self.hits.clear();
        let status = status.filter(|_| connected);
        let state = status.map(|s| s.state);
        let learning = state == Some(State::Learning);

        // Background field: advances only while the window is in front.
        let (pw, ph) = ((W * scale).round() as usize, (H * scale).round() as usize);
        if self.field.as_ref().is_none_or(|f| (f.w, f.h) != (pw, ph)) {
            self.field = Some(Field::new(g, pw, ph, scale, *t));
            self.field_dirty = true;
        }
        if self.focused {
            self.field_ms = self.clock.elapsed().as_secs_f64() * 1000.0;
        }
        let due = self.focused && self.field_ms - self.field_drawn >= FIELD_MS;
        if due || self.field_dirty {
            let shapes = shapes(status);
            let holes = self.holes(scale);
            let hot = state.is_some_and(|s| s != State::Paused);
            let busy = matches!(state, Some(State::Learning | State::Starting));
            let frame = Frame { now: self.field_ms, shapes: &shapes, hot, busy, quiet: self.screen != Screen::Home, holes: &holes };
            if let Some(f) = &mut self.field {
                f.step(g, &frame);
            }
            self.field_drawn = self.field_ms;
            self.field_dirty = false;
        }

        unsafe { c.rt.BeginDraw() };
        c.clear(t.bg);
        if let Some(f) = &self.field {
            c.field(&f.pixels, f.w as u32, f.h as u32, rect(0.0, 0.0, W, H));
        }

        let k = self.enter.value(now);
        let slide = (1.0 - k) * if self.screen == Screen::Home { -18.0 } else { 18.0 };
        unsafe {
            c.rt.SetTransform(&windows_numerics::Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: slide, M32: 0.0 });
        }
        match self.screen {
            Screen::Home => self.home(c, g, t, now, status, learning, k),
            Screen::Services => self.services(c, g, t, status, learning, k),
            Screen::Settings => self.settings(c, g, t, now, status, config, k),
        }
        unsafe { c.rt.SetTransform(&windows_numerics::Matrix3x2::identity()) };
        self.bar(c, g, t, status);
        self.toast(c, g, t);
        // Text placed this frame cuts the field from the next frame on.
        let voids = std::mem::take(&mut self.voids_next);
        if voids.len() != self.voids.len() || voids.iter().zip(&self.voids).any(|(a, b)| (a.left, a.top, a.right, a.bottom) != (b.left, b.top, b.right, b.bottom)) {
            self.field_dirty = true;
        }
        self.voids = voids;
        unsafe {
            let _ = c.rt.EndDraw(None, None);
        }
    }

    // Where text sat last frame, in device pixels.
    fn holes(&self, scale: f32) -> Vec<Hole> {
        self.voids.iter().map(|r| Hole { x0: r.left * scale, y0: r.top * scale, x1: r.right * scale, y1: r.bottom * scale }).collect()
    }

    /// Draws a layout and keeps the field's dots off its ink.
    fn label(&mut self, c: &Canvas, l: &IDWriteTextLayout, x: f32, y: f32, col: Rgb, a: f32) {
        c.text(l, x, y, col, a);
        let (left, top, right, bottom) = ink(l);
        let pad = 3.0;
        self.voids_next.push(rect(x + left - pad, y + top - pad, right - left + 2.0 * pad, bottom - top + 2.0 * pad));
    }

    fn blink(&self) -> bool {
        (self.clock.elapsed().as_secs_f32() * 1.7) % 1.0 < 0.55
    }

    /// Square frame with bracket ticks at the corners, the shape every box and button shares.
    fn frame(&self, c: &Canvas, r: D2D_RECT_F, edge: Rgb, tick: Rgb, a: f32) {
        c.stroke_round(r, 0.0, edge, a, 1.0);
        let s = 5.0;
        for (x, y, dx, dy) in [(r.left, r.top, 1.0, 1.0), (r.right, r.top, -1.0, 1.0), (r.left, r.bottom, 1.0, -1.0), (r.right, r.bottom, -1.0, -1.0)] {
            let (x0, y0) = (if dx > 0.0 { x } else { x - 2.0 }, if dy > 0.0 { y } else { y - 2.0 });
            c.fill_rect(rect(x0, y0, 2.0, 2.0), tick, a);
            c.fill_rect(rect(if dx > 0.0 { x } else { x - s }, y0, s, 1.0), tick, a);
            c.fill_rect(rect(x0, if dy > 0.0 { y } else { y - s }, 1.0, s), tick, a);
        }
    }

    /// A row of 1-dip dots, for separators and leaders.
    fn dotted(&self, c: &Canvas, x0: f32, x1: f32, y: f32, col: Rgb, a: f32) {
        let mut x = x0;
        while x < x1 {
            c.fill_rect(rect(x, y, 1.0, 1.0), col, a);
            x += 3.0;
        }
    }

    fn pixels(&self, c: &Canvas, art: &[&str], cx: f32, cy: f32, px: f32, col: Rgb, a: f32) {
        let (cols, rows) = (art[0].len() as f32, art.len() as f32);
        let (x0, y0) = (cx - cols * px / 2.0, cy - rows * px / 2.0);
        for (r, line) in art.iter().enumerate() {
            for (i, ch) in line.bytes().enumerate() {
                if ch == b'X' {
                    c.fill_rect(rect(x0 + i as f32 * px, y0 + r as f32 * px, px - 1.0, px - 1.0), col, a);
                }
            }
        }
    }

    fn bar(&mut self, c: &Canvas, g: &Gfx, t: &Theme, status: Option<&Status>) {
        if let Some(status) = status {
            if let Some(name) = &status.network {
                let d = match status.link {
                    Link::Wifi => icons::WIFI,
                    Link::Ethernet => icons::ETHERNET,
                    Link::Other => icons::GLOBE,
                };
                c.icon(g, d, 14.0, 11.5, 13.0, t.mute, 1.0, Some(1.5));
                let l = g.layout(name, Font::Mono, 11.0, 400, 0.0);
                let m = metrics(&l);
                self.label(c, &l, 33.0, (BAR - m.height) / 2.0, t.mute, 1.0);
            }
        }
        let now = Instant::now();
        if let Some(status) = status {
            self.badge(c, g, t, status, now);
        }
        let min = rect(W - 88.0, 0.0, 44.0, BAR);
        let close = rect(W - 44.0, 0.0, 44.0, BAR);
        let (hm, hc) = (self.hov(Hit::Min, now), self.hov(Hit::Close, now));
        c.fill_rect(min, t.hover, hm);
        c.fill_rect(close, Rgb(0xc4, 0x2b, 0x1c), hc);
        c.fill_rect(rect(W - 71.0, 18.0, 10.0, 1.0), t.mute.mix(t.fg, hm), 1.0);
        let xc = t.mute.mix(Rgb(255, 255, 255), hc);
        c.line(W - 27.0, 13.0, W - 17.0, 23.0, xc, 1.0, 1.0);
        c.line(W - 17.0, 13.0, W - 27.0, 23.0, xc, 1.0, 1.0);
        self.hits.push((min, Hit::Min));
        self.hits.push((close, Hit::Close));
    }

    /// Next to the window buttons: a new version waiting for a click, or the update going on.
    fn badge(&mut self, c: &Canvas, g: &Gfx, t: &Theme, status: &Status, now: Instant) {
        let update = &status.update;
        let (text, ready) = match update.state {
            UpdateState::Available => (update.version.clone().unwrap_or_default(), true),
            UpdateState::Downloading | UpdateState::Installing => ("обновляю".to_string(), false),
            _ => return,
        };
        let l = g.layout(&text, Font::Mono, 10.5, 600, 0.0);
        let m = metrics(&l);
        let w = 12.0 + 5.0 * 2.0 + 6.0 + m.width;
        let r = rect(W - 96.0 - w, 9.0, w, 18.0);
        let v = if ready { self.hov(Hit::Update, now) } else { 0.0 };
        let ink = if ready { t.ok } else { t.mute };
        c.fill_rect(r, ink, (0.12 + 0.88 * v) * if ready { 1.0 } else { 0.6 });
        c.stroke_round(r, 0.0, ink, 0.6, 1.0);
        let fg = ink.mix(t.bg, v);
        // While downloading the arrow blinks; a waiting update keeps it steady.
        if ready || self.blink() {
            self.pixels(c, &ARROW, r.left + 11.0, r.top + 9.0, 2.0, fg, 1.0);
        }
        c.text(&l, r.left + 22.0, r.top + (18.0 - m.height) / 2.0, fg, 1.0);
        if ready {
            self.hits.push((r, Hit::Update));
        }
    }

    /// A terminal button: uppercase mono label in a bracketed box; hover inverts it.
    fn button(&mut self, c: &Canvas, g: &Gfx, t: &Theme, label: &str, cx: f32, y: f32, h: f32, size: f32, hit: Hit, k: f32) -> f32 {
        let l = g.layout(&label.to_uppercase(), Font::Mono, size, 600, 0.0);
        let m = metrics(&l);
        let w = m.width + 32.0;
        let r = rect((cx - w / 2.0).round(), y, w.round(), h);
        let v = self.hov(hit, Instant::now());
        c.fill_rect(r, t.bg.mix(t.fg, v), k);
        self.frame(c, r, t.edge.mix(t.fg, v), t.fg, k);
        c.text(&l, r.left + (w - m.width) / 2.0, y + (h - m.height) / 2.0, t.fg.mix(t.bg, v), k);
        self.hits.push((r, hit));
        self.voids_next.push(rect(r.left - 2.0, r.top - 2.0, w + 4.0, h + 4.0));
        w
    }

    fn home(&mut self, c: &Canvas, g: &Gfx, t: &Theme, now: Instant, status: Option<&Status>, learning: bool, k: f32) {
        let (cx, cy, r) = (W / 2.0, BAR + 20.0 + 88.0, 86.0);
        let state = status.map(|s| s.state);
        let usable = status.is_some();
        let v = if usable { self.hov(Hit::Power, now) } else { 0.0 };
        let trace = self.trace.value(now);
        let secs = self.clock.elapsed().as_secs_f32();
        let at = |turn: f32, radius: f32| {
            let a = (turn * 360.0 - 90.0).to_radians();
            (cx + radius * a.cos(), cy + radius * a.sin())
        };

        // The dial sits on a dark disc so the field never runs through it.
        let disc = r + 9.0;
        c.fill_circle(cx, cy, disc, t.bg, k);
        // The field only knows rectangles: a stack of strips approximates the disc.
        const STRIPS: usize = 14;
        for i in 0..STRIPS {
            let y0 = cy - disc + 2.0 * disc * i as f32 / STRIPS as f32;
            let mid = y0 + disc / STRIPS as f32 - cy;
            let half = (disc * disc - mid * mid).max(0.0).sqrt() + 3.0;
            self.voids_next.push(rect(cx - half, y0, 2.0 * half, 2.0 * disc / STRIPS as f32));
        }
        for i in 0..120 {
            let (x, y) = at(i as f32 / 120.0, r - 14.0);
            let major = i % 10 == 0;
            let s = if major { 2.0 } else { 1.0 };
            c.fill_rect(rect(x - s / 2.0, y - s / 2.0, s, s), if major { t.mute } else { t.faint }, k);
        }
        // Reticle ticks at the four sides.
        for (dx, dy) in [(0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)] {
            let (x, y) = (cx + dx * (r + 4.0), cy + dy * (r + 4.0));
            let (w, h) = if dx == 0.0 { (1.0, 7.0) } else { (7.0, 1.0) };
            c.fill_rect(rect(x - w / 2.0, y - h / 2.0, w, h), t.mute.mix(t.fg, v), k);
        }

        // The ring: square dots that light clockwise, the way the bypass comes up.
        const DOTS: usize = 64;
        let pct = self.learn.value(now) / 100.0;
        let orbit = (secs / 3.2) % 1.0;
        let spin = (secs / 1.2) % 1.0;
        for i in 0..DOTS {
            let f = i as f32 / DOTS as f32;
            let (x, y) = at(f, r);
            let gap = |a: f32, b: f32| {
                let d = (a - b).rem_euclid(1.0);
                d.min(1.0 - d)
            };
            let (lit, glow) = match state {
                _ if learning => {
                    let head = f <= pct && gap(f, pct) * DOTS as f32 <= 1.0;
                    (f < pct, if head && self.blink() { 1.0 } else { 0.0 })
                }
                Some(State::Starting) => {
                    let d = (spin - f).rem_euclid(1.0);
                    (d < 0.2, 1.0 - d / 0.2)
                }
                _ => (f < trace, (1.0 - gap(f, orbit) * DOTS as f32 / 7.0).max(0.0)),
            };
            let col = if lit { t.soft.mix(t.fg, 0.35 + 0.65 * glow) } else { t.edge.mix(t.mute, v) };
            let s = 3.5 + glow * 0.5;
            c.fill_rect(rect(x - s / 2.0, y - s / 2.0, s, s), col, k);
        }

        if learning {
            // Digits are centred by their ink, not by the line box, and "%" sits on the same baseline.
            let pct = format!("{}", self.learn.value(now).round() as i32);
            let big = g.layout(&pct, Font::Mono, 40.0, 600, 0.0);
            let zero = g.layout("0", Font::Mono, 40.0, 600, 0.0);
            let small = g.layout("%", Font::Mono, 16.0, 400, 0.0);
            let mb = metrics(&big);
            let gap = 3.0;
            let left = ink(&big).0;
            let right = mb.width + gap + ink(&small).2;
            let x = (cx - (left + right) / 2.0).round();
            let (_, top, _, bottom) = ink(&zero);
            let y = (cy - (top + bottom) / 2.0).round();
            // On hover the percentage gives way to the power glyph: the button still pauses everything.
            let a = k * (1.0 - v);
            c.text(&big, x, y, t.fg, a);
            c.text(&small, x + mb.width + gap, y + baseline(&big) - baseline(&small), t.mute, a);
            self.pixels(c, &POWER, cx, cy, 4.0, t.fg, k * v);
        } else {
            let glyph = t.faint.mix(t.fg, trace.max(v));
            self.pixels(c, &POWER, cx, cy, 4.0, glyph, k);
        }
        if usable {
            self.hits.push((rect(cx - r, cy - r, r * 2.0, r * 2.0), Hit::Power));
        }

        // Title: uppercase, decoded out of noise when the state changes, with a block cursor.
        let ty = cy + r + 26.0;
        let title = decode(&self.title.0.to_uppercase(), now.duration_since(self.title.1).as_secs_f32(), 0.45);
        if !title.is_empty() {
            let l = g.layout(&title, Font::Mono, 26.0, 600, W - 40.0);
            align(&l, DWRITE_TEXT_ALIGNMENT_CENTER);
            self.label(c, &l, 20.0, ty, t.fg, k);
            if self.blink() {
                let right = metrics(&l).left + metrics(&l).width;
                c.fill_rect(rect(20.0 + right + 3.0, ty + baseline(&l) + 2.0, 13.0, 2.5), t.fg, k);
            }
        }
        let mut sub_h: f32 = 0.0;
        if !self.sub.0.is_empty() {
            let sub = decode(&format!("> {}", self.sub.0), now.duration_since(self.sub.1).as_secs_f32(), 0.3);
            let l = g.layout(&sub, Font::Mono, 11.5, 400, 310.0);
            align(&l, DWRITE_TEXT_ALIGNMENT_CENTER);
            sub_h = metrics(&l).height;
            self.label(c, &l, (W - 310.0) / 2.0, ty + 44.0, t.soft, k);
        }

        let view = text::view(true, self.missing, status);
        let mut below = ty + 44.0 + sub_h.max(16.0) + 14.0;
        let learn = status.and_then(|s| s.learn.as_ref()).filter(|_| learning);
        if let Some(learn) = learn {
            self.steps(c, t, cx, below, learn.phase, k);
            below += 18.0;
        }
        if let Some(action) = view.action {
            self.button(c, g, t, action, cx, below, 28.0, 11.0, Hit::Action, k);
            below += 28.0;
        }
        if let Some(learn) = learn {
            self.log(c, g, t, &learn.log, below + 16.0, k);
        }

        let y = H - 28.0 - 32.0;
        let a = g.layout("СЕРВИСЫ", Font::Mono, 12.0, 600, 0.0);
        let b = g.layout("НАСТРОЙКИ", Font::Mono, 12.0, 600, 0.0);
        let (wa, wb) = (metrics(&a).width + 32.0, metrics(&b).width + 32.0);
        let left = cx - (wa + 10.0 + wb) / 2.0;
        self.button(c, g, t, "Сервисы", left + wa / 2.0, y, 32.0, 12.0, Hit::Services, k);
        self.button(c, g, t, "Настройки", left + wa + 10.0 + wb / 2.0, y, 32.0, 12.0, Hit::Settings, k);
    }

    /// Three dotted segments for the learning steps: what the provider blocks, variants, the best on all sites.
    fn steps(&mut self, c: &Canvas, t: &Theme, cx: f32, y: f32, phase: Phase, k: f32) {
        let current = match phase {
            Phase::Probing => 0,
            Phase::Round1 => 1,
            Phase::Round2 => 2,
            Phase::Done | Phase::Failed => 3,
        };
        let (n, pitch, gap) = (12, 4.0, 10.0);
        let seg = n as f32 * pitch;
        let left = cx - (3.0 * seg + 2.0 * gap) / 2.0;
        self.voids_next.push(rect(left - 4.0, y - 4.0, 3.0 * seg + 2.0 * gap + 8.0, 11.0));
        let head = (self.clock.elapsed().as_secs_f32() * 9.0) % (n as f32 + 4.0);
        for i in 0..3 {
            for j in 0..n {
                let x = left + i as f32 * (seg + gap) + j as f32 * pitch;
                let running = i == current && (j as f32) <= head && (j as f32) > head - 4.0;
                let col = if i < current || running { t.fg } else if i == current { t.mute } else { t.faint };
                c.fill_rect(rect(x, y, 3.0, 3.0), col, k);
            }
        }
    }

    /// The last lines of the learning log, like the tail of a console.
    fn log(&mut self, c: &Canvas, g: &Gfx, t: &Theme, lines: &[rkn_core::ipc::LogLine], y: f32, k: f32) {
        let cw = metrics(&g.layout("0", Font::Mono, 10.5, 400, 0.0)).widthIncludingTrailingWhitespace.max(1.0);
        let (x0, x1) = (28.0, W - 28.0);
        let start = lines.len().saturating_sub(3);
        let shown = &lines[start..];
        for (i, line) in shown.iter().enumerate() {
            let a = k * (0.45 + 0.55 * (i + 1) as f32 / shown.len() as f32);
            let ly = y + i as f32 * 16.0;
            let key = format!("> {}", clip(&line.key, 18));
            let kl = g.layout(&key, Font::Mono, 10.5, 400, 0.0);
            let kw = metrics(&kl).width;
            self.label(c, &kl, x0, ly, t.soft, a);
            let col = match line.kind {
                LineKind::Problem => t.bad,
                LineKind::Chosen => t.ok,
                _ => t.mute,
            };
            let room = ((x1 - x0 - kw - 10.0) / cw).max(4.0) as usize;
            let tl = g.layout(&clip(&line.text, room), Font::Mono, 10.5, 400, 0.0);
            self.label(c, &tl, x0 + kw + 10.0, ly, col, a);
        }
    }

    fn header(&mut self, c: &Canvas, g: &Gfx, t: &Theme, title: &str, k: f32) {
        let back = rect(16.0, 46.0, 28.0, 28.0);
        let v = self.hov(Hit::Back, Instant::now());
        c.fill_rect(back, t.bg.mix(t.fg, v), k);
        self.frame(c, back, t.edge.mix(t.fg, v), t.fg, k);
        c.icon(g, icons::BACK, back.left + 7.0, back.top + 7.0, 14.0, t.fg.mix(t.bg, v), k, Some(1.6));
        self.hits.push((back, Hit::Back));
        let l = g.layout(&title.to_uppercase(), Font::Mono, 22.0, 600, 0.0);
        let m = metrics(&l);
        let y = 60.0 - m.height / 2.0;
        self.label(c, &l, 56.0, y, t.fg, k);
        if self.blink() {
            c.fill_rect(rect(56.0 + m.width + 3.0, y + baseline(&l) + 2.0, 11.0, 2.0), t.fg, k);
        }
    }

    fn panel(&self, c: &Canvas, t: &Theme, r: D2D_RECT_F, k: f32) {
        c.fill_rect(r, t.bg, 0.9 * k);
        self.frame(c, r, t.edge, t.mute, k);
    }

    fn services(&mut self, c: &Canvas, g: &Gfx, t: &Theme, status: Option<&Status>, learning: bool, k: f32) {
        self.header(c, g, t, "Сервисы", k);
        let sites = status.map(|s| s.sites.as_slice()).unwrap_or_default();
        let top = 92.0;
        c.clip(rect(0.0, top, W, H - top));
        let mut y = top - self.scroll;
        let mut games: Vec<_> = sites.iter().filter(|s| s.kind == Kind::Game).collect();
        games.sort_by_key(|s| !s.running);
        let found: Vec<_> = sites.iter().filter(|s| s.found).collect();
        let groups = [
            ("САЙТЫ", sites.iter().filter(|s| s.kind == Kind::Site && !s.found).collect::<Vec<_>>()),
            ("ИГРЫ", games),
            ("ЛАУНЧЕРЫ", sites.iter().filter(|s| s.kind == Kind::Launcher).collect()),
            // Sites the bypass found on its own, folded away until asked for.
            ("НАЙДЕННЫЕ", found),
        ];
        if sites.is_empty() {
            let msg = if status.is_some() { "> список появится после первой проверки" } else { "> служба не отвечает" };
            let l = g.layout(msg, Font::Mono, 11.5, 400, W - 40.0);
            align(&l, DWRITE_TEXT_ALIGNMENT_CENTER);
            self.label(c, &l, 20.0, top + 24.0, t.mute, k);
        }
        for (title, items) in groups.iter().filter(|(_, items)| !items.is_empty()) {
            let foldable = *title == "НАЙДЕННЫЕ";
            let head_text = if foldable {
                format!("{} // {title} {}", if self.found_open { "[-]" } else { "[+]" }, items.len())
            } else {
                format!("// {title}")
            };
            let hv = if foldable { self.hov(Hit::Found, Instant::now()) } else { 0.0 };
            let head = g.layout(&head_text, Font::Mono, 10.5, 600, 0.0);
            let hw = metrics(&head).width;
            self.label(c, &head, 18.0, y, t.mute.mix(t.fg, hv), k);
            if foldable {
                self.hits.push((rect(12.0, y - 4.0, W - 24.0, 20.0), Hit::Found));
            }
            let through = items.iter().filter(|s| s.state == SiteState::Bypassed).count();
            let mut lead_end = W - 18.0;
            if foldable && !self.found_open {
                self.dotted(c, 18.0 + hw + 8.0, lead_end, y + 8.0, t.faint, k);
                y += 28.0;
                continue;
            }
            if through > 0 {
                let l = g.layout(&format!("{through} через обход"), Font::Mono, 10.5, 400, 160.0);
                align(&l, DWRITE_TEXT_ALIGNMENT_TRAILING);
                c.text(&l, W - 18.0 - 160.0, y, t.ok, k);
                lead_end = W - 18.0 - metrics(&l).width - 8.0;
            }
            self.dotted(c, 18.0 + hw + 8.0, lead_end, y + 8.0, t.faint, k);
            y += 20.0;
            let panel = rect(16.0, y, W - 32.0, items.len() as f32 * 40.0 + 8.0);
            self.panel(c, t, panel, k);
            for (i, s) in items.iter().enumerate() {
                let ry = y + 4.0 + i as f32 * 40.0;
                let row = rect(20.0, ry, W - 40.0, 40.0);
                if i > 0 {
                    self.dotted(c, 28.0, W - 28.0, ry, t.edge, k);
                }
                let icon = icons::for_site(&s.id, s.icon.as_deref());
                match icon {
                    Some(icon) => c.icon(g, icon.path, 30.0, ry + 12.5, 15.0, t.ink, k, None),
                    None => c.icon(g, icons::GLOBE, 30.0, ry + 12.5, 15.0, t.ink, 0.8 * k, Some(1.6)),
                }
                let (label, tone) = text::site_short(s, learning);
                let color = tone_color(t, tone);
                let tl = g.layout(label, Font::Mono, 10.5, 400, 0.0);
                let tm = metrics(&tl);
                let pw = tm.width + 24.0;
                let px = row.right - 8.0 - pw;
                let chip = rect(px, ry + 10.0, pw, 20.0);
                c.fill_rect(chip, color, 0.1 * k);
                c.stroke_round(chip, 0.0, color, 0.45 * k, 1.0);
                c.fill_rect(rect(px + 7.0, ry + 18.5, 3.0, 3.0), color, k);
                c.text(&tl, px + 15.0, ry + 20.0 - tm.height / 2.0, color, k);
                let mut right = px - 8.0;
                if s.running {
                    let gl = g.layout("в игре", Font::Mono, 10.0, 400, 0.0);
                    let gm = metrics(&gl);
                    let gw = gm.width + 14.0;
                    let chip = rect(right - gw, ry + 11.0, gw, 18.0);
                    c.fill_rect(chip, t.fg, 0.9 * k);
                    c.text(&gl, chip.left + 7.0, ry + 20.0 - gm.height / 2.0, t.bg, k);
                    right = chip.left - 8.0;
                }
                let name = g.layout(&s.name, Font::Mono, 12.5, 400, (right - 54.0).max(40.0));
                let nm = metrics(&name);
                c.text(&name, 54.0, ry + 20.0 - nm.height / 2.0, t.ink, k);
            }
            y += panel.bottom - panel.top + 18.0;
        }
        c.unclip();
        self.max_scroll = (y + self.scroll - top - (H - top - 16.0)).max(0.0);
    }

    #[allow(clippy::too_many_arguments)]
    fn settings(&mut self, c: &Canvas, g: &Gfx, t: &Theme, now: Instant, status: Option<&Status>, config: Option<&Config>, k: f32) {
        self.header(c, g, t, "Настройки", k);
        let x = 16.0;
        let w = W - 32.0;
        let text_w = w - 32.0 - 40.0 - 12.0;
        let mut rows = Vec::new();
        let mut total = 4.0;
        let portable = rkn_core::paths::portable();
        for (i, (label, hint)) in TOGGLES.iter().enumerate() {
            let l = g.layout(label, Font::Mono, 12.5, 600, text_w);
            let h = g.layout(hint, Font::Mono, 10.5, 400, text_w);
            let (lm, hm) = (metrics(&l), metrics(&h));
            let rh = 18.0 + lm.height + 2.0 + hm.height;
            rows.push((i, l, h, lm.height, rh));
            total += rh;
        }
        let top = 92.0;
        let panel = rect(x, top, w, total + 4.0);
        self.panel(c, t, panel, k);
        let mut y = top + 4.0;
        for (n, (i, l, h, lh, rh)) in rows.iter().enumerate() {
            let i = *i;
            let row = rect(x, y, w, *rh);
            if n > 0 {
                self.dotted(c, x + 16.0, x + w - 16.0, y, t.edge, k);
            }
            c.text(l, x + 16.0, y + 9.0, t.ink, k);
            c.text(h, x + 16.0, y + 9.0 + lh + 2.0, t.mute, k);
            let ready = config.is_some();
            let value = self.knobs.get(&i).map_or(0.0, |kn| kn.value(now));
            let hv = if ready { self.hov(Hit::Toggle(i), now) } else { 0.0 };
            // A square switch: the knob is a solid block that slides, the box fills when on.
            let sw = rect(x + w - 16.0 - 40.0, y + rh / 2.0 - 9.0, 40.0, 18.0);
            let a = if ready { k } else { 0.4 * k };
            c.fill_rect(sw, t.fg, value * 0.9 * a);
            c.stroke_round(sw, 0.0, t.edge.mix(t.fg, value.max(hv)), a, 1.0);
            let knob = rect(sw.left + 3.0 + value * 22.0, sw.top + 3.0, 12.0, 12.0);
            c.fill_rect(knob, t.mute.mix(t.bg, value), a);
            if ready {
                self.hits.push((row, Hit::Toggle(i)));
            }
            y += rh;
        }
        // The one thing to do with the bypass itself, on its own under the switches.
        let learning = status.is_some_and(|s| s.state == State::Learning);
        let by = panel.bottom + 24.0;
        if status.is_some() {
            let label = if learning { "Остановить подбор" } else { "Подобрать обход заново" };
            self.button(c, g, t, label, W / 2.0, by, 32.0, 12.0, Hit::Relearn, k);
            let hint = if learning { "Сейчас подбирается способ под вашу сеть" } else { "Если какой-то сервис перестал открываться" };
            let l = g.layout(hint, Font::Mono, 10.5, 400, W - 40.0);
            align(&l, DWRITE_TEXT_ALIGNMENT_CENTER);
            self.label(c, &l, 20.0, by + 40.0, t.mute, k);
        }

        // Bottom line, like a status bar: the version, what the updater knows, one button.
        let update = status.map(|s| s.update.clone()).unwrap_or_default();
        let (line, tone) = text::update(&update);
        let fy = H - 62.0;
        self.dotted(c, 16.0, W - 16.0, fy - 10.0, t.edge, k);
        let name = g.layout(concat!("zapretUI ", env!("CARGO_PKG_VERSION")), Font::Mono, 11.5, 600, 0.0);
        self.label(c, &name, 18.0, fy, t.ink, k);
        if portable {
            let tag = g.layout("ПОРТАТИВНАЯ", Font::Mono, 9.5, 600, 0.0);
            let tm = metrics(&tag);
            let r = rect(18.0 + metrics(&name).width + 8.0, fy + 1.0, tm.width + 10.0, 15.0);
            c.stroke_round(r, 0.0, t.mute, k, 1.0);
            self.label(c, &tag, r.left + 5.0, r.top + (15.0 - tm.height) / 2.0, t.mute, k);
        }
        let button = match update.state {
            UpdateState::Available => Some("Обновить"),
            UpdateState::Idle | UpdateState::Latest | UpdateState::Failed => Some("Проверить"),
            _ => None,
        };
        let mut room = W - 36.0;
        if let (Some(label), Some(_)) = (button, status) {
            let bl = g.layout(&label.to_uppercase(), Font::Mono, 10.5, 600, 0.0);
            let bw = metrics(&bl).width + 24.0;
            self.button(c, g, t, label, W - 16.0 - bw / 2.0, fy + 2.0, 26.0, 10.5, Hit::Update, k);
            room = W - 36.0 - bw - 10.0;
        }
        let l = g.layout(&line, Font::Mono, 10.5, 400, room);
        self.label(c, &l, 18.0, fy + 18.0, tone_color(t, tone), k);
    }

    fn toast(&mut self, c: &Canvas, g: &Gfx, t: &Theme) {
        let Some((text, at)) = &self.notice else { return };
        let age = at.elapsed().as_secs_f32();
        if age > 5.4 {
            self.notice = None;
            return;
        }
        let a = (age / 0.2).min(1.0).min(((5.4 - age) / 0.4).min(1.0));
        let l = g.layout(&format!("! {text}"), Font::Mono, 11.0, 400, 300.0);
        align(&l, DWRITE_TEXT_ALIGNMENT_CENTER);
        let m = metrics(&l);
        let w = m.width + 32.0;
        let h = m.height + 16.0;
        let r = rect(((W - w) / 2.0).round(), H - 72.0 - h, w.round(), h);
        c.fill_rect(r, t.raise, a);
        self.frame(c, r, t.edge, t.warn, a);
        c.text(&l, (W - 300.0) / 2.0, r.top + 8.0, t.ink, a);
    }
}

/// Logos for the field: what is being tuned while learning, what goes through the bypass while working.
fn shapes(status: Option<&Status>) -> Vec<&'static str> {
    let fallback = || ["youtube", "discord", "twitch", "roblox"].iter().filter_map(|s| icons::by_slug(s)).map(|i| i.path).collect::<Vec<_>>();
    let Some(status) = status else { return fallback() };
    let paths = |pick: &dyn Fn(&rkn_core::ipc::SiteStatus) -> bool| {
        let mut out: Vec<&'static str> = Vec::new();
        for s in status.sites.iter().filter(|s| pick(s)) {
            if let Some(icon) = icons::for_site(&s.id, s.icon.as_deref()) {
                if !out.contains(&icon.path) {
                    out.push(icon.path);
                }
            }
        }
        out
    };
    let focus = match status.state {
        State::Learning => Some(SiteState::Tuning),
        State::Working => Some(SiteState::Bypassed),
        _ => None,
    };
    if let Some(focus) = focus {
        let picked = paths(&|s| s.state == focus);
        if picked.len() >= 2 {
            return picked;
        }
    }
    let all = paths(&|_| true);
    if all.len() >= 2 { all } else { fallback() }
}
