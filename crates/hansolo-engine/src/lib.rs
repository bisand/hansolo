//! The HanSolo mining engine.
//!
//! PUBLIC API CONTRACT — the app depends on exactly these items on [`Miner`]:
//! `new`, `snapshot`, `revision`, `start`, `stop`, `is_running`,
//! `detect_hardware`, `set_best_ever`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hansolo_core::{Config, MinerSnapshot};
use parking_lot::Mutex;

/// A handle to the miner. Cheap to clone; every clone drives the same miner.
#[derive(Clone, Default)]
pub struct Miner {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    snapshot: Mutex<MinerSnapshot>,
    revision: AtomicU64,
}

impl Miner {
    /// An idle miner. Spawns nothing until [`start`](Miner::start) or
    /// [`detect_hardware`](Miner::detect_hardware).
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of the current state.
    pub fn snapshot(&self) -> MinerSnapshot {
        self.shared.snapshot.lock().clone()
    }

    /// Increments whenever the snapshot changes, so a UI can skip cloning.
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Relaxed)
    }

    /// Starts mining with `config`, stopping any previous run first.
    /// Returns an error for configuration that cannot work (e.g. an invalid address).
    pub fn start(&self, config: Config) -> Result<(), String> {
        let _ = config;
        Err("engine not implemented yet".into())
    }

    /// Stops mining. Returns promptly; devices wind down in the background.
    pub fn stop(&self) {}

    pub fn is_running(&self) -> bool {
        false
    }

    /// Runs hardware detection in the background and fills `snapshot().hardware`.
    pub fn detect_hardware(&self, config: &Config) {
        let _ = config;
    }

    /// Seeds the persisted best-ever share difficulty.
    pub fn set_best_ever(&self, difficulty: f64) {
        self.shared.snapshot.lock().shares.best_ever_difficulty = difficulty;
        self.shared.revision.fetch_add(1, Ordering::Relaxed);
    }
}
