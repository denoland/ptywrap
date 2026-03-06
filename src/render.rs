use image::RgbImage;

/// Standard 16-color terminal palette.
const PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],       // 0 black
    [170, 0, 0],     // 1 red
    [0, 170, 0],     // 2 green
    [170, 85, 0],    // 3 yellow/brown
    [0, 0, 170],     // 4 blue
    [170, 0, 170],   // 5 magenta
    [0, 170, 170],   // 6 cyan
    [170, 170, 170], // 7 white
    [85, 85, 85],    // 8 bright black
    [255, 85, 85],   // 9 bright red
    [85, 255, 85],   // 10 bright green
    [255, 255, 85],  // 11 bright yellow
    [85, 85, 255],   // 12 bright blue
    [255, 85, 255],  // 13 bright magenta
    [85, 255, 255],  // 14 bright cyan
    [255, 255, 255], // 15 bright white
];

const DEFAULT_FG: [u8; 3] = [170, 170, 170]; // color 7
const DEFAULT_BG: [u8; 3] = [0, 0, 0]; // color 0

fn color_to_rgb(color: vt100::Color, bold: bool, is_fg: bool) -> [u8; 3] {
    match color {
        vt100::Color::Default => {
            if is_fg && bold {
                PALETTE[15] // bright white for bold default fg
            } else if is_fg {
                DEFAULT_FG
            } else {
                DEFAULT_BG
            }
        }
        vt100::Color::Idx(idx) => {
            if idx < 8 && bold && is_fg {
                PALETTE[idx as usize + 8]
            } else if idx < 16 {
                PALETTE[idx as usize]
            } else if idx < 232 {
                // 6x6x6 color cube (indices 16-231)
                let idx = idx - 16;
                let b = idx % 6;
                let g = (idx / 6) % 6;
                let r = idx / 36;
                let to_val = |c: u8| if c == 0 { 0u8 } else { 55 + 40 * c };
                [to_val(r), to_val(g), to_val(b)]
            } else {
                // Grayscale ramp (indices 232-255)
                let v = 8 + 10 * (idx - 232);
                [v, v, v]
            }
        }
        vt100::Color::Rgb(r, g, b) => [r, g, b],
    }
}

fn get_glyph(ch: char) -> Option<[u8; 8]> {
    use font8x8::UnicodeFonts;
    if let Some(g) = font8x8::BASIC_FONTS.get(ch) {
        return Some(g);
    }
    if let Some(g) = font8x8::BOX_FONTS.get(ch) {
        return Some(g);
    }
    if let Some(g) = font8x8::BLOCK_FONTS.get(ch) {
        return Some(g);
    }
    None
}

pub fn render_screenshot(screen: &vt100::Screen, scale: u32) -> RgbImage {
    let (rows, cols) = screen.size();
    let img_w = cols as u32 * 8 * scale;
    let img_h = rows as u32 * 8 * scale;
    let mut img = RgbImage::new(img_w, img_h);

    for row in 0..rows {
        for col in 0..cols {
            let cell = screen.cell(row, col).unwrap();
            let ch = cell.contents().chars().next().unwrap_or(' ');
            let bold = cell.bold();
            let inverse = cell.inverse();

            let mut fg = color_to_rgb(cell.fgcolor(), bold, true);
            let mut bg = color_to_rgb(cell.bgcolor(), false, false);

            if inverse {
                std::mem::swap(&mut fg, &mut bg);
            }

            let glyph = get_glyph(ch).unwrap_or(get_glyph('?').unwrap_or([0; 8]));

            let base_x = col as u32 * 8 * scale;
            let base_y = row as u32 * 8 * scale;

            for gy in 0..8u32 {
                let glyph_row = glyph[gy as usize];
                for gx in 0..8u32 {
                    let lit = (glyph_row >> gx) & 1 != 0;
                    let color = if lit { fg } else { bg };
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let px = base_x + gx * scale + sx;
                            let py = base_y + gy * scale + sy;
                            img.put_pixel(px, py, image::Rgb(color));
                        }
                    }
                }
            }
        }
    }

    img
}
