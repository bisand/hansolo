//! The HanSolo mining engine.
//!
//! PUBLIC API CONTRACT — the app depends on exactly these items on [`Miner`]:
//! `new`, `snapshot`, `revision`, `start`, `stop`, `is_running`,
//! `detect_hardware`, `set_best_ever`.
//!
//! # Shape of a run
//!
//! [`Miner::start`] validates the configuration synchronously (so the UI gets an
//! immediate error for a bad address) and hands everything else to a
//! *supervisor* thread, because hardware planning benchmarks for seconds:
//!
//! 1. `hansolo_hash::plan` detects and benchmarks, and returns devices.
//! 2. Each device gets an OS thread running [`Device::run`](hansolo_core::Device::run).
//! 3. A *found* thread drains the crossbeam channel devices report into,
//!    re-verifies every header and records the share ([`run`] module).
//! 4. A small tokio runtime runs the work source (Stratum or Bitcoin Core) and
//!    the once-a-second statistics sampler.
//!
//! All of it writes into one [`MinerSnapshot`] behind a mutex. Every run carries
//! an *epoch*; [`Miner::stop`] bumps the shared epoch before it touches the
//! snapshot, and a run whose epoch is stale can no longer write. That is what
//! makes stop/start safe to hammer from a UI without waiting for old threads
//! (a device finishing its batch, a socket timing out) to wind down.

mod address;
mod format;
mod net;
mod node;
mod run;
mod stats;
mod stratum;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

use hansolo_core::snapshot::{HardwareReport, LogEntry, LogLevel, MinerStatus};
use hansolo_core::{Config, Device, MinerSnapshot, WorkCell, WorkSource};
use parking_lot::Mutex;

pub use address::{PayoutAddress, parse_payout_address};
pub use format::{format_difficulty, format_hashrate, format_status_line};
pub use node::template::{bip34_height_push, local_share_difficulty};
pub use stratum::protocol::{StratumEndpoint, parse_stratum_url};

/// Log lines kept in the snapshot.
pub const LOG_CAPACITY: usize = 500;
/// Shares kept in `snapshot.shares.recent`.
pub const RECENT_SHARES: usize = 50;
/// Hashrate history samples (one per second, 15 minutes).
pub const HISTORY_LEN: usize = 900;

/// A handle to the miner. Cheap to clone; every clone drives the same miner.
#[derive(Clone, Default)]
pub struct Miner {
    shared: Arc<Shared>,
}

#[derive(Default)]
pub(crate) struct Shared {
    state: Mutex<State>,
    revision: AtomicU64,
    /// Bumped by every start and stop. A run may only write while it matches.
    epoch: AtomicU64,
    run: Mutex<Option<RunHandle>>,
}

/// The snapshot plus bookkeeping that must change under the same lock.
#[derive(Default)]
pub(crate) struct State {
    pub snap: MinerSnapshot,
    /// Engine ids of `snap.shares.recent`, index-aligned, so a pool response
    /// can find its record after newer shares have arrived.
    pub recent_ids: VecDeque<u64>,
}

impl State {
    pub(crate) fn push_log(&mut self, level: LogLevel, message: String) {
        let log = &mut self.snap.log;
        while log.len() >= LOG_CAPACITY {
            log.pop_front();
        }
        log.push_back(LogEntry {
            at: SystemTime::now(),
            level,
            message,
        });
    }
}

impl Shared {
    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    /// Writes regardless of epoch; for the handle's own transitions.
    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock());
        self.bump();
    }
}

/// What `stop` needs to tear a run down.
pub(crate) struct RunHandle {
    pub epoch: u64,
    pub stop: Arc<AtomicBool>,
    pub work: Arc<WorkCell>,
    pub cancel: tokio::sync::watch::Sender<bool>,
}

impl RunHandle {
    fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.work.set(None);
        // Dropping the source future closes its socket; see `run::supervise`.
        let _ = self.cancel.send(true);
    }
}

/// Where a run's devices come from.
pub(crate) enum DeviceSource {
    Plan,
    Given(Box<HardwareReport>, Vec<Box<dyn Device>>),
}

impl Miner {
    /// An idle miner. Spawns nothing until [`start`](Miner::start) or
    /// [`detect_hardware`](Miner::detect_hardware).
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of the current state.
    pub fn snapshot(&self) -> MinerSnapshot {
        self.shared.state.lock().snap.clone()
    }

    /// Increments whenever the snapshot changes, so a UI can skip cloning.
    pub fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Relaxed)
    }

    /// Starts mining with `config`, stopping any previous run first.
    /// Returns an error for configuration that cannot work (e.g. an invalid address).
    pub fn start(&self, config: Config) -> Result<(), String> {
        self.start_inner(config, DeviceSource::Plan)
    }

    /// Like [`start`](Miner::start), but mines with the given devices instead of
    /// running `hansolo_hash::plan`. For embedders with their own hardware
    /// layer, and for tests.
    pub fn start_with_devices(
        &self,
        config: Config,
        report: HardwareReport,
        devices: Vec<Box<dyn Device>>,
    ) -> Result<(), String> {
        self.start_inner(config, DeviceSource::Given(Box::new(report), devices))
    }

    fn start_inner(&self, config: Config, devices: DeviceSource) -> Result<(), String> {
        let validated = run::validate(&config)?;
        let mut run = self.shared.run.lock();
        if let Some(old) = run.take() {
            self.shared.epoch.fetch_add(1, Ordering::SeqCst);
            old.shutdown();
        }
        let epoch = self.shared.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let (handle, ctx, submit_rx) =
            run::RunCtx::create(self.shared.clone(), epoch, config, validated);

        self.shared.update(|st| {
            let snap = &mut st.snap;
            let best_ever = snap.shares.best_ever_difficulty;
            let log = std::mem::take(&mut snap.log);
            let hardware = std::mem::take(&mut snap.hardware);
            *snap = MinerSnapshot::default();
            snap.log = log;
            snap.hardware = hardware;
            snap.shares.best_ever_difficulty = best_ever;
            snap.status = MinerStatus::Detecting;
            snap.started_at = Some(SystemTime::now());
            let (mode, url) = match &ctx.config.source {
                WorkSource::Stratum { url, .. } => ("Stratum", url.clone()),
                WorkSource::Node { rpc_url, .. } => ("Bitcoin Core", rpc_url.clone()),
            };
            snap.connection.mode = mode.into();
            snap.connection.url = url;
            snap.connection.user = ctx.user.clone();
            st.recent_ids.clear();
            st.push_log(
                LogLevel::Info,
                format!("Starting ({mode}), paying to {}", ctx.config.payout_address),
            );
        });

        std::thread::Builder::new()
            .name("hansolo-supervisor".into())
            .spawn(move || run::supervise(ctx, devices, submit_rx))
            .map_err(|e| format!("could not spawn the miner thread: {e}"))?;
        *run = Some(handle);
        Ok(())
    }

    /// Stops mining. Returns promptly; devices wind down in the background.
    pub fn stop(&self) {
        let mut run = self.shared.run.lock();
        let Some(handle) = run.take() else { return };
        // Epoch first: from here on the old run cannot overwrite what we write.
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        handle.shutdown();
        self.shared.update(|st| {
            let snap = &mut st.snap;
            snap.status = MinerStatus::Stopped;
            snap.connection.connected = false;
            snap.hashrate.current = 0.0;
            for d in &mut snap.devices {
                d.hashrate = 0.0;
                d.status = "stopped".into();
            }
            st.push_log(LogLevel::Info, "Stopped".into());
        });
    }

    pub fn is_running(&self) -> bool {
        self.shared.run.lock().is_some()
    }

    /// Runs hardware detection in the background and fills `snapshot().hardware`.
    pub fn detect_hardware(&self, config: &Config) {
        let shared = self.shared.clone();
        let config = config.clone();
        let spawned = std::thread::Builder::new()
            .name("hansolo-detect".into())
            .spawn(move || {
                let report = hansolo_hash::detect(&config);
                // A running miner owns `hardware` (its report includes benchmarks).
                if shared.run.lock().is_none() {
                    shared.update(|st| st.snap.hardware = report);
                }
            });
        if let Err(e) = spawned {
            self.shared.update(|st| {
                st.push_log(
                    LogLevel::Error,
                    format!("hardware detection failed to start: {e}"),
                )
            });
        }
    }

    /// Seeds the persisted best-ever share difficulty.
    pub fn set_best_ever(&self, difficulty: f64) {
        self.shared
            .update(|st| st.snap.shares.best_ever_difficulty = difficulty);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miner_is_a_shareable_handle() {
        fn assert_handle<T: Clone + Send + Sync + 'static>() {}
        assert_handle::<Miner>();
    }

    #[test]
    fn idle_miner() {
        let miner = Miner::new();
        assert!(!miner.is_running());
        miner.stop(); // no-op
        let rev = miner.revision();
        miner.set_best_ever(42.0);
        assert!(miner.revision() > rev);
        assert_eq!(miner.snapshot().shares.best_ever_difficulty, 42.0);
        assert_eq!(miner.snapshot().status, MinerStatus::Stopped);
    }
}
