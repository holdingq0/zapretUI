// The zapretUI mark: a power symbol on a 16×16 dot matrix, the same square dots as the window's
// background. The tray paints it in the colour of the current state; build.rs bakes the .exe icon
// from it on a dark tile with the unlit dots showing.

pub const MARK: [&str; 16] = [
    "................",
    ".......##.......",
    "....#..##..#....",
    "...##..##..##...",
    "..##...##...##..",
    ".##....##....##.",
    ".##....##....##.",
    ".##....##....##.",
    ".##..........##.",
    ".##..........##.",
    ".##..........##.",
    "..##........##..",
    "..###......###..",
    "...##########...",
    ".....######.....",
    "................",
];

// Built by build.rs only; the running app paints the bare mark.
#[allow(dead_code)]
pub struct Tile {
    pub back: [u8; 3],
    pub off: [u8; 3],
}

/// Straight-alpha BGRA, `size`×`size`, top row first.
/// Dots get a gap once a cell is 3 px or more, so large icons read as a dot matrix.
pub fn render(size: usize, ink: [u8; 3], alpha: f32, tile: Option<&Tile>) -> Vec<u8> {
    let mut out = vec![0u8; size * size * 4];
    let put = |out: &mut Vec<u8>, x: usize, y: usize, c: [u8; 3], a: f32| {
        let i = (y * size + x) * 4;
        out[i] = c[2];
        out[i + 1] = c[1];
        out[i + 2] = c[0];
        out[i + 3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
    };
    if let Some(tile) = tile {
        // A rounded square, its corners antialiased by distance.
        let r = size as f32 * 0.2;
        for y in 0..size {
            for x in 0..size {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let qx = (r - px).max(px - (size as f32 - r)).max(0.0);
                let qy = (r - py).max(py - (size as f32 - r)).max(0.0);
                let d = (qx * qx + qy * qy).sqrt() - r;
                let a = (0.5 - d).clamp(0.0, 1.0);
                if a > 0.0 {
                    put(&mut out, x, y, tile.back, a);
                }
            }
        }
    }
    // The tile leaves a margin; a bare mark uses the whole square.
    let (inset, span) = if tile.is_some() { (size as f32 * 0.09, size as f32 * 0.82) } else { (0.0, size as f32) };
    let cell = span / 16.0;
    let gap = if cell >= 3.0 { (cell * 0.22).round().max(1.0) } else { 0.0 };
    for (row, line) in MARK.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            let lit = ch == b'#';
            let color = match (lit, tile) {
                (true, _) => ink,
                (false, Some(t)) if cell >= 3.0 => t.off,
                _ => continue,
            };
            let x0 = (inset + col as f32 * cell).round() as usize;
            let y0 = (inset + row as f32 * cell).round() as usize;
            let x1 = ((inset + (col + 1) as f32 * cell - gap).round() as usize).max(x0 + 1).min(size);
            let y1 = ((inset + (row + 1) as f32 * cell - gap).round() as usize).max(y0 + 1).min(size);
            for y in y0..y1 {
                for x in x0..x1 {
                    put(&mut out, x, y, color, if lit { alpha } else { 1.0 });
                }
            }
        }
    }
    out
}

#[allow(dead_code)]
const SIZES: [usize; 8] = [16, 20, 24, 32, 40, 48, 64, 128];

/// The .ico both build scripts embed: the mark on a dark tile, 16 to 128 px.
#[allow(dead_code)]
pub fn ico() -> Vec<u8> {
    let tile = Tile { back: [0x0b, 0x0b, 0x0b], off: [0x26, 0x26, 0x26] };
    let images: Vec<Vec<u8>> = SIZES
        .iter()
        .map(|&size| {
            let bgra = render(size, [0xf2, 0xf2, 0xf2], 1.0, Some(&tile));
            let mut dib = Vec::new();
            for v in [40u32, size as u32, size as u32 * 2] {
                dib.extend(v.to_le_bytes());
            }
            dib.extend(1u16.to_le_bytes());
            dib.extend(32u16.to_le_bytes());
            dib.extend([0u8; 24]);
            // Colour rows bottom-up, then an all-zero AND mask padded to 32 bits per row.
            for y in (0..size).rev() {
                dib.extend(&bgra[y * size * 4..(y + 1) * size * 4]);
            }
            dib.extend(vec![0u8; size.div_ceil(32) * 4 * size]);
            dib
        })
        .collect();
    let mut out = Vec::new();
    out.extend([0u8, 0, 1, 0]);
    out.extend((SIZES.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * SIZES.len();
    for (size, image) in SIZES.iter().zip(&images) {
        out.extend([*size as u8, *size as u8, 0, 0]);
        out.extend(1u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend((image.len() as u32).to_le_bytes());
        out.extend((offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend(image);
    }
    out
}
