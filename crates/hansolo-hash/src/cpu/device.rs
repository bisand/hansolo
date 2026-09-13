//! The CPU as a [`Device`]: N worker threads on one backend.
//!
//! Each worker owns a lane (`ctx.lane(thread_index)`) and walks its own
//! extranonce rolls, so threads never overlap. Batches are sized adaptively to
//! take about [`BATCH_TARGET`], which keeps the stop flag and new-work checks
//! responsive on anything from a Pi Zero to a workstation without making the
//! per-batch bookkeeping a measurable cost.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hansolo_core::{Device, DeviceCtx, DeviceInfo, DeviceKind, Work};

use super::{Backend, Job};
use crate::{priority, verify_candidate};

/// How long one batch of nonces should take.
const BATCH_TARGET: Duration = Duration::from_millis(25);
const MIN_BATCH: u32 = 1 << 10;
const MAX_BATCH: u32 = 1 << 22;
const NONCE_SPACE: u64 = 1 << 32;

pub struct CpuDevice {
    backend: Backend,
    threads: usize,
    low_priority: bool,
    name: String,
    logical_cores: usize,
}

impl CpuDevice {
    /// `threads` is clamped to 1..=65536 (one sub-lane each).
    pub fn new(
        backend: Backend,
        threads: usize,
        low_priority: bool,
        name: impl Into<String>,
    ) -> Self {
        let logical_cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        CpuDevice {
            backend,
            threads: threads.clamp(1, 1 << 16),
            low_priority,
            name: name.into(),
            logical_cores,
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    pub fn threads(&self) -> usize {
        self.threads
    }
}

impl Device for CpuDevice {
    fn info(&self) -> DeviceInfo {
        let plural = if self.threads == 1 {
            "thread"
        } else {
            "threads"
        };
        DeviceInfo {
            name: self.name.clone(),
            kind: DeviceKind::Cpu,
            backend: format!("{} ×{} {plural}", self.backend.label(), self.threads),
            detail: format!(
                "{} worker {plural} on {} logical cores{}",
                self.threads,
                self.logical_cores,
                if self.low_priority {
                    ", low priority"
                } else {
                    ""
                }
            ),
        }
    }

    fn run(self: Box<Self>, ctx: DeviceCtx) {
        // Pinning helps on Linux and Windows, where the scheduler otherwise
        // migrates busy threads; macOS ignores affinity on Apple Silicon.
        let cores = if cfg!(any(target_os = "linux", windows)) {
            core_affinity::get_core_ids().filter(|ids| self.threads <= ids.len())
        } else {
            None
        };
        let this = &*self;
        let ctx = &ctx;
        std::thread::scope(|s| {
            for t in 0..this.threads {
                let core = cores.as_ref().map(|ids| ids[t]);
                std::thread::Builder::new()
                    .name(format!("hansolo-cpu-{t}"))
                    .spawn_scoped(s, move || {
                        if let Some(core) = core {
                            core_affinity::set_for_current(core);
                        }
                        if this.low_priority {
                            priority::lower_current_thread();
                        }
                        worker(this.backend, ctx, t);
                    })
                    .expect("spawn CPU worker thread");
            }
        });
        ctx.stats.set_status("stopped");
    }
}

fn worker(backend: Backend, ctx: &DeviceCtx, thread: usize) {
    let lane = ctx.lane(thread as u16);
    // Only the first thread writes the shared status line.
    let lead = thread == 0;
    let mut roll: u32 = 0;
    let mut batch = MIN_BATCH * 16;
    let mut candidates = Vec::new();
    let mut waiting = false;

    while !ctx.stopping() {
        let generation = ctx.work.generation();
        let Some(work) = ctx.work.get() else {
            if lead && !waiting {
                ctx.stats.set_status("waiting for work");
                waiting = true;
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        if lead {
            ctx.stats.set_status("hashing");
            waiting = false;
        }
        mine_work(
            backend,
            ctx,
            &work,
            lane,
            generation,
            &mut roll,
            &mut batch,
            &mut candidates,
        );
    }
}

/// Mines `work` until it's replaced or the miner stops, rolling the extranonce
/// every time the nonce space runs out.
#[allow(clippy::too_many_arguments)]
fn mine_work(
    backend: Backend,
    ctx: &DeviceCtx,
    work: &Arc<Work>,
    lane: u32,
    generation: u64,
    roll: &mut u32,
    batch: &mut u32,
    candidates: &mut Vec<u32>,
) {
    let step = backend.lanes();
    loop {
        // Rolls keep counting across works, so re-sent identical work never
        // repeats a header.
        let extranonce = Work::extranonce(lane, *roll);
        *roll = roll.wrapping_add(1);
        let header = work.header(&extranonce);
        let job = Job::new(&header, &work.share_target);

        let mut next: u64 = 0;
        while next < NONCE_SPACE {
            let count = (*batch as u64).min(NONCE_SPACE - next) as u32;
            let started = Instant::now();
            backend.search(&job, next as u32, count, candidates);
            ctx.stats.add_hashes(count as u64);
            for &nonce in candidates.iter() {
                verify_candidate(ctx, work, extranonce, &header, &job.midstate, nonce);
            }
            candidates.clear();
            next += count as u64;

            // Aim the next batch at BATCH_TARGET.
            let secs = started.elapsed().as_secs_f64().max(1e-6);
            let ideal = count as f64 * BATCH_TARGET.as_secs_f64() / secs;
            let smoothed = (*batch as f64 * 0.5 + ideal * 0.5) as u32;
            *batch = (smoothed.clamp(MIN_BATCH, MAX_BATCH) / step).max(1) * step;

            if ctx.stopping() || ctx.work.generation() != generation {
                return;
            }
        }
    }
}

impl std::fmt::Debug for CpuDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuDevice")
            .field("backend", &self.backend)
            .field("threads", &self.threads)
            .field("low_priority", &self.low_priority)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use hansolo_core::sha::{header_hash_from_midstate, midstate};
    use hansolo_core::{DeviceStats, Target, WorkCell};

    use super::*;

    fn work(share_target: Target) -> Work {
        Work {
            id: 42,
            job_id: "t".into(),
            version: 0x2000_0000,
            prev_hash: [7; 32],
            bits: 0x1d00ffff,
            time: 1_700_000_000,
            coinbase_prefix: vec![1, 2, 3, 4],
            coinbase_suffix: vec![5, 6, 7],
            merkle_branch: vec![[9; 32]],
            share_target,
            height: Some(1),
            clean: false,
        }
    }

    /// Runs a CPU device against easy work (difficulty 2^-16, top word
    /// 0x0000_FFFF, so the early reject passes far more than it would at
    /// difficulty 1) and checks every reported share independently.
    #[test]
    fn finds_valid_shares_and_stops() {
        let target = Target::from_difficulty(1.0 / 65536.0);
        assert!(
            target.top_word() > 0,
            "the test needs a target easier than 32 zero bits"
        );
        let cell = Arc::new(WorkCell::new());
        let (tx, rx) = crossbeam_channel::unbounded();
        let stats = Arc::new(DeviceStats::default());
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = DeviceCtx {
            index: 3,
            work: cell.clone(),
            found: tx,
            stats: stats.clone(),
            stop: stop.clone(),
        };
        let backend = *Backend::supported().last().unwrap();
        let device = Box::new(CpuDevice::new(backend, 2, true, "test"));

        let handle = std::thread::spawn(move || device.run(ctx));
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(stats.status.lock().as_str(), "waiting for work");

        cell.set(Some(work(target)));
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut found = Vec::new();
        while found.len() < 400 && Instant::now() < deadline {
            if let Ok(f) = rx.recv_timeout(Duration::from_millis(100)) {
                found.push(f);
            }
        }
        let stop_at = Instant::now();
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        assert!(
            stop_at.elapsed() < Duration::from_millis(500),
            "device took too long to stop"
        );
        assert!(found.len() >= 400, "only {} shares", found.len());

        let w = work(target);
        let mut lanes = std::collections::HashSet::new();
        for f in &found {
            assert_eq!(f.work_id, 42);
            assert_eq!(f.device, 3);
            let lane = u32::from_be_bytes(f.extranonce[..4].try_into().unwrap());
            assert_eq!(lane >> 16, 3);
            lanes.insert(lane);
            let mut expected = w.header(&f.extranonce);
            expected[76..80].copy_from_slice(&f.nonce().to_le_bytes());
            assert_eq!(f.header, expected);
            let hash = header_hash_from_midstate(&midstate(&expected), &expected);
            assert_eq!(f.hash, hash);
            assert!(target.is_met_by(&hash));
        }
        assert_eq!(lanes.len(), 2, "both threads should find shares");
        assert!(stats.hashes.load(Ordering::Relaxed) > 0);
        assert_eq!(
            stats.found.load(Ordering::Relaxed) as usize,
            found.len() + rx.len()
        );
    }
}
