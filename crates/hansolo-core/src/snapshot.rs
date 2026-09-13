//! Everything the UI shows, as plain data.
//!
//! The engine owns a [`MinerSnapshot`] behind a lock and updates it about once
//! a second and on every event; the UI clones it when it wakes. Nothing in here
//! refers back into the engine, so the UI can never block mining.

use std::collections::VecDeque;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::device::DeviceKind;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum MinerStatus {
    #[default]
    Stopped,
    /// Probing CPU features, GPUs and ASICs.
    Detecting,
    /// Timing candidate hashing paths to pick the fastest.
    Benchmarking,
    /// Waiting on the pool or node.
    Connecting,
    Mining,
    /// Connected before, lost it, retrying.
    Reconnecting,
    Error(String),
}

impl MinerStatus {
    pub fn label(&self) -> &str {
        match self {
            MinerStatus::Stopped => "Stopped",
            MinerStatus::Detecting => "Detecting hardware",
            MinerStatus::Benchmarking => "Benchmarking",
            MinerStatus::Connecting => "Connecting",
            MinerStatus::Mining => "Mining",
            MinerStatus::Reconnecting => "Reconnecting",
            MinerStatus::Error(_) => "Error",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MinerSnapshot {
    pub status: MinerStatus,
    pub started_at: Option<SystemTime>,
    pub hashrate: HashrateStats,
    pub devices: Vec<DeviceSnapshot>,
    pub connection: ConnectionSnapshot,
    pub job: Option<JobSnapshot>,
    pub shares: ShareStats,
    pub network: NetworkSnapshot,
    pub hardware: HardwareReport,
    /// Newest last. Bounded by the engine.
    pub log: VecDeque<LogEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HashrateStats {
    /// Hashes per second over the last few seconds.
    pub current: f64,
    pub avg_1m: f64,
    pub avg_15m: f64,
    pub total_hashes: u64,
    /// One sample per second, oldest first, at most 15 minutes.
    pub history: VecDeque<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    pub index: usize,
    pub name: String,
    pub kind: DeviceKind,
    pub backend: String,
    pub hashrate: f64,
    pub total_hashes: u64,
    pub found: u64,
    pub errors: u64,
    pub temperature_c: Option<f32>,
    pub power_w: Option<f32>,
    pub status: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionSnapshot {
    /// "Stratum" or "Bitcoin Core".
    pub mode: String,
    pub url: String,
    pub connected: bool,
    /// Worker/user name used to authorise.
    pub user: String,
    pub connected_since: Option<SystemTime>,
    pub last_work_at: Option<SystemTime>,
    pub latency_ms: Option<u32>,
    /// Pool-assigned difficulty (Stratum), or the local share difficulty (node).
    pub share_difficulty: f64,
    /// Server software/version when it says (node `subversion`, pool banner).
    pub server: Option<String>,
    pub reconnects: u32,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub work_id: u64,
    pub job_id: String,
    pub height: Option<u64>,
    /// Display order (as explorers print it).
    pub prev_hash: String,
    pub version: u32,
    pub bits: u32,
    pub time: u32,
    pub network_target: String,
    pub share_target: String,
    pub merkle_branch_len: usize,
    /// Transactions in the template, when known (node mode).
    pub tx_count: Option<usize>,
    /// Reward in satoshis, when known.
    pub coinbase_value: Option<u64>,
    pub coinbase_hex: String,
    pub received_at: Option<SystemTime>,
    /// Jobs received since the miner started.
    pub jobs_received: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShareStats {
    pub submitted: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub stale: u64,
    /// Best difficulty found this session.
    pub best_difficulty: f64,
    /// Best ever, persisted across sessions by the application.
    pub best_ever_difficulty: f64,
    /// Display-order hash of the session best.
    pub best_hash: Option<String>,
    pub last_share_at: Option<SystemTime>,
    pub blocks_found: u64,
    /// The most recent shares, newest last, bounded.
    pub recent: VecDeque<ShareRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareRecord {
    pub at: SystemTime,
    pub difficulty: f64,
    pub device: String,
    pub result: ShareResult,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShareResult {
    Pending,
    Accepted,
    Rejected,
    Stale,
    /// Met the network target. The one that matters.
    Block,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NetworkSnapshot {
    pub height: Option<u64>,
    pub difficulty: f64,
    /// Estimated network hashrate, hashes per second.
    pub hashrate: Option<f64>,
    /// Current block subsidy plus fees, satoshis, when known.
    pub block_reward: Option<u64>,
    pub chain: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HardwareReport {
    pub os: String,
    pub arch: String,
    pub cpu_brand: String,
    pub physical_cores: usize,
    pub logical_cores: usize,
    pub memory_bytes: u64,
    /// ("SHA-NI", true), ("AVX2", false), …
    pub cpu_features: Vec<(String, bool)>,
    pub gpus: Vec<GpuReport>,
    pub asics: Vec<AsicReport>,
    pub benchmarks: Vec<BenchmarkResult>,
    /// A sentence or two on what was chosen and why.
    pub strategy: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GpuReport {
    pub name: String,
    /// "Metal", "Vulkan", "DX12", …
    pub api: String,
    pub device_type: String,
    pub usable: bool,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AsicReport {
    pub name: String,
    /// "usb:/dev/ttyUSB0" or "http://192.168.1.50".
    pub location: String,
    pub supported: bool,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub backend: String,
    pub kind: DeviceKind,
    /// Hashes per second on one thread (CPU) or the whole device (GPU).
    pub hashrate: f64,
    pub selected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub at: SystemTime,
    pub level: LogLevel,
    pub message: String,
}

/// Probability of finding at least one block within `seconds` at `hashrate`.
pub fn block_probability(hashrate: f64, network_difficulty: f64, seconds: f64) -> f64 {
    if hashrate <= 0.0 || network_difficulty <= 0.0 {
        return 0.0;
    }
    let expected = hashrate * seconds / (network_difficulty * crate::target::HASHES_PER_DIFFICULTY);
    -(-expected).exp_m1()
}

/// Expected seconds until a block at `hashrate`.
pub fn expected_seconds_to_block(hashrate: f64, network_difficulty: f64) -> Option<f64> {
    (hashrate > 0.0 && network_difficulty > 0.0)
        .then(|| network_difficulty * crate::target::HASHES_PER_DIFFICULTY / hashrate)
}
