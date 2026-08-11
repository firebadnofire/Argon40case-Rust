use std::{fs, path::Path};

use anyhow::{Context, Result, bail};

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 64;
pub const BUFFER_SIZE: usize = WIDTH * HEIGHT / 8;
pub const FONT_CHARACTERS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framebuffer {
    bytes: [u8; BUFFER_SIZE],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawMode {
    Replace,
    Xor,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Default for Framebuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framebuffer {
    pub fn new() -> Self {
        Self {
            bytes: [0; BUFFER_SIZE],
        }
    }

    pub fn bytes(&self) -> &[u8; BUFFER_SIZE] {
        &self.bytes
    }

    pub fn clear(&mut self, white: bool) {
        self.bytes.fill(if white { 0xff } else { 0 });
    }

    pub fn load_background(&mut self, path: &Path) -> Result<()> {
        let input =
            fs::read(path).with_context(|| format!("read OLED background {}", path.display()))?;
        self.bytes.fill(0);
        let count = input.len().min(BUFFER_SIZE);
        self.bytes[..count].copy_from_slice(&input[..count]);
        Ok(())
    }

    pub fn pixel(&self, x: usize, y: usize) -> Option<bool> {
        if x >= WIDTH || y >= HEIGHT {
            return None;
        }
        let byte = self.bytes[WIDTH * (y / 8) + x];
        Some(byte & (1 << (y % 8)) != 0)
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, value: bool, mode: DrawMode) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let index = WIDTH * (y / 8) + x;
        let mask = 1 << (y % 8);
        match mode {
            DrawMode::Replace => {
                self.bytes[index] = (self.bytes[index] & !mask) | if value { mask } else { 0 };
            }
            DrawMode::Xor if value => self.bytes[index] ^= mask,
            DrawMode::Or if value => self.bytes[index] |= mask,
            DrawMode::Xor | DrawMode::Or => {}
        }
    }

    pub fn write_byte_row(&mut self, x: usize, y: usize, value: u8, mode: DrawMode) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let index = WIDTH * (y / 8) + x;
        match mode {
            DrawMode::Replace => self.bytes[index] = value,
            DrawMode::Xor => self.bytes[index] ^= value,
            DrawMode::Or => self.bytes[index] |= value,
        }
    }

    pub fn filled_rectangle(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        mode: DrawMode,
    ) {
        for column in x..x.saturating_add(width).min(WIDTH) {
            for row in y..y.saturating_add(height).min(HEIGHT) {
                self.set_pixel(column, row, true, mode);
            }
        }
    }

    pub fn write_text(
        &mut self,
        text: &str,
        x: isize,
        y: usize,
        char_width: usize,
        font: &[u8],
        mode: DrawMode,
    ) -> Result<()> {
        let char_width = char_width.max(6);
        let mut char_height = char_width * 8 / 6;
        char_height = char_height.next_multiple_of(8);
        let rows = char_height / 8;
        let expected = FONT_CHARACTERS * char_width * rows;
        if font.len() < expected {
            bail!(
                "OLED font is {} bytes; expected at least {expected}",
                font.len()
            );
        }
        let mut cursor = x;
        for character in text.bytes() {
            for column in 0..char_width {
                let draw_x = cursor + column as isize;
                if draw_x < 0 || draw_x >= WIDTH as isize {
                    continue;
                }
                for font_row in 0..rows {
                    let value = font[character as usize * char_width
                        + column
                        + FONT_CHARACTERS * char_width * font_row];
                    for bit in 0..8 {
                        let draw_y = y + font_row * 8 + bit;
                        if draw_y < HEIGHT {
                            self.set_pixel(
                                draw_x as usize,
                                draw_y,
                                value & (0x80 >> bit) != 0,
                                mode,
                            );
                        }
                    }
                }
            }
            cursor += char_width as isize;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn write_text_aligned(
        &mut self,
        text: &str,
        x: isize,
        y: usize,
        box_width: usize,
        align: Align,
        char_width: usize,
        font: &[u8],
        mode: DrawMode,
    ) -> Result<()> {
        let used = text.len() * char_width.max(6);
        let offset = match align {
            Align::Left => 0,
            Align::Center => box_width.saturating_sub(used) / 2,
            Align::Right => box_width.saturating_sub(used),
        };
        self.write_text(text, x + offset as isize, y, char_width, font, mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_use_legacy_page_layout() {
        let mut fb = Framebuffer::new();
        fb.set_pixel(3, 9, true, DrawMode::Replace);
        assert_eq!(fb.bytes()[WIDTH + 3], 0b0000_0010);
        assert_eq!(fb.pixel(3, 9), Some(true));
        fb.set_pixel(3, 9, false, DrawMode::Replace);
        assert_eq!(fb.pixel(3, 9), Some(false));
        assert_eq!(fb.pixel(WIDTH, 0), None);
    }

    #[test]
    fn rectangle_clips_to_screen() {
        let mut fb = Framebuffer::new();
        fb.filled_rectangle(126, 62, 8, 8, DrawMode::Replace);
        assert_eq!(
            fb.bytes()
                .iter()
                .map(|value| value.count_ones())
                .sum::<u32>(),
            4
        );
    }

    #[test]
    fn text_and_alignment_use_binary_font_layout() {
        let mut font = vec![0_u8; FONT_CHARACTERS * 6];
        font[b'A' as usize * 6] = 0x80;
        let mut fb = Framebuffer::new();
        fb.write_text_aligned("A", 0, 0, 10, Align::Right, 6, &font, DrawMode::Replace)
            .unwrap();
        assert_eq!(fb.pixel(4, 0), Some(true));
    }

    #[test]
    fn background_is_truncated_or_zero_padded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("background.bin");
        fs::write(&path, [1, 2, 3]).unwrap();
        let mut fb = Framebuffer::new();
        fb.clear(true);
        fb.load_background(&path).unwrap();
        assert_eq!(&fb.bytes()[..4], &[1, 2, 3, 0]);
    }
}
