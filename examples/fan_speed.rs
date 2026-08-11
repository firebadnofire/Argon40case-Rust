use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use argon40::{
    MCU_ADDRESS,
    hardware::{LinuxDevice, Mcu},
};

fn main() -> Result<()> {
    let percent: u8 = std::env::args()
        .nth(1)
        .context("usage: fan_speed <0..100>")?
        .parse()?;
    let device = LinuxDevice::open(
        Path::new("/dev/i2c-1"),
        MCU_ADDRESS,
        Arc::new(Mutex::new(())),
    )?;
    Mcu::new(device).set_fan(percent)
}
