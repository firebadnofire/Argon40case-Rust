use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use argon40::{
    OLED_ADDRESS,
    hardware::{LinuxDevice, OledDevice},
    oled::{DrawMode, Framebuffer},
};

fn main() -> Result<()> {
    let device = LinuxDevice::open(
        Path::new("/dev/i2c-1"),
        OLED_ADDRESS,
        Arc::new(Mutex::new(())),
    )?;
    let mut oled = OledDevice::new(device);
    let font = fs::read("/etc/argon/oled/font16x12.bin")?;
    let mut framebuffer = Framebuffer::new();
    framebuffer.write_text("Hello!", 10, 0, 12, &font, DrawMode::Replace)?;
    framebuffer.filled_rectangle(10, 17, 108, 14, DrawMode::Replace);
    framebuffer.set_pixel(15, 35, true, DrawMode::Replace);
    oled.power(true)?;
    oled.flush(&framebuffer, true)?;
    oled.reset_addressing()
}
