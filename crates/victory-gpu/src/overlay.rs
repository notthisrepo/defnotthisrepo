//! A diagnostics overlay (`VICTORY_OVERLAY=1`): a block of text over the
//! top left corner of every presented frame, with what the frame line of
//! the log says once every few seconds, renewed every second: frame rate
//! and time, where the time goes (preparing, the card, copying back, the
//! waits), the PC's and the card's memory, what was allocated, textures
//! that are not there yet, the shader cache, and what was dropped.
//!
//! The text is rasterised here with a 5x7 font of our own into pixels of
//! the back buffer's format, and [`crate::host`] copies them over the back
//! buffer while it is a copy destination anyway (no pipeline, no shader,
//! no target view: nothing of the title's state is touched). Off, nothing
//! of this runs.

use std::sync::{Arc, Mutex, OnceLock};

/// Whether the overlay is shown (`VICTORY_OVERLAY`, any value but `0`).
pub(crate) fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_OVERLAY").is_some_and(|v| v != "0"))
}

/// What the overlay shows: the last second's figures (per frame where it
/// says so) and totals since the start.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Stats {
    pub(crate) fps: f64,
    pub(crate) frame_ms: f64,
    /// Per frame: preparing submissions on the CPU, of that copying title
    /// buffers and refreshing textures; waiting for the card; copying back
    /// what shaders wrote.
    pub(crate) prepare_ms: f64,
    pub(crate) copy_ms: f64,
    pub(crate) texture_ms: f64,
    pub(crate) gpu_ms: f64,
    pub(crate) back_ms: f64,
    /// Per frame: megabytes of title buffers copied, of textures refreshed,
    /// and copied back.
    pub(crate) copy_mb: f64,
    pub(crate) texture_mb: f64,
    pub(crate) back_mb: f64,
    /// Per frame: the submitting thread standing still for the card, and
    /// the card with nothing of ours queued.
    pub(crate) wait_ms: f64,
    pub(crate) card_idle_ms: f64,
    pub(crate) draws: f64,
    /// Per frame: draws dropped without a host pipeline, indirect
    /// executions dropped.
    pub(crate) dropped: f64,
    pub(crate) dropped_indirect: f64,
    /// The process: megabytes private (committed) and in memory.
    pub(crate) private_mb: u64,
    pub(crate) working_mb: u64,
    /// The card: megabytes in use and the budget; the PC's memory shared
    /// with it in use.
    pub(crate) vram_mb: u64,
    pub(crate) vram_budget_mb: u64,
    pub(crate) shared_mb: u64,
    /// Host buffers made since the start (upload, shared, card), their
    /// megabytes, and how many were made in the last second.
    pub(crate) buffers: u64,
    pub(crate) buffers_mb: u64,
    pub(crate) buffers_new: u64,
    /// Host copies of title memory alive and their megabytes.
    pub(crate) copies: u64,
    pub(crate) copies_mb: u64,
    /// Textures picked by index: uploaded since the start, their megabytes,
    /// slots waiting, passed over because their memory is not mapped,
    /// passes that made none because the card was full.
    pub(crate) textures_up: u64,
    pub(crate) textures_up_mb: u64,
    pub(crate) textures_waiting: u64,
    pub(crate) textures_unmapped: u64,
    pub(crate) textures_no_room: u64,
    /// Buffer views left null because the host would refuse them.
    pub(crate) views_refused: u64,
    /// The shader cache: found in it, translated anew, and translations
    /// that failed (their pipelines' draws are dropped); host pipelines
    /// queued and not made yet.
    pub(crate) shader_hits: u64,
    pub(crate) shader_misses: u64,
    pub(crate) shader_failures: u64,
    pub(crate) pipelines_making: u64,
}

/// The overlay's lines for `s`: at most [`COLUMNS`] characters each, in
/// the font's characters only (upper case, digits and a few signs).
pub(crate) fn lines(s: &Stats) -> Vec<String> {
    let lines = vec![
        format!("FPS {:.1}  FRAME {:.1} MS  DRAWS {:.0}", s.fps, s.frame_ms, s.draws),
        format!("CPU PREP {:.1} MS (COPY {:.1} TEX {:.1})", s.prepare_ms, s.copy_ms, s.texture_ms),
        format!("GPU WAIT {:.1} MS  COPY BACK {:.1} MS", s.gpu_ms, s.back_ms),
        format!("SYNC: STOOD {:.1} MS  CARD IDLE {:.1} MS", s.wait_ms, s.card_idle_ms),
        format!("MB/FRAME: COPY {:.0} TEX {:.0} BACK {:.0}", s.copy_mb, s.texture_mb, s.back_mb),
        format!("RAM {} MB PRIVATE, {} MB IN MEMORY", s.private_mb, s.working_mb),
        format!("VRAM {}/{} MB  SHARED {} MB", s.vram_mb, s.vram_budget_mb, s.shared_mb),
        format!("ALLOC: {} BUFFERS {} MB (+{}/S)", s.buffers, s.buffers_mb, s.buffers_new),
        format!("HOST COPIES {} ALIVE {} MB", s.copies, s.copies_mb),
        format!("TEX UP {} ({} MB) WAITING {}", s.textures_up, s.textures_up_mb, s.textures_waiting),
        format!("TEX FAIL: UNMAPPED {} NO ROOM {} VIEWS {}", s.textures_unmapped, s.textures_no_room, s.views_refused),
        format!("SHADERS: CACHED {} NEW {} FAILED {}", s.shader_hits, s.shader_misses, s.shader_failures),
        format!("PIPELINES QUEUED {}  DROPPED {:.0}+{:.0}/FRAME", s.pipelines_making, s.dropped, s.dropped_indirect),
    ];
    lines.into_iter().map(|l| l.chars().take(COLUMNS).collect()).collect()
}

/// The most characters of a line.
pub(crate) const COLUMNS: usize = 46;

/// A glyph: seven rows of five dots, the leftmost in bit 4.
type Glyph = [u8; 7];

/// What a character that has no glyph shows.
const UNKNOWN: Glyph = [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100];

/// The font: digits, upper case letters and the signs the lines use.
/// Lower case letters show as upper case.
fn glyph(c: char) -> Glyph {
    match c.to_ascii_uppercase() {
        ' ' => [0; 7],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100],
        ',' => [0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b00100, 0b01000],
        ':' => [0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000],
        '/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        '%' => [0b11000, 0b11001, 0b00010, 0b00100, 0b01000, 0b10011, 0b00011],
        '(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        ')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        '-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        '+' => [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000],
        '=' => [0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000],
        _ => UNKNOWN,
    }
}

/// Dots of a character's cell (five and a gap; seven and two rows of gap),
/// the screen pixels of a dot, and the margin around the text in pixels.
const CELL: (usize, usize) = (6, 9);
const SCALE: usize = 2;
const MARGIN: usize = 6;

/// `lines` as a mask, one byte per pixel (0 background, 1 text), with its
/// width and height: every line [`COLUMNS`] characters wide, whatever it
/// holds, so the block keeps its size from second to second.
pub(crate) fn mask(lines: &[String]) -> (Vec<u8>, usize, usize) {
    let width = 2 * MARGIN + COLUMNS * CELL.0 * SCALE;
    let height = 2 * MARGIN + lines.len() * CELL.1 * SCALE;
    let mut out = vec![0u8; width * height];
    for (row, line) in lines.iter().enumerate() {
        for (column, c) in line.chars().take(COLUMNS).enumerate() {
            let dots = glyph(c);
            let (left, top) = (MARGIN + column * CELL.0 * SCALE, MARGIN + row * CELL.1 * SCALE);
            for (y, bits) in dots.iter().enumerate() {
                for x in 0..5 {
                    if bits & (0b10000 >> x) == 0 {
                        continue;
                    }
                    for dy in 0..SCALE {
                        let at = (top + y * SCALE + dy) * width + left + x * SCALE;
                        out[at..at + SCALE].fill(1);
                    }
                }
            }
        }
    }
    (out, width, height)
}

/// The pixel formats a back buffer has (`crate::host`'s `swap_format`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pixel {
    Rgba8,
    Bgra8,
    Rgb10A2,
    RgbaF16,
}

impl Pixel {
    /// The format with DXGI's number `format`, if a back buffer has it.
    pub(crate) fn of(format: i32) -> Option<Pixel> {
        Some(match format {
            28 => Pixel::Rgba8,
            87 => Pixel::Bgra8,
            24 => Pixel::Rgb10A2,
            10 => Pixel::RgbaF16,
            _ => return None,
        })
    }

    /// Bytes of a pixel.
    pub(crate) fn bytes(self) -> usize {
        match self {
            Pixel::RgbaF16 => 8,
            _ => 4,
        }
    }

    /// The bytes of an opaque pixel: the text's (a light grey, so that it
    /// does not glare on an HDR back buffer) or the background's (black).
    fn encode(self, text: bool) -> [u8; 8] {
        let mut out = [0u8; 8];
        match self {
            // The same value in all three channels: the order of red and
            // blue makes no difference.
            Pixel::Rgba8 | Pixel::Bgra8 => {
                let v = if text { 0xE6 } else { 0 };
                out[..4].copy_from_slice(&[v, v, v, 0xFF]);
            }
            Pixel::Rgb10A2 => {
                let v: u32 = if text { 0x39A } else { 0 };
                out[..4].copy_from_slice(&(v | v << 10 | v << 20 | 3 << 30).to_le_bytes());
            }
            Pixel::RgbaF16 => {
                // Half floats: 0.875 and 1.0.
                let v: u16 = if text { 0x3B00 } else { 0 };
                for (i, half) in [v, v, v, 0x3C00].into_iter().enumerate() {
                    out[2 * i..2 * i + 2].copy_from_slice(&half.to_le_bytes());
                }
            }
        }
        out
    }
}

/// A row's bytes in a buffer a texture is copied from: Direct3D 12 wants
/// rows 256 bytes apart or a multiple of that.
pub(crate) const ROW_ALIGNMENT: usize = 256;

/// The overlay's pixels, ready to be copied over a back buffer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Image {
    pub(crate) pixel: Pixel,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Bytes from one row to the next ([`ROW_ALIGNMENT`]).
    pub(crate) pitch: u32,
    /// `pitch * height` bytes.
    pub(crate) bytes: Vec<u8>,
}

/// `lines` as pixels of format `pixel`.
pub(crate) fn image(lines: &[String], pixel: Pixel) -> Image {
    let (mask, width, height) = mask(lines);
    let size = pixel.bytes();
    let pitch = (width * size).next_multiple_of(ROW_ALIGNMENT);
    let (text, background) = (pixel.encode(true), pixel.encode(false));
    let mut bytes = vec![0u8; pitch * height];
    for y in 0..height {
        let row = &mut bytes[y * pitch..y * pitch + width * size];
        for (x, out) in row.chunks_exact_mut(size).enumerate() {
            let from = if mask[y * width + x] != 0 { &text } else { &background };
            out.copy_from_slice(&from[..size]);
        }
    }
    Image { pixel, width: width as u32, height: height as u32, pitch: pitch as u32, bytes }
}

/// The figures last given ([`publish`]) and their image, made when first
/// asked for in a format.
static SHOWN: Mutex<(Option<Stats>, Option<Arc<Image>>)> = Mutex::new((None, None));

/// New figures: the next [`current`] shows them.
pub(crate) fn publish(stats: Stats) {
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    if shown.0.as_ref() != Some(&stats) {
        *shown = (Some(stats), None);
    }
}

/// The overlay's image in format `pixel`, if figures were given: the same
/// one until they change.
pub(crate) fn current(pixel: Pixel) -> Option<Arc<Image>> {
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    let stats = shown.0.clone()?;
    match &shown.1 {
        Some(made) if made.pixel == pixel => Some(made.clone()),
        _ => {
            let made = Arc::new(image(&lines(&stats), pixel));
            shown.1 = Some(made.clone());
            Some(made)
        }
    }
}

/// Where the block goes on a back buffer of `width` by `height` pixels and
/// how much of it: its left and top, and the width and height copied (the
/// block is cut where the back buffer ends). `None` if nothing of it fits.
pub(crate) fn placement(image: &Image, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    const AT: (u32, u32) = (16, 16);
    let (w, h) = (image.width.min(width.saturating_sub(AT.0)), image.height.min(height.saturating_sub(AT.1)));
    (w > 0 && h > 0).then_some((AT.0, AT.1, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character the lines can hold has a glyph of its own: none
    /// falls back to the question mark, and no two look the same.
    #[test]
    fn the_lines_use_only_characters_the_font_has() {
        let full = Stats {
            fps: 123.4,
            frame_ms: 5678.9,
            draws: 12345.0,
            private_mb: 67_890,
            ..Default::default()
        };
        let mut seen = std::collections::HashMap::new();
        for line in lines(&full) {
            assert!(line.chars().count() <= COLUMNS, "{line:?} is longer than a line");
            for c in line.chars() {
                assert!(c == '?' || glyph(c) != UNKNOWN, "no glyph for {c:?} in {line:?}");
                if let Some(other) = seen.insert(glyph(c), c.to_ascii_uppercase()) {
                    assert_eq!(other, c.to_ascii_uppercase(), "{c:?} looks like {other:?}");
                }
            }
        }
    }

    /// Lines of the widest figures still fit.
    #[test]
    fn long_figures_are_cut_at_the_line_and_not_beyond() {
        let wide = Stats {
            fps: 999.9,
            frame_ms: 99_999.9,
            prepare_ms: 99_999.9,
            copy_ms: 99_999.9,
            texture_ms: 99_999.9,
            gpu_ms: 99_999.9,
            back_ms: 99_999.9,
            draws: 999_999.0,
            private_mb: 999_999,
            working_mb: 999_999,
            vram_mb: 99_999,
            vram_budget_mb: 99_999,
            shared_mb: 99_999,
            buffers: 9_999_999,
            buffers_mb: 9_999_999,
            buffers_new: 99_999,
            textures_up: 9_999_999,
            textures_up_mb: 9_999_999,
            textures_waiting: 999_999,
            ..Default::default()
        };
        for line in lines(&wide) {
            assert!(line.chars().count() <= COLUMNS);
        }
    }

    /// The mask holds exactly the font's dots, each `SCALE` pixels square,
    /// where the character's cell is.
    #[test]
    fn a_character_is_drawn_dot_by_dot_in_its_cell() {
        let (mask, width, height) = mask(&[" 1".to_string(), "T".to_string()]);
        assert_eq!(width, 2 * MARGIN + COLUMNS * CELL.0 * SCALE);
        assert_eq!(height, 2 * MARGIN + 2 * CELL.1 * SCALE);
        let lit = |column: usize, row: usize, x: usize, y: usize| {
            let (left, top) = (MARGIN + column * CELL.0 * SCALE, MARGIN + row * CELL.1 * SCALE);
            (0..SCALE).all(|dy| (0..SCALE).all(|dx| mask[(top + y * SCALE + dy) * width + left + x * SCALE + dx] == 1))
        };
        for (column, row, c) in [(1, 0, '1'), (0, 1, 'T')] {
            let dots = glyph(c);
            for (y, bits) in dots.iter().enumerate() {
                for x in 0..5 {
                    assert_eq!(lit(column, row, x, y), bits & (0b10000 >> x) != 0, "{c:?} dot {x},{y}");
                }
            }
        }
        // Nothing else is lit: the two glyphs' dots, SCALE x SCALE each.
        let dots: u32 = [glyph('1'), glyph('T')].iter().flatten().map(|b| b.count_ones()).sum();
        assert_eq!(mask.iter().filter(|&&m| m == 1).count(), dots as usize * SCALE * SCALE);
    }

    /// Rows are 256 bytes apart or a multiple, every pixel is opaque, and
    /// text and background have the bytes of their format.
    #[test]
    fn pixels_have_their_formats_bytes_and_rows_their_alignment() {
        let text = vec!["FPS 1".to_string()];
        let (mask, width, height) = mask(&text);
        for pixel in [Pixel::Rgba8, Pixel::Bgra8, Pixel::Rgb10A2, Pixel::RgbaF16] {
            let made = image(&text, pixel);
            assert_eq!((made.width as usize, made.height as usize), (width, height));
            assert_eq!(made.pitch as usize % ROW_ALIGNMENT, 0);
            assert!(made.pitch as usize >= width * pixel.bytes());
            assert_eq!(made.bytes.len(), made.pitch as usize * height);
            for y in 0..height {
                for x in 0..width {
                    let at = y * made.pitch as usize + x * pixel.bytes();
                    let got = &made.bytes[at..at + pixel.bytes()];
                    let lit = mask[y * width + x] == 1;
                    match pixel {
                        Pixel::Rgba8 | Pixel::Bgra8 => {
                            assert_eq!(got, if lit { [0xE6, 0xE6, 0xE6, 0xFF] } else { [0, 0, 0, 0xFF] });
                        }
                        Pixel::Rgb10A2 => {
                            let v = u32::from_le_bytes(got.try_into().unwrap());
                            assert_eq!(v >> 30, 3, "opaque");
                            let channels = [v & 0x3FF, v >> 10 & 0x3FF, v >> 20 & 0x3FF];
                            assert_eq!(channels, if lit { [0x39A; 3] } else { [0; 3] });
                        }
                        Pixel::RgbaF16 => {
                            let half = |i: usize| u16::from_le_bytes([got[2 * i], got[2 * i + 1]]);
                            assert_eq!(half(3), 0x3C00, "alpha 1.0");
                            assert_eq!([half(0), half(1), half(2)], if lit { [0x3B00; 3] } else { [0; 3] });
                        }
                    }
                }
            }
        }
    }

    /// DXGI's numbers of the four back buffer formats, and nothing else.
    #[test]
    fn only_back_buffer_formats_are_taken() {
        assert_eq!(Pixel::of(28), Some(Pixel::Rgba8));
        assert_eq!(Pixel::of(87), Some(Pixel::Bgra8));
        assert_eq!(Pixel::of(24), Some(Pixel::Rgb10A2));
        assert_eq!(Pixel::of(10), Some(Pixel::RgbaF16));
        assert_eq!(Pixel::of(29), None);
        assert_eq!(Pixel::of(0), None);
    }

    /// The block is cut at the back buffer's edges and left out where
    /// nothing of it fits.
    #[test]
    fn the_block_is_cut_where_the_back_buffer_ends() {
        let made = image(&["A".to_string()], Pixel::Rgba8);
        assert_eq!(placement(&made, 3840, 2160), Some((16, 16, made.width, made.height)));
        assert_eq!(placement(&made, 116, 30), Some((16, 16, 100, 14)));
        assert_eq!(placement(&made, 16, 2160), None);
        assert_eq!(placement(&made, 3840, 10), None);
    }

    /// The image is made once for figures and format, and anew when
    /// either changes.
    #[test]
    fn the_image_is_kept_until_the_figures_or_the_format_change() {
        // This test owns `SHOWN`: no other test publishes.
        publish(Stats { fps: 30.0, ..Default::default() });
        let first = current(Pixel::Rgba8).unwrap();
        assert!(Arc::ptr_eq(&first, &current(Pixel::Rgba8).unwrap()));
        publish(Stats { fps: 30.0, ..Default::default() });
        assert!(Arc::ptr_eq(&first, &current(Pixel::Rgba8).unwrap()), "the same figures keep the image");
        let other = current(Pixel::Rgb10A2).unwrap();
        assert_eq!(other.pixel, Pixel::Rgb10A2);
        publish(Stats { fps: 31.0, ..Default::default() });
        let second = current(Pixel::Rgb10A2).unwrap();
        assert!(!Arc::ptr_eq(&other, &second));
        assert_ne!(other.bytes, second.bytes);
    }
}
