//! One mining run: its shared context, the supervisor thread, and share handling.
//!
//! Sources (Stratum, node) never talk to devices directly. They call
//! [`RunCtx::publish`] with a [`Work`] plus whatever they need later to submit
//! a solution for it ([`JobExtra`]). Devices report [`Found`]s; the found thread
//! looks the work up in the recent-jobs registry, re-hashes the header itself
//! (a device bug must never reach the pool as a stream of invalid shares),
//! records the share, and forwards a [`Submission`] to the source.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crossbeam_channel::RecvTimeoutError;
use hansolo_core::sha::sha256d;
use hansolo_core::snapshot::{
    DeviceSnapshot, HardwareReport, JobSnapshot, LogLevel, MinerStatus, ShareRecord, ShareResult,
};
use hansolo_core::target::{HASHES_PER_DIFFICULTY, hash_difficulty};
use hansolo_core::{
    Config, DeviceCtx, DeviceInfo, DeviceKind, DeviceStats, Found, Work, WorkCell, WorkSource,
};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::address::{PayoutAddress, parse_payout_address};
use crate::node::template::Template;
use crate::{DeviceSource, RECENT_SHARES, RunHandle, Shared, State, node, stats, stratum};

/// Works kept for late solutions (a share for the previous job is still good
/// until the pool cleans jobs or a new block arrives).
const RECENT_JOBS: usize = 8;

/// Configuration checked in `start`, before any thread exists.
pub(crate) struct Validated {
    pub payout: PayoutAddress,
}

pub(crate) fn validate(config: &Config) -> Result<Validated, String> {
    let payout = parse_payout_address(&config.payout_address)?;
    match &config.source {
        WorkSource::Stratum { url, .. } => {
            stratum::protocol::parse_stratum_url(url)?;
        }
        WorkSource::Node {
            rpc_url,
            rpc_user,
            cookie_file,
            ..
        } => {
            node::rpc::RpcUrl::parse(rpc_url)?;
            if cookie_file.as_deref().is_none_or(str::is_empty) && rpc_user.is_empty() {
                return Err("node mode needs an RPC user/password or a cookie file".into());
            }
        }
    }
    Ok(Validated { payout })
}

/// Source-specific data kept alongside a published work.
#[derive(Clone)]
pub(crate) enum JobExtra {
    Stratum {
        /// Connection the job arrived on; its extranonce1 is baked into the work.
        session: u64,
        /// Zero bytes the pool expects before our 8-byte extranonce in extranonce2.
        pad: usize,
    },
    Node(Arc<Template>),
}

#[derive(Clone)]
pub(crate) struct JobEntry {
    pub work: Arc<Work>,
    pub extra: JobExtra,
}

/// A verified solution on its way to the source.
pub(crate) struct Submission {
    pub entry: JobEntry,
    pub found: Found,
    /// Id of the share's record in the snapshot.
    pub share_id: u64,
    pub is_block: bool,
}

struct DeviceSlot {
    info: DeviceInfo,
    stats: Arc<DeviceStats>,
}

pub(crate) struct RunCtx {
    shared: Arc<Shared>,
    epoch: u64,
    pub config: Config,
    pub payout: PayoutAddress,
    /// The Stratum username (or node RPC user) shown in the UI.
    pub user: String,
    pub stop: Arc<AtomicBool>,
    pub work: Arc<WorkCell>,
    cancel: tokio::sync::watch::Receiver<bool>,
    submit_tx: mpsc::UnboundedSender<Submission>,
    next_work_id: AtomicU64,
    next_share_id: AtomicU64,
    jobs: Mutex<VecDeque<JobEntry>>,
    devices: Mutex<Vec<DeviceSlot>>,
    /// Hashrate estimated from benchmarks, until real samples exist.
    benchmark_estimate: Mutex<f64>,
}

impl RunCtx {
    pub(crate) fn create(
        shared: Arc<Shared>,
        epoch: u64,
        config: Config,
        validated: Validated,
    ) -> (RunHandle, Arc<RunCtx>, mpsc::UnboundedReceiver<Submission>) {
        let stop = Arc::new(AtomicBool::new(false));
        let work = Arc::new(WorkCell::new());
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let (submit_tx, submit_rx) = mpsc::unbounded_channel();
        let user = match &config.source {
            WorkSource::Stratum { username, .. } => stratum_username(&config, username.as_deref()),
            WorkSource::Node {
                rpc_user,
                cookie_file,
                ..
            } => {
                if cookie_file.as_deref().is_some_and(|c| !c.is_empty()) {
                    "(cookie)".into()
                } else {
                    rpc_user.clone()
                }
            }
        };
        let ctx = Arc::new(RunCtx {
            shared,
            epoch,
            config,
            payout: validated.payout,
            user,
            stop: stop.clone(),
            work: work.clone(),
            cancel: cancel_rx,
            submit_tx,
            next_work_id: AtomicU64::new(1),
            next_share_id: AtomicU64::new(1),
            jobs: Mutex::new(VecDeque::new()),
            devices: Mutex::new(Vec::new()),
            benchmark_estimate: Mutex::new(0.0),
        });
        let handle = RunHandle {
            epoch,
            stop,
            work,
            cancel: cancel_tx,
        };
        (handle, ctx, submit_rx)
    }

    pub(crate) fn is_current(&self) -> bool {
        self.shared.epoch.load(Ordering::SeqCst) == self.epoch
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || !self.is_current()
    }

    /// Applies `f` to the state if this run is still the live one.
    pub(crate) fn update(&self, f: impl FnOnce(&mut State)) -> bool {
        let mut st = self.shared.state.lock();
        if !self.is_current() {
            return false;
        }
        f(&mut st);
        drop(st);
        self.shared.bump();
        true
    }

    pub(crate) fn log(&self, level: LogLevel, message: impl Into<String>) {
        let message = message.into();
        self.update(|st| st.push_log(level, message));
    }

    pub(crate) fn set_status(&self, status: MinerStatus) {
        self.update(|st| st.snap.status = status);
    }

    /// Ends the run with an error the user must fix (e.g. wrong chain).
    pub(crate) fn fail(&self, message: impl Into<String>) {
        let message = message.into();
        let mut run = self.shared.run.lock();
        if run.as_ref().is_some_and(|h| h.epoch == self.epoch) {
            let handle = run.take().expect("checked");
            self.update(|st| {
                st.push_log(LogLevel::Error, message.clone());
                st.snap.status = MinerStatus::Error(message.clone());
                st.snap.connection.connected = false;
                st.snap.connection.last_error = Some(message);
            });
            self.shared.epoch.fetch_add(1, Ordering::SeqCst);
            handle.shutdown();
        }
    }

    pub(crate) fn next_work_id(&self) -> u64 {
        self.next_work_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Best available estimate of this miner's hashrate, hashes per second.
    pub(crate) fn hashrate_estimate(&self) -> f64 {
        let (avg, cur) = {
            let st = self.shared.state.lock();
            (st.snap.hashrate.avg_1m, st.snap.hashrate.current)
        };
        if avg > 0.0 {
            avg
        } else if cur > 0.0 {
            cur
        } else {
            *self.benchmark_estimate.lock()
        }
    }

    /// Publishes work to the devices and records it for later solutions.
    pub(crate) fn publish(
        &self,
        work: Work,
        extra: JobExtra,
        tx_count: Option<usize>,
        coinbase_value: Option<u64>,
    ) {
        if self.stopping() {
            return;
        }
        let work = Arc::new(work);
        {
            let mut jobs = self.jobs.lock();
            if work.clean {
                jobs.clear();
            }
            while jobs.len() >= RECENT_JOBS {
                jobs.pop_front();
            }
            jobs.push_back(JobEntry {
                work: work.clone(),
                extra,
            });
        }
        let mut blank = work.coinbase_prefix.clone();
        blank.extend_from_slice(&[0u8; hansolo_core::work::EXTRANONCE_LEN]);
        blank.extend_from_slice(&work.coinbase_suffix);
        let mut prev_display = work.prev_hash;
        prev_display.reverse();
        let network_target = work.network_target();
        let now = SystemTime::now();
        self.update(|st| {
            let snap = &mut st.snap;
            let jobs_received = snap.job.as_ref().map_or(0, |j| j.jobs_received) + 1;
            snap.job = Some(JobSnapshot {
                work_id: work.id,
                job_id: work.job_id.clone(),
                height: work.height,
                prev_hash: hex::encode(prev_display),
                version: work.version,
                bits: work.bits,
                time: work.time,
                network_target: hex::encode(network_target.0),
                share_target: hex::encode(work.share_target.0),
                merkle_branch_len: work.merkle_branch.len(),
                tx_count,
                coinbase_value,
                coinbase_hex: hex::encode(&blank),
                received_at: Some(now),
                jobs_received,
            });
            snap.connection.last_work_at = Some(now);
            snap.connection.share_difficulty = work.share_target.difficulty();
            if work.height.is_some() {
                snap.network.height = work.height;
            }
            if snap.status != MinerStatus::Mining {
                snap.status = MinerStatus::Mining;
            }
        });
        // Devices see the new generation only after the registry knows the work,
        // so no solution can arrive for an unknown id.
        self.work.set(Some((*work).clone()));
    }

    /// Stops devices hashing (disconnected) and forgets outstanding jobs.
    pub(crate) fn clear_work(&self) {
        self.jobs.lock().clear();
        self.work.set(None);
    }

    fn device_name(&self, index: usize) -> String {
        self.devices
            .lock()
            .get(index)
            .map_or_else(|| format!("device {index}"), |d| d.info.name.clone())
    }

    pub(crate) fn device_stats(&self) -> Vec<Arc<DeviceStats>> {
        self.devices
            .lock()
            .iter()
            .map(|d| d.stats.clone())
            .collect()
    }

    /// Records a pool/node verdict on a share.
    pub(crate) fn share_result(&self, share_id: u64, result: ShareResult) {
        self.update(|st| {
            if let Some(i) = st.recent_ids.iter().position(|&id| id == share_id)
                && let Some(record) = st.snap.shares.recent.get_mut(i)
                && record.result != ShareResult::Block
            {
                record.result = result;
            }
        });
    }

    /// A block candidate the node refused: loud, and counted as rejected.
    pub(crate) fn block_rejected(&self, share_id: u64, message: String) {
        self.update(|st| {
            if let Some(i) = st.recent_ids.iter().position(|&id| id == share_id)
                && let Some(record) = st.snap.shares.recent.get_mut(i)
            {
                record.result = ShareResult::Rejected;
            }
            st.snap.shares.submitted += 1;
            st.snap.shares.rejected += 1;
            st.push_log(LogLevel::Error, message);
        });
    }

    fn push_share(st: &mut State, id: u64, record: ShareRecord) {
        let shares = &mut st.snap.shares;
        while shares.recent.len() >= RECENT_SHARES {
            shares.recent.pop_front();
            st.recent_ids.pop_front();
        }
        shares.recent.push_back(record);
        st.recent_ids.push_back(id);
    }

    /// Verifies and records one solution. Runs on the found thread.
    pub(crate) fn handle_found(&self, found: Found) {
        let entry = self
            .jobs
            .lock()
            .iter()
            .find(|j| j.work.id == found.work_id)
            .cloned();
        let device = self.device_name(found.device);
        let share_id = self.next_share_id.fetch_add(1, Ordering::Relaxed);

        let Some(entry) = entry else {
            // The job was cleaned (new block) before the device noticed.
            let difficulty = hash_difficulty(&sha256d(&found.header));
            self.update(|st| {
                st.snap.shares.stale += 1;
                Self::push_share(
                    st,
                    share_id,
                    ShareRecord {
                        at: SystemTime::now(),
                        difficulty,
                        device,
                        result: ShareResult::Stale,
                    },
                );
            });
            return;
        };

        let work = &entry.work;
        let hash = sha256d(&found.header);
        let header_matches = found.header[0..4] == work.version.to_le_bytes()
            && found.header[4..36] == work.prev_hash
            && found.header[36..68] == work.merkle_root(&found.extranonce)
            && found.header[72..76] == work.bits.to_le_bytes();
        if !header_matches || hash != found.hash || !work.share_target.is_met_by(&hash) {
            let errors = self
                .devices
                .lock()
                .get(found.device)
                .map_or(0, |d| d.stats.errors.fetch_add(1, Ordering::Relaxed) + 1);
            // Log the first few and then sparsely; a broken backend would flood.
            if errors.is_power_of_two() {
                let why = if !header_matches {
                    "header does not match the work"
                } else if hash != found.hash {
                    "reported hash is wrong"
                } else {
                    "hash does not meet the share target"
                };
                self.log(
                    LogLevel::Warning,
                    format!("{device}: discarded an invalid solution ({why}); {errors} so far"),
                );
            }
            return;
        }

        let difficulty = hash_difficulty(&hash);
        let is_block = work.network_target().is_met_by(&hash);
        let mut display = hash;
        display.reverse();
        let display = hex::encode(display);
        let local = matches!(entry.extra, JobExtra::Node(_));
        let now = SystemTime::now();

        self.update(|st| {
            let shares = &mut st.snap.shares;
            shares.last_share_at = Some(now);
            if difficulty > shares.best_difficulty {
                shares.best_difficulty = difficulty;
                shares.best_hash = Some(display.clone());
            }
            if difficulty > shares.best_ever_difficulty {
                shares.best_ever_difficulty = difficulty;
            }
            if local && !is_block {
                // Node mode: shares are local statistics, nobody to reject them.
                shares.submitted += 1;
                shares.accepted += 1;
            }
            let result = match (is_block, local) {
                (true, _) => ShareResult::Block,
                (false, true) => ShareResult::Accepted,
                (false, false) => ShareResult::Pending,
            };
            Self::push_share(
                st,
                share_id,
                ShareRecord {
                    at: now,
                    difficulty,
                    device: device.clone(),
                    result,
                },
            );
            if is_block {
                st.push_log(
                    LogLevel::Success,
                    format!(
                        "!!! BLOCK CANDIDATE !!! {device} found {display} (difficulty {}) at height {}",
                        crate::format_difficulty(difficulty),
                        work.height.map_or_else(|| "?".into(), |h| h.to_string())
                    ),
                );
            }
        });

        if local && !is_block {
            return;
        }
        let _ = self.submit_tx.send(Submission {
            entry,
            found,
            share_id,
            is_block,
        });
    }
}

fn stratum_username(config: &Config, username: Option<&str>) -> String {
    match username.map(str::trim) {
        Some(u) if !u.is_empty() => u.to_string(),
        _ if config.worker_name.trim().is_empty() => config.payout_address.trim().to_string(),
        _ => format!(
            "{}.{}",
            config.payout_address.trim(),
            config.worker_name.trim()
        ),
    }
}

/// Sum of the selected benchmarks, scaled to the configured CPU threads.
pub(crate) fn estimate_from_report(report: &HardwareReport, config: &Config) -> f64 {
    let threads = config.cpu.threads.unwrap_or(report.logical_cores).max(1) as f64;
    report
        .benchmarks
        .iter()
        .filter(|b| b.selected)
        .map(|b| match b.kind {
            DeviceKind::Cpu => b.hashrate * threads,
            _ => b.hashrate,
        })
        .sum()
}

/// A difficulty at which `hashrate` finds one share per `seconds`, rounded to a
/// power of two so small hashrate wobbles do not change it.
pub(crate) fn difficulty_for_interval(hashrate: f64, seconds: f64) -> f64 {
    let d = hashrate * seconds / HASHES_PER_DIFFICULTY;
    if !d.is_finite() || d <= 0.0 {
        return 0.0;
    }
    2f64.powi(d.log2().round() as i32)
}

/// The supervisor thread: plan, spawn devices, run the source until cancelled.
pub(crate) fn supervise(
    ctx: Arc<RunCtx>,
    source: DeviceSource,
    submit_rx: mpsc::UnboundedReceiver<Submission>,
) {
    let (report, devices) = match source {
        DeviceSource::Plan => {
            let progress_ctx = ctx.clone();
            let progress = move |line: &str| {
                if line.to_ascii_lowercase().contains("bench") {
                    progress_ctx.update(|st| {
                        if st.snap.status == MinerStatus::Detecting {
                            st.snap.status = MinerStatus::Benchmarking;
                        }
                    });
                }
                progress_ctx.log(LogLevel::Info, line);
            };
            let plan = hansolo_hash::plan(&ctx.config, &progress);
            (plan.report, plan.devices)
        }
        DeviceSource::Given(report, devices) => (*report, devices),
    };
    if ctx.stopping() {
        return;
    }
    *ctx.benchmark_estimate.lock() = estimate_from_report(&report, &ctx.config);

    let infos: Vec<DeviceInfo> = devices.iter().map(|d| d.info()).collect();
    ctx.update(|st| {
        st.snap.hardware = report;
        st.snap.devices = infos
            .iter()
            .enumerate()
            .map(|(index, info)| DeviceSnapshot {
                index,
                name: info.name.clone(),
                kind: info.kind,
                backend: info.backend.clone(),
                hashrate: 0.0,
                total_hashes: 0,
                found: 0,
                errors: 0,
                temperature_c: None,
                power_w: None,
                status: "starting".into(),
            })
            .collect();
        if infos.is_empty() {
            st.push_log(
                LogLevel::Warning,
                "No hashing devices available; connecting anyway".into(),
            );
        } else {
            for info in &infos {
                st.push_log(
                    LogLevel::Info,
                    format!(
                        "Device: {} [{}] {}",
                        info.name,
                        info.kind.label(),
                        info.backend
                    ),
                );
            }
        }
        st.snap.status = MinerStatus::Connecting;
    });

    let (found_tx, found_rx) = crossbeam_channel::unbounded::<Found>();
    for (index, (device, info)) in devices.into_iter().zip(infos).enumerate() {
        let stats = Arc::new(DeviceStats::default());
        ctx.devices.lock().push(DeviceSlot {
            info: info.clone(),
            stats: stats.clone(),
        });
        let device_ctx = DeviceCtx {
            index,
            work: ctx.work.clone(),
            found: found_tx.clone(),
            stats: stats.clone(),
            stop: ctx.stop.clone(),
        };
        let log_ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("hansolo-dev{index}"))
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    device.run(device_ctx)
                }));
                if result.is_err() {
                    stats.set_status("error: crashed");
                    log_ctx.log(LogLevel::Error, format!("{} crashed", info.name));
                }
            });
        if let Err(e) = spawned {
            ctx.log(
                LogLevel::Error,
                format!("could not start a device thread: {e}"),
            );
        }
    }
    drop(found_tx);

    let found_ctx = ctx.clone();
    let _ = std::thread::Builder::new()
        .name("hansolo-found".into())
        .spawn(move || {
            loop {
                match found_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(_) if found_ctx.stopping() => {}
                    Ok(found) => found_ctx.handle_found(found),
                    Err(RecvTimeoutError::Timeout) if found_ctx.stopping() => break,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("hansolo-net")
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            ctx.fail(format!("could not start the network runtime: {e}"));
            return;
        }
    };
    let mut cancel = ctx.cancel.clone();
    runtime.block_on(async {
        let stats_task = tokio::spawn(stats::sample_loop(ctx.clone()));
        let source = async {
            match ctx.config.source.clone() {
                WorkSource::Stratum { url, password, .. } => {
                    stratum::client::run(ctx.clone(), url, password, submit_rx).await
                }
                WorkSource::Node { .. } => node::client::run(ctx.clone(), submit_rx).await,
            }
        };
        tokio::select! {
            () = source => {}
            _ = cancel.wait_for(|c| *c) => {}
        }
        stats_task.abort();
    });
    // Anything still pending (a socket read) is dropped here, closing connections.
    runtime.shutdown_timeout(Duration::from_millis(500));
    ctx.clear_work();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        let config = Config {
            payout_address: "bc1qexample".into(),
            ..Config::default()
        };
        assert_eq!(stratum_username(&config, None), "bc1qexample.hansolo");
        assert_eq!(stratum_username(&config, Some("  ")), "bc1qexample.hansolo");
        assert_eq!(stratum_username(&config, Some("me.rig")), "me.rig");
    }

    #[test]
    fn interval_difficulty() {
        assert_eq!(difficulty_for_interval(0.0, 15.0), 0.0);
        // 2^32 H/s for one second is difficulty ~1.
        assert_eq!(difficulty_for_interval(HASHES_PER_DIFFICULTY, 1.0), 1.0);
        assert_eq!(
            difficulty_for_interval(HASHES_PER_DIFFICULTY * 3.0, 1.0),
            4.0
        );
    }
}
