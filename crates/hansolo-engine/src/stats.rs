//! Once-a-second statistics.
//!
//! Devices only bump monotonic counters; rates are derived here from deltas.
//! GPUs and ASICs report in bursts (a whole batch at once), so the per-device
//! figure is exponentially smoothed; the 1- and 15-minute averages are plain
//! means of the raw per-second totals, which bursts cannot distort.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::HISTORY_LEN;
use crate::run::RunCtx;

/// Weight of the newest sample in the smoothed per-device rate.
const SMOOTHING: f64 = 0.3;

#[derive(Default)]
pub(crate) struct Sampler {
    last: Option<Instant>,
    last_hashes: Vec<u64>,
    smoothed: Vec<f64>,
    raw_totals: VecDeque<f64>,
}

/// One device's reading, as fed to [`Sampler::sample`].
pub(crate) struct Reading {
    pub hashes: u64,
}

pub(crate) struct Rates {
    pub per_device: Vec<f64>,
    pub current: f64,
    pub avg_1m: f64,
    pub avg_15m: f64,
}

impl Sampler {
    pub(crate) fn sample(&mut self, now: Instant, readings: &[Reading]) -> Rates {
        let dt = self.last.map(|l| now.duration_since(l).as_secs_f64());
        self.last = Some(now);
        if self.last_hashes.len() != readings.len() {
            self.last_hashes = readings.iter().map(|r| r.hashes).collect();
            self.smoothed = vec![0.0; readings.len()];
        }
        let mut raw_total = 0.0;
        for (i, r) in readings.iter().enumerate() {
            let delta = r.hashes.saturating_sub(self.last_hashes[i]);
            self.last_hashes[i] = r.hashes;
            let Some(dt) = dt.filter(|&d| d > 0.0) else {
                continue;
            };
            let rate = delta as f64 / dt;
            raw_total += rate;
            self.smoothed[i] = if self.smoothed[i] == 0.0 {
                rate
            } else {
                self.smoothed[i] + SMOOTHING * (rate - self.smoothed[i])
            };
        }
        if dt.is_some() {
            while self.raw_totals.len() >= HISTORY_LEN {
                self.raw_totals.pop_front();
            }
            self.raw_totals.push_back(raw_total);
        }
        let mean = |n: usize| {
            let take = self.raw_totals.len().min(n);
            if take == 0 {
                0.0
            } else {
                self.raw_totals.iter().rev().take(take).sum::<f64>() / take as f64
            }
        };
        Rates {
            current: self.smoothed.iter().sum(),
            per_device: self.smoothed.clone(),
            avg_1m: mean(60),
            avg_15m: mean(HISTORY_LEN),
        }
    }
}

pub(crate) async fn sample_loop(ctx: Arc<RunCtx>) {
    let mut sampler = Sampler::default();
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if ctx.stopping() {
            return;
        }
        let devices = ctx.device_stats();
        let readings: Vec<Reading> = devices
            .iter()
            .map(|s| Reading {
                hashes: s.hashes.load(Ordering::Relaxed),
            })
            .collect();
        let rates = sampler.sample(Instant::now(), &readings);
        ctx.update(|st| {
            let snap = &mut st.snap;
            let mut total = 0u64;
            for (i, (stats, d)) in devices.iter().zip(snap.devices.iter_mut()).enumerate() {
                d.hashrate = rates.per_device.get(i).copied().unwrap_or(0.0);
                d.total_hashes = stats.hashes.load(Ordering::Relaxed);
                d.found = stats.found.load(Ordering::Relaxed);
                d.errors = stats.errors.load(Ordering::Relaxed);
                d.temperature_c = stats.temperature_c();
                d.power_w = match stats.power_mw.load(Ordering::Relaxed) {
                    0 => None,
                    mw => Some(mw as f32 / 1000.0),
                };
                d.status = stats.status.lock().clone();
                total += d.total_hashes;
            }
            let h = &mut snap.hashrate;
            h.current = rates.current;
            h.avg_1m = rates.avg_1m;
            h.avg_15m = rates.avg_15m;
            h.total_hashes = total;
            while h.history.len() >= HISTORY_LEN {
                h.history.pop_front();
            }
            h.history.push_back(rates.current);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_from_deltas() {
        let mut s = Sampler::default();
        let t0 = Instant::now();
        let r = s.sample(t0, &[Reading { hashes: 0 }, Reading { hashes: 0 }]);
        assert_eq!(r.current, 0.0);
        let r = s.sample(
            t0 + Duration::from_secs(1),
            &[Reading { hashes: 1000 }, Reading { hashes: 500 }],
        );
        assert_eq!(r.per_device, vec![1000.0, 500.0]);
        assert_eq!(r.current, 1500.0);
        assert_eq!(r.avg_1m, 1500.0);
        // A burst is smoothed per device but counted fully in the averages.
        let r = s.sample(
            t0 + Duration::from_secs(2),
            &[Reading { hashes: 4000 }, Reading { hashes: 1000 }],
        );
        assert!((r.per_device[0] - 1600.0).abs() < 1e-9);
        assert_eq!(r.avg_1m, (1500.0 + 3500.0) / 2.0);
    }
}
