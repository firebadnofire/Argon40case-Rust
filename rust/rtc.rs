use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike, Weekday};

use crate::hardware::I2cDevice;

pub const CONTROL_STATUS_2: u8 = 1;
pub const TIME_START: u8 = 2;
pub const ALARM_MINUTE: u8 = 9;
pub const ALARM_HOUR: u8 = 10;
pub const ALARM_DAY: u8 = 11;
pub const ALARM_WEEKDAY: u8 = 12;
pub const TIMER_CONTROL: u8 = 14;
pub const TIMER_VALUE: u8 = 15;
pub const ALARM_FLAG: u8 = 0x08;
pub const TIMER_FLAG: u8 = 0x04;

pub fn bcd_to_decimal(value: u8) -> u8 {
    (value & 0x0f) + ((value >> 4) & 0x0f) * 10
}

pub fn decimal_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

pub fn decode_time(registers: [u8; 7]) -> Result<NaiveDateTime> {
    let second = bcd_to_decimal(registers[0] & 0x7f) as u32;
    let minute = bcd_to_decimal(registers[1] & 0x7f) as u32;
    let hour = bcd_to_decimal(registers[2] & 0x3f) as u32;
    let day = bcd_to_decimal(registers[3] & 0x3f) as u32;
    let month = bcd_to_decimal(registers[5] & 0x1f) as u32;
    let year = 2000 + bcd_to_decimal(registers[6]) as i32;
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .with_context(|| format!("RTC returned invalid date {year:04}-{month:02}-{day:02}"))?;
    date.and_hms_opt(hour, minute, second)
        .with_context(|| format!("RTC returned invalid time {hour:02}:{minute:02}:{second:02}"))
}

pub fn encode_time(time: NaiveDateTime) -> Result<[u8; 7]> {
    if !(2000..=2099).contains(&time.year()) {
        bail!(
            "RTC year {} is outside supported range 2000..=2099",
            time.year()
        );
    }
    let weekday = match time.weekday() {
        Weekday::Sun => 0,
        day => day.num_days_from_sunday() as u8,
    };
    Ok([
        decimal_to_bcd(time.second() as u8),
        decimal_to_bcd(time.minute() as u8),
        decimal_to_bcd(time.hour() as u8),
        decimal_to_bcd(time.day() as u8),
        decimal_to_bcd(weekday),
        decimal_to_bcd(time.month() as u8),
        decimal_to_bcd((time.year() - 2000) as u8),
    ])
}

pub struct Pcf8563<D> {
    device: D,
}

impl<D: I2cDevice> Pcf8563<D> {
    pub fn new(device: D) -> Self {
        Self { device }
    }

    pub fn read_utc(&mut self) -> Result<NaiveDateTime> {
        let bytes = self
            .device
            .read_sequential(TIME_START, 7)
            .context("read RTC time registers 0x02..0x08")?;
        decode_time(bytes.try_into().expect("requested seven bytes"))
    }

    pub fn write_utc(&mut self, time: NaiveDateTime) -> Result<()> {
        for (offset, value) in encode_time(time)?.into_iter().enumerate() {
            self.device
                .write_byte_data(TIME_START + offset as u8, value)
                .with_context(|| {
                    format!("write RTC register 0x{:02x}", TIME_START + offset as u8)
                })?;
        }
        Ok(())
    }

    pub fn clear_event_flags(&mut self) -> Result<()> {
        let value = self
            .device
            .read_byte_data(CONTROL_STATUS_2)
            .context("read RTC control/status 2")?;
        self.device
            .write_byte_data(CONTROL_STATUS_2, value & !(ALARM_FLAG | TIMER_FLAG))
            .context("clear RTC alarm/timer flags")
    }

    pub fn clear_alarm(&mut self) -> Result<()> {
        for register in ALARM_MINUTE..=ALARM_WEEKDAY {
            self.device
                .write_byte_data(register, 0x80)
                .with_context(|| format!("disable RTC alarm register 0x{register:02x}"))?;
        }
        let value = self.device.read_byte_data(CONTROL_STATUS_2)?;
        self.device
            .write_byte_data(CONTROL_STATUS_2, value & !0x1a)
            .context("disable RTC alarm interrupt and clear flag")
    }

    pub fn clear_timer(&mut self) -> Result<()> {
        let value = self.device.read_byte_data(CONTROL_STATUS_2)?;
        self.device
            .write_byte_data(CONTROL_STATUS_2, value & !0x15)
            .context("disable RTC timer interrupt and clear flag")?;
        self.device.write_byte_data(TIMER_CONTROL, 0x03)?;
        self.device.write_byte_data(TIMER_VALUE, 0x00)?;
        Ok(())
    }

    pub fn set_timer_interval(&mut self, value: u8, seconds: bool) -> Result<()> {
        if value == 0 {
            bail!("RTC timer interval must be 1..=255");
        }
        let status = self.device.read_byte_data(CONTROL_STATUS_2)?;
        self.device
            .write_byte_data(CONTROL_STATUS_2, (status & !0x15) | 0x01)
            .context("enable RTC timer interrupt")?;
        self.device
            .write_byte_data(TIMER_CONTROL, if seconds { 0x82 } else { 0x83 })?;
        // The vendor source BCD-encodes this full u8, including values above 99. Preserve it.
        self.device
            .write_byte_data(TIMER_VALUE, decimal_to_bcd(value))?;
        Ok(())
    }

    pub fn set_alarm_utc(
        &mut self,
        time: NaiveDateTime,
        include_day: bool,
        include_weekday: bool,
    ) -> Result<()> {
        let weekday = match time.weekday() {
            Weekday::Sun => 0,
            day => day.num_days_from_sunday() as u8,
        };
        self.device
            .write_byte_data(ALARM_MINUTE, decimal_to_bcd(time.minute() as u8))?;
        self.device
            .write_byte_data(ALARM_HOUR, decimal_to_bcd(time.hour() as u8))?;
        self.device.write_byte_data(
            ALARM_DAY,
            if include_day {
                decimal_to_bcd(time.day() as u8)
            } else {
                0x80
            },
        )?;
        self.device.write_byte_data(
            ALARM_WEEKDAY,
            if include_weekday {
                decimal_to_bcd(weekday)
            } else {
                0x80
            },
        )?;
        let value = self.device.read_byte_data(CONTROL_STATUS_2)?;
        self.device
            .write_byte_data(CONTROL_STATUS_2, (value & !0x18) | 0x02)
            .context("enable RTC alarm interrupt")
    }

    pub fn into_inner(self) -> D {
        self.device
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::FakeI2cDevice;

    #[test]
    fn bcd_round_trip() {
        for value in 0..=99 {
            assert_eq!(bcd_to_decimal(decimal_to_bcd(value)), value);
        }
    }

    #[test]
    fn rtc_register_round_trip_and_weekday() {
        let time = NaiveDate::from_ymd_opt(2028, 2, 29)
            .unwrap()
            .and_hms_opt(23, 59, 58)
            .unwrap();
        let encoded = encode_time(time).unwrap();
        assert_eq!(encoded, [0x58, 0x59, 0x23, 0x29, 0x02, 0x02, 0x28]);
        assert_eq!(decode_time(encoded).unwrap(), time);
    }

    #[test]
    fn rejects_impossible_register_values() {
        assert!(decode_time([0, 0, 0, 0x31, 0, 0x02, 0x26]).is_err());
    }

    #[test]
    fn alarm_writes_exact_legacy_registers() {
        let fake = FakeI2cDevice::default().with_register(CONTROL_STATUS_2, 0x18);
        let mut rtc = Pcf8563::new(fake);
        let alarm = NaiveDate::from_ymd_opt(2026, 8, 16)
            .unwrap()
            .and_hms_opt(7, 30, 0)
            .unwrap();
        rtc.set_alarm_utc(alarm, true, true).unwrap();
        let fake = rtc.into_inner();
        assert_eq!(fake.register(ALARM_MINUTE), 0x30);
        assert_eq!(fake.register(ALARM_HOUR), 0x07);
        assert_eq!(fake.register(ALARM_DAY), 0x16);
        assert_eq!(fake.register(ALARM_WEEKDAY), 0x00);
        assert_eq!(fake.register(CONTROL_STATUS_2), 0x02);
    }

    #[test]
    fn daily_and_weekly_alarm_masks_match_legacy_registers() {
        let alarm = NaiveDate::from_ymd_opt(2026, 8, 16)
            .unwrap()
            .and_hms_opt(7, 30, 0)
            .unwrap();
        let mut daily = Pcf8563::new(FakeI2cDevice::default());
        daily.set_alarm_utc(alarm, false, false).unwrap();
        let daily = daily.into_inner();
        assert_eq!(daily.register(ALARM_DAY), 0x80);
        assert_eq!(daily.register(ALARM_WEEKDAY), 0x80);

        let mut weekly = Pcf8563::new(FakeI2cDevice::default());
        weekly.set_alarm_utc(alarm, false, true).unwrap();
        let weekly = weekly.into_inner();
        assert_eq!(weekly.register(ALARM_DAY), 0x80);
        assert_eq!(weekly.register(ALARM_WEEKDAY), 0x00);
    }

    #[test]
    fn clearing_alarm_clears_ti_tp_aie_and_flag() {
        let fake = FakeI2cDevice::default().with_register(CONTROL_STATUS_2, 0xff);
        let mut rtc = Pcf8563::new(fake);
        rtc.clear_alarm().unwrap();
        let fake = rtc.into_inner();
        assert_eq!(fake.register(CONTROL_STATUS_2), 0xe5);
        for register in ALARM_MINUTE..=ALARM_WEEKDAY {
            assert_eq!(fake.register(register), 0x80);
        }
    }

    #[test]
    fn timer_transactions_match_legacy() {
        let fake = FakeI2cDevice::default().with_register(CONTROL_STATUS_2, 0xff);
        let mut rtc = Pcf8563::new(fake);
        rtc.set_timer_interval(30, true).unwrap();
        let fake = rtc.into_inner();
        assert_eq!(fake.register(CONTROL_STATUS_2), 0xeb);
        assert_eq!(fake.register(TIMER_CONTROL), 0x82);
        assert_eq!(fake.register(TIMER_VALUE), 0x30);

        let mut rtc = Pcf8563::new(fake);
        rtc.clear_timer().unwrap();
        let fake = rtc.into_inner();
        assert_eq!(fake.register(CONTROL_STATUS_2), 0xea);
        assert_eq!(fake.register(TIMER_CONTROL), 0x03);
        assert_eq!(fake.register(TIMER_VALUE), 0x00);
    }
}
