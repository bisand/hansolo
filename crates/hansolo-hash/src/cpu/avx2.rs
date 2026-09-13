//! AVX2 backend: eight nonces at a time in 256-bit vectors.
//!
//! The round function is the shared [`sha_top!`](super::sha_top) instantiated
//! for `__m256i`; there are no rotate instructions before AVX-512, so rotations
//! are shift pairs. Worth having on x86 CPUs with AVX2 but no SHA-NI (Haswell to
//! Comet Lake), where it beats the scalar path severalfold.

use core::arch::x86_64::*;

use super::{Job, sha_top};

#[inline]
#[target_feature(enable = "avx2")]
fn splat(x: u32) -> __m256i {
    _mm256_set1_epi32(x as i32)
}
#[inline]
#[target_feature(enable = "avx2")]
fn add(a: __m256i, b: __m256i) -> __m256i {
    _mm256_add_epi32(a, b)
}
#[inline]
#[target_feature(enable = "avx2")]
fn xor(a: __m256i, b: __m256i) -> __m256i {
    _mm256_xor_si256(a, b)
}
#[inline]
#[target_feature(enable = "avx2")]
fn and(a: __m256i, b: __m256i) -> __m256i {
    _mm256_and_si256(a, b)
}
#[inline]
#[target_feature(enable = "avx2")]
fn or(a: __m256i, b: __m256i) -> __m256i {
    _mm256_or_si256(a, b)
}
/// Rotate right by `R`; `L` must be `32 - R`.
#[inline]
#[target_feature(enable = "avx2")]
fn rotr<const R: i32, const L: i32>(x: __m256i) -> __m256i {
    _mm256_or_si256(_mm256_srli_epi32::<R>(x), _mm256_slli_epi32::<L>(x))
}
#[inline]
#[target_feature(enable = "avx2")]
fn sig0(x: __m256i) -> __m256i {
    xor(
        xor(rotr::<7, 25>(x), rotr::<18, 14>(x)),
        _mm256_srli_epi32::<3>(x),
    )
}
#[inline]
#[target_feature(enable = "avx2")]
fn sig1(x: __m256i) -> __m256i {
    xor(
        xor(rotr::<17, 15>(x), rotr::<19, 13>(x)),
        _mm256_srli_epi32::<10>(x),
    )
}
#[inline]
#[target_feature(enable = "avx2")]
fn big_sig0(a: __m256i) -> __m256i {
    xor(xor(rotr::<2, 30>(a), rotr::<13, 19>(a)), rotr::<22, 10>(a))
}
#[inline]
#[target_feature(enable = "avx2")]
fn big_sig1(e: __m256i) -> __m256i {
    xor(xor(rotr::<6, 26>(e), rotr::<11, 21>(e)), rotr::<25, 7>(e))
}
#[inline]
#[target_feature(enable = "avx2")]
fn ch(e: __m256i, f: __m256i, g: __m256i) -> __m256i {
    xor(g, and(e, xor(f, g)))
}
#[inline]
#[target_feature(enable = "avx2")]
fn maj(a: __m256i, b: __m256i, c: __m256i) -> __m256i {
    or(and(a, b), and(c, or(a, b)))
}

#[target_feature(enable = "avx2")]
fn search_impl(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
    let c = job.consts(|x| splat(x));
    let lanes = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);
    // Byte-swaps each 32-bit lane, so the result compares as the hash's top word.
    let bswap = _mm256_setr_epi8(
        3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12, 3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8,
        15, 14, 13, 12,
    );
    let target = splat(job.target_top);
    let zero = _mm256_setzero_si256();
    let full = count - count % 8;
    let mut i = 0;
    while i < full {
        let base = start.wrapping_add(i);
        // Header nonces are little-endian; the message word is the byte swap.
        let n = _mm256_shuffle_epi8(add(splat(base), lanes), bswap);
        let h7 = sha_top!(&c, n);
        let hit = if job.target_top == 0 {
            _mm256_cmpeq_epi32(h7, zero)
        } else {
            // Unsigned `top <= target` as `max(top, target) == target`.
            let top = _mm256_shuffle_epi8(h7, bswap);
            _mm256_cmpeq_epi32(_mm256_max_epu32(top, target), target)
        };
        let mut mask = _mm256_movemask_ps(_mm256_castsi256_ps(hit)) as u32;
        while mask != 0 {
            out.push(base.wrapping_add(mask.trailing_zeros()));
            mask &= mask - 1;
        }
        i += 8;
    }
    if full < count {
        super::scalar::search(job, start.wrapping_add(full), count - full, out);
    }
}

/// Searches eight lanes at a time. Falls back to portable code without AVX2.
pub fn search(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 was detected at run time.
        unsafe { search_impl(job, start, count, out) }
    } else {
        super::scalar::search(job, start, count, out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_reference() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            eprintln!("skipping: no AVX2 on this CPU");
            return;
        }
        super::super::testutil::check_search(super::search);
    }
}
