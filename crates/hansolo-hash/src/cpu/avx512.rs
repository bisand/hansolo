//! AVX-512F backend: sixteen nonces at a time.
//!
//! Same shared round function as AVX2, but with native rotates
//! (`_mm512_ror_epi32`) and mask compares, and twice the lanes. Only AVX-512F is
//! required. On CPUs with both AVX-512 and SHA-NI (Ice Lake and later, Zen 4)
//! the benchmark decides which wins.

use core::arch::x86_64::*;

use super::{Job, sha_top};

#[inline]
#[target_feature(enable = "avx512f")]
fn splat(x: u32) -> __m512i {
    _mm512_set1_epi32(x as i32)
}
#[inline]
#[target_feature(enable = "avx512f")]
fn add(a: __m512i, b: __m512i) -> __m512i {
    _mm512_add_epi32(a, b)
}
#[inline]
#[target_feature(enable = "avx512f")]
fn xor(a: __m512i, b: __m512i) -> __m512i {
    _mm512_xor_si512(a, b)
}
#[inline]
#[target_feature(enable = "avx512f")]
fn sig0(x: __m512i) -> __m512i {
    xor(
        xor(_mm512_ror_epi32::<7>(x), _mm512_ror_epi32::<18>(x)),
        _mm512_srli_epi32::<3>(x),
    )
}
#[inline]
#[target_feature(enable = "avx512f")]
fn sig1(x: __m512i) -> __m512i {
    xor(
        xor(_mm512_ror_epi32::<17>(x), _mm512_ror_epi32::<19>(x)),
        _mm512_srli_epi32::<10>(x),
    )
}
#[inline]
#[target_feature(enable = "avx512f")]
fn big_sig0(a: __m512i) -> __m512i {
    xor(
        xor(_mm512_ror_epi32::<2>(a), _mm512_ror_epi32::<13>(a)),
        _mm512_ror_epi32::<22>(a),
    )
}
#[inline]
#[target_feature(enable = "avx512f")]
fn big_sig1(e: __m512i) -> __m512i {
    xor(
        xor(_mm512_ror_epi32::<6>(e), _mm512_ror_epi32::<11>(e)),
        _mm512_ror_epi32::<25>(e),
    )
}
#[inline]
#[target_feature(enable = "avx512f")]
fn ch(e: __m512i, f: __m512i, g: __m512i) -> __m512i {
    _mm512_ternarylogic_epi32::<0xCA>(e, f, g)
}
#[inline]
#[target_feature(enable = "avx512f")]
fn maj(a: __m512i, b: __m512i, c: __m512i) -> __m512i {
    _mm512_ternarylogic_epi32::<0xE8>(a, b, c)
}

/// Byte-swaps each lane without AVX-512BW's shuffle: after a left rotate by 8,
/// bytes 0 and 2 are in their swapped places; after one by 24, bytes 1 and 3.
#[inline]
#[target_feature(enable = "avx512f")]
fn bswap(x: __m512i) -> __m512i {
    let low = splat(0x00FF_00FF);
    _mm512_or_si512(
        _mm512_and_si512(_mm512_rol_epi32::<8>(x), low),
        _mm512_andnot_si512(low, _mm512_rol_epi32::<24>(x)),
    )
}

#[target_feature(enable = "avx512f")]
fn search_impl(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
    let c = job.consts(|x| splat(x));
    let lanes = _mm512_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
    let target = splat(job.target_top);
    let full = count - count % 16;
    let mut i = 0;
    while i < full {
        let base = start.wrapping_add(i);
        // Header nonces are little-endian; the message word is the byte swap.
        let n = bswap(add(splat(base), lanes));
        let h7 = sha_top!(&c, n);
        let mut mask = (if job.target_top == 0 {
            _mm512_cmpeq_epi32_mask(h7, _mm512_setzero_si512())
        } else {
            _mm512_cmple_epu32_mask(bswap(h7), target)
        }) as u32;
        while mask != 0 {
            out.push(base.wrapping_add(mask.trailing_zeros()));
            mask &= mask - 1;
        }
        i += 16;
    }
    if full < count {
        super::scalar::search(job, start.wrapping_add(full), count - full, out);
    }
}

/// Searches sixteen lanes at a time. Falls back to portable code without AVX-512F.
pub fn search(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
    if std::arch::is_x86_feature_detected!("avx512f") {
        // SAFETY: AVX-512F was detected at run time.
        unsafe { search_impl(job, start, count, out) }
    } else {
        super::scalar::search(job, start, count, out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_reference() {
        if !std::arch::is_x86_feature_detected!("avx512f") {
            eprintln!("skipping: no AVX-512F on this CPU");
            return;
        }
        super::super::testutil::check_search(super::search);
    }
}
