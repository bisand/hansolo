//! Test helpers: a scalar hashing device and snapshot polling.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use hansolo_core::snapshot::HardwareReport;
use hansolo_core::{Device, DeviceCtx, DeviceInfo, DeviceKind, Found, MinerSnapshot, Work, sha};
use hansolo_engine::Miner;

/// Hashes with the portable SHA-256 from hansolo-core, one lane per device.
pub struct ScalarDevice {
    pub name: String,
    /// Nonces per batch before re-checking stop/work.
    pub batch: u32,
    /// Sleep after each batch, to slow the device down.
    pub pause: Option<Duration>,
    /// Report every hash as a solution, without checking the target, to make
    /// sure the engine refuses them.
    pub liar: bool,
}

impl ScalarDevice {
    pub fn boxed(name: &str) -> Box<dyn Device> {
        Box::new(ScalarDevice {
            name: name.into(),
            batch: 2048,
            pause: None,
            liar: false,
        })
    }
}

impl Device for ScalarDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            name: self.name.clone(),
            kind: DeviceKind::Cpu,
            backend: "test-scalar".into(),
            detail: String::new(),
        }
    }

    fn run(self: Box<Self>, ctx: DeviceCtx) {
        let lane = ctx.lane(0);
        let mut generation = u64::MAX;
        let mut work: Option<Arc<Work>> = None;
        let mut roll = 0u32;
        let mut nonce = 0u32;
        let mut extranonce = Work::extranonce(lane, roll);
        let mut header = [0u8; 80];
        let mut midstate = [0u32; 8];
        while !ctx.stopping() {
            if ctx.work.generation() != generation {
                generation = ctx.work.generation();
                work = ctx.work.get();
                roll = 0;
                nonce = 0;
                if let Some(w) = &work {
                    extranonce = Work::extranonce(lane, roll);
                    header = w.header(&extranonce);
                    midstate = sha::midstate(&header);
                }
                ctx.stats.set_status(if work.is_some() {
                    "hashing"
                } else {
                    "waiting for work"
                });
            }
            let Some(w) = &work else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            for _ in 0..self.batch {
                header[76..80].copy_from_slice(&nonce.to_le_bytes());
                let hash = sha::header_hash_from_midstate(&midstate, &header);
                let report = if self.liar {
                    nonce.is_multiple_of(64)
                } else {
                    w.share_target.is_met_by(&hash)
                };
                if report {
                    ctx.stats.found.fetch_add(1, Ordering::Relaxed);
                    let mut claimed = hash;
                    if self.liar {
                        claimed = [0; 32];
                    }
                    let _ = ctx.found.send(Found {
                        work_id: w.id,
                        device: ctx.index,
                        extranonce,
                        header,
                        hash: claimed,
                    });
                }
                nonce = nonce.wrapping_add(1);
                if nonce == 0 {
                    roll += 1;
                    extranonce = Work::extranonce(lane, roll);
                    header = w.header(&extranonce);
                    midstate = sha::midstate(&header);
                }
            }
            ctx.stats.add_hashes(self.batch as u64);
            if let Some(p) = self.pause {
                std::thread::sleep(p);
            }
        }
        ctx.stats.set_status("stopped");
    }
}

pub fn report() -> HardwareReport {
    HardwareReport {
        os: "test".into(),
        ..HardwareReport::default()
    }
}

/// Polls the snapshot until `pred` holds, panicking with the log after `timeout`.
pub fn wait_for(
    miner: &Miner,
    timeout: Duration,
    what: &str,
    pred: impl Fn(&MinerSnapshot) -> bool,
) -> MinerSnapshot {
    let start = Instant::now();
    loop {
        let snap = miner.snapshot();
        if pred(&snap) {
            return snap;
        }
        if start.elapsed() > timeout {
            let log: Vec<String> = snap
                .log
                .iter()
                .map(|l| format!("{:?} {}", l.level, l.message))
                .collect();
            panic!(
                "timed out waiting for {what}\nstatus {:?}\nshares {:?}\nlog:\n{}",
                snap.status,
                snap.shares,
                log.join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
