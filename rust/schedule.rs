use std::fmt;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Duration, NaiveDateTime, Timelike, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleCommand {
    On,
    Off,
}

impl fmt::Display for ScheduleCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::On => "on",
            Self::Off => "off",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    Any,
    Values(Vec<u32>),
}

impl Field {
    fn parse(value: &str, name: &str, minimum: u32, maximum: u32) -> Result<Self> {
        if value == "*" {
            return Ok(Self::Any);
        }
        let mut values = Vec::new();
        for part in value.split(',') {
            let number: u32 = part
                .parse()
                .with_context(|| format!("invalid {name} value {part:?}"))?;
            if !(minimum..=maximum).contains(&number) {
                bail!("{name} value {number} is outside {minimum}..={maximum}");
            }
            if !values.contains(&number) {
                values.push(number);
            }
        }
        if values.is_empty() {
            bail!("{name} field is empty");
        }
        values.sort_unstable();
        Ok(Self::Values(values))
    }

    fn contains(&self, value: u32) -> bool {
        matches!(self, Self::Any) || matches!(self, Self::Values(values) if values.contains(&value))
    }

    fn render(&self) -> String {
        match self {
            Self::Any => "*".to_owned(),
            Self::Values(values) => values
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    pub fn is_any(&self) -> bool {
        matches!(self, Self::Any)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub minute: Field,
    pub hour: Field,
    pub day: Field,
    /// Retained for round-trip compatibility but deliberately ignored, like the legacy daemon.
    pub month_text: String,
    pub weekday: Field,
    pub command: ScheduleCommand,
}

impl Schedule {
    pub fn parse(line: &str) -> Result<Self> {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 6 {
            bail!("schedule requires exactly 6 fields, got {}", fields.len());
        }
        let minute = Field::parse(fields[0], "minute", 0, 59)?;
        if minute == Field::Any {
            bail!("minute '*' is not supported by the legacy schedule format");
        }
        let command = match fields[5].to_ascii_lowercase().as_str() {
            "on" => ScheduleCommand::On,
            "off" => ScheduleCommand::Off,
            other => bail!("unsupported schedule command {other:?}; expected on or off"),
        };
        // Parse month for basic compatibility validation, but preserve the legacy behavior that ignores it.
        let _ = Field::parse(fields[3], "month", 1, 12)?;
        Ok(Self {
            minute,
            hour: Field::parse(fields[1], "hour", 0, 23)?,
            day: Field::parse(fields[2], "day-of-month", 1, 31)?,
            month_text: fields[3].to_owned(),
            weekday: Field::parse(fields[4], "day-of-week", 0, 7)?,
            command,
        })
    }

    pub fn matches(&self, time: NaiveDateTime) -> bool {
        let weekday = match time.weekday() {
            Weekday::Sun => 0,
            value => value.num_days_from_sunday(),
        };
        let weekday_match = match &self.weekday {
            Field::Any => true,
            Field::Values(values) => values
                .iter()
                .any(|value| *value == weekday || (*value == 7 && weekday == 0)),
        };
        self.minute.contains(time.minute())
            && self.hour.contains(time.hour())
            && self.day.contains(time.day())
            && weekday_match
    }

    pub fn next_after(&self, time: NaiveDateTime) -> Option<NaiveDateTime> {
        let mut candidate = time.with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        let limit = time.checked_add_months(chrono::Months::new(12 * 12))?;
        while candidate <= limit {
            if self.matches(candidate) {
                return Some(candidate);
            }
            candidate += Duration::minutes(1);
        }
        None
    }

    pub fn render(&self) -> String {
        format!(
            "{} {} {} {} {} {}",
            self.minute.render(),
            self.hour.render(),
            self.day.render(),
            self.month_text,
            self.weekday.render(),
            self.command
        )
    }
}

pub fn parse_schedule_file(input: &str) -> Result<Vec<Schedule>> {
    input
        .lines()
        .enumerate()
        .filter_map(|(index, raw)| {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                None
            } else {
                Some((index + 1, line))
            }
        })
        .map(|(line_number, line)| {
            Schedule::parse(line).with_context(|| format!("schedule line {line_number}"))
        })
        .collect()
}

pub fn serialize_schedule_file(schedules: &[Schedule]) -> String {
    let mut output = String::from(
        "#\n# Argon RTC Configuration\n# minute hour day-of-month month day-of-week on|off\n# Month is retained but ignored for legacy compatibility.\n#\n",
    );
    for schedule in schedules {
        output.push_str(&schedule.render());
        output.push('\n');
    }
    output
}

pub fn next_startup(schedules: &[Schedule], after: NaiveDateTime) -> Option<NaiveDateTime> {
    schedules
        .iter()
        .filter(|schedule| schedule.command == ScheduleCommand::On)
        .filter_map(|schedule| schedule.next_after(after))
        .min()
}

pub fn next_startup_schedule(
    schedules: &[Schedule],
    after: NaiveDateTime,
) -> Option<(&Schedule, NaiveDateTime)> {
    schedules
        .iter()
        .filter(|schedule| schedule.command == ScheduleCommand::On)
        .filter_map(|schedule| schedule.next_after(after).map(|time| (schedule, time)))
        .min_by_key(|(_, time)| *time)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
    }

    #[test]
    fn parses_csv_wildcards_and_round_trips() {
        let schedule = Schedule::parse("0,30 1,13 * * * off").unwrap();
        assert_eq!(schedule.render(), "0,30 1,13 * * * off");
        assert!(schedule.matches(dt(2026, 8, 11, 13, 30)));
        assert!(!schedule.matches(dt(2026, 8, 11, 13, 31)));
    }

    #[test]
    fn rejects_generic_cron_features_and_any_minute() {
        assert!(Schedule::parse("*/5 * * * * on").is_err());
        assert!(Schedule::parse("* 1 * * * on").is_err());
        assert!(Schedule::parse("0 1 * * * command").is_err());
    }

    #[test]
    fn sunday_accepts_zero_and_seven_correcting_legacy_ui_bug() {
        let sunday = dt(2026, 8, 16, 8, 0);
        assert!(Schedule::parse("0 8 * * 0 on").unwrap().matches(sunday));
        assert!(Schedule::parse("0 8 * * 7 on").unwrap().matches(sunday));
    }

    #[test]
    fn month_is_ignored_for_compatibility() {
        assert!(
            Schedule::parse("0 8 * 1 * on")
                .unwrap()
                .matches(dt(2026, 8, 11, 8, 0))
        );
    }

    #[test]
    fn next_occurrence_handles_month_end_and_leap_year() {
        let monthly = Schedule::parse("0 8 31 * * on").unwrap();
        assert_eq!(
            monthly.next_after(dt(2026, 4, 30, 9, 0)),
            Some(dt(2026, 5, 31, 8, 0))
        );
        let leap = Schedule::parse("0 8 29 2 * on").unwrap();
        // Month is ignored, so this intentionally means the 29th of every month.
        assert_eq!(
            leap.next_after(dt(2027, 2, 28, 9, 0)),
            Some(dt(2027, 3, 29, 8, 0))
        );
    }

    #[test]
    fn next_startup_ignores_shutdown_entries() {
        let schedules = vec![
            Schedule::parse("5 8 * * * off").unwrap(),
            Schedule::parse("10 8 * * * on").unwrap(),
        ];
        assert_eq!(
            next_startup(&schedules, dt(2026, 8, 11, 8, 0)),
            Some(dt(2026, 8, 11, 8, 10))
        );
    }
}
