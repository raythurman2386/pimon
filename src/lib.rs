//! Testable Raspberry Pi sensor backend, shared by the GPUI Kit UI.
//!
//! Every read goes through [`Probe`], an abstract source that unit tests
//! replace with canned sysfs/vcgencmd output, so the parsing, history and
//! threshold logic are covered without touching the host.

pub mod sensors;
pub mod sweep;
pub mod theme;

pub use sensors::{
    parse_gencmd_temp, Probe, Sample, Snapshot, SnapshotError, ThrottleState, CPUFREQ_GLOB,
    I2C_GLOB, LOADAVG, MEMINFO, THERMAL_GLOB, VCGENCMD,
};
pub use sweep::collect_snapshot;
pub use theme::{parse_hex_color, OmarchyPalette, RgbaColor};
