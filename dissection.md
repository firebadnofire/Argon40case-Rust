# Repository dissection

This document is a compact context map for an agentic code model working on the Rust port.

## What this repository is

`argon40` is one Cargo package containing a library and three binaries. It replaces the former Python daemons and shell configurators. Normal operation requires no Python or shell implementation. All case protocols are implemented in Rust over Linux kernel interfaces.

```text
Cargo.toml / Cargo.lock
install.sh                  Raspberry Pi OS build/install entry point
rust/
├── lib.rs                  Module exports and hardware addresses
├── button.rs               Pure pulse classification
├── config.rs               Strict legacy-compatible config parsers/writers
├── fan.rs                  Fan curve, hysteresis, kick policy, hardware trait
├── firmware.rs             64-byte packet builder, ACK validation, UART transfer
├── hardware.rs             SMBus, GPIO edge events, MCU/OLED adapters, fakes
├── ir.rs                   NEC decoder, code parser, deterministic LIRC output
├── oled.rs                 Owned 128x64 framebuffer and rendering primitives
├── rtc.rs                  PCF8563-style BCD/register driver
├── schedule.rs             Restricted cron-like local-time scheduler
├── system_info.rs          /proc, /sys, mountinfo, statvfs metrics
└── bin/
    ├── argon40d.rs         Unified daemon and OLED page renderer
    ├── argon40ctl.rs       Administration, install/uninstall, diagnostics
    └── argon40-shutdown.rs systemd final-shutdown helper
config/                     Example legacy-compatible configs
assets/oled/                Vendor binary fonts/backgrounds plus provenance
systemd/argon40d.service    Unified service unit
docs/hardware-validation.md Evidence and untested boundaries
```

## Runtime wiring

`argon40d` explicitly constructs hardware at startup. It creates one shared bus mutex and separate addressed I2C file descriptors; every transaction takes the shared mutex.

```text
argon40d
├── fan thread
│   ├── /sys thermal + /sys hwmon drive temperatures
│   ├── FanCurve + FanPolicy
│   └── Mcu<LinuxDevice> -> SMBus byte writes at 0x1a
├── button thread
│   ├── GPIO character-device both-edge events on line 4
│   ├── monotonic nanosecond timestamps -> classify_pulse
│   └── systemctl reboot/poweroff or AtomicBool OLED-next signal
├── OLED thread (optional)
│   ├── strict config + embedded installed assets
│   ├── system_info -> render_page -> Framebuffer
│   └── OledDevice<LinuxDevice> -> SMBus commands/blocks at 0x3c
└── RTC thread (optional)
    ├── Pcf8563<LinuxDevice> at 0x51 -> host CLOCK_REALTIME
    ├── schedule parser/next-startup -> RTC alarm
    └── minute polling of local-time `off` schedules -> poweroff
```

Signals: SIGTERM/SIGINT set a stop flag; SIGUSR1 requests the next OLED page. `argon40ctl oled next` sends SIGUSR1 through systemd.

## Protocol ownership

- `Mcu::set_fan` and `request_power_cut` use SMBus write-byte, matching legacy `smbus.write_byte(0x1a, value)`.
- IR programming uses SMBus write-I2C-block-data command `0xaa` with four code bytes.
- Firmware bootloader entry uses SMBus write-byte `0xbb`.
- OLED control uses SMBus byte-data commands; framebuffer chunks use command `0x6a` and 32 data bytes.
- RTC uses SMBus byte-data for registers and write-pointer/read-byte sequencing for its seven time bytes.
- Firmware transport uses 64-byte UART packets, little-endian words, additive checksums, odd transmit IDs, and expected even ACK IDs.

Tests in the owning modules use `FakeI2cDevice` or pure inputs; `cargo test` never opens hardware or invokes host power actions.

## Persistent compatibility contract

| Path | Consumer | Format |
| --- | --- | --- |
| `/etc/argononed.conf` | fan thread/CLI | `temperature=speed` |
| `/etc/argoneonoled.conf` | OLED thread/CLI | strict key/value; no shell sourcing |
| `/etc/argoneonrtc.conf` | RTC thread/CLI | six restricted cron-like fields |
| `/etc/argon/oled/*.bin` | OLED renderer | legacy font/background bytes |
| `/dev/i2c-1` | daemon/helpers | override with `--i2c-bus` |
| `/dev/gpiochip0` | daemon/IR diagnostics | override with `--gpio-chip` |
| `/dev/serial0` | firmware flash | override with `--serial` |

Config is loaded when the service starts. Mutating CLI config commands atomically replace files and `try-restart` the daemon.

## External installer boundary

The two official installer URLs were inspected on 2026-08-11. They were each 822 lines and had evolved beyond this repository's historical source. Relevant assumptions confirmed by them are:

- Runtime code is placed under `/etc/argon`, while the three compatible config files live directly under `/etc`.
- Services and a final-shutdown hook are installed and user commands are exposed through symlinks.
- The installer downloads the same 15 `font*.bin`/`bg*.bin` assets now retained in `assets/oled`.
- The current installer also downloads dashboard, status, EEPROM, UPS, registration, unit-selection, and newer GPIO-mode components that never existed in this checkout.

This port replaces the behavior in the repository named by the task; it does not claim parity with those unrelated, newer, externally hosted components. The Rust installer is self-contained and does not execute or depend on the external installer.

## Safety boundaries

- Button diagnostic mode is the default; only explicit `--button-actions` enables destructive actions.
- Direct fan/OLED/RTC/IR writes should run only after stopping competing services.
- `power-cut`, MCU IR programming, firmware bootloader entry, firmware flash, and uninstall require `--yes`.
- Firmware inspect/packet dump never touches hardware.
- Firmware bootloader entry is separate from UART transfer, ACKs fail closed, and retries are bounded.
- Reboot never sends MCU power-cut byte `0xff`; halt/poweroff does so only in the final shutdown helper.
- Optional OLED/RTC initialization failures are logged without killing fan control; failure to open the MCU is fatal.

## Compatibility differences from legacy

- Sunday 7 works as generated by one old configuration path; legacy matching only recognized 0.
- OLED screensaver zero is disabled rather than blanking instantly.
- NEC `MENU` and `BACK` are distinct instead of accidental `MENUBACK` concatenation.
- Invalid configuration is reported; OLED/RTC config is never executed as shell.
- Firmware checksum/ID mismatches abort instead of continuing by default.
- Metrics avoid localized `df`, `mount`, `mdadm`, and `hddtemp` output.

## Change routing

| Goal | Primary files | Required cross-checks |
| --- | --- | --- |
| Fan behavior | `fan.rs`, daemon fan loop | config parser, MCU byte transaction, thermal safety tests |
| Button behavior | `button.rs`, GPIO layer, daemon | diagnostic logs, reboot versus shutdown helper |
| OLED primitive/page | `oled.rs`, daemon renderer | asset format, exact SMBus block shape, page config names |
| RTC hardware | `rtc.rs` | SMBus semantics, UTC/local conversion, hardware datasheet/source behavior |
| Scheduling | `schedule.rs` | ignored month compatibility, DST, RTC alarm conversion |
| IR | `ir.rs`, CLI capture | GPIO line ownership, three-sample verification, `0xaa` wire form |
| Firmware | `firmware.rs`, CLI | never combine ordinary tests with `0xbb` or flash |
| Install/layout | CLI installer, systemd unit | shutdown-hook location, config preservation, asset provenance |

## Validation commands

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo build --release
```

Host success proves policy, parsing, and constructed transactions only. Consult `docs/hardware-validation.md` before describing any hardware behavior as tested.
