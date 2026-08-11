use std::{fs, path::Path};

use anyhow::{Context, Result, bail};

use crate::fan::FanCurve;

pub const FAN_CONFIG_PATH: &str = "/etc/argononed.conf";
pub const OLED_CONFIG_PATH: &str = "/etc/argoneonoled.conf";
pub const RTC_CONFIG_PATH: &str = "/etc/argoneonrtc.conf";
pub const OLED_ASSET_PATH: &str = "/etc/argon/oled";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OledPage {
    Clock,
    Cpu,
    Storage,
    Raid,
    Ram,
    Temperature,
    Ip,
}

impl OledPage {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "clock" => Some(Self::Clock),
            "cpu" => Some(Self::Cpu),
            "storage" => Some(Self::Storage),
            "raid" => Some(Self::Raid),
            "ram" => Some(Self::Ram),
            "temp" => Some(Self::Temperature),
            "ip" => Some(Self::Ip),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clock => "clock",
            Self::Cpu => "cpu",
            Self::Storage => "storage",
            Self::Raid => "raid",
            Self::Ram => "ram",
            Self::Temperature => "temp",
            Self::Ip => "ip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OledConfig {
    pub enabled: bool,
    pub switch_seconds: u64,
    pub screensaver_seconds: Option<u64>,
    pub pages: Vec<OledPage>,
}

impl Default for OledConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            switch_seconds: 0,
            screensaver_seconds: Some(120),
            pages: vec![OledPage::Clock, OledPage::Ip],
        }
    }
}

impl OledConfig {
    pub fn parse(input: &str) -> Result<Self> {
        let mut config = Self::default();
        for (index, raw) in input.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, raw_value) = line
                .split_once('=')
                .with_context(|| format!("OLED config line {} has no '='", index + 1))?;
            let value = raw_value.trim().trim_matches('"');
            match key.trim() {
                "enabled" => match value {
                    "Y" | "y" => config.enabled = true,
                    "N" | "n" => config.enabled = false,
                    _ => bail!("OLED config line {}: enabled must be Y or N", index + 1),
                },
                "switchduration" => {
                    config.switch_seconds = value.parse().with_context(|| {
                        format!("OLED config line {}: invalid switchduration", index + 1)
                    })?;
                }
                "screensaver" => {
                    let seconds: u64 = value.parse().with_context(|| {
                        format!("OLED config line {}: invalid screensaver", index + 1)
                    })?;
                    config.screensaver_seconds = (seconds != 0).then_some(seconds);
                }
                "screenlist" => {
                    let mut pages = Vec::new();
                    for name in value.split_whitespace() {
                        let page = OledPage::parse(name).with_context(|| {
                            format!("OLED config line {}: unknown page {name:?}", index + 1)
                        })?;
                        pages.push(page);
                    }
                    config.pages = pages;
                }
                other => bail!("OLED config line {}: unknown key {other:?}", index + 1),
            }
        }
        Ok(config)
    }

    pub fn serialize(&self) -> String {
        let pages = self
            .pages
            .iter()
            .map(|page| page.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "#\n# Argon OLED Configuration\n#\nenabled={}\nswitchduration={}\nscreensaver={}\nscreenlist=\"{}\"\n",
            if self.enabled { "Y" } else { "N" },
            self.switch_seconds,
            self.screensaver_seconds.unwrap_or(0),
            pages
        )
    }
}

pub fn load_fan_curve(path: &Path) -> Result<FanCurve> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("read fan configuration {}", path.display()))?;
    FanCurve::parse(&data).with_context(|| format!("parse fan configuration {}", path.display()))
}

pub fn load_oled_config(path: &Path) -> Result<OledConfig> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("read OLED configuration {}", path.display()))?;
    OledConfig::parse(&data).with_context(|| format!("parse OLED configuration {}", path.display()))
}

pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().context("configuration path has no parent")?;
    let temporary = parent.join(format!(
        ".{}.argon40.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config")
    ));
    fs::write(&temporary, contents)
        .with_context(|| format!("write temporary configuration {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("replace configuration {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oled_parser_is_data_not_shell() {
        let config = OledConfig::parse(
            "enabled=N\nswitchduration=30\nscreensaver=0\nscreenlist=\"clock cpu temp\"\n",
        )
        .unwrap();
        assert!(!config.enabled);
        assert_eq!(config.switch_seconds, 30);
        assert_eq!(config.screensaver_seconds, None);
        assert_eq!(
            config.pages,
            vec![OledPage::Clock, OledPage::Cpu, OledPage::Temperature]
        );
        assert!(OledConfig::parse("enabled=$(touch /tmp/nope)").is_err());
    }

    #[test]
    fn oled_round_trip() {
        let original = OledConfig::default();
        assert_eq!(OledConfig::parse(&original.serialize()).unwrap(), original);
    }
}
