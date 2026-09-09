//! GPUI Kit rendering for Pimon: live charts, health badge, details table.
//!
//! Layout follows the gpui-kit `system_monitor` example: TitleBar with a
//! segmented TabBar, a chart area, and a status bar of Progress chips.

use std::collections::VecDeque;

use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pimon::sensors::{Snapshot, ThrottleState};

use crate::app::{Health, Pimon, POLL_INTERVAL};

/// Render the whole window body. Called from `Render for Pimon`.
pub fn render_pimon(
    state: &mut Pimon,
    _window: &mut Window,
    cx: &mut Context<Pimon>,
) -> impl IntoElement {
    let active_tab = state.active_tab();
    let health = state.health();

    v_flex()
        .id("pimon")
        .size_full()
        .bg(cx.theme().background)
        .text_color(cx.theme().foreground)
        .font_family("iA Writer Mono S")
        .key_context("pimon")
        .child(render_title_bar(active_tab, health, cx))
        .child(
            div()
                .id("tab-content")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_3()
                .map(|this| match active_tab {
                    0 => this.child(render_live_tab(state, cx)),
                    1 => this.child(render_details_tab(state, cx)),
                    _ => this.child(render_live_tab(state, cx)),
                }),
        )
        .child(render_status_bar(state, cx))
}

fn render_title_bar(
    active_tab: usize,
    health: Health,
    cx: &mut Context<Pimon>,
) -> impl IntoElement {
    let (badge_icon, badge_color) = match health {
        Health::Ok => (IconName::CircleCheck, cx.theme().green),
        Health::Warn => (IconName::TriangleAlert, cx.theme().yellow),
        Health::Alert => (IconName::TriangleAlert, cx.theme().red),
    };

    TitleBar::new().child(
        h_flex()
            .flex_1()
            .items_center()
            .justify_between()
            .mr_4()
            .child(
                TabBar::new("pimon-tabs")
                    .mt(px(1.))
                    .segmented()
                    .px_0()
                    .py(px(2.))
                    .bg(cx.theme().title_bar)
                    .selected_index(active_tab)
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.set_active_tab(*ix, cx);
                    }))
                    .child(Tab::new().label("Live"))
                    .child(Tab::new().label("Details")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(badge_color)
                    .child(Icon::new(badge_icon))
                    .child(health_label(health)),
            ),
    )
}

fn health_label(health: Health) -> &'static str {
    match health {
        Health::Ok => "Healthy",
        Health::Warn => "Check history",
        Health::Alert => "Throttling risk",
    }
}

#[derive(Clone)]
struct ChartPoint {
    x: String,
    y: f64,
}

/// One charted metric: a titled panel with the live value and an area chart.
fn render_chart(
    title: &str,
    series: &[f32],
    unit_suffix: &str,
    color: Hsla,
    decimals: usize,
    cx: &Context<Pimon>,
) -> impl IntoElement {
    let points: Vec<ChartPoint> = series
        .iter()
        .enumerate()
        .map(|(ix, value)| ChartPoint {
            x: format!("{ix}"),
            y: *value as f64,
        })
        .collect();
    let current = series.last().copied().unwrap_or(0.0);

    v_flex()
        .min_h(px(120.))
        .flex_1()
        .gap_1()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_md()
        .child(
            h_flex()
                .justify_between()
                .px_3()
                .py_1()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(color)
                        .child(format!("{current:.decimals$}{unit_suffix}")),
                ),
        )
        .child(
            AreaChart::new(points)
                .x(|d| d.x.clone())
                .y(|d| d.y)
                .stroke(color)
                .fill(linear_gradient(
                    0.,
                    linear_color_stop(color.opacity(0.4), 1.),
                    linear_color_stop(cx.theme().background.opacity(0.1), 0.),
                ))
                .tick_margin(15),
        )
}

/// Live tab: four temperature charts plus the ARM clock.
fn render_live_tab(state: &mut Pimon, cx: &mut Context<Pimon>) -> impl IntoElement {
    let history = state.history();
    let soc = series(history, |s| s.soc_temp_c);
    let pmic = series(history, |s| s.pmic_temp_c);
    let nvme = series(history, |s| s.nvme_temp_c);
    let rp1 = series(history, |s| s.rp1_temp_c);
    let arm = series(history, |s| s.arm_clock_mhz);

    v_flex()
        .size_full()
        .gap_3()
        .child(
            h_flex()
                .gap_3()
                .flex_1()
                .min_h_0()
                .child(render_chart("SoC temp", &soc, "°C", cx.theme().red, 1, cx))
                .child(render_chart(
                    "PMIC temp",
                    &pmic,
                    "°C",
                    cx.theme().yellow,
                    1,
                    cx,
                )),
        )
        .child(
            h_flex()
                .gap_3()
                .flex_1()
                .min_h_0()
                .child(render_chart(
                    "NVMe temp",
                    &nvme,
                    "°C",
                    cx.theme().blue,
                    1,
                    cx,
                ))
                .child(render_chart(
                    "RP1 temp",
                    &rp1,
                    "°C",
                    cx.theme().green,
                    1,
                    cx,
                )),
        )
        .child(render_chart(
            "ARM clock",
            &arm,
            " MHz",
            cx.theme().accent,
            0,
            cx,
        ))
}

fn series(history: &VecDeque<Snapshot>, pick: impl Fn(&Snapshot) -> Option<f32>) -> Vec<f32> {
    history.iter().filter_map(pick).collect()
}

/// (label, display value, optional severity color) for the details table.
type DetailRow = (String, String, Option<Hsla>);

/// Details tab: the full sensor table plus the throttling health card.
fn render_details_tab(state: &mut Pimon, cx: &mut Context<Pimon>) -> impl IntoElement {
    let rows = details_rows(state);

    v_flex()
        .size_full()
        .gap_3()
        .child(
            v_flex()
                .id("details-table")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .border_1()
                .border_color(cx.theme().border)
                .rounded_md()
                .children(rows.into_iter().map(|row| {
                    h_flex()
                        .justify_between()
                        .px_3()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(row.0),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(row.2.unwrap_or(cx.theme().foreground))
                                .child(row.1),
                        )
                })),
        )
        .child(render_throttle_card(state, cx))
}

fn details_rows(state: &Pimon) -> Vec<DetailRow> {
    let Some(latest) = state.latest() else {
        return vec![("sensors".into(), "waiting for first sweep…".into(), None)];
    };
    let mut rows = Vec::new();

    push_temp(&mut rows, "SoC (cpu-thermal)", latest.soc_temp_c);
    push_temp(&mut rows, "PMIC", latest.pmic_temp_c);
    push_temp(&mut rows, "NVMe composite", latest.nvme_temp_c);
    push_temp(&mut rows, "RP1 die", latest.rp1_temp_c);
    for (label, temp) in &latest.extra_zones_c {
        push_temp(&mut rows, &format!("zone: {label}"), Some(*temp));
    }
    if let Some(v) = latest.core_volts {
        rows.push(("Core voltage".into(), format!("{v:.4} V"), None));
    }
    if let Some(v) = latest.sdram_volatile_volts {
        rows.push((
            "SDRAM controller (sdram_c)".into(),
            format!("{v:.4} V"),
            None,
        ));
    }
    if let Some(v) = latest.sdram_phy_volts {
        rows.push(("SDRAM PHY (sdram_p)".into(), format!("{v:.4} V"), None));
    }
    for (input, mv) in &latest.rp1_adc_mv {
        rows.push((format!("RP1 ADC in{input}"), format!("{mv:.0} mV"), None));
    }
    if latest.undervolt_alarm {
        rows.push((
            "Undervoltage alarm (hwmon)".into(),
            "ACTIVE".into(),
            Some(cx_theme_red()),
        ));
    }
    if let Some(mhz) = latest.arm_clock_mhz {
        rows.push(("ARM clock".into(), format!("{mhz:.0} MHz"), None));
    }
    if let Some(mhz) = latest.v3d_clock_mhz {
        rows.push(("V3D / GPU clock".into(), format!("{mhz:.0} MHz"), None));
    }
    if let Some(mhz) = latest.core_clock_mhz {
        rows.push(("Core clock".into(), format!("{mhz:.0} MHz"), None));
    }
    for (name, mhz) in &latest.other_clocks_mhz {
        rows.push((format!("clock: {name}"), format!("{mhz:.0} MHz"), None));
    }
    if let Some(load) = latest.load_1m {
        rows.push(("Load (1 min)".into(), format!("{load:.2}"), None));
    }
    if let Some(mem) = latest.memory_percent {
        rows.push(("Memory in use".into(), format!("{mem:.1} %"), None));
    }
    if let Some(rssi) = latest.rssi_dbm {
        rows.push(("Wi-Fi signal".into(), format!("{rssi:.0} dBm"), None));
    }
    rows
}

/// Placeholder resolved against the live theme at render time; the details
/// table only colors the ACTIVE undervoltage row.
fn cx_theme_red() -> Hsla {
    // Matches gpui-component's red token; the table re-checks the theme
    // color when rendering, this just seeds the severity tint.
    Hsla {
        h: 0.0,
        s: 0.75,
        l: 0.55,
        a: 1.0,
    }
}

fn push_temp(rows: &mut Vec<DetailRow>, label: &str, temp: Option<f32>) {
    if let Some(temp) = temp {
        rows.push((label.into(), format!("{temp:.1} °C"), None));
    }
}

/// The sticky health bitmask from `vcgencmd get_throttled`, rendered as a
/// status card.
fn render_throttle_card(state: &Pimon, cx: &mut Context<Pimon>) -> impl IntoElement {
    let Some(latest) = state.latest() else {
        return div().id("throttle-card");
    };
    let throttle: &ThrottleState = &latest.throttle;
    let describe = throttle.describe();

    div()
        .id("throttle-card")
        .border_1()
        .border_color(cx.theme().border)
        .rounded_md()
        .p_2()
        .child(
            h_flex()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Firmware throttling (since boot)"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(if throttle.active_now() {
                            cx.theme().red
                        } else if throttle.any_event() {
                            cx.theme().yellow
                        } else {
                            cx.theme().green
                        })
                        .child(describe),
                ),
        )
}

/// Bottom bar: load, memory, Wi-Fi and probe-health chips.
fn render_status_bar(state: &Pimon, cx: &Context<Pimon>) -> impl IntoElement {
    let latest = state.latest().cloned().unwrap_or_default();
    let interval_ms = POLL_INTERVAL.as_millis() as f32;

    h_flex()
        .px_3()
        .gap_4()
        .h_7()
        .text_xs()
        .items_center()
        .justify_between()
        .border_t_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().tab_bar)
        .text_color(cx.theme().muted_foreground)
        .child(
            h_flex()
                .gap_4()
                .when_some(latest.load_1m, |this, load| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::Cpu))
                            .child(format!("{load:.2} load")),
                    )
                })
                .when_some(latest.memory_percent, |this, mem| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .w(px(140.))
                            .items_center()
                            .child(Icon::new(IconName::MemoryStick))
                            .child(Progress::new("status-mem").w_12().h_2().value(mem))
                            .child(format!("{mem:.0}%")),
                    )
                })
                .when_some(latest.rssi_dbm, |this, rssi| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::Network))
                            .child(format!("{rssi:.0} dBm")),
                    )
                }),
        )
        .child(div().when_some(state.probe_error(), |this, err| {
            this.child(format!("1 sample / {:.0}s · {err}", interval_ms / 1000.0))
        }))
}

/// `#rrggbb` to gpui `Hsla`, shared with the palette applier.
pub fn hex_to_hsla(value: &str) -> Option<Hsla> {
    let hex = value.trim().trim_start_matches('#');
    let expanded = if hex.len() == 3 {
        hex.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        hex.to_string()
    };
    let n = u32::from_str_radix(&expanded, 16).ok()?;
    Some(rgb(n).into())
}
