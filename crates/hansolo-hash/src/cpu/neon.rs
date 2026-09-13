//! NEON backend: four nonces at a time with plain 128-bit SIMD.
//!
//! For 64-bit ARM cores without the SHA-256 extension, which includes the
//! Raspberry Pi 4 and 5 (their Broadcom SoCs don't license ARM's crypto
//! extensions), and so a likely kiosk target. The round function is the shared
//! [`sha_top!`](super::sha_top) over `uint32x4_t`. On cores that do have
//! `sha2`, the benchmark picks the ARMv8 SHA2 backend instead.

use core::arch::aarch64::*;

use super::{Job, sha_top};

#[inline]
#[target_feature(enable = "neon")]
fn splat(x: u32) -> uint32x4_t {
    vdupq_n_u32(x)
}
#[inline]
#[target_feature(enable = "neon")]
fn add(a: uint32x4_t, b: uint32x4_t) -> uint32x4_t {
    vaddq_u32(a, b)
}
/// Rotate right by `R`; `L` must be `32 - R`.
#[inline]
#[target_feature(enable = "neon")]
fn rotr<const R: i32, const L: i32>(x: uint32x4_t) -> uint32x4_t {
    vorrq_u32(vshrq_n_u32::<R>(x), vshlq_n_u32::<L>(x))
}
#[inline]
#[target_feature(enable = "neon")]
fn sig0(x: uint32x4_t) -> uint32x4_t {
    veorq_u32(
        veorq_u32(rotr::<7, 25>(x), rotr::<18, 14>(x)),
        vshrq_n_u32::<3>(x),
    )
}
#[inline]
#[target_feature(enable = "neon")]
fn sig1(x: uint32x4_t) -> uint32x4_t {
    veorq_u32(
        veorq_u32(rotr::<17, 15>(x), rotr::<19, 13>(x)),
        vshrq_n_u32::<10>(x),
    )
}
#[inline]
#[target_feature(enable = "neon")]
fn big_sig0(a: uint32x4_t) -> uint32x4_t {
    veorq_u32(
        veorq_u32(rotr::<2, 30>(a), rotr::<13, 19>(a)),
        rotr::<22, 10>(a),
    )
}
#[inline]
#[target_feature(enable = "neon")]
fn big_sig1(e: uint32x4_t) -> uint32x4_t {
    veorq_u32(
        veorq_u32(rotr::<6, 26>(e), rotr::<11, 21>(e)),
        rotr::<25, 7>(e),
    )
}
#[inline]
#[target_feature(enable = "neon")]
fn ch(e: uint32x4_t, f: uint32x4_t, g: uint32x4_t) -> uint32x4_t {
    // Bitwise select: e ? f : g.
    vbslq_u32(e, f, g)
}
#[inline]
#[target_feature(enable = "neon")]
fn maj(a: uint32x4_t, b: uint32x4_t, c: uint32x4_t) -> uint32x4_t {
    // Where a and b differ, c decides; where they agree, either does.
    vbslq_u32(veorq_u32(a, b), c, a)
}
#[inline]
#[target_feature(enable = "neon")]
fn bswap(x: uint32x4_t) -> uint32x4_t {
    vreinterpretq_u32_u8(vrev32q_u8(vreinterpretq_u8_u32(x)))
}

#[target_feature(enable = "neon")]
fn search_impl(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
    let c = job.consts(|x| splat(x));
    // SAFETY: four readable u32s.
    let lanes = unsafe { vld1q_u32([0u32, 1, 2, 3].as_ptr()) };
    let target = splat(job.target_top);
    let full = count - count % 4;
    let mut i = 0;
    while i < full {
        let base = start.wrapping_add(i);
        // Header nonces are little-endian; the message word is the byte swap.
        let n = bswap(add(splat(base), lanes));
        let h7 = sha_top!(&c, n);
        let hit = vcleq_u32(bswap(h7), target);
        // Cheap common case: no lane passes.
        if vmaxvq_u32(hit) != 0 {
            let mut mask = [0u32; 4];
            // SAFETY: `mask` has room for four u32s.
            unsafe { vst1q_u32(mask.as_mut_ptr(), hit) };
            for (l, m) in mask.into_iter().enumerate() {
                if m != 0 {
                    out.push(base.wrapping_add(l as u32));
                }
            }
        }
        i += 4;
    }
    if full < count {
        super::scalar::search(job, start.wrapping_add(full), count - full, out);
    }
}

/// Searches four lanes at a time. NEON is baseline on aarch64.
pub fn search(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
    if std::arch::is_aarch64_feature_detected!("neon") {
        // SAFETY: NEON was detected (it is part of the aarch64 baseline anyway).
        unsafe { search_impl(job, start, count, out) }
    } else {
        super::scalar::search(job, start, count, out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_reference() {
        super::super::testutil::check_search(super::search);
    }
}
