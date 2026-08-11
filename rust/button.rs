use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    Reboot,
    Shutdown,
    OledNext,
    Ignore,
}

pub fn classify_pulse(duration: Duration) -> ButtonAction {
    let millis = duration.as_millis();
    match millis {
        20..=30 => ButtonAction::Reboot,
        40..=50 => ButtonAction::Shutdown,
        60..=70 => ButtonAction::OledNext,
        _ => ButtonAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_documented_timing_windows() {
        assert_eq!(
            classify_pulse(Duration::from_millis(19)),
            ButtonAction::Ignore
        );
        assert_eq!(
            classify_pulse(Duration::from_millis(20)),
            ButtonAction::Reboot
        );
        assert_eq!(
            classify_pulse(Duration::from_millis(30)),
            ButtonAction::Reboot
        );
        assert_eq!(
            classify_pulse(Duration::from_millis(31)),
            ButtonAction::Ignore
        );
        assert_eq!(
            classify_pulse(Duration::from_millis(40)),
            ButtonAction::Shutdown
        );
        assert_eq!(
            classify_pulse(Duration::from_millis(50)),
            ButtonAction::Shutdown
        );
        assert_eq!(
            classify_pulse(Duration::from_millis(51)),
            ButtonAction::Ignore
        );
        assert_eq!(
            classify_pulse(Duration::from_millis(60)),
            ButtonAction::OledNext
        );
        assert_eq!(
            classify_pulse(Duration::from_millis(70)),
            ButtonAction::OledNext
        );
        assert_eq!(
            classify_pulse(Duration::from_millis(71)),
            ButtonAction::Ignore
        );
    }
}
