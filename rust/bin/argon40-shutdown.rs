use std::{
    env,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result};
use argon40::{
    MCU_ADDRESS, OLED_ADDRESS, RTC_ADDRESS,
    hardware::{LinuxDevice, Mcu, OledDevice},
    oled::Framebuffer,
    rtc::Pcf8563,
};

fn device(bus: &Path, address: u16, lock: &Arc<Mutex<()>>) -> Result<LinuxDevice> {
    LinuxDevice::open(bus, address, Arc::clone(lock))
}

fn main() -> Result<()> {
    let action = env::args()
        .nth(1)
        .context("systemd shutdown action argument is required")?;
    let bus_path = env::var_os("ARGON40_I2C_BUS").unwrap_or_else(|| "/dev/i2c-1".into());
    let bus = Path::new(&bus_path);
    let lock = Arc::new(Mutex::new(()));

    let mut mcu = Mcu::new(device(bus, MCU_ADDRESS, &lock).context("open Argon MCU for shutdown")?);
    mcu.set_fan(0).context("stop fan during shutdown")?;

    if let Ok(oled_device) = device(bus, OLED_ADDRESS, &lock) {
        let mut oled = OledDevice::new(oled_device);
        let blank = Framebuffer::new();
        if let Err(error) = oled.flush(&blank, false).and_then(|()| oled.power(false)) {
            eprintln!("argon40-shutdown: optional OLED cleanup failed: {error:#}");
        }
    }

    if action == "poweroff" || action == "halt" {
        if let Ok(rtc_device) = device(bus, RTC_ADDRESS, &lock) {
            if let Err(error) = Pcf8563::new(rtc_device).clear_event_flags() {
                eprintln!("argon40-shutdown: optional RTC event cleanup failed: {error:#}");
            }
        }
        mcu.request_power_cut()
            .context("request physical power cut")?;
    }
    Ok(())
}
