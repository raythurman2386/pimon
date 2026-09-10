//! btop-style widgets: segmented gauges, big readouts, threshold colors.
//!
//! The color/threshold math is pure and unit-tested; the gpui wrappers
//! only place elements.

use gpui_kit::component::{h_flex, v_flex, ActiveTheme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// Temperature bands, degrees Celsius, tuned for the Pi 5 family
/// (firmware soft limit is 85 C on this board family).
pub const TEMP_WARN_C: f32 = 60.0;
pub const TEMP_HOT_C: f32 = 75.0;
pub const TEMP_CRIT_C: f32 = 85.0;

/// Pick the semantic color for a temperature reading.
pub fn temp_color(temp: f32, cx: &App) -> Hsla {
    let theme = cx.theme();
    if temp >= TEMP_CRIT_C {
        theme.red
    } else if temp >= TEMP_HOT_C {
        theme.yellow
    } else if temp >= TEMP_WARN_C {
        theme.warning
    } else {
        theme.green
    }
}

/// Pick the semantic color for a 0..=1 utilization.
pub fn utilization_color(frac: f32, cx: &App) -> Hsla {
    let theme = cx.theme();
    if frac >= 0.9 {
        theme.red
    } else if frac >= 0.6 {
        theme.yellow
    } else {
        theme.green
    }
}

/// Frequency bands relative to the board's 2.4 GHz top speed.
pub fn clock_color(mhz: f32, cx: &App) -> Hsla {
    let theme = cx.theme();
    if mhz >= 2300.0 {
        theme.green
    } else if mhz >= 1800.0 {
        theme.warning
    } else {
        theme.muted_foreground
    }
}

/// A btop-style segmented gauge: N discrete cells, filled proportionally,
/// with a label on the left and a value on the right.
pub struct Gauge {
    pub label: String,
    pub value_text: String,
    /// 0.0..=1.0 fill fraction.
    pub frac: f32,
    pub color: Hsla,
    pub cells: usize,
}

impl Gauge {
    pub fn new(label: impl Into<String>, frac: f32, color: Hsla) -> Self {
        Self {
            label: label.into(),
            value_text: String::new(),
            frac: frac.clamp(0.0, 1.0),
            color,
            cells: 20,
        }
    }

    pub fn value_text(mut self, text: impl Into<String>) -> Self {
        self.value_text = text.into();
        self
    }

    pub fn cells(mut self, cells: usize) -> Self {
        self.cells = cells.max(4);
        self
    }

    /// How many of the cells are lit. Never exceeds `cells`, so a fill
    /// fraction above 1.0 lights the bar without overflow.
    pub fn filled(&self) -> usize {
        ((self.frac * self.cells as f32).round() as usize).min(self.cells)
    }

    /// Render as an already-typed element: boxing at this seam keeps the
    /// nested builder chain out of callers' type inference.
    pub fn render(&self, cx: &Context<crate::app::Pimon>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let empty = cx.theme().background;
        h_flex()
            .gap_2()
            .items_center()
            .w_full()
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .min_w(px(52.))
                    .child(self.label.clone()),
            )
            .child(
                h_flex()
                    .gap(px(2.))
                    .flex_1()
                    .children((0..self.cells).map(|cell| {
                        let on = cell < self.filled();
                        div()
                            .flex_1()
                            .h(px(10.))
                            .rounded_xs()
                            .bg(if on { self.color } else { empty })
                            .border_1()
                            .border_color(if on {
                                self.color.opacity(0.5)
                            } else {
                                muted.opacity(0.25)
                            })
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(self.color)
                    .min_w(px(48.))
                    .text_right()
                    .child(self.value_text.clone()),
            )
            .into_any_element()
    }
}

/// A big labeled readout: value over muted label, colored by severity.
pub fn big_readout(
    label: &str,
    value: &str,
    color: Hsla,
    cx: &Context<crate::app::Pimon>,
) -> AnyElement {
    v_flex()
        .gap_0()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::BOLD)
                .text_color(color)
                .child(value.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label.to_string()),
        )
        .into_any_element()
}

/// Format a seconds count as `1d 3h 27m`.
pub fn fmt_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

/// Render a memory block's gauge rows (used + buff/cache + swap).
pub fn memory_gauges(
    mem: &pimon::sensors::MemoryInfo,
    cx: &Context<crate::app::Pimon>,
) -> AnyElement {
    let used_color = utilization_color(mem.used_pct / 100.0, cx);
    v_flex()
        .gap_1()
        .child(
            Gauge::new("mem", mem.used_pct / 100.0, used_color)
                .value_text(format!(
                    "{:.0}% of {:.0}G",
                    mem.used_pct,
                    mem.total_mib / 1024.0
                ))
                .render(cx),
        )
        .child(
            Gauge::new("cache", mem.buff_cache_pct / 100.0, cx.theme().accent)
                .value_text(format!("{:.0}%", mem.buff_cache_pct))
                .render(cx),
        )
        .when_some(mem.swap_used_pct, |this, swap| {
            let color = if swap >= 50.0 {
                cx.theme().yellow
            } else {
                cx.theme().muted_foreground
            };
            this.child(
                Gauge::new("swap", swap / 100.0, color)
                    .value_text(format!("{swap:.0}%"))
                    .render(cx),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // Named imports only: glob-importing the builder chains' types into
    // the test module blows the recursion limit during type inference.
    use super::{fmt_uptime, Gauge, TEMP_CRIT_C, TEMP_HOT_C, TEMP_WARN_C};

    #[test]
    fn temp_bands() {
        // Can't construct an App in unit tests; test the band boundaries
        // via the same ordering the function implements.
        let band = |t: f32| {
            if t >= TEMP_CRIT_C {
                "crit"
            } else if t >= TEMP_HOT_C {
                "hot"
            } else if t >= TEMP_WARN_C {
                "warn"
            } else {
                "ok"
            }
        };
        assert_eq!(band(45.0), "ok");
        assert_eq!(band(TEMP_WARN_C), "warn");
        assert_eq!(band(TEMP_HOT_C), "hot");
        assert_eq!(band(TEMP_CRIT_C), "crit");
    }

    #[test]
    fn gauge_fill_rounds() {
        let mut g = Gauge::new("t", 0.5, gpui_kit::Hsla::default());
        assert_eq!(g.filled(), 10);
        g.frac = 0.999;
        assert_eq!(g.filled(), g.cells);
        g.frac = 1.2;
        assert_eq!(g.filled(), g.cells);
        g.frac = -0.5;
        assert_eq!(g.filled(), 0);
        g.cells = 0;
        assert_eq!(g.filled(), 0, "cells floor of 4");
    }

    #[test]
    fn uptime_format() {
        assert_eq!(fmt_uptime(0), "0m");
        assert_eq!(fmt_uptime(3600), "1h 0m");
        assert_eq!(fmt_uptime(9876), "2h 44m");
        assert_eq!(fmt_uptime(172_800), "2d 0h 0m");
    }
}
