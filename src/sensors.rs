//! Sensor probing for the Raspberry Pi 5 family (BCM2712), including the
//! Pi 500 keyboard computer this app was built on.
//!
//! Sources (all verified live on a Pi 500, kernel 6.18+rpt-rpi-2712):
//! - `/sys/class/thermal` glob: CPU/SoC temperature zones (zone0 = cpu-thermal)
//! - `/sys/class/hwmon` glob: NVMe composite temp, RP1 ADC millivolt inputs
//!   and die temp, plus the firmware undervoltage alarm on `rpi_volt`
//! - `vcgencmd`: PMIC temp, core/SDRAM voltages, clock domains, throttle bitmask
//! - `/proc/loadavg`, `/proc/meminfo`: load and memory in use
//!
//! All parsing is pure and unit-tested here; the I/O lives in `Probe`
//! implementations so tests can feed canned kernel and firmware output.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Where the standard probe looks on a real system.
pub const VCGENCMD: &str = "/usr/bin/vcgencmd";
pub const THERMAL_GLOB: &str = "/sys/class/thermal/thermal_zone*";
pub const LOADAVG: &str = "/proc/loadavg";
pub const MEMINFO: &str = "/proc/meminfo";

/// A timestamped reading from one source, normalized to its display unit.
///
/// [`Sample`] is exported for callers that want raw labeled readings; the
/// sweep itself produces [`Snapshot`]s.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub label: String,
    pub value: f32,
    pub unit: Unit,
}

/// Display unit for a sample value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Degrees Celsius.
    Celsius,
    /// Volts, displayed with four decimals like vcgencmd reports them.
    Volts,
    /// Megahertz.
    Megahertz,
    /// System load (from /proc/loadavg).
    Load,
    /// Percent of total memory in use.
    MemoryPercent,
    /// Decibel milliwatts, Wi-Fi signal strength.
    Rssi,
    /// Millivolts straight from the RP1 ADC inputs.
    Millivolts,
}

impl Unit {
    pub fn suffix(self) -> &'static str {
        match self {
            Unit::Celsius => "°C",
            Unit::Volts => "V",
            Unit::Megahertz => "MHz",
            Unit::Load => "",
            Unit::MemoryPercent => "%",
            Unit::Rssi => "dBm",
            Unit::Millivolts => "mV",
        }
    }
}

/// Sticky firmware health bits from `vcgencmd get_throttled`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThrottleState {
    pub undervolted_now: bool,
    pub freq_capped_now: bool,
    pub throttled_now: bool,
    pub soft_temp_limit_now: bool,
    pub undervolted_past: bool,
    pub freq_capped_past: bool,
    pub throttled_past: bool,
    pub soft_temp_limit_past: bool,
}

impl ThrottleState {
    /// Bit meanings from the firmware: bits 0-3 are live conditions,
    /// bits 16-23 are sticky occurrences since the last reboot.
    pub fn from_bits(bits: u32) -> Self {
        Self {
            undervolted_now: bits & (1 << 0) != 0,
            freq_capped_now: bits & (1 << 1) != 0,
            throttled_now: bits & (1 << 2) != 0,
            soft_temp_limit_now: bits & (1 << 3) != 0,
            undervolted_past: bits & (1 << 16) != 0,
            freq_capped_past: bits & (1 << 17) != 0,
            throttled_past: bits & (1 << 18) != 0,
            soft_temp_limit_past: bits & (1 << 19) != 0,
        }
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let hex = text.trim().trim_start_matches("throttled=0x");
        u32::from_str_radix(hex, 16).ok().map(Self::from_bits)
    }

    /// True when any live condition is active.
    pub fn active_now(&self) -> bool {
        self.undervolted_now
            || self.freq_capped_now
            || self.throttled_now
            || self.soft_temp_limit_now
    }

    /// True when anything has happened since boot, live or historical.
    pub fn any_event(&self) -> bool {
        self.active_now()
            || self.undervolted_past
            || self.freq_capped_past
            || self.throttled_past
            || self.soft_temp_limit_past
    }

    /// Short human summary, e.g. `OK`, `throttled now`, `undervolt since boot`.
    pub fn describe(&self) -> String {
        let mut live = Vec::new();
        if self.undervolted_now {
            live.push("undervoltage");
        }
        if self.freq_capped_now {
            live.push("freq capped");
        }
        if self.throttled_now {
            live.push("throttled");
        }
        if self.soft_temp_limit_now {
            live.push("soft temp limit");
        }
        if !live.is_empty() {
            return format!("throttling now: {}", live.join(", "));
        }

        let mut past = Vec::new();
        if self.undervolted_past {
            past.push("undervoltage");
        }
        if self.freq_capped_past {
            past.push("freq cap");
        }
        if self.throttled_past {
            past.push("throttle");
        }
        if self.soft_temp_limit_past {
            past.push("soft temp");
        }
        if past.is_empty() {
            "healthy".into()
        } else {
            format!("since boot: {}", past.join(", "))
        }
    }
}

/// One full sensor sweep, used directly as a chart data point.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Milliseconds since app start, for the chart x axis.
    pub elapsed_ms: u64,
    pub soc_temp_c: Option<f32>,
    pub nvme_temp_c: Option<f32>,
    pub rp1_temp_c: Option<f32>,
    pub pmic_temp_c: Option<f32>,
    pub core_volts: Option<f32>,
    pub sdram_volatile_volts: Option<f32>,
    pub sdram_phy_volts: Option<f32>,
    pub arm_clock_mhz: Option<f32>,
    pub v3d_clock_mhz: Option<f32>,
    pub core_clock_mhz: Option<f32>,
    /// Every firmware clock domain that answered, MHz.
    pub all_clocks_mhz: BTreeMap<String, f32>,
    pub load_1m: Option<f32>,
    pub load_5m: Option<f32>,
    pub load_15m: Option<f32>,
    /// Seconds since boot, from /proc/uptime.
    pub uptime_secs: Option<u64>,
    pub rssi_dbm: Option<f32>,
    pub throttle: ThrottleState,
    /// Level of the firmware undervoltage alarm (`rpi_volt` hwmon).
    pub undervolt_alarm: bool,
    /// Why this sweep is mostly empty, if it is. Rate metrics (cores,
    /// utilization, disk and network throughput) are attached separately
    /// by the stateful [`crate::sweep::Sweeper`].
    pub error: Option<String>,
    /// Per-core utilization 0.0..=1.0, one entry per core.
    pub cores: Vec<CoreState>,
    /// Aggregate CPU utilization across all cores, 0.0..=1.0.
    pub utilization_pct: Option<f32>,
    /// Memory block: percents and MiB readouts.
    pub memory: Option<MemoryInfo>,
    /// NVMe throughput since the previous sweep, MiB/s.
    pub disk_io: Option<DiskIo>,
    /// wlan0 throughput since the previous sweep, KiB/s.
    pub net_io: Option<NetIo>,
    /// Enabled voltage regulators, volts, from /sys/class/regulator.
    pub regulators_v: BTreeMap<String, f32>,
    /// Raw RP1 ADC inputs in millivolts, keyed by input number (in1..in4).
    pub rp1_adc_mv: BTreeMap<u8, f32>,
    /// Extra temperature zones found in /sys/class/thermal beyond zone0,
    /// e.g. on other boards or after a user adds a sensor overlay.
    pub extra_zones_c: BTreeMap<String, f32>,
}

impl Snapshot {
    /// Highest available temperature, for the health badge.
    pub fn max_temp_c(&self) -> Option<f32> {
        self.soc_temp_c
            .into_iter()
            .chain(self.nvme_temp_c)
            .chain(self.rp1_temp_c)
            .chain(self.pmic_temp_c)
            .chain(self.extra_zones_c.values().copied())
            .fold(None, |acc, t| match acc {
                Some(best) if best >= t => Some(best),
                _ => Some(t),
            })
    }
}

/// Errors a probe run can surface. Reserved for typed failures; the sweep
/// currently reports problems through [`Snapshot::error`] strings instead.
#[derive(Debug)]
pub enum SnapshotError {
    /// The `vcgencmd` binary is missing or not executable.
    VcgencmdMissing,
    /// Any other failure; the message names the source.
    Other(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::VcgencmdMissing => {
                write!(f, "vcgencmd not found at {VCGENCMD}")
            }
            SnapshotError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// Abstract sensor source. Real systems use [`SystemProbe`]; tests inject
/// [`sensors`-level fakes] through the same trait.
pub trait Probe {
    /// Read a whole file as lossy UTF-8.
    fn read_file(&self, path: &Path) -> Option<String>;
    /// Expand a glob pattern and return matching paths.
    fn glob(&self, pattern: &str) -> Vec<PathBuf>;
    /// Run vcgencmd with the given arguments, returning trimmed stdout.
    fn vcgencmd(&self, args: &[&str]) -> Option<String>;
}

/// Probe against the real Pi filesystem and firmware.
pub struct SystemProbe {
    vcgencmd_path: PathBuf,
}

impl Default for SystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemProbe {
    pub fn new() -> Self {
        Self {
            vcgencmd_path: PathBuf::from(VCGENCMD),
        }
    }

    /// True when this machine looks like a Raspberry Pi we can probe.
    /// Checks for the firmware device tree blob; harmless elsewhere.
    pub fn device_supported(&self) -> bool {
        self.read_file(Path::new("/proc/device-tree/model"))
            .map(|raw| raw.contains("Raspberry Pi"))
            .unwrap_or(false)
    }
}

impl Probe for SystemProbe {
    fn read_file(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn glob(&self, pattern: &str) -> Vec<PathBuf> {
        let mut paths = glob_paths(pattern);
        paths.sort();
        paths
    }

    fn vcgencmd(&self, args: &[&str]) -> Option<String> {
        let out = std::process::Command::new(&self.vcgencmd_path)
            .args(args)
            .output()
            .ok()?;
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            None
        }
    }
}

/// Minimal glob supporting one trailing `*` component, which is all the
/// sensor paths in this app need. Avoids a regex or glob dependency.
fn glob_paths(pattern: &str) -> Vec<PathBuf> {
    let path = Path::new(pattern);
    let pattern = pattern.to_string();
    let Some(stem) = path.parent() else {
        return Vec::new();
    };
    let Some(last) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    if !last.contains('*') {
        let p = PathBuf::from(&pattern);
        return p.exists().then_some(p).into_iter().collect();
    }
    let prefix = last.split('*').next().unwrap_or_default().to_string();
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(stem) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) {
                out.push(entry.path());
            }
        }
    }
    out
}

/// Parse `vcgencmd measure_temp` style output: `temp=39.5'C`, also matching
/// the `measure_temp pmic` variant and CJK-locale digits.
pub fn parse_gencmd_temp(output: &str) -> Option<f32> {
    let value = output.trim().strip_prefix("temp=")?;
    let value = value.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.');
    value.parse::<f32>().ok().filter(|t| t.is_finite())
}

/// Parse `volt=0.8635V` style output into volts.
pub fn parse_gencmd_value(output: &str, prefix: &str) -> Option<f32> {
    let value = output.trim().strip_prefix(prefix)?;
    let value = value.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.');
    value.parse::<f32>().ok().filter(|v| v.is_finite())
}

/// Parse one sysfs temperature file in millidegrees (`37500` = 37.5 C).
pub fn parse_millidegrees(raw: &str) -> Option<f32> {
    raw.trim().parse::<f32>().ok().map(|m| m / 1000.0)
}

/// Parse a `MemTotal`/`MemAvailable` line in kB.
pub fn parse_meminfo_kb(raw: &str, key: &str) -> Option<u64> {
    for line in raw.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == key {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Parse the first load average out of `/proc/loadavg`.
pub fn parse_loadavg(raw: &str) -> Option<f32> {
    raw.split_whitespace().next()?.parse().ok()
}

/// Parse the dBm level out of one `/proc/net/wireless` data line:
/// `wlan0: 0000  67.  -43.  -256 ...` — fields are iface, status, link
/// quality, level (dBm), noise. The level is the first negative field.
pub fn parse_rssi_dbm(raw: &str) -> Option<f32> {
    raw.split_whitespace()
        .filter_map(|tok| tok.parse::<f32>().ok())
        .find(|v| *v < 0.0)
}

/// Parse `/proc/uptime` into seconds (the first field is a float).
pub fn parse_uptime_secs(raw: &str) -> Option<u64> {
    let secs: f64 = raw.split_whitespace().next()?.parse().ok()?;
    u64::try_from(secs.floor() as i64).ok()
}

/// Per-core tick counters from `/proc/stat`, skipping the `cpu` aggregate
/// line. Order matches `cpu0`, `cpu1`, ...
pub fn parse_core_times(raw: &str) -> Vec<CpuTimes> {
    raw.lines()
        .filter_map(|line| line.strip_prefix("cpu"))
        .filter_map(|rest| {
            // `cpu ` (aggregate) has no digits after the prefix; `cpu0 `
            // does, and the index itself is not a tick value.
            let fields = rest.trim_start_matches(|c: char| c.is_ascii_digit());
            (fields.len() < rest.len()).then(|| CpuTimes::parse(fields))?
        })
        .collect()
}

/// (`sectors_read`, `sectors_written`) for one disk from `/proc/diskstats`.
/// Partition lines (`nvme0n1p1`) are skipped; only the whole device.
pub fn parse_diskstats_sectors(raw: &str, device: &str) -> Option<(u64, u64)> {
    for line in raw.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(2) == Some(&device) {
            let read: u64 = fields.get(5)?.parse().ok()?;
            let written: u64 = fields.get(9)?.parse().ok()?;
            return Some((read, written));
        }
    }
    None
}

/// Pick the primary disk name from `/proc/diskstats`: the first line whose
/// device looks like a whole NVMe or MMC device, not a partition.
pub fn parse_primary_disk(raw: &str) -> Option<String> {
    for line in raw.lines() {
        let Some(name) = line.split_whitespace().nth(2) else {
            continue;
        };
        let is_nvme = name.starts_with("nvme") && !name.contains('p');
        let is_mmc = name.starts_with("mmcblk") && !name.contains('p');
        if is_nvme || is_mmc {
            return Some(name.to_string());
        }
    }
    None
}

/// (`rx_bytes`, `tx_bytes`) for one interface from `/proc/net/dev`.
pub fn parse_net_dev_bytes(raw: &str, iface: &str) -> Option<(u64, u64)> {
    for line in raw.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() != iface {
            continue;
        }
        let nums: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|f| f.parse().ok())
            .collect();
        let rx = *nums.first()?;
        let tx = *nums.get(8)?;
        return Some((rx, tx));
    }
    None
}

/// First wireless-looking interface name from `/proc/net/dev` (`wl*`),
/// matching what the RSSI reader listens to.
pub fn parse_primary_wireless(raw: &str) -> Option<String> {
    raw.lines().find_map(|line| {
        let name = line.split(':').next()?.trim();
        name.starts_with("wl").then(|| name.to_string())
    })
}

/// Enabled regulators (name, volts) from a `/sys/class/regulator` glob.
/// Disabled rails are skipped; microvolts of 0 means "unknown".
pub fn parse_regulators(probe: &dyn Probe) -> BTreeMap<String, f32> {
    let mut out = BTreeMap::new();
    for dir in probe.glob(REGULATOR_GLOB) {
        if probe
            .read_file(&dir.join("state"))
            .map(|s| s.trim() != "enabled")
            .unwrap_or(true)
        {
            continue;
        }
        let Some(name) = probe
            .read_file(&dir.join("name"))
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        let Some(uv) = probe
            .read_file(&dir.join("microvolts"))
            .as_deref()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|uv| *uv > 0)
        else {
            continue;
        };
        out.insert(name, uv as f32 / 1_000_000.0);
    }
    out
}

/// All firmware clock domains, MHz, from `vcgencmd measure_clock <d>`.
pub fn parse_all_clocks(probe: &dyn Probe, domains: &[&str]) -> BTreeMap<String, f32> {
    let mut out = BTreeMap::new();
    for domain in domains {
        if let Some(mhz) = crate::sweep::clock_mhz(probe, domain) {
            out.insert((*domain).to_string(), mhz);
        }
    }
    out
}

/// Clock domains worth listing; the charted three are handled separately.
pub const CLOCK_DOMAINS: &[&str] = &[
    "arm", "core", "h264", "isp", "v3d", "uart", "pwm", "emmc", "pixel", "vec", "hdmi", "dpi",
];

/// Start-of-app instant so snapshots can carry a monotonic x value.
#[derive(Clone, Copy)]
pub struct Clock(pub Instant);

impl Clock {
    pub fn started() -> Self {
        Self(Instant::now())
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
}

/// Raw CPU tick counters for one core, from /proc/stat.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CpuTimes {
    pub idle: u64,
    pub total: u64,
}

impl CpuTimes {
    /// Parse a `/proc/stat` cpu line (the `cpuN ` prefix is already split
    /// off): user nice system idle iowait irq softirq steal ...
    pub fn parse(fields: &str) -> Option<Self> {
        let nums: Vec<u64> = fields
            .split_whitespace()
            .filter_map(|f| f.parse().ok())
            .collect();
        if nums.len() < 4 {
            return None;
        }
        let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
        let total: u64 = nums.iter().sum();
        Some(Self { idle, total })
    }

    /// Utilization between two samples, 0.0..=1.0. When total did not
    /// advance (or went backwards, after a counter reset) there is no
    /// measurable delta and the result is `None`.
    pub fn utilization_since(&self, prev: &Self) -> Option<f32> {
        let total = self.total.checked_sub(prev.total)?;
        let idle = self.idle.checked_sub(prev.idle)?;
        if total == 0 {
            return None;
        }
        let busy = total.saturating_sub(idle);
        Some((busy as f32 / total as f32).clamp(0.0, 1.0))
    }
}

/// One CPU core's utilization for the current snapshot, 0.0..=1.0.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CoreState {
    pub utilization: f32,
}

/// Memory usage block for the gauges.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemoryInfo {
    pub used_pct: f32,
    pub buff_cache_pct: f32,
    pub swap_used_pct: Option<f32>,
    pub total_mib: f32,
    pub used_mib: f32,
}

/// Disk throughput block, MiB per second.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DiskIo {
    pub read_mib_s: f32,
    pub write_mib_s: f32,
}

/// Network throughput block, KiB per second.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetIo {
    pub rx_kib_s: f32,
    pub tx_kib_s: f32,
}

/// Where the standard probe looks on a real system.
pub const STAT: &str = "/proc/stat";
pub const UPTIME: &str = "/proc/uptime";
pub const DISKSTATS: &str = "/proc/diskstats";
pub const NET_DEV: &str = "/proc/net/dev";
pub const REGULATOR_GLOB: &str = "/sys/class/regulator/regulator*";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gencmd_temps() {
        assert_eq!(parse_gencmd_temp("temp=39.5'C"), Some(39.5));
        assert_eq!(parse_gencmd_temp("temp=44.0'C"), Some(44.0));
        assert_eq!(parse_gencmd_temp("temp=48'C"), Some(48.0));
        assert_eq!(parse_gencmd_temp("junk"), None);
        assert_eq!(parse_gencmd_temp("temp='C"), None);
    }

    #[test]
    fn parses_gencmd_values() {
        assert_eq!(parse_gencmd_value("volt=0.8635V", "volt="), Some(0.8635));
        assert_eq!(parse_gencmd_value("volt=1.1V", "volt="), Some(1.1));
        assert_eq!(parse_gencmd_value("freq(48)=2400000000", "volt="), None);
    }

    #[test]
    fn parses_throttle_bits() {
        let clean = ThrottleState::from_hex("throttled=0x0").unwrap();
        assert!(!clean.any_event());
        assert_eq!(clean.describe(), "healthy");

        let now = ThrottleState::from_bits(0b100);
        assert!(now.active_now());
        assert_eq!(now.describe(), "throttling now: throttled");

        let past = ThrottleState::from_bits(1 << 16);
        assert!(!past.active_now());
        assert!(past.any_event());
        assert_eq!(past.describe(), "since boot: undervoltage");

        let combo = ThrottleState::from_bits((1 << 0) | (1 << 16) | (1 << 17));
        assert!(combo.active_now());
        assert_eq!(combo.describe(), "throttling now: undervoltage");
    }

    #[test]
    fn parses_millidegrees_meminfo_load() {
        assert_eq!(parse_millidegrees("37500\n"), Some(37.5));
        assert_eq!(parse_millidegrees("48321"), Some(48.321));
        assert_eq!(parse_millidegrees(""), None);

        let meminfo = "MemTotal:       15990780 kB\nMemAvailable:   14200000 kB\nSwapTotal:       2048000 kB\n";
        assert_eq!(parse_meminfo_kb(meminfo, "MemTotal"), Some(15990780));
        assert_eq!(parse_meminfo_kb(meminfo, "MemAvailable"), Some(14200000));
        assert_eq!(parse_meminfo_kb(meminfo, "SwapFree"), None);
        assert_eq!(parse_meminfo_kb(meminfo, "Missing"), None);

        assert_eq!(parse_loadavg("2.10 1.85 1.62 4/1234 5678\n"), Some(2.10));
        assert_eq!(parse_loadavg(""), None);
    }

    #[test]
    fn parses_rssi() {
        assert_eq!(parse_rssi_dbm("wlan0: 0000   67.  -43.  -256"), Some(-43.0));
        assert_eq!(parse_rssi_dbm("no numbers here"), None);
        assert_eq!(parse_rssi_dbm("0 40 20"), None);
    }

    #[test]
    fn cpu_times_and_utilization() {
        let prev = CpuTimes::parse("100 0 100 700 0 0 0").unwrap();
        assert_eq!(prev.idle, 700);
        assert_eq!(prev.total, 900);
        // Next sample: 150 busy ticks added, 50 idle ticks added.
        let next = CpuTimes::parse("250 0 100 750 0 0 0").unwrap();
        let util = next.utilization_since(&prev).unwrap();
        // Delta total 200, delta idle 50 -> busy 150.
        assert!((util - 0.75).abs() < 1e-6);
        // No tick advance -> None, not a divide-by-zero.
        assert_eq!(next.utilization_since(&next), None);
        // iowait counts as idle.
        let with_wait = CpuTimes::parse("0 0 0 100 50 0 0").unwrap();
        assert_eq!(with_wait.idle, 150);
        assert_eq!(CpuTimes::parse("junk"), None);
    }

    #[test]
    fn parses_core_times_skipping_aggregate() {
        let raw = "cpu  10 0 10 90 0 0 0\ncpu0 1 0 1 8 0 0 0\ncpu1 2 0 2 6 0 0 0\nintr 0\n";
        let cores = parse_core_times(raw);
        assert_eq!(cores.len(), 2);
        assert_eq!(cores[0].idle, 8);
        assert_eq!(cores[1].total, 10);
        assert!(parse_core_times("cpu 1 2 3 4").is_empty());
    }

    #[test]
    fn parses_uptime_diskstats_netdev() {
        assert_eq!(parse_uptime_secs("9876.54 12345.67\n"), Some(9876));
        assert_eq!(parse_uptime_secs(""), None);

        let disks = "   8       0 nvme0n1 4379 41 273090 2982 631 1053 15534 1494 0 4476 4476 0 0 0 0 0 0\n   8       1 nvme0n1p1 100 0 800 20 5 0 10 5 0 25 25 0 0 0 0 0 0\n";
        assert_eq!(
            parse_diskstats_sectors(disks, "nvme0n1"),
            Some((273090, 15534))
        );
        assert_eq!(parse_diskstats_sectors(disks, "missing"), None);
        assert_eq!(parse_primary_disk(disks).as_deref(), Some("nvme0n1"));
        // Partition-looking names never win.
        let parts = "   1  2 nvme0n1p1 1 1 1 1 1 1 1 1 0 1 1\n";
        assert_eq!(parse_primary_disk(parts), None);

        let net = "  eth0: 1000 0 0 0 0 0 0 0 2000 0 0 0 0 0 0 0\n  wlan0: 500 0 0 0 0 0 0 0 700 0 0 0 0 0 0 0\n";
        assert_eq!(parse_net_dev_bytes(net, "wlan0"), Some((500, 700)));
        assert_eq!(parse_net_dev_bytes(net, "missing"), None);
        assert_eq!(parse_primary_wireless(net).as_deref(), Some("wlan0"));
    }

    #[test]
    fn max_temp_prefers_highest() {
        let snap = Snapshot {
            soc_temp_c: Some(48.8),
            nvme_temp_c: Some(37.9),
            pmic_temp_c: Some(46.6),
            extra_zones_c: BTreeMap::from([("hat".into(), 51.0)]),
            ..Default::default()
        };
        assert_eq!(snap.max_temp_c(), Some(51.0));
    }
}
