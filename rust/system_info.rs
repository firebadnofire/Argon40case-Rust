use std::{
    collections::BTreeMap,
    fs,
    net::UdpSocket,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use nix::sys::statvfs::statvfs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuCounters {
    pub total: u64,
    pub idle: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryInfo {
    pub total_kib: u64,
    pub available_kib: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemInfo {
    pub source: String,
    pub mount_point: PathBuf,
    pub total_kib: u64,
    pub used_kib: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaidInfo {
    pub name: String,
    pub level: String,
    pub state: String,
    pub devices: u32,
    pub degraded: u32,
}

pub fn parse_proc_stat(input: &str) -> Result<BTreeMap<String, CpuCounters>> {
    let mut result = BTreeMap::new();
    for line in input.lines().filter(|line| line.starts_with("cpu")) {
        let mut fields = line.split_whitespace();
        let name = fields.next().unwrap_or_default();
        if name == "cpu"
            || !name
                .strip_prefix("cpu")
                .is_some_and(|suffix| suffix.chars().all(|c| c.is_ascii_digit()))
        {
            continue;
        }
        let values = fields
            .map(str::parse::<u64>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("parse /proc/stat row {line:?}"))?;
        if values.len() < 5 {
            continue;
        }
        result.insert(
            name.to_owned(),
            CpuCounters {
                total: values.iter().sum(),
                idle: values[3] + values[4],
            },
        );
    }
    Ok(result)
}

pub fn cpu_usage(
    before: &BTreeMap<String, CpuCounters>,
    after: &BTreeMap<String, CpuCounters>,
) -> Vec<(String, u8)> {
    before
        .iter()
        .filter_map(|(name, first)| {
            let second = after.get(name)?;
            let total = second.total.saturating_sub(first.total);
            let idle = second.idle.saturating_sub(first.idle);
            let usage = ((total.saturating_sub(idle)) * 100)
                .checked_div(total)
                .unwrap_or(0)
                .min(100) as u8;
            Some((name.clone(), usage))
        })
        .collect()
}

pub fn sample_cpu_usage(interval: Duration) -> Result<Vec<(String, u8)>> {
    let before = parse_proc_stat(&fs::read_to_string("/proc/stat").context("read /proc/stat")?)?;
    thread::sleep(interval);
    let after = parse_proc_stat(&fs::read_to_string("/proc/stat").context("read /proc/stat")?)?;
    Ok(cpu_usage(&before, &after))
}

pub fn parse_meminfo(input: &str) -> Result<MemoryInfo> {
    let values = input
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let value = rest.split_whitespace().next()?.parse::<u64>().ok()?;
            Some((name, value))
        })
        .collect::<BTreeMap<_, _>>();
    let total_kib = *values
        .get("MemTotal")
        .context("MemTotal missing from /proc/meminfo")?;
    let available_kib = values.get("MemAvailable").copied().unwrap_or_else(|| {
        values.get("MemFree").copied().unwrap_or(0)
            + values.get("Buffers").copied().unwrap_or(0)
            + values.get("Cached").copied().unwrap_or(0)
    });
    Ok(MemoryInfo {
        total_kib,
        available_kib,
    })
}

pub fn memory_info() -> Result<MemoryInfo> {
    parse_meminfo(&fs::read_to_string("/proc/meminfo").context("read /proc/meminfo")?)
}

pub fn cpu_temperature_c() -> Result<f32> {
    let thermal_root = Path::new("/sys/class/thermal");
    let mut fallback = None;
    for entry in
        fs::read_dir(thermal_root).with_context(|| format!("list {}", thermal_root.display()))?
    {
        let path = entry?.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("thermal_zone"))
        {
            continue;
        }
        let temperature = fs::read_to_string(path.join("temp"))
            .ok()
            .and_then(|value| value.trim().parse::<f32>().ok())
            .map(|value| value / 1000.0);
        let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim().contains("cpu") {
            return temperature.context("CPU thermal zone has invalid temperature");
        }
        fallback = fallback.or(temperature);
    }
    fallback.context("no readable CPU thermal zone found")
}

pub fn maximum_drive_temperature_c() -> Option<f32> {
    let mut maximum: Option<f32> = None;
    for entry in fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let path = entry.path();
        let name = fs::read_to_string(path.join("name")).unwrap_or_default();
        if !name.trim().contains("drivetemp") && !name.trim().contains("nvme") {
            continue;
        }
        let Ok(files) = fs::read_dir(&path) else {
            continue;
        };
        for file in files.flatten() {
            let file_name = file.file_name();
            let file_name = file_name.to_string_lossy();
            if !file_name.starts_with("temp") || !file_name.ends_with("_input") {
                continue;
            }
            if let Ok(value) = fs::read_to_string(file.path())
                .and_then(|value| value.trim().parse::<f32>().map_err(std::io::Error::other))
            {
                let celsius = value / 1000.0;
                maximum = Some(maximum.map_or(celsius, |current| current.max(celsius)));
            }
        }
    }
    maximum
}

pub fn local_ip() -> String {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("254.255.255.255:1")?;
            socket.local_addr()
        })
        .map(|address| address.ip().to_string())
        .unwrap_or_else(|_| "N/A".to_owned())
}

fn unescape_mount(value: &str) -> PathBuf {
    PathBuf::from(
        value
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\134", "\\"),
    )
}

pub fn mounted_filesystems() -> Result<Vec<FilesystemInfo>> {
    let mountinfo =
        fs::read_to_string("/proc/self/mountinfo").context("read /proc/self/mountinfo")?;
    let mut output = Vec::new();
    for line in mountinfo.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let before_fields = before.split_whitespace().collect::<Vec<_>>();
        let after_fields = after.split_whitespace().collect::<Vec<_>>();
        if before_fields.len() < 5
            || after_fields.len() < 2
            || !after_fields[1].starts_with("/dev/")
        {
            continue;
        }
        let mount_point = unescape_mount(before_fields[4]);
        let Ok(stats) = statvfs(&mount_point) else {
            continue;
        };
        let block_size = stats.fragment_size();
        let total_kib = stats.blocks().saturating_mul(block_size) / 1024;
        let available_kib = stats.blocks_available().saturating_mul(block_size) / 1024;
        output.push(FilesystemInfo {
            source: after_fields[1].to_owned(),
            mount_point,
            total_kib,
            used_kib: total_kib.saturating_sub(available_kib),
        });
    }
    Ok(output)
}

pub fn raid_arrays() -> Vec<RaidInfo> {
    let mut arrays = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return arrays;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let md = entry.path().join("md");
        if !md.is_dir() {
            continue;
        }
        let read = |file: &str| {
            fs::read_to_string(md.join(file))
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        arrays.push(RaidInfo {
            name,
            level: read("level"),
            state: read("array_state"),
            devices: read("raid_disks").parse().unwrap_or(0),
            degraded: read("degraded").parse().unwrap_or(0),
        });
    }
    arrays
}

pub fn human_kib(mut value: u64) -> String {
    let suffixes = ["KB", "MB", "GB", "TB", "PB"];
    let mut suffix = 0;
    let mut remainder = 0;
    while value > 1023 && suffix + 1 < suffixes.len() {
        remainder = value & 1023;
        value >>= 10;
        suffix += 1;
    }
    if remainder >= 500 {
        value += 1;
    }
    format!("{value}{}", suffixes[suffix])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_and_calculates_usage() {
        let a = parse_proc_stat("cpu 1 2 3 4 5 6\ncpu0 10 0 10 80 0 0\n").unwrap();
        let b = parse_proc_stat("cpu 1 2 3 4 5 6\ncpu0 20 0 20 160 0 0\n").unwrap();
        assert_eq!(cpu_usage(&a, &b), vec![("cpu0".to_owned(), 20)]);
    }

    #[test]
    fn memavailable_preferred_with_legacy_fallback() {
        assert_eq!(
            parse_meminfo("MemTotal: 1000 kB\nMemAvailable: 400 kB\n")
                .unwrap()
                .available_kib,
            400
        );
        assert_eq!(
            parse_meminfo("MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 20 kB\nCached: 30 kB\n")
                .unwrap()
                .available_kib,
            150
        );
    }

    #[test]
    fn formats_sizes_compatibly() {
        assert_eq!(human_kib(1024), "1MB");
        assert_eq!(human_kib(2047), "2MB");
    }
}
