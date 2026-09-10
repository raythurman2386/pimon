//! Assembles one [`Snapshot`] from a [`Probe`]: sysfs globs, vcgencmd
//! calls and /proc reads in a single pass. Pure against the trait, so
//! tests feed a canned probe instead of touching hardware.
//!
//! Counter-style metrics (CPU utilization, disk and network throughput)
//! are deltas between sweeps, so stateful sweeping goes through
//! [`Sweeper`]; the stateless `collect_snapshot` reports no rates.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use crate::sensors::{
    parse_all_clocks, parse_core_times, parse_diskstats_sectors, parse_gencmd_temp,
    parse_gencmd_value, parse_meminfo_kb, parse_millidegrees, parse_net_dev_bytes,
    parse_primary_disk, parse_primary_wireless, parse_regulators, Clock, CoreState, CpuTimes,
    DiskIo, MemoryInfo, NetIo, Probe, Snapshot, ThrottleState, CLOCK_DOMAINS, DISKSTATS, LOADAVG,
    MEMINFO, NET_DEV, STAT, THERMAL_GLOB, UPTIME, VCGENCMD,
};

const HWMON_GLOB: &str = "/sys/class/hwmon/hwmon*";

/// Stateful wrapper producing per-sweep deltas for CPU, disk and net rates.
#[derive(Default)]
pub struct Sweeper {
    prev_times: Vec<CpuTimes>,
    prev_disk: Option<(String, u64, u64)>,
    prev_net: Option<(String, u64, u64)>,
    prev_instant: Option<Instant>,
}

impl Sweeper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run one sweep, computing rates against the previous run. The first
    /// sweep establishes baselines and reports no rates.
    pub fn sweep(&mut self, probe: &dyn Probe, clock: &Clock) -> Snapshot {
        let now = Instant::now();
        let mut snap = collect_snapshot(probe, clock);

        // Per-core and aggregate utilization from /proc/stat tick deltas.
        let times = read_core_times(probe);
        let mut sum = 0.0f32;
        let mut counted = 0usize;
        for (core, next) in times.iter().enumerate() {
            let Some(prev) = self.prev_times.get(core) else {
                break;
            };
            if let Some(util) = next.utilization_since(prev) {
                snap.cores.push(CoreState { utilization: util });
                sum += util;
                counted += 1;
            }
        }
        if counted > 0 {
            snap.utilization_pct = Some(sum / counted as f32);
        }
        self.prev_times = times;

        // Disk and network throughput from sector/byte counter deltas.
        let dt = self
            .prev_instant
            .map(|prev| now.duration_since(prev).as_secs_f32())
            .filter(|dt| *dt > 0.0);
        let disk_raw = probe.read_file(Path::new(DISKSTATS));
        if let Some(device) = disk_raw.as_deref().and_then(parse_primary_disk) {
            if let Some((read, written)) = disk_raw
                .as_deref()
                .and_then(|raw| parse_diskstats_sectors(raw, &device))
            {
                snap.disk_io = self.rate_disk(&device, read, written, dt);
            }
        }
        let net_raw = probe.read_file(Path::new(NET_DEV));
        if let Some(iface) = net_raw.as_deref().and_then(parse_primary_wireless) {
            if let Some((rx, tx)) = net_raw
                .as_deref()
                .and_then(|raw| parse_net_dev_bytes(raw, &iface))
            {
                snap.net_io = self.rate_net(&iface, rx, tx, dt);
            }
        }
        self.prev_instant = Some(now);

        snap
    }

    /// MiB/s from 512-byte sector deltas; `None` on the first sweep or a
    /// device swap.
    fn rate_disk(
        &mut self,
        device: &str,
        read: u64,
        written: u64,
        dt: Option<f32>,
    ) -> Option<DiskIo> {
        let prev = self
            .prev_disk
            .replace((device.to_string(), read, written))?;
        if prev.0 != device {
            return None;
        }
        let dt = dt?;
        let sectors = |now: u64, before: u64| now.saturating_sub(before) as f32;
        Some(DiskIo {
            read_mib_s: sectors(read, prev.1) * 512.0 / dt / (1024.0 * 1024.0),
            write_mib_s: sectors(written, prev.2) * 512.0 / dt / (1024.0 * 1024.0),
        })
    }

    /// KiB/s from byte deltas; `None` on the first sweep or iface change.
    fn rate_net(&mut self, iface: &str, rx: u64, tx: u64, dt: Option<f32>) -> Option<NetIo> {
        let prev = self.prev_net.replace((iface.to_string(), rx, tx))?;
        if prev.0 != iface {
            return None;
        }
        let dt = dt?;
        let delta = |now: u64, before: u64| now.saturating_sub(before) as f32;
        Some(NetIo {
            rx_kib_s: delta(rx, prev.1) / dt / 1024.0,
            tx_kib_s: delta(tx, prev.2) / dt / 1024.0,
        })
    }
}

/// Run one full stateless sensor sweep. Never fails: missing sensors stay
/// `None` and a mostly-empty sweep carries a human-readable `error`.
pub fn collect_snapshot(probe: &dyn Probe, clock: &Clock) -> Snapshot {
    let (soc_from_sysfs, extra_zones_c) = read_thermal_zones(probe);
    let (nvme_temp_c, rp1_temp_c, rp1_adc_mv, undervolt_alarm) = read_hwmons(probe);
    let (soc_fallback, pmic_temp_c, core_volts, sdram_volatile_volts, sdram_phy_volts, throttle) =
        read_firmware(probe);
    let soc_temp_c = soc_from_sysfs.or(soc_fallback);
    let (load_1m, load_5m, load_15m, memory, uptime_secs, rssi_dbm) = read_proc(probe);
    let regulators_v = parse_regulators(probe);
    let all_clocks_mhz = parse_all_clocks(probe, CLOCK_DOMAINS);

    // A sweep with neither sysfs nor firmware temps is an error worth
    // naming in the UI, not just empty charts.
    let error = if soc_temp_c.is_none() && pmic_temp_c.is_none() && extra_zones_c.is_empty() {
        Some(if probe.vcgencmd(&["measure_temp"]).is_none() {
            format!("vcgencmd not found at {VCGENCMD}")
        } else {
            "no thermal zones or firmware sensors found".into()
        })
    } else {
        None
    };

    Snapshot {
        elapsed_ms: clock.elapsed_ms(),
        soc_temp_c,
        nvme_temp_c,
        rp1_temp_c,
        pmic_temp_c,
        core_volts,
        sdram_volatile_volts,
        sdram_phy_volts,
        arm_clock_mhz: all_clocks_mhz.get("arm").copied(),
        v3d_clock_mhz: all_clocks_mhz.get("v3d").copied(),
        core_clock_mhz: all_clocks_mhz.get("core").copied(),
        all_clocks_mhz,
        load_1m,
        load_5m,
        load_15m,
        uptime_secs,
        rssi_dbm,
        throttle,
        undervolt_alarm,
        cores: Vec::new(),
        utilization_pct: None,
        memory,
        disk_io: None,
        net_io: None,
        regulators_v,
        rp1_adc_mv,
        extra_zones_c,
        error,
    }
}

/// /sys/class/thermal zones: zone0 is the SoC on every Pi 5-family board;
/// any further zones are labeled by their zone type.
fn read_thermal_zones(probe: &dyn Probe) -> (Option<f32>, BTreeMap<String, f32>) {
    let mut soc = None;
    let mut extra = BTreeMap::new();
    for (ix, zone) in probe.glob(THERMAL_GLOB).into_iter().enumerate() {
        let Some(temp) = probe
            .read_file(&zone.join("temp"))
            .as_deref()
            .and_then(parse_millidegrees)
        else {
            continue;
        };
        if ix == 0 {
            soc = Some(temp);
        } else {
            let label = probe
                .read_file(&zone.join("type"))
                .map(|t| t.trim().to_string())
                .unwrap_or_else(|| format!("zone{ix}"));
            extra.insert(label, temp);
        }
    }
    (soc, extra)
}

/// hwmon devices: NVMe composite temp, RP1 ADC inputs + die temp, and
/// the firmware undervoltage alarm on rpi_volt.
type HwmonReads = (Option<f32>, Option<f32>, BTreeMap<u8, f32>, bool);

fn read_hwmons(probe: &dyn Probe) -> HwmonReads {
    let mut nvme = None;
    let mut rp1 = None;
    let mut adc = BTreeMap::new();
    let mut alarm = false;

    for hwmon in probe.glob(HWMON_GLOB) {
        let Some(name) = probe
            .read_file(&hwmon.join("name"))
            .map(|n| n.trim().to_string())
        else {
            continue;
        };
        match name.as_str() {
            "nvme" => nvme = read_temp(probe, &hwmon, "temp1_input"),
            "rp1_adc" => {
                rp1 = read_temp(probe, &hwmon, "temp1_input");
                for input in 1..=4 {
                    if let Some(mv) = probe
                        .read_file(&hwmon.join(format!("in{input}_input")))
                        .as_deref()
                        .and_then(|raw| raw.trim().parse::<f32>().ok())
                    {
                        adc.insert(input, mv);
                    }
                }
            }
            "rpi_volt" => {
                alarm = probe
                    .read_file(&hwmon.join("in0_lcrit_alarm"))
                    .map(|v| v.trim() == "1")
                    .unwrap_or(false);
            }
            _ => {}
        }
    }
    (nvme, rp1, adc, alarm)
}

/// Firmware telemetry via vcgencmd. Returns the gencmd SoC reading as a
/// fallback for the caller to fold in (sysfs wins); the PMIC has no sysfs
/// exposure, so gencmd is its only source.
type FirmwareReads = (
    Option<f32>,
    Option<f32>,
    Option<f32>,
    Option<f32>,
    Option<f32>,
    ThrottleState,
);

fn read_firmware(probe: &dyn Probe) -> FirmwareReads {
    let soc_fallback = probe
        .vcgencmd(&["measure_temp"])
        .as_deref()
        .and_then(parse_gencmd_temp);
    let pmic = probe
        .vcgencmd(&["measure_temp", "pmic"])
        .as_deref()
        .and_then(parse_gencmd_temp);
    let volts = |rail: &str| {
        probe
            .vcgencmd(&["measure_volts", rail])
            .as_deref()
            .and_then(|o| parse_gencmd_value(o, "volt="))
    };
    let throttle = probe
        .vcgencmd(&["get_throttled"])
        .as_deref()
        .and_then(ThrottleState::from_hex)
        .unwrap_or_default();
    (
        soc_fallback,
        pmic,
        volts("core"),
        volts("sdram_c"),
        volts("sdram_p"),
        throttle,
    )
}

/// Kernel counters: loads, memory block, uptime, Wi-Fi level.
type ProcReads = (
    Option<f32>,
    Option<f32>,
    Option<f32>,
    Option<MemoryInfo>,
    Option<u64>,
    Option<f32>,
);

fn read_proc(probe: &dyn Probe) -> ProcReads {
    let loads = probe
        .read_file(Path::new(LOADAVG))
        .as_deref()
        .map(|raw| {
            let mut it = raw.split_whitespace().filter_map(|f| f.parse::<f32>().ok());
            (it.next(), it.next(), it.next())
        })
        .unwrap_or((None, None, None));
    let memory = probe
        .read_file(Path::new(MEMINFO))
        .as_deref()
        .and_then(parse_memory_info);
    let uptime = probe
        .read_file(Path::new(UPTIME))
        .as_deref()
        .and_then(crate::sensors::parse_uptime_secs);
    let rssi = probe
        .read_file(Path::new("/proc/net/wireless"))
        .as_deref()
        .and_then(parse_wireless_level);
    (loads.0, loads.1, loads.2, memory, uptime, rssi)
}

/// Memory gauges from a `/proc/meminfo` string: used vs buff/cache vs swap.
fn parse_memory_info(raw: &str) -> Option<MemoryInfo> {
    let total_kib = parse_meminfo_kb(raw, "MemTotal")?;
    if total_kib == 0 {
        return None;
    }
    let available_kib = parse_meminfo_kb(raw, "MemAvailable").unwrap_or(0);
    let buffers = parse_meminfo_kb(raw, "Buffers").unwrap_or(0);
    let cached = parse_meminfo_kb(raw, "Cached").unwrap_or(0);
    let total = total_kib as f32;
    let used_kib = total_kib.saturating_sub(available_kib);
    let buff_cache_kib = buffers.saturating_add(cached).min(total_kib);
    let swap = match (
        parse_meminfo_kb(raw, "SwapTotal"),
        parse_meminfo_kb(raw, "SwapFree"),
    ) {
        (Some(total), Some(free)) if total > 0 => {
            Some(((total - free) as f32 / total as f32 * 100.0).min(100.0))
        }
        _ => None,
    };
    Some(MemoryInfo {
        used_pct: (used_kib as f32 / total * 100.0).min(100.0),
        buff_cache_pct: buff_cache_kib as f32 / total * 100.0,
        swap_used_pct: swap,
        total_mib: total / 1024.0,
        used_mib: used_kib as f32 / 1024.0,
    })
}

/// Per-core tick counters for the `Sweeper` delta math.
fn read_core_times(probe: &dyn Probe) -> Vec<CpuTimes> {
    probe
        .read_file(Path::new(STAT))
        .as_deref()
        .map(parse_core_times)
        .unwrap_or_default()
}

fn read_temp(probe: &dyn Probe, hwmon: &Path, file: &str) -> Option<f32> {
    probe
        .read_file(&hwmon.join(file))
        .as_deref()
        .and_then(parse_millidegrees)
}

/// `frequency(48)=2400000000` — everything after '=' is the Hz value.
pub fn clock_mhz(probe: &dyn Probe, domain: &str) -> Option<f32> {
    let raw = probe.vcgencmd(&["measure_clock", domain])?;
    let hz: f64 = raw.rsplit('=').next()?.trim().parse().ok()?;
    let mhz = hz / 1_000_000.0;
    (mhz.is_finite() && mhz >= 0.0).then_some(mhz as f32)
}

/// `/proc/net/wireless` level column for the wlan interface, in dBm.
fn parse_wireless_level(raw: &str) -> Option<f32> {
    let line = raw.lines().find(|l| l.trim_start().starts_with("wlan"))?;
    crate::sensors::parse_rssi_dbm(line.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::Probe;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct FakeProbe {
        files: Mutex<BTreeMap<String, String>>,
        gencmd: Mutex<BTreeMap<Vec<String>, String>>,
        globs: Mutex<BTreeMap<String, Vec<String>>>,
    }

    impl FakeProbe {
        fn new() -> Self {
            Self {
                files: Mutex::new(BTreeMap::new()),
                gencmd: Mutex::new(BTreeMap::new()),
                globs: Mutex::new(BTreeMap::new()),
            }
        }

        fn file(&self, path: &str, content: &str) {
            self.files
                .lock()
                .unwrap()
                .insert(path.into(), content.into());
        }

        fn gencmd(&self, args: &[&str], out: &str) {
            self.gencmd
                .lock()
                .unwrap()
                .insert(args.iter().map(|s| s.to_string()).collect(), out.into());
        }

        fn glob(&self, pattern: &str, paths: &[&str]) {
            self.globs.lock().unwrap().insert(
                pattern.into(),
                paths.iter().map(|s| s.to_string()).collect(),
            );
        }
    }

    impl Probe for FakeProbe {
        fn read_file(&self, path: &Path) -> Option<String> {
            self.files
                .lock()
                .unwrap()
                .get(&path.to_string_lossy().to_string())
                .cloned()
        }

        fn glob(&self, pattern: &str) -> Vec<PathBuf> {
            self.globs
                .lock()
                .unwrap()
                .get(pattern)
                .map(|paths| paths.iter().map(PathBuf::from).collect())
                .unwrap_or_default()
        }

        fn vcgencmd(&self, args: &[&str]) -> Option<String> {
            self.gencmd
                .lock()
                .unwrap()
                .get(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .cloned()
        }
    }

    fn fake_clock() -> Clock {
        Clock::started()
    }

    /// A probe shaped like the real Pi 500 this app was built on.
    fn pi500_probe() -> FakeProbe {
        let probe = FakeProbe::new();
        probe.glob(THERMAL_GLOB, &["/sys/class/thermal/thermal_zone0"]);
        probe.file("/sys/class/thermal/thermal_zone0/temp", "37500\n");
        probe.glob(
            HWMON_GLOB,
            &[
                "/sys/class/hwmon/hwmon0",
                "/sys/class/hwmon/hwmon1",
                "/sys/class/hwmon/hwmon2",
                "/sys/class/hwmon/hwmon3",
            ],
        );
        probe.file("/sys/class/hwmon/hwmon0/name", "cpu_thermal\n");
        probe.file("/sys/class/hwmon/hwmon0/temp1_input", "37500\n");
        probe.file("/sys/class/hwmon/hwmon1/name", "nvme\n");
        probe.file("/sys/class/hwmon/hwmon1/temp1_input", "37850\n");
        probe.file("/sys/class/hwmon/hwmon2/name", "rp1_adc\n");
        probe.file("/sys/class/hwmon/hwmon2/temp1_input", "45594\n");
        probe.file("/sys/class/hwmon/hwmon2/in1_input", "2063\n");
        probe.file("/sys/class/hwmon/hwmon2/in2_input", "2561\n");
        probe.file("/sys/class/hwmon/hwmon2/in3_input", "1445\n");
        probe.file("/sys/class/hwmon/hwmon2/in4_input", "1456\n");
        probe.file("/sys/class/hwmon/hwmon3/name", "rpi_volt\n");
        probe.file("/sys/class/hwmon/hwmon3/in0_lcrit_alarm", "0\n");
        probe.gencmd(&["measure_temp"], "temp=39.5'C");
        probe.gencmd(&["measure_temp", "pmic"], "temp=44.0'C");
        probe.gencmd(&["measure_volts", "core"], "volt=0.8635V");
        probe.gencmd(&["measure_volts", "sdram_c"], "volt=0.6000V");
        probe.gencmd(&["measure_volts", "sdram_p"], "volt=1.1000V");
        probe.gencmd(&["get_throttled"], "throttled=0x0");
        for (domain, hz) in [
            ("arm", "2400000000"),
            ("v3d", "1150000000"),
            ("core", "910000000"),
            ("emmc", "199800000"),
            ("uart", "44000000"),
        ] {
            probe.gencmd(&["measure_clock", domain], &format!("frequency(48)={hz}"));
        }
        probe.file(LOADAVG, "2.10 1.85 1.62 4/1234 5678\n");
        probe.file(UPTIME, "9876.54 38124.10\n");
        probe.file(
            MEMINFO,
            "MemTotal:       15990780 kB\nMemAvailable:   14200000 kB\nBuffers:         200000 kB\nCached:         8000000 kB\nSwapTotal:       2048000 kB\nSwapFree:       1843200 kB\n",
        );
        probe.file(
            "/proc/net/wireless",
            "Inter-| header\nwlan0: 0000   67.  -43.  -256\n",
        );
        probe.file(STAT, "cpu  10 0 10 90 0 0 0\ncpu0 1 0 1 8 0 0 0\ncpu1 2 0 2 6 0 0 0\ncpu2 0 0 0 10 0 0 0\ncpu3 0 0 0 10 0 0 0\n");
        probe.file(
            DISKSTATS,
            " 259        0 nvme0n1 4379 41 273090 2982 631 1053 15534 1494 0 4476 4476 0 0 0 0 0 0\n 259        1 nvme0n1p1 100 0 800 20 5 0 10 5 0 25 25 0 0 0 0 0 0\n",
        );
        probe.file(
            NET_DEV,
            "  eth0: 1000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0\n  wlan0: 52428800 0 0 0 0 0 0 0 10485760 0 0 0 0 0 0 0\n",
        );
        probe.glob(
            "/sys/class/regulator/regulator*",
            &[
                "/sys/class/regulator/regulator.1",
                "/sys/class/regulator/regulator.5",
            ],
        );
        probe.file("/sys/class/regulator/regulator.1/name", "3v3\n");
        probe.file("/sys/class/regulator/regulator.1/state", "enabled\n");
        probe.file("/sys/class/regulator/regulator.1/microvolts", "3300000\n");
        probe.file("/sys/class/regulator/regulator.5/name", "vcc-sd\n");
        probe.file("/sys/class/regulator/regulator.5/state", "disabled\n");
        probe
    }

    #[test]
    fn full_pi_snapshot_matches_real_device_shape() {
        let probe = pi500_probe();
        let snap = collect_snapshot(&probe, &fake_clock());
        assert_eq!(snap.soc_temp_c, Some(37.5));
        assert_eq!(snap.nvme_temp_c, Some(37.85));
        assert_eq!(snap.rp1_temp_c, Some(45.594));
        assert_eq!(snap.pmic_temp_c, Some(44.0));
        assert_eq!(snap.core_volts, Some(0.8635));
        assert_eq!(snap.sdram_volatile_volts, Some(0.6));
        assert_eq!(snap.sdram_phy_volts, Some(1.1));
        assert_eq!(snap.arm_clock_mhz, Some(2400.0));
        assert_eq!(snap.v3d_clock_mhz, Some(1150.0));
        assert_eq!(snap.core_clock_mhz, Some(910.0));
        assert_eq!(snap.all_clocks_mhz.get("emmc"), Some(&199.8));
        assert_eq!(snap.load_1m, Some(2.10));
        assert_eq!(snap.load_5m, Some(1.85));
        assert_eq!(snap.load_15m, Some(1.62));
        assert_eq!(snap.uptime_secs, Some(9876));
        let mem = snap.memory.unwrap();
        let expected_used_pct = (15990780.0 - 14200000.0) / 15990780.0 * 100.0;
        assert!((mem.used_pct - expected_used_pct).abs() < 0.01);
        assert!((mem.total_mib - 15616.0).abs() < 1.0);
        assert_eq!(mem.swap_used_pct, Some(10.0));
        assert_eq!(snap.rssi_dbm, Some(-43.0));
        assert_eq!(snap.rp1_adc_mv.get(&2), Some(&2561.0));
        assert_eq!(snap.regulators_v.get("3v3"), Some(&3.3));
        assert!(!snap.regulators_v.contains_key("vcc-sd"));
        assert!(!snap.undervolt_alarm);
        assert!(!snap.throttle.any_event());
        assert!(snap.error.is_none());
    }

    #[test]
    fn sweeper_computes_cpu_disk_net_rates() {
        let probe = pi500_probe();
        let mut sweeper = Sweeper::new();
        let first = sweeper.sweep(&probe, &fake_clock());
        // First sweep: baseline, no rates yet.
        assert!(first.cores.is_empty());
        assert!(first.disk_io.is_none());
        assert!(first.net_io.is_none());

        // All four cores advance: cpu0 20% busy over the window, the rest
        // fully idle. 990 total ticks on cpu0, 198 busy.
        probe.file(STAT, "cpu  10 0 10 90 0 0 0\ncpu0 100 0 100 800 0 0 0\ncpu1 2 0 2 96 0 0 0\ncpu2 0 0 0 100 0 0 0\ncpu3 0 0 0 100 0 0 0\n");
        probe.file(
            DISKSTATS,
            " 259        0 nvme0n1 4379 41 275136 2982 631 1053 16582 1494 0 4476 4476 0 0 0 0 0 0\n 259        1 nvme0n1p1 100 0 800 20 5 0 10 5 0 25 25 0 0 0 0 0 0\n",
        );
        probe.file(
            NET_DEV,
            "  eth0: 1000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0\n  wlan0: 53477376 0 0 0 0 0 0 0 11534336 0 0 0 0 0 0 0\n",
        );
        let second = sweeper.sweep(&probe, &fake_clock());
        assert_eq!(second.cores.len(), 4);
        assert!((second.cores[0].utilization - 198.0 / 990.0).abs() < 1e-5);
        let util = second.utilization_pct.expect("aggregate utilization");
        // Only cpu0 is busy: 0.2 averaged over 4 cores.
        assert!((util - 198.0 / 990.0 / 4.0).abs() < 1e-5, "util {util}");
        let disk = second.disk_io.expect("disk rates after delta");
        // 2048 sectors read = 1 MiB; 1048 sectors written = 0.5 MiB.
        assert!(disk.read_mib_s > 0.0 && disk.write_mib_s > 0.0);
        let net = second.net_io.expect("net rates after delta");
        // 1 MiB received, 1 MiB transmitted.
        assert!(net.rx_kib_s > 0.0 && net.tx_kib_s > 0.0);
    }

    #[test]
    fn undervolt_alarm_is_flagged() {
        let probe = FakeProbe::new();
        probe.file("/sys/class/hwmon/hwmon0/name", "rpi_volt\n");
        probe.file("/sys/class/hwmon/hwmon0/in0_lcrit_alarm", "1\n");
        probe.glob(HWMON_GLOB, &["/sys/class/hwmon/hwmon0"]);
        probe.gencmd(&["measure_temp"], "temp=39.5'C");
        let snap = collect_snapshot(&probe, &fake_clock());
        assert!(snap.undervolt_alarm);
    }

    #[test]
    fn extra_zones_are_labeled_by_type() {
        let probe = FakeProbe::new();
        probe.glob(
            THERMAL_GLOB,
            &[
                "/sys/class/thermal/thermal_zone0",
                "/sys/class/thermal/thermal_zone1",
            ],
        );
        probe.file("/sys/class/thermal/thermal_zone0/temp", "48000");
        probe.file("/sys/class/thermal/thermal_zone1/temp", "51000");
        probe.file("/sys/class/thermal/thermal_zone1/type", "hat-temps");
        probe.gencmd(&["measure_temp"], "temp=48.0'C");
        let snap = collect_snapshot(&probe, &fake_clock());
        assert_eq!(snap.soc_temp_c, Some(48.0));
        assert_eq!(snap.extra_zones_c.get("hat-temps"), Some(&51.0));
        assert_eq!(snap.max_temp_c(), Some(51.0));
    }

    #[test]
    fn missing_vcgencmd_yields_error() {
        let probe = FakeProbe::new();
        let snap = collect_snapshot(&probe, &fake_clock());
        assert!(snap.soc_temp_c.is_none());
        assert_eq!(
            snap.error.as_deref(),
            Some("vcgencmd not found at /usr/bin/vcgencmd")
        );
    }

    #[test]
    fn throttle_history_is_parsed() {
        let probe = FakeProbe::new();
        probe.gencmd(&["measure_temp"], "temp=39.5'C");
        probe.gencmd(&["get_throttled"], "throttled=0x30001");
        let snap = collect_snapshot(&probe, &fake_clock());
        assert!(snap.throttle.undervolted_now);
        assert!(snap.throttle.freq_capped_past);
        assert!(snap.throttle.any_event());
    }
}
