use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F,
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_FILL_MODE_WINDING,
    D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_BITMAP_PROPERTIES, D2D1_CAP_STYLE_ROUND, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINE_JOIN_ROUND, D2D1_PRESENT_OPTIONS_NONE,
    D2D1_QUADRATIC_BEZIER_SEGMENT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory,
    ID2D1HwndRenderTarget, ID2D1PathGeometry, ID2D1RenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::Direct2D::Common::D2D1_BEZIER_SEGMENT;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_LINE_METRICS, DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_METRICS,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
    IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapCacheOnDemand, WICBitmapLockRead,
    WICRect,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{HSTRING, Result};
use windows_numerics::{Matrix3x2, Vector2};

use crate::svg::{self, Seg};
use crate::theme::Rgb;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Font {
    Serif,
    Sans,
    Mono,
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn color(c: Rgb, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.0 as f32 / 255.0, g: c.1 as f32 / 255.0, b: c.2 as f32 / 255.0, a }
}

/// Device-independent pieces, alive while the window is open.
pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    wic: IWICImagingFactory,
    families: HashMap<Font, String>,
    formats: RefCell<HashMap<(Font, u32, i32), IDWriteTextFormat>>,
    geometry: RefCell<HashMap<usize, ID2D1PathGeometry>>,
}

impl Gfx {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let mut families = HashMap::new();
            families.insert(Font::Serif, pick(&dw, &["Sitka Display", "Sitka Heading", "Sitka Text", "Georgia"]));
            families.insert(Font::Sans, pick(&dw, &["Segoe UI Variable Text", "Segoe UI"]));
            families.insert(Font::Mono, pick(&dw, &["Cascadia Mono", "Consolas"]));
            Ok(Self { d2d, dw, wic, families, formats: RefCell::default(), geometry: RefCell::default() })
        }
    }

    pub fn format(&self, font: Font, size: f32, weight: i32) -> IDWriteTextFormat {
        let key = (font, size.to_bits(), weight);
        if let Some(f) = self.formats.borrow().get(&key) {
            return f.clone();
        }
        let family = HSTRING::from(self.families[&font].as_str());
        let f = unsafe {
            self.dw
                .CreateTextFormat(&family, None, DWRITE_FONT_WEIGHT(weight), DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, &HSTRING::from("ru-ru"))
                .expect("text format")
        };
        self.formats.borrow_mut().insert(key, f.clone());
        f
    }

    /// Lays out one string. `max_w` = 0 means a single line of natural width.
    pub fn layout(&self, s: &str, font: Font, size: f32, weight: i32, max_w: f32) -> IDWriteTextLayout {
        let format = self.format(font, size, weight);
        unsafe {
            let l = self.dw.CreateTextLayout(&wide(s), &format, if max_w > 0.0 { max_w } else { 10_000.0 }, 10_000.0).expect("text layout");
            let _ = l.SetWordWrapping(if max_w > 0.0 { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
            l
        }
    }

    pub fn path(&self, d: &'static str) -> ID2D1PathGeometry {
        let key = d.as_ptr() as usize;
        if let Some(g) = self.geometry.borrow().get(&key) {
            return g.clone();
        }
        let g = self.build(&svg::parse(d), true).expect("path geometry");
        self.geometry.borrow_mut().insert(key, g.clone());
        g
    }

    pub fn build(&self, segs: &[Seg], filled: bool) -> Result<ID2D1PathGeometry> {
        unsafe {
            let g = self.d2d.CreatePathGeometry()?;
            let sink = g.Open()?;
            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
            let begin = if filled { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW };
            let mut open = false;
            let mut cur = Vector2 { X: 0.0, Y: 0.0 };
            let mut start = cur;
            for seg in segs {
                if !open && !matches!(seg, Seg::Move(..) | Seg::Close) {
                    sink.BeginFigure(cur, begin);
                    start = cur;
                    open = true;
                }
                match *seg {
                    Seg::Move(x, y) => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                            open = false;
                        }
                        cur = Vector2 { X: x, Y: y };
                    }
                    Seg::Line(x, y) => {
                        cur = Vector2 { X: x, Y: y };
                        sink.AddLine(cur);
                    }
                    Seg::Cubic(a, b, c, d, x, y) => {
                        cur = Vector2 { X: x, Y: y };
                        sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: Vector2 { X: a, Y: b }, point2: Vector2 { X: c, Y: d }, point3: cur });
                    }
                    Seg::Quad(a, b, x, y) => {
                        cur = Vector2 { X: x, Y: y };
                        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: Vector2 { X: a, Y: b }, point2: cur });
                    }
                    Seg::Arc { rx, ry, rot, large, sweep, x, y } => {
                        cur = Vector2 { X: x, Y: y };
                        sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: cur,
                            size: D2D_SIZE_F { width: rx, height: ry },
                            rotationAngle: rot,
                            sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                            arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                        });
                    }
                    Seg::Close => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                            open = false;
                        }
                        cur = start;
                    }
                }
            }
            if open {
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }
            sink.Close()?;
            Ok(g)
        }
    }

    /// Renders into an offscreen software bitmap and returns its alpha channel, row by row.
    pub fn raster(&self, w: u32, h: u32, draw: impl FnOnce(&ID2D1RenderTarget, &ID2D1SolidColorBrush)) -> Vec<u8> {
        let mut out = vec![0u8; (w * h) as usize];
        if w == 0 || h == 0 {
            return out;
        }
        unsafe {
            let Ok(bitmap) = self.wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand) else { return out };
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let Ok(rt) = self.d2d.CreateWicBitmapRenderTarget(&bitmap, &props) else { return out };
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let Ok(brush) = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None) else { return out };
            rt.BeginDraw();
            rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            draw(&rt, &brush);
            if rt.EndDraw(None, None).is_err() {
                return out;
            }
            drop(rt);
            let lock_rect = WICRect { X: 0, Y: 0, Width: w as i32, Height: h as i32 };
            let Ok(lock) = bitmap.Lock(&lock_rect, WICBitmapLockRead.0 as u32) else { return out };
            let Ok(stride) = lock.GetStride() else { return out };
            let mut size = 0u32;
            let mut data: *mut u8 = std::ptr::null_mut();
            if lock.GetDataPointer(&mut size, &mut data).is_err() || data.is_null() {
                return out;
            }
            let src = std::slice::from_raw_parts(data, size as usize);
            for y in 0..h as usize {
                for x in 0..w as usize {
                    out[y * w as usize + x] = src[y * stride as usize + x * 4 + 3];
                }
            }
        }
        out
    }

}

fn pick(dw: &IDWriteFactory, names: &[&str]) -> String {
    unsafe {
        let mut collection = None;
        if dw.GetSystemFontCollection(&mut collection, false).is_ok() {
            if let Some(collection) = collection {
                for name in names {
                    let mut index = 0u32;
                    let mut exists = windows::core::BOOL(0);
                    if collection.FindFamilyName(&HSTRING::from(*name), &mut index, &mut exists).is_ok() && exists.as_bool() {
                        return (*name).to_string();
                    }
                }
            }
        }
    }
    names.last().copied().unwrap_or("Segoe UI").to_string()
}

pub fn metrics(layout: &IDWriteTextLayout) -> DWRITE_TEXT_METRICS {
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = layout.GetMetrics(&mut m);
    }
    m
}

/// Baseline of the first line, from the layout's top.
pub fn baseline(layout: &IDWriteTextLayout) -> f32 {
    let mut lines = [DWRITE_LINE_METRICS::default()];
    let mut count = 0u32;
    unsafe {
        let _ = layout.GetLineMetrics(Some(&mut lines), &mut count);
    }
    lines[0].baseline
}

/// Where the glyphs actually put ink, from the layout's top-left: (left, top, right, bottom).
pub fn ink(layout: &IDWriteTextLayout) -> (f32, f32, f32, f32) {
    let m = metrics(layout);
    let o = unsafe { layout.GetOverhangMetrics().unwrap_or_default() };
    (-o.left, -o.top, m.layoutWidth + o.right, m.layoutHeight + o.bottom)
}

pub fn align(layout: &IDWriteTextLayout, a: DWRITE_TEXT_ALIGNMENT) {
    unsafe {
        let _ = layout.SetTextAlignment(a);
    }
}

/// The window's render target plus the one brush everything is painted with.
pub struct Canvas {
    pub rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    round: ID2D1StrokeStyle,
    field: Option<(ID2D1Bitmap, u32, u32)>,
}

impl Canvas {
    pub fn new(gfx: &Gfx, hwnd: HWND, w: u32, h: u32, dpi: f32, kind: D2D1_RENDER_TARGET_TYPE) -> Result<Self> {
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: kind,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: dpi,
                dpiY: dpi,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U { width: w, height: h },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = gfx.d2d.CreateHwndRenderTarget(&props, &hprops)?;
            let brush = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)?;
            let round = gfx.d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    ..Default::default()
                },
                None,
            )?;
            Ok(Self { rt, brush, round, field: None })
        }
    }

    pub fn resize(&mut self, w: u32, h: u32, dpi: f32) {
        unsafe {
            let _ = self.rt.Resize(&D2D_SIZE_U { width: w, height: h });
            self.rt.SetDpi(dpi, dpi);
        }
        self.field = None;
    }

    fn paint(&self, c: Rgb, a: f32) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color(c, a)) };
        &self.brush
    }

    pub fn clear(&self, c: Rgb) {
        unsafe { self.rt.Clear(Some(&color(c, 1.0))) };
    }

    pub fn fill_rect(&self, r: D2D_RECT_F, c: Rgb, a: f32) {
        unsafe { self.rt.FillRectangle(&r, self.paint(c, a)) };
    }

    pub fn stroke_round(&self, r: D2D_RECT_F, radius: f32, c: Rgb, a: f32, width: f32) {
        let half = width / 2.0;
        let r = D2D_RECT_F { left: r.left + half, top: r.top + half, right: r.right - half, bottom: r.bottom - half };
        unsafe { self.rt.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, self.paint(c, a), width, None) };
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, c: Rgb, a: f32) {
        let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
        unsafe { self.rt.FillEllipse(&e, self.paint(c, a)) };
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgb, a: f32, width: f32) {
        unsafe { self.rt.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, self.paint(c, a), width, None) };
    }

    /// Draws a 24×24 path scaled to `size` with its top-left at (x, y).
    pub fn icon(&self, gfx: &Gfx, d: &'static str, x: f32, y: f32, size: f32, c: Rgb, a: f32, stroke: Option<f32>) {
        let s = size / 24.0;
        let g = gfx.path(d);
        unsafe {
            let mut prev = Matrix3x2::default();
            self.rt.GetTransform(&mut prev);
            let m = Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: x, M32: y };
            self.rt.SetTransform(&(m * prev));
            match stroke {
                Some(w) => self.rt.DrawGeometry(&g, self.paint(c, a), w / s, &self.round),
                None => self.rt.FillGeometry(&g, self.paint(c, a), None),
            }
            self.rt.SetTransform(&prev);
        }
    }

    pub fn text(&self, layout: &IDWriteTextLayout, x: f32, y: f32, c: Rgb, a: f32) {
        unsafe { self.rt.DrawTextLayout(Vector2 { X: x, Y: y }, layout, self.paint(c, a), D2D1_DRAW_TEXT_OPTIONS_NONE) };
    }


    /// Uploads the background field (BGRA, device pixels) and draws it under everything.
    pub fn field(&mut self, pixels: &[u32], w: u32, h: u32, dips: D2D_RECT_F) {
        unsafe {
            if self.field.as_ref().is_none_or(|(_, fw, fh)| (*fw, *fh) != (w, h)) {
                let mut dpi = (96.0f32, 96.0f32);
                self.rt.GetDpi(&mut dpi.0, &mut dpi.1);
                let props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                    dpiX: dpi.0,
                    dpiY: dpi.1,
                };
                self.field = self.rt.CreateBitmap(D2D_SIZE_U { width: w, height: h }, None, 0, &props).ok().map(|b| (b, w, h));
            }
            if let Some((bitmap, _, _)) = &self.field {
                let _ = bitmap.CopyFromMemory(None, pixels.as_ptr().cast(), w * 4);
                self.rt.DrawBitmap(bitmap, Some(&dips), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None);
            }
        }
    }

    pub fn clip(&self, r: D2D_RECT_F) {
        unsafe { self.rt.PushAxisAlignedClip(&r, windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn unclip(&self) {
        unsafe { self.rt.PopAxisAlignedClip() };
    }
}
