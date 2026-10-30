//! The picker's text: one font, the system's sans-serif (as fontconfig's
//! fc-match finds it), each line drawn once into an image (fontdue).

use std::process::Command;

use fontdue::{Font, FontSettings};

pub struct Text {
    font: Font,
}

/// A line of text drawn: white, its coverage as alpha (premultiplied
/// ARGB8888, as the renderer imports it).
pub struct Line {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Text {
    /// The system's sans-serif font.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let found = Command::new("fc-match")
            .args(["--format=%{file}", "sans-serif"])
            .output()
            .map_err(|e| format!("can't run fc-match, to find a font: {e}"))?;
        let path = String::from_utf8(found.stdout)?;
        let data = std::fs::read(&path).map_err(|e| format!("reading the font {path}: {e}"))?;
        let font = Font::from_bytes(data, FontSettings::default())
            .map_err(|e| format!("the font {path}: {e}"))?;
        Ok(Text { font })
    }

    /// text, px pixels tall (the font's size): as wide as it is, as tall as
    /// the font's line.
    pub fn line(&self, text: &str, px: f32) -> Line {
        let lines = self.font.horizontal_line_metrics(px);
        let ascent = lines.map_or(px, |m| m.ascent).ceil() as i32;
        let height = lines.map_or(px * 1.3, |m| m.new_line_size).ceil() as usize;
        // Where each glyph goes, from the left, with kerning.
        let mut x = 0.0;
        let mut previous = None;
        let mut placed = Vec::new();
        for c in text.chars() {
            if let Some(p) = previous {
                x += self.font.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            placed.push((c, x));
            x += self.font.metrics(c, px).advance_width;
            previous = Some(c);
        }
        let width = (x.ceil() as usize).max(1);
        let mut pixels = vec![0; width * height * 4];
        for (c, at) in placed {
            let (metrics, coverage) = self.font.rasterize(c, px);
            let left = at.round() as i32 + metrics.xmin;
            let top = ascent - metrics.height as i32 - metrics.ymin;
            for row in 0..metrics.height {
                for column in 0..metrics.width {
                    let (x, y) = (left + column as i32, top + row as i32);
                    if x < 0 || y < 0 || x as usize >= width || y as usize >= height {
                        continue;
                    }
                    let at = (y as usize * width + x as usize) * 4;
                    let alpha = coverage[row * metrics.width + column];
                    // Glyphs may overlap a little: the most cover wins.
                    let alpha = alpha.max(pixels[at + 3]);
                    pixels[at..at + 4].copy_from_slice(&[alpha; 4]);
                }
            }
        }
        Line {
            width,
            height,
            pixels,
        }
    }
}
