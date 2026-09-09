//! Assembles one [`Snapshot`] from a [`Probe`]: sysfs globs, vcgencmd
//! calls and /proc reads in a single pass. Pure against the trait, so
//! tests feed a canned probe instead of touching hardware.

use std::collections::BTreeMap;
use std::path::Path;

use crate::sensors::{
    parse_gencmd_temp, parse_gencmd_value, parse_loadavg, parse_meminfo_kb, parse_millidegrees,
    Clock, Probe, Snapshot, ThrottleState, LOADAVG, MEMINFO, THERMAL_GLOB, VCGENCMD,
};

const HWMON_GLOB: &str = "/sys/class/hwmon/hwmon*";

/// Run one full sensor sweep. Never fails: missing sensors stay `None`
/// and a mostly-empty sweep carries a human-readable `error`.
pub fn collect_snapshot(probe: &dyn Probe, clock: &Clock) -> Snapshot {
    let (soc_from_sysfs, extra_zones_c) = read_thermal_zones(probe);
    let (nvme_temp_c, rp1_temp_c, rp1_adc_mv, undervolt_alarm) = read_hwmons(probe);
    let (soc_fallback, pmic_temp_c, core_volts, sdram_volatile_volts, sdram_phy_volts, throttle) =
        read_firmware(probe);
    let soc_temp_c = soc_from_sysfs.or(soc_fallback);
    let (load_1m, memory_percent, rssi_dbm) = read_proc(probe);

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
        arm_clock_mhz: clock_mhz(probe, "arm"),
        v3d_clock_mhz: clock_mhz(probe, "v3d"),
        core_clock_mhz: clock_mhz(probe, "core"),
        load_1m,
        memory_percent,
        rssi_dbm,
        throttle,
        undervolt_alarm,
        rp1_adc_mv,
        // Only zone0 is charted today; further clock domains land here
        // once the details view grows a clock section.
        other_clocks_mhz: BTreeMap::new(),
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

/// Kernel counters: load, memory in use, Wi-Fi level.
fn read_proc(probe: &dyn Probe) -> (Option<f32>, Option<f32>, Option<f32>) {
    let load = probe
        .read_file(Path::new(LOADAVG))
        .as_deref()
        .and_then(parse_loadavg);
    let meminfo = probe.read_file(Path::new(MEMINFO));
    let memory_percent = meminfo.as_deref().and_then(|raw| {
        let total = parse_meminfo_kb(raw, "MemTotal")?;
        let available = parse_meminfo_kb(raw, "MemAvailable")?;
        if total == 0 {
            return None;
        }
        let used = total.saturating_sub(available) as f32;
        Some((used / total as f32 * 100.0).min(100.0))
    });
    let rssi = probe
        .read_file(Path::new("/proc/net/wireless"))
        .as_deref()
        .and_then(parse_wireless_level);
    (load, memory_percent, rssi)
}

fn read_temp(probe: &dyn Probe, hwmon: &Path, file: &str) -> Option<f32> {
    probe
        .read_file(&hwmon.join(file))
        .as_deref()
        .and_then(parse_millidegrees)
}

fn clock_mhz(probe: &dyn Probe, domain: &str) -> Option<f32> {
    // `frequency(48)=2400000000` — everything after '=' is the Hz value.
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

    #[test]
    fn full_pi_snapshot_matches_real_device_shape() {
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
        probe.gencmd(&["measure_clock", "arm"], "frequency(48)=2400000000");
        probe.gencmd(&["measure_clock", "v3d"], "frequency(48)=1150000000");
        probe.gencmd(&["measure_clock", "core"], "frequency(48)=910000000");
        probe.file(LOADAVG, "2.10 1.85 1.62 4/1234 5678\n");
        probe.file(
            MEMINFO,
            "MemTotal:       15990780 kB\nMemAvailable:   14200000 kB\n",
        );
        probe.file(
            "/proc/net/wireless",
            "Inter-| header\nwlan0: 0000   67.  -43.  -256\n",
        );

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
        assert_eq!(snap.load_1m, Some(2.10));
        let expected_mem = (15990780.0 - 14200000.0) / 15990780.0 * 100.0;
        assert_eq!(snap.memory_percent, Some(expected_mem as f32));
        assert_eq!(snap.rssi_dbm, Some(-43.0));
        assert_eq!(snap.rp1_adc_mv.get(&2), Some(&2561.0));
        assert!(!snap.undervolt_alarm);
        assert!(!snap.throttle.any_event());
        assert!(snap.error.is_none());
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
