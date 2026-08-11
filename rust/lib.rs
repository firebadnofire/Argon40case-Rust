pub mod button;
pub mod config;
pub mod fan;
pub mod firmware;
pub mod hardware;
pub mod ir;
pub mod oled;
pub mod rtc;
pub mod schedule;
pub mod system_info;

pub const MCU_ADDRESS: u16 = 0x1a;
pub const OLED_ADDRESS: u16 = 0x3c;
pub const RTC_ADDRESS: u16 = 0x51;
