use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use argon40::{
    MCU_ADDRESS, OLED_ADDRESS, RTC_ADDRESS,
    button::{ButtonAction, classify_pulse},
    config::{
        FAN_CONFIG_PATH, OLED_ASSET_PATH, OLED_CONFIG_PATH, OledConfig, OledPage, RTC_CONFIG_PATH,
        load_fan_curve, load_oled_config,
    },
    fan::{FanCurve, FanDecision, FanPolicy, apply_decision},
    hardware::{GpioPulseSource, HostPower, LinuxDevice, Mcu, OledDevice, SystemdPower},
    oled::{Align, DrawMode, Framebuffer, WIDTH},
    rtc::Pcf8563,
    schedule::{ScheduleCommand, next_startup_schedule, parse_schedule_file},
    system_info,
};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use clap::Parser;
use log::{error, info, warn};
use nix::{
    sys::time::TimeSpec,
    time::{ClockId, clock_settime},
};

#[derive(Debug, Parser)]
#[command(about = "Argon40 fan, button, OLED, and RTC service")]
struct Args {
    #[arg(long, default_value = "/dev/i2c-1")]
    i2c_bus: PathBuf,
    #[arg(long, default_value = "/dev/gpiochip0")]
    gpio_chip: PathBuf,
    #[arg(long, default_value_t = 4)]
    button_line: u32,
    /// Enable reboot/shutdown actions after pulse timing has been validated. Default is diagnostic.
    #[arg(long)]
    button_actions: bool,
    #[arg(long)]
    no_button: bool,
    #[arg(long)]
    no_oled: bool,
    #[arg(long)]
    no_rtc: bool,
    #[arg(long, default_value_t = 30)]
    fan_interval_seconds: u64,
}

fn open_linux_device(bus: &Path, address: u16, lock: &Arc<Mutex<()>>) -> Result<LinuxDevice> {
    LinuxDevice::open(bus, address, Arc::clone(lock))
}

fn fan_loop(mut mcu: Mcu<LinuxDevice>, curve: FanCurve, interval: Duration, stop: Arc<AtomicBool>) {
    let mut policy = FanPolicy::new(0, Duration::from_secs(30));
    while !stop.load(Ordering::Relaxed) {
        let cpu = system_info::cpu_temperature_c();
        match cpu {
            Ok(cpu_temperature) => {
                let temperature = system_info::maximum_drive_temperature_c()
                    .map_or(cpu_temperature, |drive| drive.max(cpu_temperature));
                let desired = curve.speed_for(temperature);
                match policy.evaluate(desired, Instant::now()) {
                    decision @ FanDecision::Apply { percent, .. } => {
                        let result = apply_decision(&mut mcu, decision, thread::sleep);
                        match result {
                            Ok(()) => {
                                policy.commit(percent);
                                info!("fan={} temperature={temperature:.1}C", percent);
                            }
                            Err(error) => error!("fan update failed; will retry: {error:#}"),
                        }
                    }
                    FanDecision::DownshiftPending { percent, remaining } => {
                        info!(
                            "fan downshift to {percent}% pending for {}s",
                            remaining.as_secs()
                        );
                    }
                    FanDecision::Unchanged => {}
                }
            }
            Err(error) => error!(
                "temperature unavailable; preserving fan={}%; {error:#}",
                policy.current_percent()
            ),
        }
        sleep_interruptible(interval, &stop);
    }
    if let Err(error) = mcu.set_fan(0) {
        error!("failed to stop fan while daemon exits: {error:#}");
    }
}

fn sleep_interruptible(duration: Duration, stop: &AtomicBool) {
    let until = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) && Instant::now() < until {
        thread::sleep(
            Duration::from_millis(200).min(until.saturating_duration_since(Instant::now())),
        );
    }
}

fn button_loop(
    mut source: GpioPulseSource,
    diagnostic: bool,
    next_page: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    let power = SystemdPower;
    while !stop.load(Ordering::Relaxed) {
        let pulse = match source.next_pulse() {
            Ok(pulse) => pulse,
            Err(error) => {
                error!("GPIO button event failed: {error:#}");
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let duration = pulse.duration();
        let action = classify_pulse(duration);
        info!(
            "button rising_ns={} falling_ns={} duration_us={} action={action:?} diagnostic={diagnostic}",
            pulse.rising_ns,
            pulse.falling_ns,
            duration.as_micros()
        );
        if diagnostic {
            continue;
        }
        match action {
            ButtonAction::Reboot => {
                if let Err(error) = power.reboot() {
                    error!("button reboot failed: {error:#}");
                }
            }
            ButtonAction::Shutdown => {
                if let Err(error) = power.poweroff() {
                    error!("button shutdown failed: {error:#}");
                }
            }
            ButtonAction::OledNext => next_page.store(true, Ordering::Release),
            ButtonAction::Ignore => {}
        }
    }
}

fn load_font(asset_dir: &Path, width: usize) -> Result<Vec<u8>> {
    let height = (width * 8 / 6).next_multiple_of(8);
    let primary = asset_dir.join(format!("font{height}x{width}.bin"));
    fs::read(&primary)
        .or_else(|_| fs::read(asset_dir.join("font8x6.bin")))
        .with_context(|| format!("load OLED font {} or font8x6.bin", primary.display()))
}

fn render_page(framebuffer: &mut Framebuffer, page: OledPage, assets: &Path) -> Result<()> {
    let background = match page {
        OledPage::Clock => "bgtime",
        OledPage::Cpu => "bgcpu",
        OledPage::Storage => "bgstorage",
        OledPage::Raid => "bgraid",
        OledPage::Ram => "bgram",
        OledPage::Temperature => "bgtemp",
        OledPage::Ip => "bgip",
    };
    framebuffer
        .load_background(&assets.join(format!("{background}.bin")))
        .unwrap_or_else(|_| framebuffer.clear(false));
    let small = load_font(assets, 6)?;
    let regular = load_font(assets, 8).unwrap_or_else(|_| small.clone());
    let left = 54;
    match page {
        OledPage::Clock => {
            let now = Local::now();
            framebuffer.write_text_aligned(
                &now.format("%b%d").to_string().to_ascii_uppercase(),
                left,
                8,
                WIDTH - left as usize,
                Align::Center,
                8,
                &regular,
                DrawMode::Replace,
            )?;
            framebuffer.write_text_aligned(
                &now.format("%a").to_string(),
                left,
                24,
                WIDTH - left as usize,
                Align::Center,
                8,
                &regular,
                DrawMode::Replace,
            )?;
            framebuffer.write_text_aligned(
                &now.format("%H:%M").to_string(),
                left,
                40,
                WIDTH - left as usize,
                Align::Center,
                8,
                &regular,
                DrawMode::Replace,
            )?;
        }
        OledPage::Cpu => {
            for (row, (name, usage)) in system_info::sample_cpu_usage(Duration::from_secs(1))?
                .into_iter()
                .take(4)
                .enumerate()
            {
                framebuffer.write_text(
                    &format!("{name}: {usage}%"),
                    left,
                    row * 16,
                    6,
                    &small,
                    DrawMode::Replace,
                )?;
                framebuffer.filled_rectangle(
                    left as usize,
                    row * 16 + 12,
                    (WIDTH - left as usize - 4) * usage as usize / 100,
                    2,
                    DrawMode::Replace,
                );
            }
        }
        OledPage::Storage => {
            for (row, disk) in system_info::mounted_filesystems()?
                .into_iter()
                .take(3)
                .enumerate()
            {
                let name = disk.source.rsplit('/').next().unwrap_or(&disk.source);
                let usage = (disk.used_kib * 100)
                    .checked_div(disk.total_kib)
                    .unwrap_or(0);
                framebuffer.write_text(
                    &format!("{:.8}", name),
                    0,
                    16 + row * 16,
                    6,
                    &small,
                    DrawMode::Replace,
                )?;
                framebuffer.write_text_aligned(
                    &format!("{usage}%"),
                    50,
                    16 + row * 16,
                    24,
                    Align::Right,
                    6,
                    &small,
                    DrawMode::Replace,
                )?;
                framebuffer.write_text_aligned(
                    &system_info::human_kib(disk.total_kib),
                    77,
                    16 + row * 16,
                    51,
                    Align::Right,
                    6,
                    &small,
                    DrawMode::Replace,
                )?;
            }
        }
        OledPage::Raid => {
            if let Some(raid) = system_info::raid_arrays().into_iter().next() {
                framebuffer.write_text_aligned(
                    &raid.name,
                    0,
                    0,
                    54,
                    Align::Center,
                    6,
                    &small,
                    DrawMode::Replace,
                )?;
                framebuffer.write_text_aligned(
                    &raid.level,
                    0,
                    8,
                    54,
                    Align::Center,
                    6,
                    &small,
                    DrawMode::Replace,
                )?;
                framebuffer.write_text(
                    &format!("State:{}", raid.state),
                    left,
                    16,
                    6,
                    &small,
                    DrawMode::Replace,
                )?;
                framebuffer.write_text(
                    &format!("Drives:{}", raid.devices),
                    left,
                    32,
                    6,
                    &small,
                    DrawMode::Replace,
                )?;
                framebuffer.write_text(
                    &format!("Failed:{}", raid.degraded),
                    left,
                    48,
                    6,
                    &small,
                    DrawMode::Replace,
                )?;
            }
        }
        OledPage::Ram => {
            let memory = system_info::memory_info()?;
            let available = memory.available_kib * 100 / memory.total_kib.max(1);
            framebuffer.write_text_aligned(
                &format!("{available}%"),
                left,
                8,
                WIDTH - left as usize,
                Align::Center,
                8,
                &regular,
                DrawMode::Replace,
            )?;
            framebuffer.write_text_aligned(
                "of",
                left,
                24,
                WIDTH - left as usize,
                Align::Center,
                8,
                &regular,
                DrawMode::Replace,
            )?;
            framebuffer.write_text_aligned(
                &system_info::human_kib(memory.total_kib),
                left,
                40,
                WIDTH - left as usize,
                Align::Center,
                8,
                &regular,
                DrawMode::Replace,
            )?;
        }
        OledPage::Temperature => {
            let celsius = system_info::cpu_temperature_c()?;
            let fahrenheit = 32.0 + celsius * 9.0 / 5.0;
            framebuffer.write_text_aligned(
                &format!("{celsius:.1}C"),
                left,
                16,
                WIDTH - left as usize,
                Align::Center,
                8,
                &regular,
                DrawMode::Replace,
            )?;
            framebuffer.write_text_aligned(
                &format!("{fahrenheit:.1}F"),
                left,
                32,
                WIDTH - left as usize,
                Align::Center,
                8,
                &regular,
                DrawMode::Replace,
            )?;
        }
        OledPage::Ip => framebuffer.write_text_aligned(
            &system_info::local_ip(),
            0,
            8,
            WIDTH,
            Align::Center,
            8,
            &regular,
            DrawMode::Replace,
        )?,
    }
    Ok(())
}

fn oled_loop(
    mut oled: OledDevice<LinuxDevice>,
    config: OledConfig,
    next: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    if !config.enabled || config.pages.is_empty() {
        let _ = oled.power(false);
        return;
    }
    let assets = Path::new(OLED_ASSET_PATH);
    let mut index = 0;
    let mut last_switch = Instant::now();
    let mut last_activity = Instant::now();
    let mut last_render = Instant::now();
    let mut render_needed = true;
    let mut sleeping = false;
    while !stop.load(Ordering::Relaxed) {
        let requested = next.swap(false, Ordering::AcqRel);
        let automatic = config.switch_seconds > 0
            && last_switch.elapsed() >= Duration::from_secs(config.switch_seconds);
        if requested || automatic {
            index = (index + 1) % config.pages.len();
            last_switch = Instant::now();
            last_activity = Instant::now();
            sleeping = false;
            render_needed = true;
        }
        if config
            .screensaver_seconds
            .is_some_and(|seconds| last_activity.elapsed() >= Duration::from_secs(seconds))
        {
            if !sleeping {
                let _ = oled.power(false);
                sleeping = true;
            }
        } else if !sleeping && (render_needed || last_render.elapsed() >= Duration::from_secs(60)) {
            let mut framebuffer = Framebuffer::new();
            match render_page(&mut framebuffer, config.pages[index], assets)
                .and_then(|()| oled.flush(&framebuffer, true))
                .and_then(|()| oled.reset_addressing())
            {
                Ok(()) => {}
                Err(error) => error!("OLED page {:?} failed: {error:#}", config.pages[index]),
            }
            // Retry transient failures on the normal one-minute refresh rather than flooding the
            // journal and I2C bus every second.
            render_needed = false;
            last_render = Instant::now();
        }
        sleep_interruptible(Duration::from_secs(1), &stop);
    }
    let mut blank = Framebuffer::new();
    blank.clear(false);
    let _ = oled.flush(&blank, false);
    let _ = oled.power(false);
}

fn set_host_clock(utc: NaiveDateTime) -> Result<()> {
    let seconds = utc.and_utc().timestamp();
    clock_settime(
        ClockId::CLOCK_REALTIME,
        TimeSpec::new(seconds, utc.and_utc().timestamp_subsec_nanos() as i64),
    )
    .context("set host CLOCK_REALTIME from RTC")
}

fn local_to_utc(local: NaiveDateTime) -> Result<NaiveDateTime> {
    let local: DateTime<Local> = Local
        .from_local_datetime(&local)
        .earliest()
        .context("local schedule time is invalid or ambiguous")?;
    Ok(local.with_timezone(&Utc).naive_utc())
}

fn rtc_loop(
    mut rtc: Pcf8563<LinuxDevice>,
    schedules: Vec<argon40::schedule::Schedule>,
    stop: Arc<AtomicBool>,
) {
    match rtc.read_utc().and_then(set_host_clock) {
        Ok(()) => info!("host clock synchronized from RTC"),
        Err(error) => error!("RTC initial time synchronization failed: {error:#}"),
    }
    let mut programmed = None;
    while !stop.load(Ordering::Relaxed) {
        let now = Local::now().naive_local();
        let selected = next_startup_schedule(&schedules, now);
        let next = selected.map(|(_, time)| time);
        if next != programmed {
            let result = match selected {
                Some((schedule, local)) => {
                    let include_day = !schedule.day.is_any();
                    let include_weekday = !schedule.weekday.is_any();
                    local_to_utc(local)
                        .and_then(|utc| rtc.set_alarm_utc(utc, include_day, include_weekday))
                }
                None => rtc.clear_alarm(),
            };
            match result {
                Ok(()) => {
                    info!("RTC next startup alarm={next:?}");
                    programmed = next;
                }
                Err(error) => error!("RTC alarm update failed: {error:#}"),
            }
        }
        if schedules
            .iter()
            .any(|schedule| schedule.command == ScheduleCommand::Off && schedule.matches(now))
        {
            info!("RTC shutdown schedule matched {now}");
            if let Err(error) = SystemdPower.poweroff() {
                error!("scheduled poweroff failed: {error:#}");
            }
            return;
        }
        let _ = rtc
            .clear_event_flags()
            .map_err(|error| warn!("RTC event flag clear failed: {error:#}"));
        sleep_interruptible(Duration::from_secs(60), &stop);
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, Arc::clone(&stop))?;
    }
    let next_page = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGUSR1, Arc::clone(&next_page))?;
    let bus_lock = Arc::new(Mutex::new(()));

    let curve = match load_fan_curve(Path::new(FAN_CONFIG_PATH)) {
        Ok(curve) => curve,
        Err(error) => {
            warn!(
                "using built-in fan curve because legacy config is unavailable/invalid: {error:#}"
            );
            FanCurve::fallback()
        }
    };
    let mcu = Mcu::new(open_linux_device(&args.i2c_bus, MCU_ADDRESS, &bus_lock)?);
    let mut threads = vec![thread::spawn({
        let stop = Arc::clone(&stop);
        move || {
            fan_loop(
                mcu,
                curve,
                Duration::from_secs(args.fan_interval_seconds),
                stop,
            )
        }
    })];

    if !args.no_button {
        match GpioPulseSource::open(&args.gpio_chip, args.button_line) {
            Ok(source) => threads.push(thread::spawn({
                let stop = Arc::clone(&stop);
                let next = Arc::clone(&next_page);
                move || button_loop(source, !args.button_actions, next, stop)
            })),
            Err(error) => error!("button input disabled after initialization failure: {error:#}"),
        }
    }
    if !args.no_oled {
        match load_oled_config(Path::new(OLED_CONFIG_PATH))
            .or_else(|error| {
                warn!("using default OLED config: {error:#}");
                Ok::<_, anyhow::Error>(OledConfig::default())
            })
            .and_then(|config| {
                open_linux_device(&args.i2c_bus, OLED_ADDRESS, &bus_lock)
                    .map(|device| (config, device))
            }) {
            Ok((config, device)) => threads.push(thread::spawn({
                let stop = Arc::clone(&stop);
                let next = Arc::clone(&next_page);
                move || oled_loop(OledDevice::new(device), config, next, stop)
            })),
            Err(error) => warn!("optional OLED disabled: {error:#}"),
        }
    }
    if !args.no_rtc {
        let schedules = fs::read_to_string(RTC_CONFIG_PATH)
            .context("read RTC schedules")
            .and_then(|data| parse_schedule_file(&data))
            .unwrap_or_else(|error| {
                warn!("RTC schedule list empty: {error:#}");
                Vec::new()
            });
        match open_linux_device(&args.i2c_bus, RTC_ADDRESS, &bus_lock) {
            Ok(device) => threads.push(thread::spawn({
                let stop = Arc::clone(&stop);
                move || rtc_loop(Pcf8563::new(device), schedules, stop)
            })),
            Err(error) => warn!("optional RTC disabled: {error:#}"),
        }
    }
    info!("argon40d started");
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
    }
    info!("argon40d stopping");
    // GPIO event reads block in the kernel. Cooperative tasks get a cleanup window; process exit
    // then terminates any edge reader which is still waiting for a physical transition.
    thread::sleep(Duration::from_secs(2));
    drop(threads);
    Ok(())
}
