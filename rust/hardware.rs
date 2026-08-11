use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
use gpiocdev::{
    Request,
    line::{Bias, EdgeDetection, EdgeKind},
};
use i2cdev::{core::I2CDevice as _, linux::LinuxI2CDevice};

use crate::{
    MCU_ADDRESS, OLED_ADDRESS,
    oled::{Framebuffer, HEIGHT, WIDTH},
};

pub trait I2cDevice {
    fn write_byte(&mut self, value: u8) -> Result<()>;
    fn read_byte(&mut self) -> Result<u8>;
    fn write_byte_data(&mut self, register: u8, value: u8) -> Result<()>;
    fn read_byte_data(&mut self, register: u8) -> Result<u8>;
    fn write_block_data(&mut self, command: u8, data: &[u8]) -> Result<()>;

    fn read_sequential(&mut self, start_register: u8, length: usize) -> Result<Vec<u8>> {
        self.write_byte(start_register)?;
        (0..length).map(|_| self.read_byte()).collect()
    }
}

pub struct LinuxDevice {
    inner: LinuxI2CDevice,
    address: u16,
    bus_lock: Arc<Mutex<()>>,
}

impl LinuxDevice {
    pub fn open(path: &Path, address: u16, bus_lock: Arc<Mutex<()>>) -> Result<Self> {
        let inner = LinuxI2CDevice::new(path, address).with_context(|| {
            format!("open I2C device {} address 0x{address:02x}", path.display())
        })?;
        Ok(Self {
            inner,
            address,
            bus_lock,
        })
    }

    fn locked<T>(
        &mut self,
        operation: &str,
        call: impl FnOnce(&mut LinuxI2CDevice) -> std::result::Result<T, i2cdev::linux::LinuxI2CError>,
    ) -> Result<T> {
        let _guard = self
            .bus_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("I2C bus mutex poisoned"))?;
        call(&mut self.inner)
            .with_context(|| format!("I2C address 0x{:02x}: {operation}", self.address))
    }
}

impl I2cDevice for LinuxDevice {
    fn write_byte(&mut self, value: u8) -> Result<()> {
        self.locked(&format!("SMBus write byte 0x{value:02x}"), |device| {
            device.smbus_write_byte(value)
        })
    }
    fn read_byte(&mut self) -> Result<u8> {
        self.locked("SMBus read byte", |device| device.smbus_read_byte())
    }
    fn write_byte_data(&mut self, register: u8, value: u8) -> Result<()> {
        self.locked(
            &format!("SMBus write register 0x{register:02x}=0x{value:02x}"),
            |device| device.smbus_write_byte_data(register, value),
        )
    }
    fn read_byte_data(&mut self, register: u8) -> Result<u8> {
        self.locked(&format!("SMBus read register 0x{register:02x}"), |device| {
            device.smbus_read_byte_data(register)
        })
    }
    fn write_block_data(&mut self, command: u8, data: &[u8]) -> Result<()> {
        if data.len() > 32 {
            bail!("SMBus block is {} bytes; maximum is 32", data.len());
        }
        self.locked(
            &format!(
                "SMBus write block command 0x{command:02x}, {} bytes",
                data.len()
            ),
            |device| device.smbus_write_i2c_block_data(command, data),
        )
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeI2cDevice {
    registers: BTreeMap<u8, u8>,
    pointer: u8,
    pub byte_writes: Vec<u8>,
    pub block_writes: Vec<(u8, Vec<u8>)>,
}

impl FakeI2cDevice {
    pub fn with_register(mut self, register: u8, value: u8) -> Self {
        self.registers.insert(register, value);
        self
    }
    pub fn register(&self, register: u8) -> u8 {
        *self.registers.get(&register).unwrap_or(&0)
    }
}

impl I2cDevice for FakeI2cDevice {
    fn write_byte(&mut self, value: u8) -> Result<()> {
        self.byte_writes.push(value);
        self.pointer = value;
        Ok(())
    }
    fn read_byte(&mut self) -> Result<u8> {
        let value = self.register(self.pointer);
        self.pointer = self.pointer.wrapping_add(1);
        Ok(value)
    }
    fn write_byte_data(&mut self, register: u8, value: u8) -> Result<()> {
        self.registers.insert(register, value);
        Ok(())
    }
    fn read_byte_data(&mut self, register: u8) -> Result<u8> {
        Ok(self.register(register))
    }
    fn write_block_data(&mut self, command: u8, data: &[u8]) -> Result<()> {
        self.block_writes.push((command, data.to_vec()));
        Ok(())
    }
}

pub struct Mcu<D> {
    device: D,
}

impl<D: I2cDevice> Mcu<D> {
    pub fn new(device: D) -> Self {
        Self { device }
    }
    pub fn set_fan(&mut self, percent: u8) -> Result<()> {
        if percent > 100 {
            bail!("fan percentage {percent} is outside 0..=100");
        }
        self.device
            .write_byte(percent)
            .with_context(|| format!("set Argon MCU fan to {percent}%"))
    }
    pub fn request_power_cut(&mut self) -> Result<()> {
        self.device
            .write_byte(0xff)
            .context("send Argon MCU power-cut command 0xff")
    }
    pub fn enter_firmware_update(&mut self) -> Result<()> {
        self.device
            .write_byte(0xbb)
            .context("send Argon MCU firmware-update command 0xbb")
    }
    pub fn program_ir_power_code(&mut self, code: [u8; 4]) -> Result<()> {
        self.device
            .write_block_data(0xaa, &code)
            .context("program Argon MCU IR power code command 0xaa")
    }
    pub fn into_inner(self) -> D {
        self.device
    }
}

impl<D: I2cDevice> crate::fan::FanOutput for Mcu<D> {
    type Error = anyhow::Error;
    fn set_percent(&mut self, percent: u8) -> Result<()> {
        self.set_fan(percent)
    }
}

pub struct OledDevice<D> {
    device: D,
}

impl<D: I2cDevice> OledDevice<D> {
    pub fn new(device: D) -> Self {
        Self { device }
    }
    fn command(&mut self, command: u8) -> Result<()> {
        self.device.write_byte_data(0, command)
    }
    pub fn power(&mut self, enabled: bool) -> Result<()> {
        self.command(if enabled { 0xaf } else { 0xae })
            .context("set OLED power")
    }
    pub fn inverse(&mut self, enabled: bool) -> Result<()> {
        self.command(if enabled { 0xa7 } else { 0xa6 })
            .context("set OLED inverse mode")
    }
    pub fn all_white(&mut self, enabled: bool) -> Result<()> {
        self.command(if enabled { 0xa5 } else { 0xa4 })
            .context("set OLED all-white mode")
    }
    pub fn reset_addressing(&mut self) -> Result<()> {
        for command in [
            0x20,
            0x01,
            0x21,
            0,
            (WIDTH - 1) as u8,
            0x22,
            0,
            (HEIGHT / 8 - 1) as u8,
            0x20,
            0x02,
            0xb0,
            0x40,
        ] {
            self.command(command)?;
        }
        Ok(())
    }
    pub fn flush_block(&mut self, framebuffer: &Framebuffer, x: usize, y: usize) -> Result<()> {
        if x >= WIDTH || y >= HEIGHT || x % 32 != 0 || y % 8 != 0 {
            bail!("invalid OLED block origin ({x},{y})");
        }
        let page = y / 8;
        for command in [
            0x20,
            0x01,
            0x21,
            x as u8,
            (x + 31) as u8,
            0x22,
            page as u8,
            page as u8,
            0x40,
        ] {
            self.command(command)?;
        }
        let offset = WIDTH * page + x;
        self.device
            .write_block_data(0x6a, &framebuffer.bytes()[offset..offset + 32])
            .with_context(|| format!("transfer OLED framebuffer block ({x},{y})"))
    }
    pub fn flush(&mut self, framebuffer: &Framebuffer, hide_during_transfer: bool) -> Result<()> {
        if hide_during_transfer {
            self.power(false)?;
        }
        for x in (0..WIDTH).step_by(32) {
            for y in (0..HEIGHT).step_by(8) {
                self.flush_block(framebuffer, x, y)?;
            }
        }
        if hide_during_transfer {
            self.power(true)?;
        }
        Ok(())
    }
    pub fn into_inner(self) -> D {
        self.device
    }
}

pub fn open_devices(
    bus: &Path,
) -> Result<(
    Mcu<LinuxDevice>,
    OledDevice<LinuxDevice>,
    crate::rtc::Pcf8563<LinuxDevice>,
)> {
    let lock = Arc::new(Mutex::new(()));
    Ok((
        Mcu::new(LinuxDevice::open(bus, MCU_ADDRESS, Arc::clone(&lock))?),
        OledDevice::new(LinuxDevice::open(bus, OLED_ADDRESS, Arc::clone(&lock))?),
        crate::rtc::Pcf8563::new(LinuxDevice::open(bus, crate::RTC_ADDRESS, lock)?),
    ))
}

#[derive(Debug, Clone, Copy)]
pub struct GpioPulse {
    pub rising_ns: u64,
    pub falling_ns: u64,
}

impl GpioPulse {
    pub fn duration(&self) -> std::time::Duration {
        std::time::Duration::from_nanos(self.falling_ns.saturating_sub(self.rising_ns))
    }
}

pub struct GpioEdgeSource {
    events: Request,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpioEdge {
    pub rising: bool,
    pub timestamp_ns: u64,
}

impl GpioEdgeSource {
    pub fn open(chip_path: impl Into<PathBuf>, line_offset: u32, consumer: &str) -> Result<Self> {
        let path = chip_path.into();
        let events = Request::builder()
            .on_chip(&path)
            .with_line(line_offset)
            .as_input()
            .with_bias(Bias::PullDown)
            .with_edge_detection(EdgeDetection::BothEdges)
            .with_consumer(consumer)
            .request()
            .with_context(|| {
                format!(
                    "request pull-down both-edge events for GPIO line {line_offset} from {}",
                    path.display()
                )
            })?;
        Ok(Self { events })
    }

    pub fn next_edge(&mut self) -> Result<GpioEdge> {
        let event = self
            .events
            .read_edge_event()
            .context("read GPIO edge event")?;
        Ok(GpioEdge {
            rising: event.kind == EdgeKind::Rising,
            timestamp_ns: event.timestamp_ns,
        })
    }
}

pub struct GpioPulseSource {
    edges: GpioEdgeSource,
    rising_ns: Option<u64>,
}

impl GpioPulseSource {
    pub fn open(chip_path: impl Into<PathBuf>, line_offset: u32) -> Result<Self> {
        Ok(Self {
            edges: GpioEdgeSource::open(chip_path, line_offset, "argon40d")?,
            rising_ns: None,
        })
    }

    pub fn next_pulse(&mut self) -> Result<GpioPulse> {
        loop {
            let event = self.edges.next_edge()?;
            if event.rising {
                self.rising_ns = Some(event.timestamp_ns);
            } else if let Some(rising_ns) = self.rising_ns.take() {
                return Ok(GpioPulse {
                    rising_ns,
                    falling_ns: event.timestamp_ns,
                });
            }
        }
    }
}

pub trait HostPower {
    fn reboot(&self) -> Result<()>;
    fn poweroff(&self) -> Result<()>;
}

pub struct SystemdPower;

impl SystemdPower {
    fn run(action: &str) -> Result<()> {
        let status = Command::new("systemctl")
            .arg(action)
            .status()
            .with_context(|| format!("execute systemctl {action}"))?;
        if !status.success() {
            bail!("systemctl {action} exited with {status}");
        }
        Ok(())
    }
}

impl HostPower for SystemdPower {
    fn reboot(&self) -> Result<()> {
        Self::run("reboot")
    }
    fn poweroff(&self) -> Result<()> {
        Self::run("poweroff")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcu_transactions_match_legacy_smbus_operations() {
        let fake = FakeI2cDevice::default();
        let mut mcu = Mcu::new(fake);
        mcu.set_fan(50).unwrap();
        mcu.request_power_cut().unwrap();
        mcu.program_ir_power_code([0, 0xff, 0x39, 0xc6]).unwrap();
        let fake = mcu.into_inner();
        assert_eq!(fake.byte_writes, vec![50, 0xff]);
        assert_eq!(fake.block_writes, vec![(0xaa, vec![0, 0xff, 0x39, 0xc6])]);
    }

    #[test]
    fn oled_full_flush_is_32_legacy_blocks() {
        let fake = FakeI2cDevice::default();
        let mut oled = OledDevice::new(fake);
        oled.flush(&Framebuffer::new(), false).unwrap();
        let fake = oled.into_inner();
        assert_eq!(fake.block_writes.len(), 32);
        assert!(
            fake.block_writes
                .iter()
                .all(|(command, data)| *command == 0x6a && data.len() == 32)
        );
    }
}
