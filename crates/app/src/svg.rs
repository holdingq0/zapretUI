// SVG path data (the `d` attribute) parsed into absolute segments. Covers everything simple-icons uses,
// including arc flags written without separators ("a1 1 0 011 1").

#[derive(Debug, Clone, Copy)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Quad(f32, f32, f32, f32),
    Arc { rx: f32, ry: f32, rot: f32, large: bool, sweep: bool, x: f32, y: f32 },
    Close,
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b',' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }

    fn command(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        if c.is_ascii_alphabetic() {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }

    fn has_number(&mut self) -> bool {
        self.skip();
        self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'))
    }

    fn number(&mut self) -> Option<f32> {
        self.skip();
        let start = self.i;
        let mut dot = false;
        let mut exp = false;
        if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
            self.i += 1;
        }
        while let Some(&c) = self.s.get(self.i) {
            match c {
                b'0'..=b'9' => self.i += 1,
                b'.' if !dot && !exp => {
                    dot = true;
                    self.i += 1;
                }
                b'e' | b'E' if !exp => {
                    exp = true;
                    self.i += 1;
                    if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }

    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = *self.s.get(self.i)?;
        self.i += 1;
        match c {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        }
    }
}

pub fn parse(d: &str) -> Vec<Seg> {
    let mut r = Reader { s: d.as_bytes(), i: 0 };
    let mut out = Vec::new();
    let (mut x, mut y) = (0f32, 0f32);
    let (mut sx, mut sy) = (0f32, 0f32);
    // Last control point, for the smooth S/T forms.
    let mut ctrl: Option<(f32, f32)> = None;
    let mut cmd = 0u8;
    loop {
        let next = match r.command() {
            Some(c) => c,
            None if cmd != 0 && r.has_number() => match cmd {
                b'M' => b'L',
                b'm' => b'l',
                c => c,
            },
            None => break,
        };
        cmd = next;
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        let parsed = (|| -> Option<()> {
            match cmd.to_ascii_uppercase() {
                b'M' => {
                    let (nx, ny) = (r.number()? + ox, r.number()? + oy);
                    out.push(Seg::Move(nx, ny));
                    (x, y, sx, sy) = (nx, ny, nx, ny);
                    ctrl = None;
                }
                b'L' => {
                    let (nx, ny) = (r.number()? + ox, r.number()? + oy);
                    out.push(Seg::Line(nx, ny));
                    (x, y) = (nx, ny);
                    ctrl = None;
                }
                b'H' => {
                    x = r.number()? + ox;
                    out.push(Seg::Line(x, y));
                    ctrl = None;
                }
                b'V' => {
                    y = r.number()? + oy;
                    out.push(Seg::Line(x, y));
                    ctrl = None;
                }
                b'C' => {
                    let v = [r.number()?, r.number()?, r.number()?, r.number()?, r.number()?, r.number()?];
                    let s = Seg::Cubic(v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy, v[4] + ox, v[5] + oy);
                    out.push(s);
                    ctrl = Some((v[2] + ox, v[3] + oy));
                    (x, y) = (v[4] + ox, v[5] + oy);
                }
                b'S' => {
                    let v = [r.number()?, r.number()?, r.number()?, r.number()?];
                    let (c1x, c1y) = ctrl.map_or((x, y), |(cx, cy)| (2.0 * x - cx, 2.0 * y - cy));
                    out.push(Seg::Cubic(c1x, c1y, v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy));
                    ctrl = Some((v[0] + ox, v[1] + oy));
                    (x, y) = (v[2] + ox, v[3] + oy);
                }
                b'Q' => {
                    let v = [r.number()?, r.number()?, r.number()?, r.number()?];
                    out.push(Seg::Quad(v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy));
                    ctrl = Some((v[0] + ox, v[1] + oy));
                    (x, y) = (v[2] + ox, v[3] + oy);
                }
                b'T' => {
                    let v = [r.number()?, r.number()?];
                    let (cx, cy) = ctrl.map_or((x, y), |(cx, cy)| (2.0 * x - cx, 2.0 * y - cy));
                    out.push(Seg::Quad(cx, cy, v[0] + ox, v[1] + oy));
                    ctrl = Some((cx, cy));
                    (x, y) = (v[0] + ox, v[1] + oy);
                }
                b'A' => {
                    let (rx, ry, rot) = (r.number()?, r.number()?, r.number()?);
                    let (large, sweep) = (r.flag()?, r.flag()?);
                    let (nx, ny) = (r.number()? + ox, r.number()? + oy);
                    out.push(Seg::Arc { rx: rx.abs(), ry: ry.abs(), rot, large, sweep, x: nx, y: ny });
                    (x, y) = (nx, ny);
                    ctrl = None;
                }
                b'Z' => {
                    out.push(Seg::Close);
                    (x, y) = (sx, sy);
                    ctrl = None;
                    cmd = 0;
                }
                _ => return None,
            }
            Some(())
        })();
        if parsed.is_none() {
            break;
        }
    }
    out
}
