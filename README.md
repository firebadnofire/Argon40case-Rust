# Argon40 case services in pure Rust

This repository is a pure-Rust replacement for the legacy Argon40 Python and shell scripts. It directly uses Linux GPIO character devices, I2C/SMBus devices, UART, `/proc`, and `/sys`; it does not require Python, libgpiod, WiringPi, a native SMBus library, CMake, or a C/C++ helper.

The project builds three executables:

- `argon40d`: one service owning fan policy, GPIO power-button events, OLED pages, and RTC schedules.
- `argon40ctl`: scriptable configuration, diagnostics, hardware commands, installation, and removal.
- `argon40-shutdown`: the systemd shutdown helper that differentiates reboot from halt/poweroff.

## Support and validation status

The on-wire transactions are ported from the legacy source in this repository:

| Feature | Implemented | Host tested | Argon hardware tested |
| --- | --- | --- | --- |
| Fan curve, minimum speed, kick, delayed downshift | Yes | Yes | Not yet |
| MCU fan writes and power-cut command | Yes | Fake-I2C transaction tests | Not yet |
| GPIO pulse measurement/classification | Yes | Classification tests | Not yet |
| Reboot and shutdown actions | Yes | Not invoked by tests | Not yet |
| 128x64 OLED primitives, assets, transfers, and pages | Yes | Framebuffer/transaction tests | Not yet |
| PCF8563-style RTC time, alarm, and flag operations | Yes | Register tests | Not yet |
| Startup/shutdown schedule engine | Yes | Date-boundary tests | Not yet |
| NEC IR decoding, learning, MCU programming, LIRC generation | Yes | Recorded/generated pulse tests | Not yet |
| Firmware packet protocol | Yes | Packet and strict-ACK tests | Not flashed |

No hardware result is implied by the host tests. See [Hardware validation](docs/hardware-validation.md) for the current inventory and the exact remaining sequence.

The protocol should apply to hardware using the same interfaces as the legacy Argon ONE and Argon EON scripts. Compatibility has not yet been claimed for a particular retail revision because the available Pi was not running the requested Raspberry Pi OS image and exposed no I2C bus during the initial inventory.

## Hardware interfaces

| Device/function | Linux interface | Address/line |
| --- | --- | --- |
| Argon MCU | `/dev/i2c-1` by default | `0x1a` |
| OLED | `/dev/i2c-1` by default | `0x3c`, 128x64 monochrome |
| RTC | `/dev/i2c-1` by default | `0x51` |
| Power button | `/dev/gpiochip0` by default | BCM/line 4 |
| IR receiver | `/dev/gpiochip0` by default | BCM/line 23 |
| Firmware UART | `/dev/serial0` | 115200 baud |

GPIO chip numbering can differ across kernels and Pi models. Inspect the chip labels and pass `--gpio-chip` where necessary. Do not assume a global sysfs GPIO number is a character-device line offset.

## Build and quality checks

Rust 1.85 or newer is required.

```text
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo build --release
```

The application commits `Cargo.lock`. `serialport` is built with default features disabled, preventing its optional `libudev` dependency. The hardware crates issue Linux ioctls directly and do not link libgpiod or a native I2C helper.

To inspect this yourself:

```text
cargo tree --all-features
cargo tree -i libudev-sys
cargo tree -i cc
cargo tree -i cmake
```

The final three commands should report that those packages are absent.

## Raspberry Pi prerequisites

Enable I2C and UART using Raspberry Pi OS configuration before installation. Typical Raspberry Pi OS settings are:

```text
sudo raspi-config nonint do_i2c 0
sudo raspi-config nonint do_serial 2
```

Confirm that the expected devices exist:

```text
ls -l /dev/i2c-* /dev/gpiochip* /dev/serial0
```

The service currently runs as root because it needs GPIO/I2C access, permission to set the host clock, and permission to request reboot/poweroff. This is documented in the systemd unit rather than hidden in an installer.

## Installation

On Raspberry Pi OS on a Raspberry Pi 4B, clone or copy this repository to the Pi and run:

```text
./install.sh
```

Run the script as your normal user. It verifies Rust 1.85 or newer, builds the locked dependency set in release mode, uses `sudo` only for the system install, and enables `argon40d`. It refuses to start a second hardware owner if the legacy `argononed.service` or `argoneond.service` is active. The script does not install Rust or silently alter Raspberry Pi boot configuration; complete the prerequisites above first.

To install without enabling the daemon, use `./install.sh --no-enable`. To exercise the complete install layout without modifying the host, use `./install.sh --root /tmp/argon40-stage`.

The equivalent manual commands are:

```text
cargo build --locked --release
sudo target/release/argon40ctl install
```

`argon40ctl install` installs:

```text
/usr/local/sbin/argon40d
/usr/local/bin/argon40ctl
/usr/local/libexec/argon40-shutdown
/etc/systemd/system/argon40d.service
/usr/lib/systemd/system-shutdown/argon40-shutdown
/etc/argon/oled/*.bin
```

It creates legacy-compatible default config files only when they do not already exist:

```text
/etc/argononed.conf
/etc/argoneonoled.conf
/etc/argoneonrtc.conf
```

Existing configuration is preserved. The installer also creates `/usr/local/bin/argonone-config` as a compatibility symlink to `argon40ctl`. It does not retain Python wrappers.

To stage an installation tree without enabling a service:

```text
target/release/argon40ctl install --root /tmp/argon40-stage --no-enable
```

To remove binaries and units while preserving configuration and OLED assets:

```text
sudo argon40ctl uninstall --yes
```

## Daemon operation

```text
sudo systemctl enable --now argon40d.service
systemctl status argon40d.service
journalctl -u argon40d.service -f
sudo systemctl restart argon40d.service
```

The daemon defaults to `/dev/i2c-1`, `/dev/gpiochip0`, and GPIO line 4. Run `argon40d --help` for overrides and optional-device switches.

For first hardware bring-up, stop the installed service and run button diagnostics in the foreground:

```text
sudo systemctl stop argon40d.service
sudo target/release/argon40d
```

Diagnostic mode is the default: it logs rising/falling monotonic timestamps, pulse duration, and classification without rebooting or shutting down. Only after measurements are correct for the attached case, add `--button-actions` to `ExecStart` in a systemd override and restart the service. The checked-in unit intentionally remains non-destructive until that validation is recorded.

## Fan configuration

`/etc/argononed.conf` keeps the legacy `temperature=speed` format:

```ini
55=10
60=55
65=100
```

Thresholds are sorted descending. A nonzero configured speed below 25% becomes 25%. Downshifts wait 30 seconds. Starting from 0% writes 100%, waits one second, and then writes the requested speed. If the config is missing or invalid, the daemon logs the error and uses the legacy fallback curve shown above.

Commands:

```text
argon40ctl fan status
sudo argon40ctl fan set 50
sudo argon40ctl fan off
argon40ctl fan config show
sudo argon40ctl fan config set 55=10 60=55 65=100
```

Stop `argon40d` before direct fan writes so two owners do not fight over the MCU. Restore a safe cooling speed after manual tests.

## Power button and shutdown

The physical button does not send Linux a literal "single click" or "double click." The case MCU recognizes a gesture and reports it as a coded pulse on GPIO line 4. These classifier values are compiled-in defaults; the three legacy config files do not remap them.

| Physical gesture while the Pi is running | GPIO pulse reported by the MCU | Configured Rust action |
| --- | --- | --- |
| Single/short tap on Argon ONE | No software pulse | No software action |
| Short OLED/page button gesture on Argon EON | 60-70 ms | Advance to the next enabled OLED page |
| Double tap | 20-30 ms | Request an orderly reboot with `systemctl reboot` |
| Hold for 3-5 seconds, then release | 40-50 ms | Request an orderly shutdown with `systemctl poweroff` |
| Any other pulse width | Anything outside the ranges above | Log and ignore |

The 20–70 ms figures are the MCU's output pulse lengths measured by the daemon, not how long the user should press the button. Button behavior can vary between Argon models and MCU firmware. In particular, do not interpret an Argon ONE single tap as the EON page-switch action. Holding beyond roughly 5 seconds can reach the case firmware's forced power-cut path; that is a hardware action outside this software mapping and may not permit an orderly shutdown.

The checked-in service starts in **diagnostic mode**, so a fresh installation only logs the rising edge, falling edge, measured pulse, and classified action. It does not reboot, shut down, or switch an OLED page. After confirming the pulse measurements on the attached case, open a systemd override:

```text
sudo systemctl edit argon40d.service
```

Enter:

```ini
[Service]
ExecStart=
ExecStart=/usr/local/sbin/argon40d --button-actions
```

Then apply it:

```text
sudo systemctl daemon-reload
sudo systemctl restart argon40d.service
journalctl -u argon40d.service -f
```

At the final systemd shutdown stage, `argon40-shutdown` always stops the fan and clears/powers off the OLED. For `halt` or `poweroff`, it also clears RTC event flags and sends byte `0xff` to MCU address `0x1a`. For `reboot`, it deliberately does not send `0xff`.

UART must remain configured as required by the case MCU's host-shutdown detection. Test reboot and physical power cut separately, with UART visible and a physical recovery method available.

The manual power-cut command is intentionally gated:

```text
sudo argon40ctl power-cut --yes
```

## OLED

The owned 1024-byte framebuffer preserves the installed binary background/font formats. Vendor assets used by the original installer are included under `assets/oled` and embedded into the installer binary. See the asset provenance note before redistribution.

Supported pages are `clock`, `cpu`, `storage`, `raid`, `ram`, `temp`, and `ip`. `/etc/argoneonoled.conf` remains compatible:

```ini
enabled=Y
switchduration=0
screensaver=120
screenlist="clock ip"
```

Unlike the shell-sourced legacy config, this is parsed strictly as data. `screensaver=0` now consistently means disabled; the old daemon blanked immediately even though its configurator described zero as manual.

```text
argon40ctl oled config show
sudo argon40ctl oled config set --enabled true --switch-seconds 30 --screensaver-seconds 120 --pages clock,cpu,temp,ip
sudo argon40ctl oled next
sudo argon40ctl oled on
sudo argon40ctl oled off
sudo argon40ctl oled clear
sudo argon40ctl oled fill
```

Direct OLED commands should be used with the daemon stopped. `oled next` signals the running daemon and is safe during normal operation.

## RTC and schedules

RTC registers store UTC; schedules are interpreted in local time. The service reads the RTC at startup and attempts to set the host clock. Startup (`on`) schedules program the next hardware alarm. Shutdown (`off`) schedules are checked once per minute.

The format is intentionally not generic cron:

```text
minute hour day-of-month month day-of-week on|off
0 1 * * * off
30 7 * * 1 on
```

Only `*` and comma-separated integers are supported. Minute cannot be `*`. For compatibility, month is validated and round-tripped but ignored by matching. Both 0 and 7 mean Sunday; accepting 7 intentionally fixes the old configurator/daemon mismatch.

```text
sudo argon40ctl rtc time
sudo argon40ctl rtc sync-from-rtc
sudo argon40ctl rtc sync-to-rtc
argon40ctl rtc schedules
sudo argon40ctl rtc schedule add 0 1 '*' '*' '*' off
sudo argon40ctl rtc schedule remove 1
sudo argon40ctl rtc clear
sudo argon40ctl rtc timer set 30 --seconds
sudo argon40ctl rtc timer clear
```

Quote `*` in shells to prevent pathname expansion. RTC writes and wake tests must follow the staged hardware validation procedure.

## IR

NEC decoding is separate from deterministic LIRC configuration generation. Package-manager behavior is intentionally not embedded in the tool.

Stop the daemon and LIRC before claiming GPIO line 23:

```text
sudo argon40ctl ir diagnose-nec
sudo argon40ctl ir learn-power
sudo argon40ctl ir learn-power --program --yes
sudo argon40ctl ir program-power 00ff39c6 --yes
sudo argon40ctl ir write-default-lirc
```

Learning requires the same code three times before an optional MCU write. The legacy accidental `MENUBACK` concatenation is corrected: `MENU` and `BACK` are distinct keys and have regression coverage.

## Firmware updater

Firmware support is intentionally split into inspection, bootloader entry, and UART transfer:

```text
argon40ctl firmware inspect ./argon1.bin
argon40ctl firmware inspect ./argon1.bin --dump-dir ./packets
sudo argon40ctl firmware enter-bootloader --yes
sudo argon40ctl firmware flash ./argon1.bin --serial /dev/serial0 --retries 2 --yes
```

`flash` never sends bootloader command `0xbb`; the operator must enter bootloader explicitly. Images must be nonempty and at most 16 MiB. Every 64-byte ACK checksum and packet ID is strictly checked with bounded retries. This improves on the legacy default, which continued after mismatches.

The checksum is transport integrity only. No vendor signature format was found in the legacy source, so the tool does not claim firmware authenticity. Firmware flashing has not been performed and should not be used without a known-good image and credible MCU recovery path.

## Intentional compatibility decisions

- Existing `/etc` config paths and OLED binary formats are retained.
- MCU commands `0xaa`, `0xbb`, and `0xff` preserve the legacy SMBus transaction forms.
- RTC register operations retain the byte-data and sequential-read semantics from the source.
- The scheduler retains ignored months and its limited field language.
- Sunday 7 is accepted in addition to 0, fixing an old generated-config bug.
- OLED screensaver zero means disabled, fixing a dangerous documentation/runtime mismatch.
- `MENU` and `BACK` are separate IR buttons, fixing adjacent string-literal concatenation.
- Firmware validation errors fail closed instead of being ignored.
- Linux storage, RAID, and drive temperatures use `/proc`, `/sys`, mountinfo, and `statvfs` rather than localized `df`, `mount`, `mdadm`, and `hddtemp` output.

## Repository map

See [dissection.md](dissection.md) for the module-level architecture and [hardware-validation.md](docs/hardware-validation.md) for evidence and remaining hardware work.
