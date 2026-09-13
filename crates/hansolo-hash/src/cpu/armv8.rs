//! ARMv8 Cryptographic Extensions backend (`sha2`): Apple Silicon, Graviton,
//! Ampere, and Cortex-A cores on SoCs that license the extension (the Raspberry
//! Pi 5's BCM2712 does not, and falls back to the portable path).
//!
//! `vsha256hq_u32` runs four rounds per call and `vsha256su0q_u32` /
//! `vsha256su1q_u32` expand the message four words at a time, so a compression
//! is a chain of 16 short steps. There is no SIMD width to exploit across
//! nonces, but there is out-of-order width: [`LANES`] nonces are advanced in
//! lockstep, each message vector expanded right before the step that uses it,
//! so independent chains overlap in the reorder window instead of queuing
//! behind each other. On an Apple M5 Pro this is 33.6 MH/s per thread against
//! 29.6 for one nonce at a time with the message expanded up front; 3 or 4
//! lanes measured the same as 2. Precomputing the few `SU0` results with
//! constant inputs measured *slower* (the extra selection costs more than the
//! instruction it saves). `bench_lanes` (ignored) re-measures.
//!
//! The outer hash runs 15 hardware steps (60 rounds) plus one scalar round,
//! which is all `state[7]` needs.

use core::arch::aarch64::*;

use hansolo_core::sha::{IV, K};

use super::{Job, PAD};

/// Nonces advanced in lockstep.
const LANES: usize = 2;

/// Whether the CPU has the SHA-256 extension.
pub fn detected() -> bool {
    std::arch::is_aarch64_feature_detected!("sha2")
}

#[inline]
#[target_feature(enable = "neon")]
fn load(words: [u32; 4]) -> uint32x4_t {
    // SAFETY: `words` is four readable u32s.
    unsafe { vld1q_u32(words.as_ptr()) }
}

/// Per-job vectors.
struct Pre {
    k: [uint32x4_t; 16],
    mid_abcd: uint32x4_t,
    mid_efgh: uint32x4_t,
    iv_abcd: uint32x4_t,
    iv_efgh: uint32x4_t,
    tail: [u32; 3],
    /// Inner message vectors 1..4: [PAD, 0, 0, 0], 0, [0, 0, 0, 640].
    in_m: [uint32x4_t; 3],
    /// Outer message vectors 2..4: [PAD, 0, 0, 0], [0, 0, 0, 256].
    out_m: [uint32x4_t; 2],
}

#[target_feature(enable = "neon")]
fn prepare(job: &Job) -> Pre {
    let m = job.midstate;
    Pre {
        k: core::array::from_fn(|j| load([K[4 * j], K[4 * j + 1], K[4 * j + 2], K[4 * j + 3]])),
        mid_abcd: load([m[0], m[1], m[2], m[3]]),
        mid_efgh: load([m[4], m[5], m[6], m[7]]),
        iv_abcd: load([IV[0], IV[1], IV[2], IV[3]]),
        iv_efgh: load([IV[4], IV[5], IV[6], IV[7]]),
        tail: job.tail,
        in_m: [load([PAD, 0, 0, 0]), vdupq_n_u32(0), load([0, 0, 0, 640])],
        out_m: [load([PAD, 0, 0, 0]), load([0, 0, 0, 256])],
    }
}

/// Message vector `j` (>= 4) from the four before it:
/// `W[4j..] = SU1(SU0(W[4j-16..], W[4j-12..]), W[4j-8..], W[4j-4..])`.
#[inline]
#[target_feature(enable = "neon,sha2")]
fn expand(m: &[uint32x4_t; 16], j: usize) -> uint32x4_t {
    vsha256su1q_u32(vsha256su0q_u32(m[j - 4], m[j - 3]), m[j - 2], m[j - 1])
}

/// The hash's top word (byte-swapped outer `state[7]`) for nonces `base..base + N`.
#[inline]
#[target_feature(enable = "neon,sha2")]
fn lanes_top<const N: usize>(p: &Pre, base: u32) -> [u32; N] {
    let t = p.tail;

    // ---- inner compression ----
    let mut m = [[p.in_m[1]; 16]; N];
    for (l, ml) in m.iter_mut().enumerate() {
        // The header stores the nonce little-endian; SHA reads big-endian words.
        ml[0] = load([t[0], t[1], t[2], base.wrapping_add(l as u32).swap_bytes()]);
        ml[1..4].copy_from_slice(&p.in_m);
    }
    let mut abcd = [p.mid_abcd; N];
    let mut efgh = [p.mid_efgh; N];
    for j in 0..16 {
        for l in 0..N {
            if j >= 4 {
                m[l][j] = expand(&m[l], j);
            }
            let wk = vaddq_u32(m[l][j], p.k[j]);
            let prev = abcd[l];
            abcd[l] = vsha256hq_u32(abcd[l], efgh[l], wk);
            efgh[l] = vsha256h2q_u32(efgh[l], prev, wk);
        }
    }

    // ---- outer compression: 15 steps, then round 60 in scalar ----
    for l in 0..N {
        m[l][0] = vaddq_u32(abcd[l], p.mid_abcd);
        m[l][1] = vaddq_u32(efgh[l], p.mid_efgh);
        m[l][2..4].copy_from_slice(&p.out_m);
    }
    let mut abcd = [p.iv_abcd; N];
    let mut efgh = [p.iv_efgh; N];
    for j in 0..16 {
        for l in 0..N {
            if j >= 4 {
                m[l][j] = expand(&m[l], j);
            }
            if j < 15 {
                let wk = vaddq_u32(m[l][j], p.k[j]);
                let prev = abcd[l];
                abcd[l] = vsha256hq_u32(abcd[l], efgh[l], wk);
                efgh[l] = vsha256h2q_u32(efgh[l], prev, wk);
            }
        }
    }
    core::array::from_fn(|l| {
        let d = vgetq_lane_u32::<3>(abcd[l]);
        let e = vgetq_lane_u32::<0>(efgh[l]);
        let f = vgetq_lane_u32::<1>(efgh[l]);
        let g = vgetq_lane_u32::<2>(efgh[l]);
        let h = vgetq_lane_u32::<3>(efgh[l]);
        let w60 = vgetq_lane_u32::<0>(m[l][15]);
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add(g ^ (e & (f ^ g)))
            .wrapping_add(K[60])
            .wrapping_add(w60);
        // Round 60's new e is the final h.
        d.wrapping_add(t1).wrapping_add(IV[7]).swap_bytes()
    })
}

#[target_feature(enable = "neon,sha2")]
fn search_lanes<const N: usize>(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
    let p = prepare(job);
    let full = count - count % N as u32;
    let mut i = 0;
    while i < full {
        let base = start.wrapping_add(i);
        for (l, top) in lanes_top::<N>(&p, base).into_iter().enumerate() {
            if top <= job.target_top {
                out.push(base.wrapping_add(l as u32));
            }
        }
        i += N as u32;
    }
    for i in full..count {
        let nonce = start.wrapping_add(i);
        if lanes_top::<1>(&p, nonce)[0] <= job.target_top {
            out.push(nonce);
        }
    }
}

/// Searches with the SHA-256 extension. Falls back to portable code if the CPU lacks it.
pub fn search(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
    if detected() {
        // SAFETY: the `sha2` (and baseline `neon`) features were detected at run time.
        unsafe { search_lanes::<LANES>(job, start, count, out) }
    } else {
        super::scalar::search(job, start, count, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(n: usize, job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
        // SAFETY: callers check `detected()` first.
        unsafe {
            match n {
                1 => search_lanes::<1>(job, start, count, out),
                2 => search_lanes::<2>(job, start, count, out),
                3 => search_lanes::<3>(job, start, count, out),
                _ => search_lanes::<4>(job, start, count, out),
            }
        }
    }

    #[test]
    fn matches_reference() {
        if !detected() {
            eprintln!("skipping: no ARMv8 SHA2 extension");
            return;
        }
        super::super::testutil::check_search(search);
        for n in [1, 3, 4] {
            super::super::testutil::check_search(|j, s, c, o| run(n, j, s, c, o));
        }
    }

    /// `cargo test -p hansolo-hash --release --lib bench_lanes -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_lanes() {
        if !detected() {
            return;
        }
        let job = Job::new(
            &super::super::bench_header(),
            &hansolo_core::Target([0; 32]),
        );
        let mut out = Vec::new();
        let count = 1u32 << 24;
        for round in 0..3 {
            for n in [1, 2, 3, 4] {
                let t = std::time::Instant::now();
                run(n, &job, 0, count, &mut out);
                println!(
                    "round {round}, {n} lane(s): {:.1} MH/s",
                    count as f64 / t.elapsed().as_secs_f64() / 1e6
                );
            }
        }
    }
}
