#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub bg: Rgb,
    pub fg: Rgb,
    pub ink: Rgb,
    pub soft: Rgb,
    pub mute: Rgb,
    pub faint: Rgb,
    pub line: Rgb,
    pub edge: Rgb,
    pub raise: Rgb,
    pub hover: Rgb,
    pub ok: Rgb,
    pub warn: Rgb,
    pub bad: Rgb,
    pub accent: Rgb,
    pub accent_soft: Rgb,
}

pub const DARK: Theme = Theme {
    dark: true,
    bg: Rgb(0x00, 0x00, 0x00),
    fg: Rgb(0xff, 0xff, 0xff),
    ink: Rgb(0xe6, 0xe6, 0xe6),
    soft: Rgb(0xa8, 0xa8, 0xa8),
    mute: Rgb(0x7a, 0x7a, 0x7a),
    faint: Rgb(0x4a, 0x4a, 0x4a),
    line: Rgb(0x1a, 0x1a, 0x1a),
    edge: Rgb(0x2c, 0x2c, 0x2c),
    raise: Rgb(0x0d, 0x0d, 0x0d),
    hover: Rgb(0x16, 0x16, 0x16),
    ok: Rgb(0x7c, 0xe3, 0x8b),
    warn: Rgb(0xe8, 0xb6, 0x5a),
    bad: Rgb(0xff, 0x7a, 0x70),
    accent: Rgb(0xb2, 0x3a, 0x2e),
    accent_soft: Rgb(0xff, 0x8a, 0x7a),
};

pub const LIGHT: Theme = Theme {
    dark: false,
    bg: Rgb(0xfa, 0xfa, 0xfa),
    fg: Rgb(0x0a, 0x0a, 0x0a),
    ink: Rgb(0x1c, 0x1c, 0x1c),
    soft: Rgb(0x55, 0x55, 0x55),
    mute: Rgb(0x7d, 0x7d, 0x7d),
    faint: Rgb(0xb5, 0xb5, 0xb5),
    line: Rgb(0xec, 0xec, 0xec),
    edge: Rgb(0xd9, 0xd9, 0xd9),
    raise: Rgb(0xff, 0xff, 0xff),
    hover: Rgb(0xf0, 0xf0, 0xf0),
    ok: Rgb(0x1f, 0x9d, 0x55),
    warn: Rgb(0xb7, 0x79, 0x1f),
    bad: Rgb(0xd2, 0x3f, 0x31),
    accent: Rgb(0xc0, 0x39, 0x2b),
    accent_soft: Rgb(0xc0, 0x39, 0x2b),
};

pub fn current() -> Theme {
    if crate::autostart::light_apps() { LIGHT } else { DARK }
}
