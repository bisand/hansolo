//! Prints what HanSolo sees on this machine and how fast each path hashes.
//!
//! `cargo run -p hansolo-hash --release --example bench [seconds]`
//!
//! 1. Detection report.
//! 2. `plan()` with its progress lines: per-thread benchmarks and the strategy.
//! 3. Each planned device run for real against synthetic difficulty-1 work,
//!    alone and then all together, reporting sustained hashrates.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use hansolo_core::{Config, Device, DeviceCtx, DeviceStats, Target, Work, WorkCell};
use hansolo_hash::{CpuDevice, format_rate};

fn work() -> Work {
    Work {
        id: 1,
        job_id: "bench".into(),
        version: 0x2000_0000,
        prev_hash: [0x11; 32],
        bits: 0x1703_0000,
        time: 1_750_000_000,
        coinbase_prefix: b"hansolo benchmark prefix".to_vec(),
        coinbase_suffix: b"suffix".to_vec(),
        merkle_branch: vec![[0x22; 32], [0x33; 32]],
        share_target: Target::from_difficulty(1.0),
        height: None,
        clean: false,
    }
}

/// Runs devices together for `secs` and returns each one's hashes per second.
fn run(devices: Vec<Box<dyn Device>>, secs: f64) -> Vec<(String, f64)> {
    let cell = Arc::new(WorkCell::new());
    cell.set(Some(work()));
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut handles = Vec::new();
    let mut stats = Vec::new();
    for (index, device) in devices.into_iter().enumerate() {
        let info = device.info();
        let s = Arc::new(DeviceStats::default());
        let ctx = DeviceCtx {
            index,
            work: cell.clone(),
            found: tx.clone(),
            stats: s.clone(),
            stop: stop.clone(),
        };
        stats.push((format!("{} [{}]", info.name, info.backend), s));
        handles.push(std::thread::spawn(move || device.run(ctx)));
    }
    // Skip start-up (thread spawn, GPU batch sizing) before measuring.
    std::thread::sleep(Duration::from_millis(700));
    let before: Vec<u64> = stats
        .iter()
        .map(|(_, s)| s.hashes.load(Ordering::Relaxed))
        .collect();
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs_f64(secs));
    let elapsed = started.elapsed().as_secs_f64();
    let after: Vec<u64> = stats
        .iter()
        .map(|(_, s)| s.hashes.load(Ordering::Relaxed))
        .collect();
    stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }
    let shares = rx.len();
    let out: Vec<_> = stats
        .iter()
        .zip(before.iter().zip(&after))
        .map(|((name, _), (b, a))| (name.clone(), (a - b) as f64 / elapsed))
        .collect();
    println!("    (difficulty-1 shares found: {shares})");
    out
}

fn main() {
    let secs: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3.0);
    let config = Config::default();

    let t = Instant::now();
    let report = hansolo_hash::detect(&config);
    println!("== detect ({:.0} ms)", t.elapsed().as_secs_f64() * 1000.0);
    println!("  {} / {} — {}", report.os, report.arch, report.cpu_brand);
    println!(
        "  {} physical / {} logical cores, {:.1} GiB",
        report.physical_cores,
        report.logical_cores,
        report.memory_bytes as f64 / (1u64 << 30) as f64
    );
    println!("  features: {:?}", report.cpu_features);
    for g in &report.gpus {
        println!(
            "  GPU: {} via {} ({}), usable {}: {}",
            g.name, g.api, g.device_type, g.usable, g.note
        );
    }
    for a in &report.asics {
        println!("  ASIC: {} at {}: {}", a.name, a.location, a.note);
    }

    println!("\n== plan");
    let t = Instant::now();
    let plan = hansolo_hash::plan(&config, &|line: &str| println!("  > {line}"));
    println!("  took {:.2} s", t.elapsed().as_secs_f64());
    for b in &plan.report.benchmarks {
        println!(
            "  {:<32} {:>12} {}",
            b.backend,
            format_rate(b.hashrate),
            if b.selected { "(selected)" } else { "" }
        );
    }

    let threads = plan.report.logical_cores;
    let cpu_backend = hansolo_hash::Backend::supported()
        .into_iter()
        .find(|b| {
            plan.report
                .benchmarks
                .iter()
                .any(|r| r.selected && r.backend == b.label())
        })
        .unwrap_or(hansolo_hash::Backend::Scalar);

    println!("\n== sustained, {secs} s each");
    for low in [false, true] {
        let dev: Box<dyn Device> = Box::new(CpuDevice::new(
            cpu_backend,
            threads,
            low,
            plan.report.cpu_brand.clone(),
        ));
        for (name, rate) in run(vec![dev], secs) {
            println!(
                "  CPU alone, low_priority={low}: {name}: {}",
                format_rate(rate)
            );
        }
    }
    for dev in plan.devices {
        if dev.info().kind == hansolo_core::DeviceKind::Gpu {
            for (name, rate) in run(vec![dev], secs) {
                println!(
                    "  GPU alone (intensity {}): {name}: {}",
                    config.gpu.intensity,
                    format_rate(rate)
                );
            }
        }
    }

    // Everything the plan picked, together, rebuilt fresh.
    let plan = hansolo_hash::plan(&config, &|_| {});
    let mut total = 0.0;
    println!("  all planned devices together:");
    for (name, rate) in run(plan.devices, secs) {
        println!("    {name}: {}", format_rate(rate));
        total += rate;
    }
    println!("    total: {}", format_rate(total));
}
