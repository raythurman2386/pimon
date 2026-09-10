# Pimon

A hardware sensor monitor for the Raspberry Pi, built with [GPUI Kit](https://github.com/longbridge/gpui-kit). It charts the SoC, PMIC, NVMe and RP1 temperatures, the ARM/GPU/core clock domains, voltages and rails, and the firmware's throttling health — the telemetry `vcgencmd` and the kernel's sysfs interfaces expose on the Pi 5 family (built on a Pi 500).

The sensor backend is pure Rust with no UI imports: every parse and assembly step is covered by unit tests against a canned probe, and the gpui layer only renders `Snapshot`s.

## Install

User-local install (binary, icon, launcher). No root:

```sh
./scripts/install.sh
```

That puts `pimon` on `~/.local/bin` and a desktop entry in the app launcher. Uninstall with `./scripts/uninstall.sh`.

Tagged releases (`v*`) build a Linux x86_64 tarball on GitHub Actions. Unpack it and run `./install.sh` inside.

## Run from source

```sh
cargo run --release
```

## What it reads

- `/sys/class/thermal` — SoC temperature (zone0) plus any extra zones
- `/sys/class/hwmon` — NVMe composite temp, RP1 ADC inputs (mV) and die temp, firmware undervoltage alarm (`rpi_volt`)
- `vcgencmd` — PMIC temp, core/SDRAM voltages, clock domains (`measure_clock`), and the sticky `get_throttled` health bitmask
- `/proc/loadavg`, `/proc/meminfo`, `/proc/net/wireless` — load, memory in use, Wi-Fi level

The sweep runs once per second; sysfs reads are effectively free and `vcgencmd` costs a few milliseconds, so the monitor stays out of its own measurements.

## Tabs

- **Live** — big temperature readouts color-coded by severity, per-core utilization gauges, memory/swap/cache gauges, NVMe and Wi-Fi throughput charts, every answering clock domain, and the voltage rails — one sample/second, five minutes of history.
- **Details** — the full sensor table: temps, per-core CPU, memory, I/O rates, RP1 ADC channels, all clock domains, regulators, loads, uptime, Wi-Fi, plus the firmware throttling state ("healthy" / "since boot: …" / "throttling now: …").

The title bar carries a health badge: green when nothing has fired since boot, yellow when a throttling or undervoltage event happened earlier this boot, red while a condition is live.

Text follows the desktop text size, and colors come from `~/.local/state/omarchy/current/theme/colors.toml` when present, following system dark/light mode and re-tinting live on a theme switch.

## Keyboard

- `Ctrl+Q` quits.

## Fonts

The iA Writer Mono font is bundled under the SIL Open Font License 1.1; see `fonts/OFL.txt`. The font is copyright Information Architects Inc. and based on IBM Plex, copyright IBM Corp.