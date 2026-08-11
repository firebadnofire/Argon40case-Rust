use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use argon40::{
    MCU_ADDRESS, OLED_ADDRESS, RTC_ADDRESS,
    config::{
        FAN_CONFIG_PATH, OLED_CONFIG_PATH, OledConfig, OledPage, RTC_CONFIG_PATH, load_fan_curve,
        load_oled_config, write_atomic,
    },
    fan::FanCurve,
    firmware,
    hardware::{GpioEdgeSource, LinuxDevice, Mcu, OledDevice},
    ir::{DEFAULT_REMOTE, Pulse, decode_nec, lirc_config, parse_hex_code},
    oled::Framebuffer,
    rtc::Pcf8563,
    schedule::{Schedule, parse_schedule_file, serialize_schedule_file},
    system_info,
};
use chrono::{Local, TimeZone, Utc};
use clap::{Args, Parser, Subcommand};
use nix::{
    sys::time::TimeSpec,
    time::{ClockId, clock_settime},
};

const DAEMON_UNIT: &str = include_str!("../../systemd/argon40d.service");
const OLED_ASSETS: &[(&str, &[u8])] = &[
    (
        "font8x6.bin",
        include_bytes!("../../assets/oled/font8x6.bin"),
    ),
    (
        "font16x12.bin",
        include_bytes!("../../assets/oled/font16x12.bin"),
    ),
    (
        "font32x24.bin",
        include_bytes!("../../assets/oled/font32x24.bin"),
    ),
    (
        "font64x48.bin",
        include_bytes!("../../assets/oled/font64x48.bin"),
    ),
    (
        "font16x8.bin",
        include_bytes!("../../assets/oled/font16x8.bin"),
    ),
    (
        "font24x16.bin",
        include_bytes!("../../assets/oled/font24x16.bin"),
    ),
    (
        "font48x32.bin",
        include_bytes!("../../assets/oled/font48x32.bin"),
    ),
    (
        "bgdefault.bin",
        include_bytes!("../../assets/oled/bgdefault.bin"),
    ),
    ("bgram.bin", include_bytes!("../../assets/oled/bgram.bin")),
    ("bgip.bin", include_bytes!("../../assets/oled/bgip.bin")),
    ("bgtemp.bin", include_bytes!("../../assets/oled/bgtemp.bin")),
    ("bgcpu.bin", include_bytes!("../../assets/oled/bgcpu.bin")),
    ("bgraid.bin", include_bytes!("../../assets/oled/bgraid.bin")),
    (
        "bgstorage.bin",
        include_bytes!("../../assets/oled/bgstorage.bin"),
    ),
    ("bgtime.bin", include_bytes!("../../assets/oled/bgtime.bin")),
];

#[derive(Debug, Parser)]
#[command(about = "Administer Argon40 cases and HATs", version)]
struct Cli {
    #[arg(long, global = true, default_value = "/dev/i2c-1")]
    i2c_bus: PathBuf,
    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Debug, Subcommand)]
enum TopCommand {
    Status,
    Fan {
        #[command(subcommand)]
        command: FanCommand,
    },
    Oled {
        #[command(subcommand)]
        command: OledCommand,
    },
    Rtc {
        #[command(subcommand)]
        command: RtcCommand,
    },
    Ir {
        #[command(subcommand)]
        command: IrCommand,
    },
    Firmware {
        #[command(subcommand)]
        command: FirmwareCommand,
    },
    PowerCut(Confirm),
    Install(InstallArgs),
    Uninstall(Confirm),
}

#[derive(Debug, Subcommand)]
enum FanCommand {
    Status,
    Set {
        percent: u8,
    },
    Off,
    Config {
        #[command(subcommand)]
        command: FanConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FanConfigCommand {
    Show,
    Set {
        #[arg(required = true, value_name = "TEMP=SPEED")]
        pairs: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
enum OledCommand {
    On,
    Off,
    Clear,
    Fill,
    Next,
    Config {
        #[command(subcommand)]
        command: OledConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
enum OledConfigCommand {
    Show,
    Set {
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long)]
        switch_seconds: Option<u64>,
        #[arg(long)]
        screensaver_seconds: Option<u64>,
        #[arg(long, value_delimiter = ',')]
        pages: Option<Vec<String>>,
    },
}

#[derive(Debug, Subcommand)]
enum RtcCommand {
    Time,
    SyncFromRtc,
    SyncToRtc,
    Clear,
    Timer {
        #[command(subcommand)]
        command: TimerCommand,
    },
    Schedules,
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TimerCommand {
    Set {
        value: u8,
        #[arg(long)]
        seconds: bool,
    },
    Clear,
}

#[derive(Debug, Subcommand)]
enum ScheduleCommand {
    Add {
        #[arg(num_args = 6, value_name = "FIELD")]
        fields: Vec<String>,
    },
    Remove {
        index: usize,
    },
}

#[derive(Debug, Subcommand)]
enum IrCommand {
    DiagnoseNec {
        #[arg(long, default_value = "/dev/gpiochip0")]
        gpio_chip: PathBuf,
        #[arg(long, default_value_t = 23)]
        line: u32,
    },
    LearnPower {
        #[arg(long, default_value = "/dev/gpiochip0")]
        gpio_chip: PathBuf,
        #[arg(long, default_value_t = 23)]
        line: u32,
        #[arg(long)]
        program: bool,
        #[command(flatten)]
        confirm: Confirm,
    },
    ProgramPower {
        code: String,
        #[command(flatten)]
        confirm: Confirm,
    },
    WriteDefaultLirc {
        #[arg(long, default_value = "/etc/lirc/lircd.conf.d/argon.lircd.conf")]
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum FirmwareCommand {
    Inspect {
        image: PathBuf,
        #[arg(long)]
        dump_dir: Option<PathBuf>,
    },
    EnterBootloader(Confirm),
    Flash {
        image: PathBuf,
        #[arg(long, default_value = "/dev/serial0")]
        serial: PathBuf,
        #[arg(long, default_value_t = 2)]
        retries: u8,
        #[command(flatten)]
        confirm: Confirm,
    },
}

#[derive(Debug, Args)]
struct Confirm {
    #[arg(long, help = "Confirm the destructive operation")]
    yes: bool,
}

#[derive(Debug, Args)]
struct InstallArgs {
    #[arg(long, default_value = "/")]
    root: PathBuf,
    #[arg(long)]
    no_enable: bool,
}

fn confirm(flag: bool, action: &str) -> Result<()> {
    if !flag {
        bail!(
            "refusing to {action}; repeat with --yes after checking hardware and recovery access"
        );
    }
    Ok(())
}

fn device(bus: &Path, address: u16) -> Result<LinuxDevice> {
    LinuxDevice::open(bus, address, Arc::new(Mutex::new(())))
}

fn mcu(bus: &Path) -> Result<Mcu<LinuxDevice>> {
    Ok(Mcu::new(device(bus, MCU_ADDRESS)?))
}
fn oled(bus: &Path) -> Result<OledDevice<LinuxDevice>> {
    Ok(OledDevice::new(device(bus, OLED_ADDRESS)?))
}
fn rtc(bus: &Path) -> Result<Pcf8563<LinuxDevice>> {
    Ok(Pcf8563::new(device(bus, RTC_ADDRESS)?))
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .with_context(|| format!("execute systemctl {}", args.join(" ")))?;
    if !status.success() {
        bail!("systemctl {} exited with {status}", args.join(" "));
    }
    Ok(())
}

fn set_host_clock(utc: chrono::NaiveDateTime) -> Result<()> {
    clock_settime(
        ClockId::CLOCK_REALTIME,
        TimeSpec::new(
            utc.and_utc().timestamp(),
            utc.and_utc().timestamp_subsec_nanos() as i64,
        ),
    )
    .context("set host CLOCK_REALTIME")
}

fn read_schedules() -> Result<Vec<Schedule>> {
    let input =
        fs::read_to_string(RTC_CONFIG_PATH).with_context(|| format!("read {RTC_CONFIG_PATH}"))?;
    parse_schedule_file(&input)
}

fn prefixed(root: &Path, absolute: &str) -> PathBuf {
    root.join(absolute.trim_start_matches('/'))
}

fn install(args: InstallArgs) -> Result<()> {
    let current = std::env::current_exe().context("locate argon40ctl executable")?;
    let directory = current
        .parent()
        .context("argon40ctl executable has no parent")?;
    let destinations = [
        (directory.join("argon40d"), "/usr/local/sbin/argon40d"),
        (directory.join("argon40ctl"), "/usr/local/bin/argon40ctl"),
        (
            directory.join("argon40-shutdown"),
            "/usr/local/libexec/argon40-shutdown",
        ),
    ];
    for (source, destination) in destinations {
        if !source.is_file() {
            bail!(
                "required sibling binary {} is missing; run cargo build --release first",
                source.display()
            );
        }
        let destination = prefixed(&args.root, destination);
        fs::create_dir_all(destination.parent().unwrap())?;
        fs::copy(&source, &destination)
            .with_context(|| format!("install {}", destination.display()))?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))?;
    }
    let unit = prefixed(&args.root, "/etc/systemd/system/argon40d.service");
    fs::create_dir_all(unit.parent().unwrap())?;
    fs::write(&unit, DAEMON_UNIT)?;
    let hook = prefixed(
        &args.root,
        "/usr/lib/systemd/system-shutdown/argon40-shutdown",
    );
    fs::create_dir_all(hook.parent().unwrap())?;
    fs::copy(
        prefixed(&args.root, "/usr/local/libexec/argon40-shutdown"),
        &hook,
    )?;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))?;
    let fan_config = prefixed(&args.root, FAN_CONFIG_PATH);
    if !fan_config.exists() {
        fs::write(
            &fan_config,
            "# temperature C = fan percent\n55=10\n60=55\n65=100\n",
        )?;
    }
    let oled_config = prefixed(&args.root, OLED_CONFIG_PATH);
    if !oled_config.exists() {
        fs::write(&oled_config, OledConfig::default().serialize())?;
    }
    let rtc_config = prefixed(&args.root, RTC_CONFIG_PATH);
    if !rtc_config.exists() {
        fs::write(&rtc_config, serialize_schedule_file(&[]))?;
    }
    let asset_directory = prefixed(&args.root, "/etc/argon/oled");
    fs::create_dir_all(&asset_directory)?;
    for (name, contents) in OLED_ASSETS {
        fs::write(asset_directory.join(name), contents)?;
    }
    let compatibility = prefixed(&args.root, "/usr/local/bin/argonone-config");
    if !compatibility.exists() {
        symlink("argon40ctl", compatibility)?;
    }
    if args.root == Path::new("/") {
        systemctl(&["daemon-reload"])?;
        if !args.no_enable {
            systemctl(&["enable", "--now", "argon40d.service"])?;
        }
    }
    println!(
        "installed Argon40 Rust binaries and integration under {}",
        args.root.display()
    );
    Ok(())
}

fn uninstall(confirm_flag: bool) -> Result<()> {
    confirm(confirm_flag, "uninstall Argon40")?;
    let _ = systemctl(&["disable", "--now", "argon40d.service"]);
    for path in [
        "/etc/systemd/system/argon40d.service",
        "/usr/lib/systemd/system-shutdown/argon40-shutdown",
        "/usr/local/sbin/argon40d",
        "/usr/local/bin/argon40ctl",
        "/usr/local/bin/argonone-config",
        "/usr/local/libexec/argon40-shutdown",
    ] {
        if let Err(error) = fs::remove_file(path) {
            if error.kind() != io::ErrorKind::NotFound {
                return Err(error).with_context(|| format!("remove {path}"));
            }
        }
    }
    let _ = systemctl(&["daemon-reload"]);
    println!(
        "removed binaries and units; legacy-compatible configuration was preserved under /etc"
    );
    Ok(())
}

fn capture_nec(source: &mut GpioEdgeSource) -> Result<([u8; 4], Vec<Pulse>)> {
    let mut previous = loop {
        let edge = source.next_edge()?;
        if edge.rising {
            break edge;
        }
    };
    let mut pulses = Vec::with_capacity(66);
    while pulses.len() < 66 {
        let edge = source.next_edge()?;
        let duration_us = edge.timestamp_ns.saturating_sub(previous.timestamp_ns) / 1_000;
        pulses.push(Pulse {
            high: previous.rising,
            duration_us: duration_us.min(u32::MAX as u64) as u32,
        });
        previous = edge;
    }
    let bytes = decode_nec(&pulses)?;
    let code: [u8; 4] = bytes[..4]
        .try_into()
        .context("NEC decoder returned fewer than four bytes")?;
    Ok((code, pulses))
}

fn print_nec(code: [u8; 4], pulses: &[Pulse]) {
    println!(
        "code={:02x}{:02x}{:02x}{:02x} pulse_count={}",
        code[0],
        code[1],
        code[2],
        code[3],
        pulses.len()
    );
    for (index, pulse) in pulses.iter().enumerate() {
        println!(
            "{index:02} level={} duration_us={}",
            if pulse.high { "high" } else { "low" },
            pulse.duration_us
        );
    }
}

fn enter_firmware_bootloader(bus: &Path) -> Result<()> {
    let mut device = mcu(bus)?;
    device.enter_firmware_update()?;
    // Preserve the vendor handshake: repeat at one-second intervals until the MCU disappears from
    // I2C, which is the observable indication that bootloader/UART mode is active.
    for _ in 0..3 {
        thread::sleep(Duration::from_secs(1));
        if device.enter_firmware_update().is_err() {
            println!("MCU left I2C after command 0xbb; firmware-update mode is active");
            return Ok(());
        }
    }
    bail!(
        "MCU still acknowledges I2C command 0xbb after four attempts; refusing to assume bootloader mode"
    )
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        TopCommand::Status => {
            let status = Command::new("systemctl")
                .args(["is-active", "argon40d.service"])
                .output();
            let daemon = status
                .ok()
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .unwrap_or_else(|| "unknown".into());
            println!("daemon: {daemon}");
            println!(
                "cpu-temperature: {}",
                system_info::cpu_temperature_c()
                    .map(|value| format!("{value:.1} C"))
                    .unwrap_or_else(|error| format!("unavailable ({error:#})"))
            );
            println!("ip: {}", system_info::local_ip());
            println!("i2c-bus: {}", cli.i2c_bus.display());
        }
        TopCommand::Fan { command } => match command {
            FanCommand::Status => {
                let curve = load_fan_curve(Path::new(FAN_CONFIG_PATH))
                    .unwrap_or_else(|_| FanCurve::fallback());
                let temperature = system_info::cpu_temperature_c()?;
                println!(
                    "temperature={temperature:.1}C requested={}%, config={FAN_CONFIG_PATH}",
                    curve.speed_for(temperature)
                );
            }
            FanCommand::Set { percent } => mcu(&cli.i2c_bus)?.set_fan(percent)?,
            FanCommand::Off => mcu(&cli.i2c_bus)?.set_fan(0)?,
            FanCommand::Config { command } => match command {
                FanConfigCommand::Show => print!("{}", fs::read_to_string(FAN_CONFIG_PATH)?),
                FanConfigCommand::Set { pairs } => {
                    let text = format!("# Argon40 fan curve\n{}\n", pairs.join("\n"));
                    FanCurve::parse(&text)?;
                    write_atomic(Path::new(FAN_CONFIG_PATH), &text)?;
                    systemctl(&["try-restart", "argon40d.service"])?;
                }
            },
        },
        TopCommand::Oled { command } => match command {
            OledCommand::On => oled(&cli.i2c_bus)?.power(true)?,
            OledCommand::Off => oled(&cli.i2c_bus)?.power(false)?,
            OledCommand::Clear | OledCommand::Fill => {
                let mut framebuffer = Framebuffer::new();
                framebuffer.clear(matches!(command, OledCommand::Fill));
                oled(&cli.i2c_bus)?.flush(&framebuffer, false)?;
            }
            OledCommand::Next => systemctl(&["kill", "-s", "USR1", "argon40d.service"])?,
            OledCommand::Config { command } => match command {
                OledConfigCommand::Show => print!("{}", fs::read_to_string(OLED_CONFIG_PATH)?),
                OledConfigCommand::Set {
                    enabled,
                    switch_seconds,
                    screensaver_seconds,
                    pages,
                } => {
                    let mut config =
                        load_oled_config(Path::new(OLED_CONFIG_PATH)).unwrap_or_default();
                    if let Some(value) = enabled {
                        config.enabled = value;
                    }
                    if let Some(value) = switch_seconds {
                        config.switch_seconds = value;
                    }
                    if let Some(value) = screensaver_seconds {
                        config.screensaver_seconds = (value != 0).then_some(value);
                    }
                    if let Some(names) = pages {
                        config.pages = names
                            .iter()
                            .map(|name| {
                                OledPage::parse(name)
                                    .with_context(|| format!("unknown OLED page {name:?}"))
                            })
                            .collect::<Result<_>>()?;
                    }
                    write_atomic(Path::new(OLED_CONFIG_PATH), &config.serialize())?;
                    systemctl(&["try-restart", "argon40d.service"])?;
                }
            },
        },
        TopCommand::Rtc { command } => match command {
            RtcCommand::Time => {
                let utc = rtc(&cli.i2c_bus)?.read_utc()?;
                let local = Utc.from_utc_datetime(&utc).with_timezone(&Local);
                println!("RTC UTC: {utc}\nRTC local: {local}");
            }
            RtcCommand::SyncFromRtc => set_host_clock(rtc(&cli.i2c_bus)?.read_utc()?)?,
            RtcCommand::SyncToRtc => rtc(&cli.i2c_bus)?.write_utc(Utc::now().naive_utc())?,
            RtcCommand::Clear => {
                let mut device = rtc(&cli.i2c_bus)?;
                device.clear_alarm()?;
                device.clear_timer()?;
                device.clear_event_flags()?;
            }
            RtcCommand::Timer { command } => {
                let mut device = rtc(&cli.i2c_bus)?;
                match command {
                    TimerCommand::Set { value, seconds } => {
                        device.set_timer_interval(value, seconds)?
                    }
                    TimerCommand::Clear => device.clear_timer()?,
                }
            }
            RtcCommand::Schedules => {
                for (index, schedule) in read_schedules()?.iter().enumerate() {
                    println!("{}: {}", index + 1, schedule.render());
                }
            }
            RtcCommand::Schedule { command } => {
                let mut schedules = read_schedules().unwrap_or_default();
                match command {
                    ScheduleCommand::Add { fields } => {
                        schedules.push(Schedule::parse(&fields.join(" "))?)
                    }
                    ScheduleCommand::Remove { index } => {
                        if index == 0 || index > schedules.len() {
                            bail!("schedule index {index} is outside 1..={}", schedules.len());
                        }
                        schedules.remove(index - 1);
                    }
                }
                write_atomic(
                    Path::new(RTC_CONFIG_PATH),
                    &serialize_schedule_file(&schedules),
                )?;
                systemctl(&["try-restart", "argon40d.service"])?;
            }
        },
        TopCommand::Ir { command } => match command {
            IrCommand::DiagnoseNec { gpio_chip, line } => {
                eprintln!(
                    "waiting for one NEC frame; stop argon40d/LIRC first so GPIO {line} is not already claimed"
                );
                let mut source = GpioEdgeSource::open(gpio_chip, line, "argon40ctl-ir")?;
                let (code, pulses) = capture_nec(&mut source)?;
                print_nec(code, &pulses);
            }
            IrCommand::LearnPower {
                gpio_chip,
                line,
                program,
                confirm: approval,
            } => {
                eprintln!("press the same NEC power button three times; stop argon40d/LIRC first");
                let mut source = GpioEdgeSource::open(gpio_chip, line, "argon40ctl-ir")?;
                let mut learned = None;
                for attempt in 1..=3 {
                    let (code, pulses) = capture_nec(&mut source)
                        .with_context(|| format!("decode NEC frame {attempt}"))?;
                    println!(
                        "sample {attempt}: {:02x}{:02x}{:02x}{:02x}",
                        code[0], code[1], code[2], code[3]
                    );
                    if learned.is_some_and(|prior| prior != code) {
                        bail!("NEC samples do not match; no MCU state was changed");
                    }
                    learned = Some(code);
                    if attempt == 1 {
                        print_nec(code, &pulses);
                    }
                }
                let code = learned.expect("three samples captured");
                if program {
                    confirm(approval.yes, "program the learned MCU IR power code")?;
                    mcu(&cli.i2c_bus)?.program_ir_power_code(code)?;
                    println!("programmed MCU power code");
                } else {
                    println!("diagnostic only; repeat with --program --yes to change MCU state");
                }
            }
            IrCommand::ProgramPower {
                code,
                confirm: approval,
            } => {
                confirm(approval.yes, "replace the MCU IR power code")?;
                mcu(&cli.i2c_bus)?.program_ir_power_code(parse_hex_code(&code)?)?;
            }
            IrCommand::WriteDefaultLirc { output } => {
                let entries = DEFAULT_REMOTE
                    .iter()
                    .map(|(name, code)| ((*name).to_owned(), *code))
                    .collect::<Vec<_>>();
                fs::write(&output, lirc_config(&entries))
                    .with_context(|| format!("write LIRC config {}", output.display()))?;
            }
        },
        TopCommand::Firmware { command } => match command {
            FirmwareCommand::Inspect { image, dump_dir } => {
                let data = firmware::load_firmware(&image)?;
                let packets = firmware::build_packets(&data)?;
                println!(
                    "image={} bytes packets={} transport-checksums-only=true",
                    data.len(),
                    packets.len()
                );
                if let Some(directory) = dump_dir {
                    fs::create_dir_all(&directory)?;
                    for packet in packets {
                        fs::write(
                            directory.join(format!("packet-{:05}.bin", packet.id)),
                            packet.bytes,
                        )?;
                    }
                }
            }
            FirmwareCommand::EnterBootloader(approval) => {
                confirm(approval.yes, "put the MCU into firmware-update mode")?;
                enter_firmware_bootloader(&cli.i2c_bus)?;
            }
            FirmwareCommand::Flash {
                image,
                serial,
                retries,
                confirm: approval,
            } => {
                confirm(
                    approval.yes,
                    "transmit firmware (MCU must already be in bootloader mode)",
                )?;
                let data = firmware::load_firmware(&image)?;
                firmware::flash_serial(&serial, &data, retries)?;
            }
        },
        TopCommand::PowerCut(approval) => {
            confirm(approval.yes, "send physical power-cut command 0xff")?;
            mcu(&cli.i2c_bus)?.request_power_cut()?;
        }
        TopCommand::Install(args) => install(args)?,
        TopCommand::Uninstall(approval) => uninstall(approval.yes)?,
    }
    io::stdout().flush()?;
    Ok(())
}
