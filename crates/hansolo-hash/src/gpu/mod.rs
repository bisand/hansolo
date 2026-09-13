//! GPU hashing through wgpu compute shaders (Metal, Vulkan, DX12).
//!
//! A GPU is thousands of slow cores, so it gets a different shape of loop from
//! the CPU: one dispatch hashes millions of nonces, each invocation computing
//! the outer `state[7]` for `base + index` (see [`shader`]). Candidates, the
//! nonces whose top word is at or below the target's, go into a small results
//! buffer through an atomic counter; the host copies back only that buffer and
//! re-checks every candidate with the portable reference before reporting it,
//! so a shader or driver bug can cost hashrate but never produce a bad share.
//!
//! Batch size adapts so a dispatch takes a time set by `GpuConfig::intensity`
//! (about 8 ms at 1 to 46 ms at 10), which keeps the stop flag responsive and
//! avoids OS GPU watchdogs. Lower intensities also leave the GPU idle between
//! dispatches so the desktop stays smooth.
//!
//! When a target is easy enough that a batch could overflow the results buffer,
//! the batch is capped; if it overflows anyway, the host rescans that range on
//! the CPU, so no share is lost.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use hansolo_core::snapshot::GpuReport;
use hansolo_core::{Device, DeviceCtx, DeviceInfo, DeviceKind, Work};

use crate::cpu::{Backend, Job};
use crate::verify_candidate;

mod shader;

use shader::params;

/// Candidate slots per dispatch.
const RESULT_CAPACITY: u32 = 1024;
const NONCE_SPACE: u64 = 1 << 32;
const MIN_BATCH: u32 = 1 << 12;
/// Keeps a dispatch well inside `u32` indices and watchdog limits even if timing misleads us.
const MAX_BATCH: u32 = 1 << 31;

/// Adapters found on this machine, with the reports describing them.
pub struct GpuProbe {
    pub reports: Vec<GpuReport>,
    /// Usable adapters and the index of their report.
    pub adapters: Vec<(wgpu::Adapter, usize)>,
    // Adapters refer back into the instance; keep it alive alongside them.
    _instance: wgpu::Instance,
}

fn api_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Dx12 => "DX12",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        _ => "Other",
    }
}

fn device_type_name(t: wgpu::DeviceType) -> &'static str {
    match t {
        wgpu::DeviceType::IntegratedGpu => "integrated",
        wgpu::DeviceType::DiscreteGpu => "discrete",
        wgpu::DeviceType::VirtualGpu => "virtual",
        wgpu::DeviceType::Cpu => "software",
        wgpu::DeviceType::Other => "other",
    }
}

/// Enumerates GPUs. Never panics for lack of drivers: no adapters just means an
/// empty report.
pub fn probe() -> GpuProbe {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let mut found = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    // Prefer the native API when one GPU shows up under several (DX12 and
    // Vulkan on Windows, for instance).
    let rank = |b: wgpu::Backend| match b {
        wgpu::Backend::Metal => 0,
        wgpu::Backend::Dx12 => 1,
        wgpu::Backend::Vulkan => 2,
        _ => 3,
    };
    found.sort_by_key(|a| rank(a.get_info().backend));

    let mut reports: Vec<GpuReport> = Vec::new();
    let mut adapters = Vec::new();
    let mut seen: Vec<(u32, u32, String, String)> = Vec::new();
    for adapter in found {
        let info = adapter.get_info();
        let api = api_name(info.backend);
        let key = (info.vendor, info.device, info.name.clone());
        let duplicate = seen
            .iter()
            .find(|(v, d, n, _)| (*v, *d, n.clone()) == key)
            .map(|s| s.3.clone());
        let (usable, note) = if let Some(first_api) = duplicate {
            (
                false,
                format!("same GPU as the {first_api} adapter, which is used instead"),
            )
        } else if info.device_type == wgpu::DeviceType::Cpu {
            (
                false,
                "software renderer; the CPU backends are faster".to_string(),
            )
        } else if !adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        {
            (false, "no compute shader support".to_string())
        } else {
            let driver = [info.driver.as_str(), info.driver_info.as_str()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            if driver.is_empty() {
                (true, "compute shaders supported".to_string())
            } else {
                (true, format!("compute shaders supported; driver {driver}"))
            }
        };
        seen.push((info.vendor, info.device, info.name.clone(), api.to_string()));
        if usable {
            adapters.push((adapter, reports.len()));
        }
        reports.push(GpuReport {
            name: info.name.clone(),
            api: api.to_string(),
            device_type: device_type_name(info.device_type).to_string(),
            usable,
            note,
        });
    }
    GpuProbe {
        reports,
        adapters,
        _instance: instance,
    }
}

/// A compiled pipeline and its buffers on one adapter.
pub struct GpuMiner {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    results: wgpu::Buffer,
    staging: wgpu::Buffer,
    workgroup_size: u32,
    max_groups: u32,
    pub name: String,
    pub api: String,
}

impl GpuMiner {
    pub fn new(adapter: &wgpu::Adapter) -> Result<GpuMiner, String> {
        let info = adapter.get_info();
        let limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("hansolo"),
            required_limits: limits.clone(),
            ..Default::default()
        }))
        .map_err(|e| format!("request device: {e}"))?;
        // wgpu's default handler panics, and release builds abort on panic.
        device.on_uncaptured_error(std::sync::Arc::new(|e| {
            eprintln!("hansolo-hash: wgpu error: {e}");
        }));

        let workgroup_size = limits
            .max_compute_invocations_per_workgroup
            .min(limits.max_compute_workgroup_size_x)
            .clamp(1, 256);
        let max_groups = limits.max_compute_workgroups_per_dimension.max(1);

        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sha256d"),
            source: wgpu::ShaderSource::Wgsl(shader::source(workgroup_size).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("sha256d"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(format!("shader: {err}"));
        }

        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let results_size = 4 * (1 + RESULT_CAPACITY as u64);
        let params = buffer(
            "params",
            4 * params::LEN as u64,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let results = buffer(
            "results",
            results_size,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        );
        let staging = buffer(
            "staging",
            results_size,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sha256d"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: results.as_entire_binding(),
                },
            ],
        });

        Ok(GpuMiner {
            device,
            queue,
            pipeline,
            bind_group,
            params,
            results,
            staging,
            workgroup_size,
            max_groups,
            name: info.name.clone(),
            api: api_name(info.backend).to_string(),
        })
    }

    /// Hashes `count` nonces from `start` on the GPU and appends the candidates
    /// to `out`. Returns `false` if the results buffer overflowed, in which case
    /// `out` is incomplete and the caller must rescan the range.
    pub fn dispatch(
        &self,
        job: &Job,
        start: u32,
        count: u32,
        out: &mut Vec<u32>,
    ) -> Result<bool, String> {
        if count == 0 {
            return Ok(true);
        }
        let groups = count.div_ceil(self.workgroup_size);
        let groups_x = groups.min(self.max_groups);
        let groups_y = groups.div_ceil(groups_x);
        if groups_y > self.max_groups {
            return Err("batch too large for this adapter".into());
        }
        let row = groups_x * self.workgroup_size;

        let mut p = [0u32; params::LEN];
        p[params::MIDSTATE..params::MIDSTATE + 8].copy_from_slice(&job.midstate);
        p[params::STATE4..params::STATE4 + 8].copy_from_slice(&job.state4);
        p[params::W16] = job.w16;
        p[params::W17] = job.w17;
        p[params::W18_BASE] = job.w18_base;
        p[params::W19_BASE] = job.w19_base;
        p[params::BASE_NONCE] = start;
        p[params::COUNT] = count;
        p[params::ROW] = row;
        p[params::TARGET_TOP] = job.target_top;
        p[params::CAPACITY] = RESULT_CAPACITY;
        self.queue
            .write_buffer(&self.params, 0, bytemuck::cast_slice(&p));
        self.queue
            .write_buffer(&self.results, 0, &0u32.to_le_bytes());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sha256d"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("sha256d"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.dispatch_workgroups(groups_x, groups_y, 1);
        }
        encoder.copy_buffer_to_buffer(&self.results, 0, &self.staging, 0, self.results.size());
        let submission = self.queue.submit([encoder.finish()]);

        let (tx, rx) = mpsc::channel();
        self.staging.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(10)),
            })
            .map_err(|e| format!("GPU poll: {e}"))?;
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("map results: {e}")),
            Err(_) => return Err("GPU did not return results".into()),
        }
        let complete = {
            let view = self
                .staging
                .get_mapped_range(..)
                .map_err(|e| format!("read results: {e}"))?;
            let words: &[u32] = bytemuck::cast_slice(&view);
            let found = words[0];
            let stored = found.min(RESULT_CAPACITY) as usize;
            out.extend_from_slice(&words[1..1 + stored]);
            found <= RESULT_CAPACITY
        };
        self.staging.unmap();
        Ok(complete)
    }

    /// Hashes per second at full throughput over roughly `duration`, after one
    /// warm-up dispatch (pipeline compilation is often lazy).
    pub fn benchmark(&self, duration: Duration) -> Result<f64, String> {
        let job = Job::new(&crate::cpu::bench_header(), &hansolo_core::Target([0; 32]));
        let mut out = Vec::new();
        let mut batch = 1u32 << 18;
        self.dispatch(&job, 0, batch, &mut out)?;
        let target = Duration::from_millis(30);
        let started = Instant::now();
        let mut hashes = 0u64;
        let mut nonce = 0u32;
        while started.elapsed() < duration {
            let t = Instant::now();
            self.dispatch(&job, nonce, batch, &mut out)?;
            out.clear();
            hashes += batch as u64;
            nonce = nonce.wrapping_add(batch);
            batch = next_batch(batch, t.elapsed(), target, u32::MAX);
        }
        Ok(hashes as f64 / started.elapsed().as_secs_f64())
    }
}

/// The batch that should take `target`, given that `batch` took `took`.
fn next_batch(batch: u32, took: Duration, target: Duration, cap: u32) -> u32 {
    let ideal = batch as f64 * target.as_secs_f64() / took.as_secs_f64().max(1e-4);
    // Grow gently (at most 2× per step); shrink immediately.
    let next = ideal.min(batch as f64 * 2.0) as u64;
    // The candidate cap can be below MIN_BATCH for very easy targets; callers
    // apply it to the count they dispatch.
    let upper = MAX_BATCH.min(cap).max(MIN_BATCH);
    next.clamp(MIN_BATCH as u64, upper as u64) as u32
}

/// Largest batch whose expected candidate count stays well inside the results buffer.
fn candidate_cap(target_top: u32) -> u32 {
    // P(candidate) = (target_top + 1) / 2^32; keep expectations at capacity / 4.
    let per = (target_top as f64 + 1.0) / NONCE_SPACE as f64;
    let cap = (RESULT_CAPACITY as f64 / 4.0) / per;
    cap.min(MAX_BATCH as f64) as u32
}

/// Dispatch duration for an intensity of 1..=10.
fn dispatch_target(intensity: u8) -> Duration {
    let i = intensity.clamp(1, 10) as u64;
    Duration::from_millis(8 + (i - 1) * 38 / 9)
}

/// A GPU as a [`Device`].
pub struct GpuDevice {
    miner: GpuMiner,
    intensity: u8,
    /// Rescans ranges whose candidates overflowed the results buffer.
    fallback: Backend,
}

impl GpuDevice {
    pub fn new(miner: GpuMiner, intensity: u8, fallback: Backend) -> GpuDevice {
        GpuDevice {
            miner,
            intensity: intensity.clamp(1, 10),
            fallback,
        }
    }
}

impl Device for GpuDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            name: self.miner.name.clone(),
            kind: DeviceKind::Gpu,
            backend: format!("wgpu/{}", self.miner.api),
            detail: format!(
                "WGSL SHA-256d compute shader, 1 queue, intensity {}, {} invocations per workgroup",
                self.intensity, self.miner.workgroup_size
            ),
        }
    }

    fn run(self: Box<Self>, ctx: DeviceCtx) {
        let lane = ctx.lane(0);
        let target = dispatch_target(self.intensity);
        // Idle fraction between dispatches: none at 10, about half the busy time at 1.
        let idle_ratio = (10 - self.intensity) as f64 / 18.0;
        let mut roll: u32 = 0;
        let mut batch = 1u32 << 18;
        let mut candidates = Vec::new();
        let mut waiting = false;

        'outer: while !ctx.stopping() {
            let generation = ctx.work.generation();
            let Some(work) = ctx.work.get() else {
                if !waiting {
                    ctx.stats.set_status("waiting for work");
                    waiting = true;
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            waiting = false;
            ctx.stats.set_status("hashing");
            let cap = candidate_cap(work.share_target.top_word());

            loop {
                let extranonce = Work::extranonce(lane, roll);
                roll = roll.wrapping_add(1);
                let header = work.header(&extranonce);
                let job = Job::new(&header, &work.share_target);
                let mut next: u64 = 0;
                while next < NONCE_SPACE {
                    let count = (batch.min(cap) as u64).min(NONCE_SPACE - next) as u32;
                    let started = Instant::now();
                    match self
                        .miner
                        .dispatch(&job, next as u32, count, &mut candidates)
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            candidates.clear();
                            self.fallback
                                .search(&job, next as u32, count, &mut candidates);
                        }
                        Err(e) => {
                            ctx.stats
                                .errors
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            ctx.stats.set_status(format!("error: {e}"));
                            candidates.clear();
                            // Back off; a lost device rarely comes back, but don't spin.
                            std::thread::sleep(Duration::from_secs(1));
                            continue 'outer;
                        }
                    }
                    let took = started.elapsed();
                    ctx.stats.add_hashes(count as u64);
                    for &nonce in &candidates {
                        verify_candidate(&ctx, &work, extranonce, &header, &job.midstate, nonce);
                    }
                    candidates.clear();
                    next += count as u64;
                    batch = next_batch(count.max(MIN_BATCH), took, target, cap);

                    if idle_ratio > 0.0 {
                        std::thread::sleep(took.mul_f64(idle_ratio).min(Duration::from_millis(80)));
                    }
                    if ctx.stopping() || ctx.work.generation() != generation {
                        continue 'outer;
                    }
                }
            }
        }
        ctx.stats.set_status("stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::testutil::{Rng, genesis, reference_candidates};

    fn miner() -> Option<GpuMiner> {
        let probe = probe();
        let Some((adapter, _)) = probe.adapters.first() else {
            eprintln!("skipping: no usable GPU adapter");
            return None;
        };
        match GpuMiner::new(adapter) {
            Ok(m) => Some(m),
            Err(e) => panic!("GPU adapter present but pipeline failed: {e}"),
        }
    }

    #[test]
    fn shader_matches_reference() {
        let Some(miner) = miner() else { return };
        let mut rng = Rng(0xDEAD_BEEF_1234_5678);
        let mut out = Vec::new();
        for round in 0..24 {
            let header = rng.header();
            let target_top = match round % 3 {
                0 => 0x03FF_FFFF, // ~1/64: about 64 candidates per 4096
                1 => 0x00FF_FFFF,
                _ => rng.next_u32() >> 8,
            };
            let mut job = Job::new(&header, &hansolo_core::Target([0; 32]));
            job.target_top = target_top;
            let start = if round % 4 == 0 {
                u32::MAX - 1000
            } else {
                rng.next_u32()
            };
            let count = 1000 + rng.next_u32() % 8000;
            out.clear();
            assert!(miner.dispatch(&job, start, count, &mut out).unwrap());
            out.sort_unstable_by_key(|&n| n.wrapping_sub(start));
            assert_eq!(
                out,
                reference_candidates(&header, start, count, target_top),
                "round {round}"
            );
        }

        // Genesis over a range big enough to need several workgroups.
        let job = Job::new(&genesis(), &hansolo_core::Target([0; 32]));
        out.clear();
        assert!(
            miner
                .dispatch(&job, 0x7c2b_ac1d - 300_000, 1_000_000, &mut out)
                .unwrap()
        );
        assert_eq!(out, vec![0x7c2b_ac1d]);
    }

    #[test]
    fn overflow_is_reported() {
        let Some(miner) = miner() else { return };
        let mut job = Job::new(&genesis(), &hansolo_core::Target([0; 32]));
        job.target_top = u32::MAX;
        let mut out = Vec::new();
        assert!(
            !miner
                .dispatch(&job, 0, RESULT_CAPACITY * 2, &mut out)
                .unwrap()
        );
        assert_eq!(out.len(), RESULT_CAPACITY as usize);
    }

    #[test]
    fn batch_limits() {
        assert!(candidate_cap(u32::MAX) <= RESULT_CAPACITY / 4);
        assert_eq!(candidate_cap(0), MAX_BATCH);
        assert_eq!(dispatch_target(1), Duration::from_millis(8));
        assert_eq!(dispatch_target(10), Duration::from_millis(46));
        assert_eq!(
            next_batch(
                1 << 20,
                Duration::from_millis(1),
                Duration::from_millis(30),
                u32::MAX
            ),
            1 << 21
        );
    }
}
