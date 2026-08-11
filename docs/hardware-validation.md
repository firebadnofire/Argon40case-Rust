# Hardware validation record

This is an evidence log, not a statement of intended support.

## Inventory captured 2026-08-11

The reachable host at `192.168.86.41` reported:

- Hostname: `rpi`
- Hardware: Raspberry Pi 4 Model B Rev 1.4
- Architecture: aarch64
- OS: openSUSE MicroOS 20260806, not Raspberry Pi OS
- Kernel: 7.1.6-1-default
- CPU thermal reading: 39.433 C at inventory time
- GPIO controllers: `pinctrl-bcm2711`, 58 lines; `raspberrypi-exp-gpio`, 8 lines
- `/dev/serial0` present
- No `/dev/i2c-*` node visible
- No installed `argononed.service` or `argoneond.service`
- No legacy Argon configuration files found
- Passwordless sudo unavailable

The workstation has `/dev/ttyUSB0` at 115200 held by a GNU Screen session. Its captured console showed qbtOS activity, so it was not treated as the requested Raspberry Pi OS/Argon target. A previously known LAN host at `192.168.86.65` was unreachable. No hardware mutation was performed on either system.

## Completed host-only validation

- Fan thresholds, minimum 25%, fallback, startup kick, and delayed downshift
- Strict fan/OLED config parsing
- GPIO duration classification
- Framebuffer layout, clipping, text position, and background handling
- Exact 32-block OLED full-transfer shape against fake SMBus
- Exact MCU byte/block transaction shape against fake SMBus
- RTC BCD encoding/decoding and alarm register writes
- Schedule CSV/wildcards, Sunday, month-end, leap handling, and command filtering
- NEC decoding and distinct MENU/BACK mappings
- Firmware first/subsequent packet layouts and fail-closed ACK checks
- `cargo fmt --check`: passed
- `cargo clippy --all-targets --all-features -- -D warnings`: passed
- `cargo test --all`: 36 passed, 0 failed
- Native x86_64 release build: passed
- Static `aarch64-unknown-linux-musl` release build: passed
- Linux/aarch64 dependency graph: no `libudev-sys`, `cc`, CMake, or bindgen package
- Project source: no C/C++ files and Rust `unsafe_code` is forbidden

## Native aarch64 smoke test

The release was also built for `aarch64-unknown-linux-musl`, copied to the reachable Pi 4, and executed there without privilege or hardware mutation:

- `argon40ctl --version` reported 0.1.0.
- `argon40ctl status` reported the inactive service, 39.4 C CPU temperature, and `192.168.86.41`.
- `argon40d --help` ran successfully.
- `argon40ctl install --root /tmp/argon40-native-stage --no-enable` created 24 expected staged files.
- The deployed executables were confirmed as statically linked aarch64 ELF files.

This is native execution and installer validation on a Raspberry Pi, not validation of Argon peripherals or Raspberry Pi OS compatibility.

## Required real-hardware sequence

These phases remain blocked until the Raspberry Pi OS target with attached Argon hardware and usable privilege/recovery access is reachable:

1. Record Pi model, Raspberry Pi OS version, kernel, GPIO labels, I2C buses, serial alias, services, config, temperature, and attached I2C addresses.
2. Confirm `0x1a`, and when fitted `0x3c`/`0x51`, without state-changing commands.
3. Stop vendor and Rust services; test fan 0/25/50/100 with physical observation and restore safe cooling.
4. Run the fan daemon and observe startup kick, final speed, downshift delay, and recoverable I/O failure.
5. Run diagnostic-default `argon40d`; record rising/falling timestamps for all three gestures before adding `--button-actions`.
6. Test OLED clear/fill/pixel/text/full transfer/pages/switch/screensaver/power with one owner.
7. Compare RTC reads with the legacy implementation, then test controlled write, host sync, alarm, and clear.
8. Commit/recover changes, then test reboot through UART; verify no `0xff` power cut occurs.
9. With a physical power-on method, test shutdown, fan/OLED cleanup, RTC flags, `0xff`, and complete power removal.
10. Test scheduled wake/shutdown only after RTC and power-cut behavior are proven.
11. Decode IR passively before programming the MCU.
12. Do not flash firmware without a known-good image and recovery procedure.

Do not update this file from expected behavior. Record commands, timestamps, observed fan/display/power behavior, relevant journal lines, and whether UART remained available.
