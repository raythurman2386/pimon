# Pimon

A hardware sensor monitor for the [pi suite](https://github.com/raythurman2386),
built with [GPUI Kit](https://github.com/longbridge/gpui-kit) — a small,
native, theme-following desktop app written for Raspberry Pi 5-class hardware
(built on a Pi 500). It charts the SoC, PMIC, NVMe and RP1 temperatures, the
ARM/GPU/core clock domains, voltages and rails, and the firmware's throttling
health — the telemetry `vcgencmd` and the kernel's sysfs interfaces expose on
the Pi 5 family.

## Features

- **Live tab** — big temperature readouts color-coded by severity, per-core
  utilization gauges, memory/swap/cache gauges, NVMe and Wi-Fi throughput
  charts, every answering clock domain, and the voltage rails — one
  sample/second, five minutes of history.
- **Details tab** — the full sensor table: temps, per-core CPU, memory, I/O
  rates, RP1 ADC channels, all clock domains, regulators, loads, uptime,
  Wi-Fi, plus the firmware throttling state ("healthy" / "since boot: …" /
  "throttling now: …").
- **Health badge** in the title bar: green when nothing has fired since boot,
  yellow when a throttling or undervoltage event happened earlier this boot,
  red while a condition is live.
- **Light touch**: the sweep runs once per second; sysfs reads are
  effectively free and `vcgencmd` costs a few milliseconds, so the monitor
  stays out of its own measurements.
- **Aesthetic**: follows the desktop dark/light mode and text scale, and
  live re-tints from the Omarchy theme palette.

The sensor backend is pure Rust with no UI imports: every parse and assembly
step is covered by unit tests against a canned probe (23 across the suite),
and the gpui layer only renders `Snapshot`s.

## What it reads

- `/sys/class/thermal` — SoC temperature (zone0) plus any extra zones
- `/sys/class/hwmon` — NVMe composite temp, RP1 ADC inputs (mV) and die
  temp, firmware undervoltage alarm (`rpi_volt`)
- `vcgencmd` — PMIC temp, core/SDRAM voltages, clock domains
  (`measure_clock`), and the sticky `get_throttled` health bitmask
- `/proc/loadavg`, `/proc/meminfo`, `/proc/net/wireless` — load, memory in
  use, Wi-Fi level

## Install

User-local install from a tagged release (no root, Ed25519-verified,
fail-closed):

```sh
curl -fsSL https://raw.githubusercontent.com/raythurman2386/pimon/master/scripts/netinstall.sh | bash
```

Or build and install from source:

```sh
cargo build --release
./scripts/install.sh
```

Uninstall with `./scripts/uninstall.sh`. The netinstaller accepts a `--prefix`
directory, an optional version argument, and `--force`; the source install
honors `PREFIX=DIR`.

Tagged `v*` releases also build x86_64 + aarch64 tarballs on GitHub Actions
(glibc 2.39+ — e.g. Raspberry Pi OS / Debian 13). Unpack the one for your
architecture and run `./install.sh` inside.

Releases are authenticated with Ed25519 signatures over `checksums.txt`; the
public key is committed as `pimon-signing-key.pub` and pinned in the
installer, which refuses anything it cannot verify.

## Keyboard

| Keys | Action |
|---|---|
| Click | Switch between the Live and Details tabs |
| `Ctrl+Q` | Quit |

## State and theming

Colors follow the desktop theme —
`~/.local/state/omarchy/current/theme/colors.toml` when present — re-tinting
live on theme switches; dark/light mode follows `gsettings` `color-scheme`;
text follows the desktop text scale (`gsettings` `text-scaling-factor`).

## Fonts

The iA Writer Mono font is bundled under the SIL Open Font License 1.1; see
`fonts/OFL.txt`. The font is copyright Information Architects Inc. and based
on IBM Plex, copyright IBM Corp.

## Development

```sh
cargo fmt --check          # formatting
cargo clippy --all-targets -- -D warnings
cargo test                 # 23 tests
cargo run --release        # monitor
pimon --version            # headless smoke test
```

CI runs fmt, clippy, tests, and the netinstall integrity harness on every
push; tagged `v*` releases build x86_64 + aarch64 tarballs (glibc 2.39+)
with an install smoke test that runs `pimon --version`.

## License

MIT — see [LICENSE](LICENSE). Bundled fonts: SIL OFL 1.1 (see above).