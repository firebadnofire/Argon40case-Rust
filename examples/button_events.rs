use anyhow::Result;
use argon40::{button::classify_pulse, hardware::GpioPulseSource};

fn main() -> Result<()> {
    let mut source = GpioPulseSource::open("/dev/gpiochip0", 4)?;
    loop {
        let pulse = source.next_pulse()?;
        println!(
            "rising_ns={} falling_ns={} duration_us={} action={:?}",
            pulse.rising_ns,
            pulse.falling_ns,
            pulse.duration().as_micros(),
            classify_pulse(pulse.duration())
        );
    }
}
