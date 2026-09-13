//! A plausible miner, for looking at the dashboard without mining.
//!
//! `--demo` feeds this into the UI instead of the engine's snapshot. It is
//! deliberately obvious about itself: the status badge says "Demo".

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use hansolo_core::DeviceKind;
use hansolo_core::snapshot::*;
use hansolo_core::target::Target;

pub fn snapshot(seconds: u64) -> MinerSnapshot {
    let now = SystemTime::now();
    let ago = |s: u64| Some(now - Duration::from_secs(s));
    let history: VecDeque<f64> = (0..900.min(seconds + 420))
        .map(|i| {
            let t = i as f64;
            let ramp = (t / 20.0).min(1.0);
            ramp * (1.58e9 + 6.0e7 * (t / 37.0).sin() + 2.5e7 * (t / 5.3).cos())
        })
        .collect();
    let current = history.back().copied().unwrap_or(0.0);
    let bits = 0x17023a04;
    let net = Target::from_compact(bits).difficulty();

    let log_lines = [
        (
            LogLevel::Info,
            "Starting (Stratum), paying to bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
        ),
        (
            LogLevel::Info,
            "Detected Apple M5 Pro: 14 cores, ARMv8 SHA2, NEON; GPU Apple M5 Pro (Metal)",
        ),
        (
            LogLevel::Info,
            "Benchmark: armv8-sha2 58.1 MH/s per thread, scalar 9.9 MH/s",
        ),
        (
            LogLevel::Info,
            "Benchmark: GPU Metal 780 MH/s at intensity 6",
        ),
        (LogLevel::Success, "Connected to public-pool.io:21496"),
        (LogLevel::Info, "New block 915,344 — clean jobs"),
        (LogLevel::Success, "Share accepted, difficulty 1.84 M"),
        (LogLevel::Warning, "Share rejected: stale (job not found)"),
        (LogLevel::Success, "New best share this session: 12.6 M"),
    ];
    let log = log_lines
        .iter()
        .enumerate()
        .map(|(i, (level, message))| LogEntry {
            at: now - Duration::from_secs(((log_lines.len() - i) * 47) as u64),
            level: *level,
            message: (*message).into(),
        })
        .collect();

    let recent = (0..24)
        .map(|i| ShareRecord {
            at: now - Duration::from_secs(900 - i * 37),
            difficulty: 1.0e6 * (1.0 + ((i * 7919) % 97) as f64 / 9.0),
            device: if i % 3 == 0 {
                "GPU · Apple M5 Pro".into()
            } else {
                "CPU · Apple M5 Pro".into()
            },
            result: if i == 17 {
                ShareResult::Stale
            } else {
                ShareResult::Accepted
            },
        })
        .collect();

    MinerSnapshot {
        status: MinerStatus::Mining,
        started_at: ago(seconds + 3_700),
        hashrate: HashrateStats {
            current,
            avg_1m: current * 0.99,
            avg_15m: current * 0.97,
            total_hashes: 5_812_000_000_000 + seconds * current as u64,
            history,
        },
        devices: vec![
            DeviceSnapshot {
                index: 0,
                name: "Apple M5 Pro".into(),
                kind: DeviceKind::Cpu,
                backend: "ARMv8 SHA2 ×14 threads".into(),
                hashrate: current * 0.5,
                total_hashes: 0,
                found: 21,
                errors: 0,
                temperature_c: Some(71.0),
                power_w: None,
                status: "hashing".into(),
            },
            DeviceSnapshot {
                index: 1,
                name: "Apple M5 Pro".into(),
                kind: DeviceKind::Gpu,
                backend: "wgpu / Metal".into(),
                hashrate: current * 0.5,
                total_hashes: 0,
                found: 12,
                errors: 0,
                temperature_c: None,
                power_w: None,
                status: "hashing, batch 2^24".into(),
            },
        ],
        connection: ConnectionSnapshot {
            mode: "Stratum".into(),
            url: "public-pool.io:21496".into(),
            connected: true,
            user: "bc1qar0s…f5mdq.hansolo".into(),
            connected_since: ago(3_650),
            last_work_at: ago(seconds % 30 + 4),
            latency_ms: Some(38),
            share_difficulty: 1.0e6,
            server: Some("public-pool".into()),
            reconnects: 0,
            last_error: None,
        },
        job: Some(JobSnapshot {
            work_id: 412,
            job_id: "6a1f".into(),
            height: Some(915_344),
            prev_hash: "00000000000000000001b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f607".into(),
            version: 0x2000_0000,
            bits,
            time: 1_789_000_000,
            network_target: hex(Target::from_compact(bits)),
            share_target: hex(Target::from_difficulty(1.0e6)),
            merkle_branch_len: 12,
            tx_count: None,
            coinbase_value: Some(318_742_113),
            coinbase_hex: "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff2503d0f70d04a5b1c36808000000000000000f2f7075626c69632d706f6f6c2f00ffffffff02".into(),
            received_at: ago(seconds % 30 + 4),
            jobs_received: 412,
        }),
        shares: ShareStats {
            submitted: 34,
            accepted: 33,
            rejected: 0,
            stale: 1,
            best_difficulty: 12.6e6,
            best_ever_difficulty: 431.0e6,
            best_hash: None,
            last_share_at: ago(seconds % 40 + 2),
            blocks_found: 0,
            recent,
        },
        network: NetworkSnapshot {
            height: Some(915_344),
            difficulty: net,
            hashrate: Some(net * 4.294_967_296e9 / 600.0),
            block_reward: Some(318_742_113),
            chain: Some("main".into()),
        },
        hardware: HardwareReport {
            os: "macOS 26.6".into(),
            arch: "aarch64".into(),
            cpu_brand: "Apple M5 Pro".into(),
            physical_cores: 14,
            logical_cores: 14,
            memory_bytes: 48 << 30,
            cpu_features: vec![("ARMv8 SHA2".into(), true), ("NEON".into(), true), ("SHA-NI".into(), false), ("AVX2".into(), false)],
            gpus: vec![GpuReport {
                name: "Apple M5 Pro".into(),
                api: "Metal".into(),
                device_type: "integrated".into(),
                usable: true,
                note: "780 MH/s".into(),
            }],
            asics: vec![],
            benchmarks: vec![
                BenchmarkResult { backend: "armv8-sha2".into(), kind: DeviceKind::Cpu, hashrate: 58.1e6, selected: true },
                BenchmarkResult { backend: "scalar".into(), kind: DeviceKind::Cpu, hashrate: 9.9e6, selected: false },
                BenchmarkResult { backend: "wgpu/Metal".into(), kind: DeviceKind::Gpu, hashrate: 780e6, selected: true },
            ],
            strategy: "ARMv8 SHA2 extensions were 5.9× faster than portable code, so all 14 cores use them. The integrated GPU adds a Metal compute queue at intensity 6.".into(),
        },
        log,
    }
}

fn hex(target: Target) -> String {
    target.0.iter().map(|b| format!("{b:02x}")).collect()
}
