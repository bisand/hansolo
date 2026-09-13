//! Hashing devices for HanSolo, and the machinery that picks them.
//!
//! PUBLIC API CONTRACT — the engine and the app depend on exactly these items:
//!
//! - [`plan`] — detect hardware, benchmark candidate paths, and build devices.
//! - [`detect`] — detection only (no benchmark, no devices), for the hardware page
//!   before mining starts.
//! - [`Plan`].
//!
//! Everything else public here (the [`cpu`] backends, [`gpu`] and [`asic`]
//! modules) is for tests, benchmarks and tools; the engine shouldn't need it.
//!
//! # How the choice is made
//!
//! A solo "lottery" miner lives or dies by hashes per watt it can squeeze out
//! of whatever it runs on, and the fastest path differs wildly by machine: SHA
//! extensions beat everything on CPUs that have them, AVX2 wins on older x86,
//! and portable code is all a Pi Zero has. Rather than guess from feature flags,
//! [`plan`] times every backend that can run and keeps the fastest (unless the
//! user forces one), then adds GPUs that are worth their power and network
//! ASICs to monitor.

use std::sync::atomic::Ordering;
use std::time::Duration;

use hansolo_core::sha::header_hash_from_midstate;
use hansolo_core::snapshot::{BenchmarkResult, HardwareReport};
use hansolo_core::work::EXTRANONCE_LEN;
use hansolo_core::{Config, Device, DeviceCtx, DeviceKind, Found, Work};

#[cfg(feature = "asic")]
pub mod asic;
pub mod cpu;
mod detect;
#[cfg(feature = "gpu")]
pub mod gpu;
mod priority;

pub use cpu::Backend;
pub use cpu::device::CpuDevice;
pub use detect::cpu_features;

/// The outcome of [`plan`]: what was found and the devices to run.
pub struct Plan {
    pub report: HardwareReport,
    pub devices: Vec<Box<dyn Device>>,
}

/// How long each CPU backend is timed for.
const CPU_BENCH: Duration = Duration::from_millis(400);
#[cfg(feature = "gpu")]
const GPU_BENCH: Duration = Duration::from_millis(600);
/// A GPU is only used if it adds at least this fraction of the CPU's total.
#[cfg(feature = "gpu")]
const GPU_MIN_FRACTION: f64 = 0.02;

/// Hardware that detection found and later stages can use directly.
struct Probed {
    report: HardwareReport,
    #[cfg(feature = "gpu")]
    gpus: gpu::GpuProbe,
    #[cfg(feature = "asic")]
    network_miners: Vec<asic::NetworkMiner>,
}

// Without the optional features there is nothing to run alongside the CPU facts.
#[cfg_attr(not(feature = "asic"), allow(unused_variables))]
fn probe(config: &Config) -> Probed {
    let mut report = HardwareReport::default();
    // GPU enumeration (driver loading) and network probes are the slow parts;
    // run them alongside the CPU facts.
    std::thread::scope(|s| {
        #[cfg(feature = "gpu")]
        let gpus = s.spawn(gpu::probe);
        #[cfg(feature = "asic")]
        let asics = s.spawn(|| asic::probe(&config.asic));

        detect::system(&mut report);

        #[cfg(feature = "gpu")]
        let gpus = gpus.join().expect("GPU probe thread");
        #[cfg(feature = "gpu")]
        {
            report.gpus = gpus.reports.clone();
        }
        #[cfg(feature = "asic")]
        let (asic_reports, network_miners) = asics.join().expect("ASIC probe thread");
        #[cfg(feature = "asic")]
        {
            report.asics = asic_reports;
        }
        Probed {
            report,
            #[cfg(feature = "gpu")]
            gpus,
            #[cfg(feature = "asic")]
            network_miners,
        }
    })
}

/// Probes the machine. Cheap enough to call from a UI's startup path on a
/// background thread (well under a second, plus up to about a second per
/// unreachable network miner, probed in parallel).
pub fn detect(config: &Config) -> HardwareReport {
    probe(config).report
}

/// Detects, benchmarks, and builds the devices for `config`.
///
/// `progress` receives short human-readable lines ("benchmarking AVX2 8-lane…")
/// which the engine forwards to the log.
// `cpu_total` and `fastest_cpu` only matter to the GPU stage.
#[cfg_attr(not(feature = "gpu"), allow(unused_variables, unused_assignments))]
pub fn plan(config: &Config, progress: &(dyn Fn(&str) + Sync)) -> Plan {
    progress("detecting hardware…");
    #[allow(unused_mut)]
    let mut probed = probe(config);
    let mut report = std::mem::take(&mut probed.report);
    let mut devices: Vec<Box<dyn Device>> = Vec::new();
    let mut strategy: Vec<String> = Vec::new();

    // ---- CPU ----
    let mut cpu_total = 0.0;
    let mut fastest_cpu = Backend::Scalar;
    if config.cpu.enabled {
        let supported = Backend::supported();
        let mut timings: Vec<(Backend, f64)> = Vec::new();
        for &backend in &supported {
            progress(&format!("benchmarking {}…", backend.label()));
            let rate = backend.benchmark(CPU_BENCH);
            progress(&format!(
                "{}: {} per thread",
                backend.label(),
                format_rate(rate)
            ));
            timings.push((backend, rate));
        }
        let (best, best_rate) = timings
            .iter()
            .copied()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap_or((Backend::Scalar, 0.0));
        fastest_cpu = best;

        let forced = config
            .cpu
            .backend
            .as_deref()
            .filter(|s| !s.trim().is_empty());
        let (chosen, forced_note) = match forced.map(|name| (name, Backend::from_name(name))) {
            None => (best, None),
            Some((_, Some(b))) if supported.contains(&b) => {
                (b, Some(format!("{} as configured", b.label())))
            }
            Some((name, Some(_))) => {
                let msg = format!(
                    "configured CPU backend {name:?} isn't supported by this CPU; using {}",
                    best.label()
                );
                progress(&msg);
                (best, Some(msg))
            }
            Some((name, None)) => {
                let msg = format!("unknown CPU backend {name:?}; using {}", best.label());
                progress(&msg);
                (best, Some(msg))
            }
        };

        let threads = config
            .cpu
            .threads
            .filter(|&t| t > 0)
            .unwrap_or(report.logical_cores.max(1));
        let chosen_rate = timings.iter().find(|t| t.0 == chosen).map_or(0.0, |t| t.1);
        cpu_total = chosen_rate * threads as f64;
        for &(backend, rate) in &timings {
            report.benchmarks.push(BenchmarkResult {
                backend: backend.label().to_string(),
                kind: DeviceKind::Cpu,
                hashrate: rate,
                selected: backend == chosen,
            });
        }

        let scalar_rate = timings
            .iter()
            .find(|t| t.0 == Backend::Scalar)
            .map_or(0.0, |t| t.1);
        let cores = if report.physical_cores > 0 && report.physical_cores != report.logical_cores {
            format!(
                "{} logical cores ({} physical)",
                report.logical_cores, report.physical_cores
            )
        } else {
            format!("{} cores", report.logical_cores)
        };
        let threads_text = format!("{threads} thread{}", if threads == 1 { "" } else { "s" });
        let sentence = match forced_note {
            Some(note) if chosen != best => format!(
                "CPU: {note}; {} would be {:.1}× faster. Using {threads_text} on {cores}.",
                best.label(),
                best_rate / chosen_rate.max(1.0)
            ),
            Some(note) => format!(
                "CPU: {note}, which is also the fastest here; using {threads_text} on {cores}."
            ),
            None if chosen == Backend::Scalar => {
                format!(
                    "CPU: no SHA or SIMD extensions to use, so portable code; using {threads_text} on {cores}."
                )
            }
            None => format!(
                "{} {} {:.1}× faster than portable code; using {threads_text} on {cores}.",
                describe(chosen),
                if describe(chosen).ends_with('s') {
                    "were"
                } else {
                    "was"
                },
                chosen_rate / scalar_rate.max(1.0)
            ),
        };
        strategy.push(sentence);

        devices.push(Box::new(CpuDevice::new(
            chosen,
            threads,
            config.cpu.low_priority,
            report.cpu_brand.clone(),
        )));
    } else {
        strategy.push("CPU mining is off.".into());
    }

    // ---- GPU ----
    #[cfg(feature = "gpu")]
    if config.gpu.enabled {
        let mut used = Vec::new();
        let mut skipped = Vec::new();
        for (adapter, report_index) in std::mem::take(&mut probed.gpus.adapters) {
            let name = report.gpus[report_index].name.clone();
            let api = report.gpus[report_index].api.clone();
            progress(&format!("benchmarking GPU {name} via {api}…"));
            let miner = match gpu::GpuMiner::new(&adapter) {
                Ok(m) => m,
                Err(e) => {
                    progress(&format!("GPU {name}: {e}"));
                    let r = &mut report.gpus[report_index];
                    r.usable = false;
                    r.note = e;
                    continue;
                }
            };
            let rate = match miner.benchmark(GPU_BENCH) {
                Ok(rate) => rate,
                Err(e) => {
                    progress(&format!("GPU {name}: {e}"));
                    report.gpus[report_index].note = e;
                    continue;
                }
            };
            progress(&format!("GPU {name}: {}", format_rate(rate)));
            let worth_it = rate > 0.0 && (cpu_total == 0.0 || rate >= GPU_MIN_FRACTION * cpu_total);
            report.benchmarks.push(BenchmarkResult {
                backend: format!("{name} (wgpu/{api})"),
                kind: DeviceKind::Gpu,
                hashrate: rate,
                selected: worth_it,
            });
            if worth_it {
                used.push(format!("{name} via {api}, 1 queue ({})", format_rate(rate)));
                devices.push(Box::new(gpu::GpuDevice::new(
                    miner,
                    config.gpu.intensity,
                    fastest_cpu,
                )));
            } else {
                report.gpus[report_index].note = format!(
                    "{} is under {:.0}% of the CPU's total; not used",
                    format_rate(rate),
                    GPU_MIN_FRACTION * 100.0
                );
                skipped.push(format!("{name} ({})", format_rate(rate)));
            }
        }
        if !used.is_empty() {
            strategy.push(format!("GPU: {}.", used.join("; ")));
        }
        if !skipped.is_empty() {
            strategy.push(format!(
                "Not using {}: too slow to be worth the power.",
                skipped.join(", ")
            ));
        }
        if used.is_empty() && skipped.is_empty() {
            strategy.push("No usable GPU found.".into());
        }
    } else {
        strategy.push("GPU mining is off.".into());
    }
    #[cfg(not(feature = "gpu"))]
    if config.gpu.enabled {
        strategy.push("GPU support isn't compiled into this build.".into());
    }

    // ---- ASICs ----
    #[cfg(feature = "asic")]
    if config.asic.enabled {
        let miners = std::mem::take(&mut probed.network_miners);
        if !miners.is_empty() {
            strategy.push(format!(
                "Monitoring {} network miner{} (they mine on their own).",
                miners.len(),
                if miners.len() == 1 { "" } else { "s" }
            ));
        }
        for miner in &miners {
            devices.push(Box::new(asic::AxeOsDevice::new(miner)));
        }
    }

    report.strategy = strategy.join(" ");
    progress(&report.strategy);
    Plan { report, devices }
}

/// The subject of the strategy sentence for a backend.
fn describe(backend: Backend) -> &'static str {
    match backend {
        Backend::Scalar => "Portable code",
        Backend::ShaNi => "Intel SHA extensions",
        Backend::Armv8Sha2 => "ARMv8 SHA2 extensions",
        Backend::Neon => "NEON (4 lanes)",
        Backend::Avx2 => "AVX2 (8 lanes)",
        Backend::Avx512 => "AVX-512 (16 lanes)",
    }
}

/// "123.4 MH/s".
pub fn format_rate(hps: f64) -> String {
    const UNITS: [&str; 7] = ["H/s", "kH/s", "MH/s", "GH/s", "TH/s", "PH/s", "EH/s"];
    let mut value = hps.max(0.0);
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Re-checks a backend's candidate with the reference hash and, if it meets the
/// share target, reports it. Returns whether it was a share.
pub(crate) fn verify_candidate(
    ctx: &DeviceCtx,
    work: &Work,
    extranonce: [u8; EXTRANONCE_LEN],
    header: &[u8; 80],
    midstate: &[u32; 8],
    nonce: u32,
) -> bool {
    let mut full = *header;
    full[76..80].copy_from_slice(&nonce.to_le_bytes());
    let hash = header_hash_from_midstate(midstate, &full);
    if !work.share_target.is_met_by(&hash) {
        return false;
    }
    ctx.stats.found.fetch_add(1, Ordering::Relaxed);
    // A closed channel means the engine is shutting down; nothing to do.
    let _ = ctx.found.send(Found {
        work_id: work.id,
        device: ctx.index,
        extranonce,
        header: full,
        hash,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates() {
        assert_eq!(format_rate(0.0), "0.0 H/s");
        assert_eq!(format_rate(12_345_678.0), "12.3 MH/s");
    }

    #[test]
    fn detect_is_fast_and_sane() {
        let started = std::time::Instant::now();
        let report = detect(&Config::default());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(report.logical_cores >= 1);
        assert!(!report.arch.is_empty());
        assert!(!report.os.is_empty());
        assert!(report.memory_bytes > 0);
    }

    #[test]
    fn plan_without_gpu_builds_a_cpu_device() {
        let mut config = Config::default();
        config.gpu.enabled = false;
        config.cpu.threads = Some(2);
        let lines = std::sync::Mutex::new(Vec::new());
        let plan = plan(&config, &|l: &str| {
            lines.lock().unwrap().push(l.to_string())
        });
        assert_eq!(plan.devices.len(), 1);
        let info = plan.devices[0].info();
        assert_eq!(info.kind, DeviceKind::Cpu);
        assert!(info.backend.contains("×2"), "{}", info.backend);
        assert_eq!(
            plan.report.benchmarks.iter().filter(|b| b.selected).count(),
            1
        );
        assert!(!plan.report.strategy.is_empty());
        assert!(!lines.lock().unwrap().is_empty());
    }

    #[test]
    fn forced_backend() {
        let mut config = Config::default();
        config.gpu.enabled = false;
        config.cpu.threads = Some(1);
        config.cpu.backend = Some("scalar".into());
        let plan = plan(&config, &|_| {});
        assert!(plan.devices[0].info().backend.starts_with("Portable"));
        let selected: Vec<_> = plan
            .report
            .benchmarks
            .iter()
            .filter(|b| b.selected)
            .collect();
        assert_eq!(selected[0].backend, "Portable");
    }
}
