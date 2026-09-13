//! The device trait.
//!
//! A device is anything that turns work into hashes: a pool of CPU threads using
//! one instruction set, a GPU queue, an ASIC. Each runs on a thread of its own
//! for as long as the miner runs, so a device that blocks on a USB read or a
//! GPU fence costs nobody else anything.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use crossbeam_channel::Sender;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::work::{Found, WorkCell};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeviceKind {
    Cpu,
    Gpu,
    Asic,
}

impl DeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            DeviceKind::Cpu => "CPU",
            DeviceKind::Gpu => "GPU",
            DeviceKind::Asic => "ASIC",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Human name, e.g. "Apple M5 Pro" or "NVIDIA GeForce RTX 4070".
    pub name: String,
    pub kind: DeviceKind,
    /// The hashing path in use, e.g. "ARMv8 SHA2 ×14", "AVX2 8-lane", "wgpu/Metal".
    pub backend: String,
    /// One more line of detail for the hardware page.
    pub detail: String,
}

/// Live counters a device updates and the engine samples.
pub struct DeviceStats {
    /// Total hashes computed. Monotonic; the engine derives rates from deltas.
    pub hashes: AtomicU64,
    /// Hashes that met the share target.
    pub found: AtomicU64,
    pub errors: AtomicU64,
    /// Millidegrees Celsius, or `i64::MIN` when unknown.
    pub temperature_mc: AtomicI64,
    /// Milliwatts, or 0 when unknown.
    pub power_mw: AtomicU64,
    /// Short status line ("hashing", "waiting for work", "error: …").
    pub status: Mutex<String>,
}

impl Default for DeviceStats {
    fn default() -> Self {
        Self {
            hashes: AtomicU64::new(0),
            found: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            temperature_mc: AtomicI64::new(i64::MIN),
            power_mw: AtomicU64::new(0),
            status: Mutex::new(String::from("starting")),
        }
    }
}

impl DeviceStats {
    #[inline]
    pub fn add_hashes(&self, n: u64) {
        self.hashes.fetch_add(n, Ordering::Relaxed);
    }

    pub fn set_status(&self, status: impl Into<String>) {
        *self.status.lock() = status.into();
    }

    pub fn temperature_c(&self) -> Option<f32> {
        match self.temperature_mc.load(Ordering::Relaxed) {
            i64::MIN => None,
            mc => Some(mc as f32 / 1000.0),
        }
    }
}

/// Everything a running device is handed.
pub struct DeviceCtx {
    /// This device's index; the high bits of every lane it uses.
    pub index: usize,
    pub work: Arc<WorkCell>,
    pub found: Sender<Found>,
    pub stats: Arc<DeviceStats>,
    /// Set when the miner stops. Check it at least every ~100 ms.
    pub stop: Arc<AtomicBool>,
}

impl DeviceCtx {
    #[inline]
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The lane for a sub-lane (thread, GPU queue slot) of this device.
    #[inline]
    pub fn lane(&self, sub_lane: u16) -> u32 {
        ((self.index as u32) << 16) | sub_lane as u32
    }
}

pub trait Device: Send {
    fn info(&self) -> DeviceInfo;

    /// Hashes until `ctx.stop` is set. Runs on a thread of its own.
    fn run(self: Box<Self>, ctx: DeviceCtx);
}
