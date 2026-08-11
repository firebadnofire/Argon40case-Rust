use std::time::{Duration, Instant};

use thiserror::Error;

pub const DEFAULT_CURVE: &str = "55=10\n60=55\n65=100\n";
pub const MIN_RUNNING_PERCENT: u8 = 25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FanPoint {
    pub temperature_c: f32,
    pub percent: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FanCurve {
    points: Vec<FanPoint>,
}

#[derive(Debug, Error, PartialEq)]
pub enum FanConfigError {
    #[error("line {line}: expected temperature=speed")]
    MissingEquals { line: usize },
    #[error("line {line}: invalid temperature {value:?}; expected 0..=100")]
    InvalidTemperature { line: usize, value: String },
    #[error("line {line}: invalid fan percentage {value:?}; expected 0..=100")]
    InvalidPercent { line: usize, value: String },
    #[error("fan curve contains no threshold pairs")]
    Empty,
}

impl FanCurve {
    pub fn parse(input: &str) -> Result<Self, FanConfigError> {
        let mut points = Vec::new();
        for (index, raw) in input.lines().enumerate() {
            let line_number = index + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (temperature, speed) = line
                .split_once('=')
                .ok_or(FanConfigError::MissingEquals { line: line_number })?;
            let temperature_c = temperature.trim().parse::<f32>().map_err(|_| {
                FanConfigError::InvalidTemperature {
                    line: line_number,
                    value: temperature.trim().to_owned(),
                }
            })?;
            if !temperature_c.is_finite() || !(0.0..=100.0).contains(&temperature_c) {
                return Err(FanConfigError::InvalidTemperature {
                    line: line_number,
                    value: temperature.trim().to_owned(),
                });
            }
            let speed_value =
                speed
                    .trim()
                    .parse::<f32>()
                    .map_err(|_| FanConfigError::InvalidPercent {
                        line: line_number,
                        value: speed.trim().to_owned(),
                    })?;
            if !speed_value.is_finite() || !(0.0..=100.0).contains(&speed_value) {
                return Err(FanConfigError::InvalidPercent {
                    line: line_number,
                    value: speed.trim().to_owned(),
                });
            }
            points.push(FanPoint {
                temperature_c,
                percent: speed_value as u8,
            });
        }
        if points.is_empty() {
            return Err(FanConfigError::Empty);
        }
        points.sort_by(|a, b| b.temperature_c.total_cmp(&a.temperature_c));
        Ok(Self { points })
    }

    pub fn fallback() -> Self {
        Self::parse(DEFAULT_CURVE).expect("built-in fan curve must be valid")
    }

    pub fn points(&self) -> &[FanPoint] {
        &self.points
    }

    pub fn speed_for(&self, temperature_c: f32) -> u8 {
        self.points
            .iter()
            .find(|point| temperature_c >= point.temperature_c)
            .map_or(0, |point| match point.percent {
                0 => 0,
                value if value < MIN_RUNNING_PERCENT => MIN_RUNNING_PERCENT,
                value => value,
            })
    }
}

pub trait FanOutput {
    type Error;
    fn set_percent(&mut self, percent: u8) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub struct FanPolicy {
    current_percent: u8,
    pending_downshift: Option<(u8, Instant)>,
    downshift_delay: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanDecision {
    Unchanged,
    Apply { percent: u8, spin_up: bool },
    DownshiftPending { percent: u8, remaining: Duration },
}

pub fn apply_decision<O, S>(
    output: &mut O,
    decision: FanDecision,
    mut sleep: S,
) -> Result<(), O::Error>
where
    O: FanOutput,
    S: FnMut(Duration),
{
    if let FanDecision::Apply { percent, spin_up } = decision {
        if spin_up && percent > 0 {
            output.set_percent(100)?;
            sleep(Duration::from_secs(1));
        }
        output.set_percent(percent)?;
    }
    Ok(())
}

impl FanPolicy {
    pub fn new(initial_percent: u8, downshift_delay: Duration) -> Self {
        Self {
            current_percent: initial_percent,
            pending_downshift: None,
            downshift_delay,
        }
    }

    pub fn current_percent(&self) -> u8 {
        self.current_percent
    }

    pub fn evaluate(&mut self, desired: u8, now: Instant) -> FanDecision {
        if desired == self.current_percent {
            self.pending_downshift = None;
            return FanDecision::Unchanged;
        }
        if desired > self.current_percent {
            let spin_up = self.current_percent == 0;
            self.pending_downshift = None;
            return FanDecision::Apply {
                percent: desired,
                spin_up,
            };
        }
        match self.pending_downshift {
            Some((pending, since)) if pending == desired => {
                let elapsed = now.saturating_duration_since(since);
                if elapsed >= self.downshift_delay {
                    self.pending_downshift = None;
                    FanDecision::Apply {
                        percent: desired,
                        spin_up: false,
                    }
                } else {
                    FanDecision::DownshiftPending {
                        percent: desired,
                        remaining: self.downshift_delay - elapsed,
                    }
                }
            }
            _ => {
                self.pending_downshift = Some((desired, now));
                FanDecision::DownshiftPending {
                    percent: desired,
                    remaining: self.downshift_delay,
                }
            }
        }
    }

    /// Record a hardware value only after its I/O operation succeeds.
    pub fn commit(&mut self, percent: u8) {
        self.current_percent = percent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sorts_and_selects_boundaries() {
        let curve = FanCurve::parse("60=55\n# ignored\n55=10\n65=100\n").unwrap();
        assert_eq!(curve.points()[0].temperature_c, 65.0);
        assert_eq!(curve.speed_for(54.99), 0);
        assert_eq!(curve.speed_for(55.0), 25);
        assert_eq!(curve.speed_for(60.0), 55);
        assert_eq!(curve.speed_for(65.0), 100);
    }

    #[test]
    fn rejects_invalid_and_empty_curves() {
        assert!(matches!(FanCurve::parse(""), Err(FanConfigError::Empty)));
        assert!(matches!(
            FanCurve::parse("101=20"),
            Err(FanConfigError::InvalidTemperature { .. })
        ));
        assert!(matches!(
            FanCurve::parse("50=-1"),
            Err(FanConfigError::InvalidPercent { .. })
        ));
        assert!(matches!(
            FanCurve::parse("bad"),
            Err(FanConfigError::MissingEquals { .. })
        ));
    }

    #[test]
    fn fallback_matches_legacy_behavior() {
        let curve = FanCurve::fallback();
        assert_eq!(curve.speed_for(55.0), 25);
        assert_eq!(curve.speed_for(60.0), 55);
        assert_eq!(curve.speed_for(65.0), 100);
    }

    #[test]
    fn upshift_is_immediate_and_downshift_is_delayed() {
        let start = Instant::now();
        let mut policy = FanPolicy::new(0, Duration::from_secs(30));
        assert_eq!(
            policy.evaluate(55, start),
            FanDecision::Apply {
                percent: 55,
                spin_up: true
            }
        );
        policy.commit(55);
        assert!(matches!(
            policy.evaluate(25, start),
            FanDecision::DownshiftPending { .. }
        ));
        assert!(matches!(
            policy.evaluate(25, start + Duration::from_secs(29)),
            FanDecision::DownshiftPending { .. }
        ));
        assert_eq!(
            policy.evaluate(25, start + Duration::from_secs(30)),
            FanDecision::Apply {
                percent: 25,
                spin_up: false
            }
        );
        policy.commit(25);
        assert_eq!(policy.current_percent(), 25);
    }

    #[test]
    fn failed_write_is_retried_until_committed() {
        let start = Instant::now();
        let mut policy = FanPolicy::new(0, Duration::from_secs(30));
        assert!(matches!(
            policy.evaluate(55, start),
            FanDecision::Apply { .. }
        ));
        assert!(matches!(
            policy.evaluate(55, start),
            FanDecision::Apply { .. }
        ));
        policy.commit(55);
        assert_eq!(policy.evaluate(55, start), FanDecision::Unchanged);
    }

    #[test]
    fn stopped_fan_gets_one_second_kick() {
        #[derive(Default)]
        struct Fake {
            writes: Vec<u8>,
        }
        impl FanOutput for Fake {
            type Error = ();
            fn set_percent(&mut self, percent: u8) -> Result<(), Self::Error> {
                self.writes.push(percent);
                Ok(())
            }
        }
        let mut fake = Fake::default();
        let mut sleeps = Vec::new();
        apply_decision(
            &mut fake,
            FanDecision::Apply {
                percent: 25,
                spin_up: true,
            },
            |duration| sleeps.push(duration),
        )
        .unwrap();
        assert_eq!(fake.writes, vec![100, 25]);
        assert_eq!(sleeps, vec![Duration::from_secs(1)]);
    }
}
