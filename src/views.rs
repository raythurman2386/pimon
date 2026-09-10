//! GPUI Kit rendering for Pimon: a btop-style dense dashboard.
//!
//! Layout: title bar with tabs + health badge; a row of big sensor
//! readouts; per-core utilization and memory gauges; I/O charts; a clock
//! and regulator grid; a status bar. The Details tab keeps the full
//! sensor table and the throttling health card.

use std::collections::VecDeque;

use gpui_kit::component::chart::AreaChart;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pimon::sensors::{MemoryInfo, Snapshot, ThrottleState};

use crate::app::{Health, Pimon, POLL_INTERVAL};
use crate::widgets::{
    big_readout, clock_color, fmt_uptime, memory_gauges, temp_color, utilization_color, Gauge,
};

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
        .child(render_title_bar(state, active_tab, health, cx))
        .child(
            div()
                .id("tab-content")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_3()
                .map(|this| match active_tab {
                    1 => this.child(render_details_tab(state, cx)),
                    _ => this.child(render_live_tab(state, cx)),
                }),
        )
        .child(render_status_bar(state, cx))
}

fn render_title_bar(
    state: &Pimon,
    active_tab: usize,
    health: Health,
    cx: &mut Context<Pimon>,
) -> impl IntoElement {
    let (badge_icon, badge_color) = match health {
        Health::Ok => (IconName::CircleCheck, cx.theme().green),
        Health::Warn => (IconName::TriangleAlert, cx.theme().yellow),
        Health::Alert => (IconName::TriangleAlert, cx.theme().red),
    };
    let uptime_text = state.latest().and_then(|s| s.uptime_secs).map(fmt_uptime);

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
                    .gap_3()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .when_some(uptime_text, |this, uptime| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(Icon::new(IconName::RotateCw))
                                .child(uptime),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .text_color(badge_color)
                            .child(Icon::new(badge_icon))
                            .child(health_label(health)),
                    ),
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

// ---------------------------------------------------------------- Live tab

/// Top strip: one big readout per temperature plus CPU and memory.
fn render_readouts(state: &Pimon, cx: &Context<Pimon>) -> impl IntoElement {
    let latest = state.latest();
    let readouts: Vec<(String, String, Hsla)> = match latest {
        None => vec![("sensors".into(), "—".into(), cx.theme().muted_foreground)],
        Some(snap) => {
            let mut rows = Vec::new();
            let push_temp = |rows: &mut Vec<_>, label: &str, temp: Option<f32>| {
                if let Some(t) = temp {
                    rows.push((label.to_string(), format!("{t:.1}°C"), temp_color(t, cx)));
                }
            };
            push_temp(&mut rows, "soc", snap.soc_temp_c);
            push_temp(&mut rows, "pmic", snap.pmic_temp_c);
            push_temp(&mut rows, "nvme", snap.nvme_temp_c);
            push_temp(&mut rows, "rp1", snap.rp1_temp_c);
            for (label, temp) in &snap.extra_zones_c {
                push_temp(&mut rows, &format!("zone {label}"), Some(*temp));
            }
            if let Some(util) = snap.utilization_pct {
                rows.push((
                    "cpu".into(),
                    format!("{:.0}%", util * 100.0),
                    utilization_color(util, cx),
                ));
            }
            if let Some(mem) = snap.memory {
                rows.push((
                    "mem".into(),
                    format!("{:.0}%", mem.used_pct),
                    utilization_color(mem.used_pct / 100.0, cx),
                ));
            }
            rows
        }
    };

    h_flex().gap_4().flex_wrap().children(
        readouts
            .into_iter()
            .map(|(label, value, color)| big_readout(&label, &value, color, cx)),
    )
}

/// Per-core utilization bars, btop style.
fn render_cores(state: &Pimon, cx: &Context<Pimon>) -> impl IntoElement {
    let latest = state.latest();
    let cores: Vec<(usize, f32)> = latest
        .map(|s| {
            s.cores
                .iter()
                .enumerate()
                .map(|(ix, core)| (ix, core.utilization))
                .collect()
        })
        .unwrap_or_default();

    v_flex()
        .gap_1()
        .children(cores.into_iter().map(|(ix, util)| {
            Gauge::new(format!("cpu{ix}"), util, utilization_color(util, cx))
                .value_text(format!("{:.0}%", util * 100.0))
                .cells(14)
                .render(cx)
        }))
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
        .min_h(px(110.))
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

/// Clock domain grid: `arm 2400 MHz` cells for every domain that answered.
fn render_clocks(state: &Pimon, cx: &Context<Pimon>) -> impl IntoElement {
    let latest = state.latest();
    let clocks: Vec<(&String, &f32)> = latest
        .map(|s| s.all_clocks_mhz.iter().collect())
        .unwrap_or_default();

    h_flex()
        .gap_4()
        .flex_wrap()
        .children(clocks.into_iter().map(|(name, mhz)| {
            let color = if name == "arm" {
                clock_color(*mhz, cx)
            } else {
                cx.theme().foreground
            };
            h_flex()
                .gap_1()
                .text_xs()
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(name.clone()),
                )
                .child(div().text_color(color).child(format!("{mhz:.0} MHz")))
        }))
}

/// Voltage rails: vcgencmd rails plus enabled regulators.
fn render_voltages(state: &Pimon, cx: &Context<Pimon>) -> impl IntoElement {
    let latest = state.latest();
    let mut rails: Vec<(String, String)> = Vec::new();
    if let Some(snap) = latest {
        if let Some(v) = snap.core_volts {
            rails.push(("core".into(), format!("{v:.4} V")));
        }
        if let Some(v) = snap.sdram_volatile_volts {
            rails.push(("sdram_c".into(), format!("{v:.4} V")));
        }
        if let Some(v) = snap.sdram_phy_volts {
            rails.push(("sdram_p".into(), format!("{v:.4} V")));
        }
        for (name, volts) in &snap.regulators_v {
            rails.push((name.clone(), format!("{volts:.2} V")));
        }
    }

    h_flex()
        .gap_4()
        .flex_wrap()
        .children(rails.into_iter().map(|(name, value)| {
            h_flex()
                .gap_1()
                .text_xs()
                .child(div().text_color(cx.theme().muted_foreground).child(name))
                .child(div().text_color(cx.theme().foreground).child(value))
        }))
}

/// Live tab: readouts, cores + memory, I/O charts, clocks and voltages.
fn render_live_tab(state: &mut Pimon, cx: &mut Context<Pimon>) -> impl IntoElement {
    let history = state.history();
    let disk_read = series(history, |s| s.disk_io.map(|d| d.read_mib_s));
    let disk_write = series(history, |s| s.disk_io.map(|d| d.write_mib_s));
    let net_rx = series(history, |s| s.net_io.map(|d| d.rx_kib_s));
    let net_tx = series(history, |s| s.net_io.map(|d| d.tx_kib_s));

    v_flex()
        .size_full()
        .gap_3()
        .child(render_readouts(state, cx))
        .child(
            h_flex()
                .gap_6()
                .flex_wrap()
                .child(render_cores(state, cx))
                .child(render_memory(state, cx)),
        )
        .child(
            h_flex()
                .gap_3()
                .child(render_chart(
                    "nvme read",
                    &disk_read,
                    " MiB/s",
                    cx.theme().blue,
                    2,
                    cx,
                ))
                .child(render_chart(
                    "nvme write",
                    &disk_write,
                    " MiB/s",
                    cx.theme().accent,
                    2,
                    cx,
                )),
        )
        .child(
            h_flex()
                .gap_3()
                .child(render_chart(
                    "wifi rx",
                    &net_rx,
                    " KiB/s",
                    cx.theme().green,
                    1,
                    cx,
                ))
                .child(render_chart(
                    "wifi tx",
                    &net_tx,
                    " KiB/s",
                    cx.theme().yellow,
                    1,
                    cx,
                )),
        )
        .child(
            v_flex()
                .gap_2()
                .border_1()
                .border_color(cx.theme().border)
                .rounded_md()
                .p_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("clocks"),
                )
                .child(render_clocks(state, cx))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("voltages"),
                )
                .child(render_voltages(state, cx)),
        )
}

fn render_memory(state: &Pimon, cx: &Context<Pimon>) -> impl IntoElement + use<> {
    match state.latest().and_then(|s| s.memory) {
        Some(mem) => memory_gauges(&mem, cx).into_any_element(),
        None => div().into_any_element(),
    }
}

fn series(history: &VecDeque<Snapshot>, pick: impl Fn(&Snapshot) -> Option<f32>) -> Vec<f32> {
    history.iter().filter_map(pick).collect()
}

// ------------------------------------------------------------- Details tab

/// (label, display value, optional severity color) for the details table.
type DetailRow = (String, String, Option<Hsla>);

/// Details tab: the full sensor table plus the throttling health card.
fn render_details_tab(state: &mut Pimon, cx: &mut Context<Pimon>) -> impl IntoElement {
    let rows = details_rows(state, cx);

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

fn details_rows(state: &Pimon, cx: &Context<Pimon>) -> Vec<DetailRow> {
    let Some(latest) = state.latest() else {
        return vec![("sensors".into(), "waiting for first sweep…".into(), None)];
    };
    let mut rows = Vec::new();

    push_temp(&mut rows, "SoC (cpu-thermal)", latest.soc_temp_c, cx);
    push_temp(&mut rows, "PMIC", latest.pmic_temp_c, cx);
    push_temp(&mut rows, "NVMe composite", latest.nvme_temp_c, cx);
    push_temp(&mut rows, "RP1 die", latest.rp1_temp_c, cx);
    for (label, temp) in &latest.extra_zones_c {
        push_temp(&mut rows, &format!("zone: {label}"), Some(*temp), cx);
    }
    if let Some(util) = latest.utilization_pct {
        rows.push((
            "CPU utilization".into(),
            format!("{:.1} %", util * 100.0),
            Some(utilization_color(util, cx)),
        ));
    }
    for (ix, core) in latest.cores.iter().enumerate() {
        rows.push((
            format!("cpu{ix}"),
            format!("{:.1} %", core.utilization * 100.0),
            Some(utilization_color(core.utilization, cx)),
        ));
    }
    if let Some(mem) = latest.memory {
        let MemoryInfo {
            used_pct,
            buff_cache_pct,
            swap_used_pct,
            total_mib,
            used_mib,
        } = mem;
        rows.push((
            "Memory".into(),
            format!("{used_mib:.0} / {total_mib:.0} MiB ({used_pct:.1} %)"),
            Some(utilization_color(used_pct / 100.0, cx)),
        ));
        rows.push(("Buff/cache".into(), format!("{buff_cache_pct:.1} %"), None));
        if let Some(swap) = swap_used_pct {
            rows.push(("Swap".into(), format!("{swap:.1} %"), None));
        }
    }
    if let Some(disk) = latest.disk_io {
        rows.push((
            "NVMe read".into(),
            format!("{:.2} MiB/s", disk.read_mib_s),
            None,
        ));
        rows.push((
            "NVMe write".into(),
            format!("{:.2} MiB/s", disk.write_mib_s),
            None,
        ));
    }
    if let Some(net) = latest.net_io {
        rows.push((
            "Wi-Fi rx".into(),
            format!("{:.1} KiB/s", net.rx_kib_s),
            None,
        ));
        rows.push((
            "Wi-Fi tx".into(),
            format!("{:.1} KiB/s", net.tx_kib_s),
            None,
        ));
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
    for (name, volts) in &latest.regulators_v {
        rows.push((format!("regulator: {name}"), format!("{volts:.2} V"), None));
    }
    if latest.undervolt_alarm {
        rows.push((
            "Undervoltage alarm (hwmon)".into(),
            "ACTIVE".into(),
            Some(cx.theme().red),
        ));
    }
    for (name, mhz) in &latest.all_clocks_mhz {
        rows.push((format!("clock: {name}"), format!("{mhz:.0} MHz"), None));
    }
    for (label, load) in [
        ("Load (1 min)", latest.load_1m),
        ("Load (5 min)", latest.load_5m),
        ("Load (15 min)", latest.load_15m),
    ] {
        if let Some(load) = load {
            rows.push((label.into(), format!("{load:.2}"), None));
        }
    }
    if let Some(uptime) = latest.uptime_secs {
        rows.push(("Uptime".into(), fmt_uptime(uptime), None));
    }
    if let Some(rssi) = latest.rssi_dbm {
        rows.push(("Wi-Fi signal".into(), format!("{rssi:.0} dBm"), None));
    }
    rows
}

fn push_temp(rows: &mut Vec<DetailRow>, label: &str, temp: Option<f32>, cx: &Context<Pimon>) {
    if let Some(temp) = temp {
        rows.push((
            label.into(),
            format!("{temp:.1} °C"),
            Some(temp_color(temp, cx)),
        ));
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

/// Bottom bar: load averages, memory, Wi-Fi and probe-health chips.
fn render_status_bar(state: &Pimon, cx: &Context<Pimon>) -> impl IntoElement {
    let latest = state.latest().cloned().unwrap_or_default();
    let interval_ms = POLL_INTERVAL.as_millis() as f32;
    let load_text = match (latest.load_1m, latest.load_15m) {
        (Some(a), Some(b)) => format!("{a:.2} / {b:.2}"),
        (Some(a), None) => format!("{a:.2}"),
        _ => String::new(),
    };

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
                .when(!load_text.is_empty(), |this| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::Cpu))
                            .child(format!("{load_text} load")),
                    )
                })
                .when_some(latest.memory, |this, mem| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .w(px(140.))
                            .items_center()
                            .child(Icon::new(IconName::MemoryStick))
                            .child(
                                gpui_kit::component::progress::Progress::new("status-mem")
                                    .w_12()
                                    .h_2()
                                    .value(mem.used_pct),
                            )
                            .child(format!("{:.0}%", mem.used_pct)),
                    )
                })
                .when_some(latest.disk_io, |this, disk| {
                    this.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::HardDrive))
                            .child(format!(
                                "↓{:.1} ↑{:.1} MiB/s",
                                disk.read_mib_s, disk.write_mib_s
                            )),
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
