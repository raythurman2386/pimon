//! Testable Raspberry Pi sensor backend, shared by the GPUI Kit UI.
//!
//! Every read goes through [`Probe`], an abstract source that unit tests
//! replace with canned sysfs/vcgencmd output, so the parsing, history and
//! threshold logic are covered without touching the host.

// Widget builder chains are deep expression trees; keep the macro
// expansion budget generous.
#![recursion_limit = "512"]

pub mod sensors;
pub mod sweep;
pub mod theme;

pub use sensors::{
    parse_gencmd_temp, CpuTimes, Probe, Sample, Snapshot, SnapshotError, ThrottleState,
    CLOCK_DOMAINS, LOADAVG, MEMINFO, THERMAL_GLOB, VCGENCMD,
};
pub use sweep::collect_snapshot;
pub use theme::{parse_hex_color, OmarchyPalette, RgbaColor};
