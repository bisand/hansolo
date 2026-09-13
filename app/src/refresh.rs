//! Writing the miner's state into the tree.
//!
//! Called a couple of times a second. Every write checks the widget's current
//! value first: `Ui::widget_mut` marks a node for repaint on access, so writing
//! an unchanged value would repaint the whole dashboard every refresh and turn
//! an idle screen into a busy one.

use std::time::{Duration, SystemTime};

use denise::Role;
use denise_ui::widgets::{Badge, Button, RadialProgress, Table};
use denise_ui::{NodeId, Ui};
use hansolo_core::DeviceKind;
use hansolo_core::snapshot::{
    LogLevel, MinerSnapshot, MinerStatus, ShareResult, block_probability, expected_seconds_to_block,
};
use hansolo_core::target::Target;

use crate::app::{App, Message};
use crate::format;
use crate::widgets::{BarRow, Bars, Chart, Dot, Pills, Text, Tone};

/// Values whose widgets have no getter to compare against.
#[derive(Default)]
pub struct Cache {
    badge_role: Option<Role>,
    running: Option<bool>,
}

pub fn set_text(ui: &mut Ui<Message>, id: NodeId, value: &str) {
    if ui.widget::<Text>(id).is_some_and(|w| w.text() != value)
        && let Some(w) = ui.widget_mut::<Text>(id)
    {
        w.set_text(value);
    }
}

fn set_tone(ui: &mut Ui<Message>, id: NodeId, tone: Tone) {
    if ui
        .widget::<Text>(id)
        .is_some_and(|w| w.current_tone() != tone)
        && let Some(w) = ui.widget_mut::<Text>(id)
    {
        w.set_tone(tone);
    }
}

fn set_dot(ui: &mut Ui<Message>, id: NodeId, role: Role) {
    if ui.widget::<Dot>(id).is_some_and(|w| w.role() != role)
        && let Some(w) = ui.widget_mut::<Dot>(id)
    {
        w.set_role(role);
    }
}

fn set_rows(ui: &mut Ui<Message>, id: NodeId, rows: Vec<Vec<String>>) {
    let same = ui.widget::<Table<Message>>(id).is_some_and(|table| {
        table.row_count() == rows.len()
            && rows.iter().enumerate().all(|(r, cells)| {
                cells
                    .iter()
                    .enumerate()
                    .all(|(c, cell)| table.cell(r, c) == cell)
            })
    });
    if !same && let Some(table) = ui.widget_mut::<Table<Message>>(id) {
        table.set_rows(rows);
    }
}

pub fn apply(app: &mut App, s: &MinerSnapshot, force: bool) {
    let page = app.page;
    let running = app.demo || app.miner.is_running();
    let nodes = &app.nodes;
    let ui = &mut app.ui;
    let cache = &mut app.cache;
    let now = SystemTime::now();

    // ------------------------------------------------------------ header
    let role = match &s.status {
        MinerStatus::Mining => Role::Success,
        MinerStatus::Stopped => Role::Neutral,
        MinerStatus::Error(_) => Role::Error,
        MinerStatus::Reconnecting => Role::Warning,
        MinerStatus::Detecting | MinerStatus::Benchmarking | MinerStatus::Connecting => Role::Info,
    };
    let label = if app.demo { "Demo" } else { s.status.label() };
    if force
        || cache.badge_role != Some(role)
        || ui
            .widget::<Badge>(nodes.status_badge)
            .is_some_and(|b| b.text() != label)
    {
        cache.badge_role = Some(role);
        if let Some(badge) = ui.widget_mut::<Badge>(nodes.status_badge) {
            badge.set_text(label);
            badge.set_role(role);
        }
    }
    let uptime = s
        .started_at
        .and_then(|t| now.duration_since(t).ok())
        .map(|d| format!("up {}", format::duration(d)))
        .unwrap_or_default();
    set_text(ui, nodes.uptime, &uptime);
    if force || cache.running != Some(running) {
        cache.running = Some(running);
        if let Some(button) = ui.widget_mut::<Button<Message>>(nodes.start_stop) {
            button.set_label(if running { "Stop" } else { "Start mining" });
            button.set_role(if running { Role::Error } else { Role::Primary });
        }
        ui.set_enabled(nodes.detect, !running);
    }

    // ------------------------------------------------------------ footer
    let c = &s.connection;
    let (dot, footer) = if c.connected {
        (
            Role::Success,
            format!(
                "Connected to {} · work {}",
                c.url,
                format::ago(c.last_work_at)
            ),
        )
    } else if running {
        (
            Role::Warning,
            match &c.last_error {
                Some(e) => format!("{} · {e}", s.status.label()),
                None => format!("{} {}", s.status.label(), c.url),
            },
        )
    } else {
        (Role::Neutral, "Not mining".to_string())
    };
    set_dot(ui, nodes.footer_dot, dot);
    set_text(ui, nodes.footer, &footer);
    let right = format!(
        "{} lifetime hashes · {} UTC",
        format::si(s.hashrate.total_hashes as f64).trim_end(),
        format::clock(now)
    );
    set_text(ui, nodes.footer_right, &right);

    let network_difficulty = if s.network.difficulty > 0.0 {
        s.network.difficulty
    } else {
        s.job
            .as_ref()
            .map(|j| Target::from_compact(j.bits).difficulty())
            .unwrap_or(0.0)
    };
    let rate = s.hashrate.avg_1m.max(s.hashrate.current);

    // Pages that are not showing are not refreshed; selecting one forces it.
    match page {
        0 => dashboard(ui, nodes, s, network_difficulty, rate, now),
        1 => hardware(ui, nodes, s),
        2 => work(ui, nodes, s, network_difficulty),
        3 => shares(ui, nodes, s),
        4 => log(ui, nodes, s),
        _ => {}
    }
}

fn dashboard(
    ui: &mut Ui<Message>,
    n: &crate::view::Nodes,
    s: &MinerSnapshot,
    net_diff: f64,
    rate: f64,
    now: SystemTime,
) {
    let h = &s.hashrate;
    let sh = &s.shares;
    let best = sh.best_difficulty;
    let best_ever = sh.best_ever_difficulty.max(best);

    set_text(ui, n.stat_values[0], &format::hashrate(h.current));
    set_text(
        ui,
        n.stat_subs[0],
        &format!(
            "1m {} · 15m {}",
            format::hashrate(h.avg_1m),
            format::hashrate(h.avg_15m)
        ),
    );

    set_text(
        ui,
        n.stat_values[1],
        &if best > 0.0 {
            format::difficulty(best)
        } else {
            "—".into()
        },
    );
    set_text(
        ui,
        n.stat_subs[1],
        &format!(
            "best ever {} · block needs {}",
            format::difficulty(best_ever),
            format::difficulty(net_diff)
        ),
    );

    set_text(
        ui,
        n.stat_values[2],
        &format!("{} / {}", sh.accepted, sh.rejected + sh.stale),
    );
    set_text(
        ui,
        n.stat_subs[2],
        &format!(
            "accepted / rejected · last {}",
            format::ago(sh.last_share_at)
        ),
    );

    let day = block_probability(rate, net_diff, 86_400.0);
    set_text(ui, n.stat_values[3], &format::odds(day));
    let reward = s
        .network
        .block_reward
        .map(format::btc)
        .unwrap_or_else(|| "3.125 BTC".into());
    set_text(ui, n.stat_subs[3], &format!("for a block worth {reward}"));
    set_tone(
        ui,
        n.stat_values[3],
        if sh.blocks_found > 0 {
            Tone::Role(Role::Success)
        } else {
            Tone::Content
        },
    );

    let chart_same = ui
        .widget::<Chart>(n.chart)
        .is_some_and(|c| c.same_as(&h.history));
    if !chart_same && let Some(chart) = ui.widget_mut::<Chart>(n.chart) {
        chart.set_samples(&h.history);
    }
    set_text(
        ui,
        n.chart_legend,
        &format!("now {}", format::hashrate(h.current)),
    );

    // The ring: log(best) / log(network), the only scale on which a share of
    // difficulty 1,000 and a block both fit on one dial.
    let progress = if net_diff > 1.0 && best_ever > 1.0 {
        (best_ever.ln() / net_diff.ln()).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let label = format!("{:.0}%", progress * 100.0);
    if ui
        .widget::<RadialProgress>(n.lottery_ring)
        .is_some_and(|r| (r.value() - progress).abs() > 0.001 || r.label() != label)
        && let Some(ring) = ui.widget_mut::<RadialProgress>(n.lottery_ring)
    {
        ring.set_value(progress);
        ring.set_label(label);
    }
    let bits = |d: f64| {
        if d > 0.0 {
            (d * hansolo_core::target::HASHES_PER_DIFFICULTY).log2() as u32
        } else {
            0
        }
    };
    set_text(
        ui,
        n.lottery_lines[0],
        &format!(
            "{} / {}",
            format::difficulty(best_ever),
            format::difficulty(net_diff)
        ),
    );
    set_text(
        ui,
        n.lottery_lines[1],
        &format!("{} / {}", bits(best_ever), bits(net_diff)),
    );
    set_text(
        ui,
        n.lottery_lines[2],
        &format::odds(block_probability(rate, net_diff, 365.25 * 86_400.0)),
    );
    set_text(
        ui,
        n.lottery_lines[3],
        &expected_seconds_to_block(rate, net_diff)
            .map(format::long_span)
            .unwrap_or_else(|| "—".into()),
    );

    let rows = s
        .devices
        .iter()
        .map(|d| {
            let kind = match d.kind {
                DeviceKind::Cpu => "CPU",
                DeviceKind::Gpu => "GPU",
                DeviceKind::Asic => "ASIC",
            };
            vec![
                format!("{kind} · {}", d.name),
                d.backend.clone(),
                format::hashrate(d.hashrate),
                d.temperature_c
                    .map(|t| format!("{t:.0} °C"))
                    .unwrap_or_else(|| "—".into()),
                d.status.clone(),
            ]
        })
        .collect();
    set_rows(ui, n.devices, rows);

    let c = &s.connection;
    set_dot(
        ui,
        n.conn_dot,
        if c.connected {
            Role::Success
        } else {
            Role::Neutral
        },
    );
    let values = [
        if c.mode.is_empty() {
            "—".to_string()
        } else {
            format!("{} · {}", c.mode, c.url)
        },
        c.server.clone().unwrap_or_else(|| "—".into()),
        if c.user.is_empty() {
            "—".into()
        } else {
            c.user.clone()
        },
        c.latency_ms
            .map(|ms| format!("{ms} ms"))
            .unwrap_or_else(|| "—".into()),
        if c.share_difficulty > 0.0 {
            format::difficulty(c.share_difficulty)
        } else {
            "—".into()
        },
        if net_diff > 0.0 {
            format::difficulty(net_diff)
        } else {
            "—".into()
        },
        s.network
            .height
            .or(s.job.as_ref().and_then(|j| j.height))
            .map(|h| format::grouped(h as f64))
            .unwrap_or_else(|| "—".into()),
        match c.last_work_at {
            Some(_) => format::ago(c.last_work_at),
            None => "—".into(),
        },
    ];
    for (id, value) in n.conn_values.iter().zip(values) {
        set_text(ui, *id, &value);
    }
    let _ = now;
}

fn hardware(ui: &mut Ui<Message>, n: &crate::view::Nodes, s: &MinerSnapshot) {
    let hw = &s.hardware;
    let known = !hw.cpu_brand.is_empty();
    let values = [
        or_dash(&hw.os),
        or_dash(&hw.arch),
        or_dash(&hw.cpu_brand),
        if known {
            format!(
                "{} physical · {} logical",
                hw.physical_cores, hw.logical_cores
            )
        } else {
            "—".into()
        },
        if hw.memory_bytes > 0 {
            format!("{:.1} GB", hw.memory_bytes as f64 / (1u64 << 30) as f64)
        } else {
            "—".into()
        },
    ];
    for (id, value) in n.hw_values.iter().zip(values) {
        set_text(ui, *id, &value);
    }
    if ui
        .widget::<Pills>(n.hw_features)
        .is_some_and(|p| p.items() != hw.cpu_features.as_slice())
        && let Some(pills) = ui.widget_mut::<Pills>(n.hw_features)
    {
        pills.set_items(hw.cpu_features.clone());
    }
    let strategy = if hw.strategy.is_empty() {
        "Hardware is benchmarked when mining starts; the fastest path wins.".to_string()
    } else {
        hw.strategy.clone()
    };
    set_text(ui, n.strategy, &strategy);

    let rows: Vec<BarRow> = hw
        .benchmarks
        .iter()
        .map(|b| BarRow {
            label: format!(
                "{} {}{}{}",
                b.kind.label(),
                b.backend,
                if b.kind == DeviceKind::Cpu {
                    ", per thread"
                } else {
                    ""
                },
                if b.selected { " · in use" } else { "" }
            ),
            value: b.hashrate,
            value_text: format::hashrate(b.hashrate),
            highlight: b.selected,
        })
        .collect();
    if ui
        .widget::<Bars>(n.bars)
        .is_some_and(|b| b.rows() != rows.as_slice())
        && let Some(bars) = ui.widget_mut::<Bars>(n.bars)
    {
        bars.set_rows(rows);
    }

    let gpus = hw
        .gpus
        .iter()
        .map(|g| {
            vec![
                format!("{} ({})", g.name, g.device_type),
                g.api.clone(),
                if g.usable {
                    or_dash(&g.note)
                } else {
                    format!("not used: {}", g.note)
                },
            ]
        })
        .collect();
    set_rows(ui, n.gpus, placeholder(gpus, known, "No GPU found", 3));
    let asics = hw
        .asics
        .iter()
        .map(|a| {
            vec![
                a.name.clone(),
                a.location.clone(),
                if a.supported {
                    or_dash(&a.note)
                } else {
                    format!("unsupported: {}", a.note)
                },
            ]
        })
        .collect();
    set_rows(ui, n.asics, placeholder(asics, known, "None found", 3));
}

fn work(ui: &mut Ui<Message>, n: &crate::view::Nodes, s: &MinerSnapshot, net_diff: f64) {
    let Some(job) = &s.job else {
        for id in &n.job_values {
            set_text(ui, *id, "—");
        }
        set_text(
            ui,
            n.coinbase,
            "No work yet. Start mining to receive a job from the pool or node.",
        );
        return;
    };
    let values = [
        job.height
            .map(|h| format::grouped(h as f64))
            .unwrap_or_else(|| "—".into()),
        format!("{} (#{})", job.job_id, job.work_id),
        job.prev_hash.clone(),
        format!("0x{:08x}", job.version),
        format!("0x{:08x}", job.bits),
        job.network_target.clone(),
        job.share_target.clone(),
        format::clock(SystemTime::UNIX_EPOCH + Duration::from_secs(job.time as u64)) + " UTC",
        format!("{} hashes", job.merkle_branch_len),
        job.tx_count
            .map(|t| format::grouped(t as f64))
            .unwrap_or_else(|| "known to the pool".into()),
        job.coinbase_value
            .map(format::btc)
            .unwrap_or_else(|| "—".into()),
        format::ago(job.received_at),
        format::grouped(job.jobs_received as f64),
        format::difficulty(net_diff),
    ];
    for (id, value) in n.job_values.iter().zip(values) {
        set_text(ui, *id, &value);
    }
    // Break the hex into groups of eight so the wrap has somewhere to break.
    let grouped: Vec<&str> = job
        .coinbase_hex
        .as_bytes()
        .chunks(16)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect();
    set_text(ui, n.coinbase, &grouped.join(" "));
}

fn shares(ui: &mut Ui<Message>, n: &crate::view::Nodes, s: &MinerSnapshot) {
    let sh = &s.shares;
    for (id, v) in n.share_values.iter().zip([
        sh.submitted,
        sh.accepted,
        sh.rejected,
        sh.stale,
        sh.blocks_found,
    ]) {
        set_text(ui, *id, &format::grouped(v as f64));
    }
    set_tone(
        ui,
        n.share_values[4],
        if sh.blocks_found > 0 {
            Tone::Role(Role::Success)
        } else {
            Tone::Content
        },
    );
    set_tone(
        ui,
        n.share_values[2],
        if sh.rejected > 0 {
            Tone::Role(Role::Error)
        } else {
            Tone::Content
        },
    );
    let rows = sh
        .recent
        .iter()
        .rev()
        .map(|r| {
            vec![
                format::clock(r.at),
                r.device.clone(),
                format::difficulty(r.difficulty),
                String::new(),
                match r.result {
                    ShareResult::Pending => "pending",
                    ShareResult::Accepted => "accepted",
                    ShareResult::Rejected => "rejected",
                    ShareResult::Stale => "stale",
                    ShareResult::Block => "BLOCK!",
                }
                .into(),
            ]
        })
        .collect();
    set_rows(ui, n.shares, rows);
}

fn log(ui: &mut Ui<Message>, n: &crate::view::Nodes, s: &MinerSnapshot) {
    let rows = s
        .log
        .iter()
        .rev()
        .map(|e| {
            vec![
                format::clock(e.at),
                match e.level {
                    LogLevel::Debug => "debug",
                    LogLevel::Info => "info",
                    LogLevel::Success => "success",
                    LogLevel::Warning => "warning",
                    LogLevel::Error => "error",
                }
                .into(),
                e.message.clone(),
            ]
        })
        .collect();
    set_rows(ui, n.log, rows);
}

/// An empty table says why it is empty.
fn placeholder(
    rows: Vec<Vec<String>>,
    detected: bool,
    empty: &str,
    columns: usize,
) -> Vec<Vec<String>> {
    if !rows.is_empty() {
        return rows;
    }
    let mut row = vec![String::new(); columns];
    row[0] = if detected {
        empty.to_string()
    } else {
        "Detecting…".to_string()
    };
    vec![row]
}

fn or_dash(s: &str) -> String {
    if s.is_empty() {
        "—".into()
    } else {
        s.to_string()
    }
}
