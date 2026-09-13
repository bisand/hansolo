//! Intel SHA Extensions backend (`sha`): Goldmont and later Atoms, Ice Lake and
//! later cores, every AMD Zen.
//!
//! `_mm_sha256rnds2_epu32` runs two rounds per call on a state packed as
//! `[CDGH]`/`[ABEF]`; `_mm_sha256msg1_epu32` and `_mm_sha256msg2_epu32` expand
//! the message. Per nonce the inner hash skips its first two rounds (they only
//! see constant header words, so they are precomputed per job) and the outer
//! hash stops after round 60, finished by one scalar round.
//!
//! The dev machine this was written on has no SHA-NI, so the round logic is
//! written once as a macro over a handful of operations and instantiated twice:
//! with the real intrinsics, and (in tests, on every architecture) with portable
//! emulations of those intrinsics written from Intel's pseudocode. The
//! emulated instance is checked against the reference; the real one is checked
//! too wherever the tests run on a CPU that has the extension.

#[cfg(test)]
use super::Job;
use super::PAD;

/// `state[7]` of the outer hash (IV added, not byte-swapped) for `nonce`.
///
/// Needs in scope, for the 128-bit type `M`: `set(l0, l1, l2, l3)`, `add`,
/// `rnds2`, `msg1`, `msg2`, `shuf_0e`, `shuf_1b`, `shuf_b1`, `alignr4`,
/// `alignr8`, `blend_f0`, `lane(x, i)`, and a `Pre<M>`.
macro_rules! shani_h7 {
    ($p:expr, $nonce:expr) => {{
        let p = $p;
        let nonce: u32 = $nonce;

        // ---- inner: rounds 0-1 are precomputed in p.abef1/p.cdgh1 ----
        // The header stores the nonce little-endian; SHA reads big-endian words.
        let m0 = set(p.tail[0], p.tail[1], p.tail[2], nonce.swap_bytes());
        let wk = add(m0, p.k[0]);
        let mut cdgh = p.abef1;
        let mut abef = rnds2(p.cdgh1, p.abef1, shuf_0e(wk));
        let mut m = [m0; 16];
        m[1] = p.pad_first;
        m[2] = p.zero;
        m[3] = p.len_inner;
        for j in 1..16 {
            if j >= 4 {
                m[j] = msg2(
                    add(msg1(m[j - 4], m[j - 3]), alignr4(m[j - 1], m[j - 2])),
                    m[j - 1],
                );
            }
            let wk = add(m[j], p.k[j]);
            let next = rnds2(cdgh, abef, wk);
            cdgh = abef;
            abef = next;
            let next = rnds2(cdgh, abef, shuf_0e(wk));
            cdgh = abef;
            abef = next;
        }
        let abef = add(abef, p.mid_abef);
        let cdgh = add(cdgh, p.mid_cdgh);
        // Back to [a, b, c, d] / [e, f, g, h]: exactly the outer message's first two vectors.
        let feba = shuf_1b(abef);
        let dchg = shuf_b1(cdgh);
        let abcd = blend_f0(feba, dchg);
        let efgh = alignr8(dchg, feba);

        // ---- outer: rounds 0..60 in hardware ----
        let mut m = [abcd; 16];
        m[1] = efgh;
        m[2] = p.pad_first;
        m[3] = p.len_outer;
        let mut cdgh = p.iv_cdgh;
        let mut abef = p.iv_abef;
        for j in 0..15 {
            if j >= 4 {
                m[j] = msg2(
                    add(msg1(m[j - 4], m[j - 3]), alignr4(m[j - 1], m[j - 2])),
                    m[j - 1],
                );
            }
            let wk = add(m[j], p.k[j]);
            let next = rnds2(cdgh, abef, wk);
            cdgh = abef;
            abef = next;
            let next = rnds2(cdgh, abef, shuf_0e(wk));
            cdgh = abef;
            abef = next;
        }
        let m15 = msg2(add(msg1(m[11], m[12]), alignr4(m[14], m[13])), m[14]);

        // ---- round 60 in scalar: its new e is the final h ----
        // abef lanes are [f, e, b, a]; cdgh lanes are [h, g, d, c].
        let (f, e) = (lane(abef, 0), lane(abef, 1));
        let (h, g, d) = (lane(cdgh, 0), lane(cdgh, 1), lane(cdgh, 2));
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add(g ^ (e & (f ^ g)))
            .wrapping_add(K[60])
            .wrapping_add(lane(m15, 0));
        d.wrapping_add(t1).wrapping_add(IV[7])
    }};
}

/// Per-job vectors.
#[derive(Clone, Copy)]
struct Pre<M> {
    tail: [u32; 3],
    k: [M; 16],
    mid_abef: M,
    mid_cdgh: M,
    /// The inner state after rounds 0-1.
    abef1: M,
    cdgh1: M,
    iv_abef: M,
    iv_cdgh: M,
    pad_first: M,
    zero: M,
    len_inner: M,
    len_outer: M,
}

/// Builds [`Pre`] from generic operations; `$rnds2` etc. as in [`shani_h7!`].
macro_rules! shani_prepare {
    ($job:expr) => {{
        let job = $job;
        let s = job.midstate;
        let pack = |s: &[u32; 8]| {
            // [ABEF] = lanes [f, e, b, a]; [CDGH] = lanes [h, g, d, c].
            (set(s[5], s[4], s[1], s[0]), set(s[7], s[6], s[3], s[2]))
        };
        let (mid_abef, mid_cdgh) = pack(&s);
        let (iv_abef, iv_cdgh) = pack(&IV);
        let k: [_; 16] =
            core::array::from_fn(|j| set(K[4 * j], K[4 * j + 1], K[4 * j + 2], K[4 * j + 3]));
        let t = job.tail;
        let wk01 = set(t[0].wrapping_add(K[0]), t[1].wrapping_add(K[1]), 0, 0);
        let abef1 = rnds2(mid_cdgh, mid_abef, wk01);
        Pre {
            tail: t,
            k,
            mid_abef,
            mid_cdgh,
            abef1,
            cdgh1: mid_abef,
            iv_abef,
            iv_cdgh,
            pad_first: set(PAD, 0, 0, 0),
            zero: set(0, 0, 0, 0),
            len_inner: set(0, 0, 0, 640),
            len_outer: set(0, 0, 0, 256),
        }
    }};
}

#[cfg(target_arch = "x86_64")]
mod hw {
    use core::arch::x86_64::*;

    use hansolo_core::sha::{IV, K};

    use super::{PAD, Pre};
    use crate::cpu::Job;

    #[inline]
    #[target_feature(enable = "sse2")]
    fn set(l0: u32, l1: u32, l2: u32, l3: u32) -> __m128i {
        _mm_set_epi32(l3 as i32, l2 as i32, l1 as i32, l0 as i32)
    }
    #[inline]
    #[target_feature(enable = "sse2")]
    fn add(a: __m128i, b: __m128i) -> __m128i {
        _mm_add_epi32(a, b)
    }
    #[inline]
    #[target_feature(enable = "sha,sse2")]
    fn rnds2(cdgh: __m128i, abef: __m128i, wk: __m128i) -> __m128i {
        _mm_sha256rnds2_epu32(cdgh, abef, wk)
    }
    #[inline]
    #[target_feature(enable = "sha,sse2")]
    fn msg1(a: __m128i, b: __m128i) -> __m128i {
        _mm_sha256msg1_epu32(a, b)
    }
    #[inline]
    #[target_feature(enable = "sha,sse2")]
    fn msg2(a: __m128i, b: __m128i) -> __m128i {
        _mm_sha256msg2_epu32(a, b)
    }
    #[inline]
    #[target_feature(enable = "sse2")]
    fn shuf_0e(x: __m128i) -> __m128i {
        _mm_shuffle_epi32::<0x0E>(x)
    }
    #[inline]
    #[target_feature(enable = "sse2")]
    fn shuf_1b(x: __m128i) -> __m128i {
        _mm_shuffle_epi32::<0x1B>(x)
    }
    #[inline]
    #[target_feature(enable = "sse2")]
    fn shuf_b1(x: __m128i) -> __m128i {
        _mm_shuffle_epi32::<0xB1>(x)
    }
    #[inline]
    #[target_feature(enable = "ssse3")]
    fn alignr4(a: __m128i, b: __m128i) -> __m128i {
        _mm_alignr_epi8::<4>(a, b)
    }
    #[inline]
    #[target_feature(enable = "ssse3")]
    fn alignr8(a: __m128i, b: __m128i) -> __m128i {
        _mm_alignr_epi8::<8>(a, b)
    }
    #[inline]
    #[target_feature(enable = "sse4.1")]
    fn blend_f0(a: __m128i, b: __m128i) -> __m128i {
        _mm_blend_epi16::<0xF0>(a, b)
    }
    #[inline]
    #[target_feature(enable = "sse4.1")]
    fn lane(x: __m128i, i: i32) -> u32 {
        match i {
            0 => _mm_extract_epi32::<0>(x) as u32,
            1 => _mm_extract_epi32::<1>(x) as u32,
            2 => _mm_extract_epi32::<2>(x) as u32,
            _ => _mm_extract_epi32::<3>(x) as u32,
        }
    }

    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    fn search_impl(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
        let p: Pre<__m128i> = shani_prepare!(job);
        for i in 0..count {
            let nonce = start.wrapping_add(i);
            let h7: u32 = shani_h7!(&p, nonce);
            if h7.swap_bytes() <= job.target_top {
                out.push(nonce);
            }
        }
    }

    pub fn search(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
        if super::detected() {
            // SAFETY: sha, sse2, ssse3 and sse4.1 were all detected at run time.
            unsafe { search_impl(job, start, count, out) }
        } else {
            crate::cpu::scalar::search(job, start, count, out)
        }
    }
}

#[cfg(target_arch = "x86_64")]
pub use hw::search;

/// Whether the CPU has SHA-NI and the SSE levels this path also uses.
#[cfg(target_arch = "x86_64")]
pub fn detected() -> bool {
    std::arch::is_x86_feature_detected!("sha")
        && std::arch::is_x86_feature_detected!("sse4.1")
        && std::arch::is_x86_feature_detected!("ssse3")
}

/// Portable emulations of the intrinsics, from Intel's documented pseudocode.
/// Lanes are little-endian: `x[0]` is bits 31:0.
#[cfg(test)]
mod emu {
    use hansolo_core::sha::{IV, K};

    use super::{Job, PAD, Pre};

    type M = [u32; 4];

    fn set(l0: u32, l1: u32, l2: u32, l3: u32) -> M {
        [l0, l1, l2, l3]
    }
    fn add(a: M, b: M) -> M {
        core::array::from_fn(|i| a[i].wrapping_add(b[i]))
    }
    fn rnds2(a: M, b: M, k: M) -> M {
        let (mut aa, mut bb, mut cc, mut dd) = (b[3], b[2], a[3], a[2]);
        let (mut ee, mut ff, mut gg, mut hh) = (b[1], b[0], a[1], a[0]);
        for &wk in &k[..2] {
            let t = ((ee & ff) ^ (!ee & gg))
                .wrapping_add(ee.rotate_right(6) ^ ee.rotate_right(11) ^ ee.rotate_right(25))
                .wrapping_add(wk)
                .wrapping_add(hh);
            let a_next = t
                .wrapping_add((aa & bb) ^ (aa & cc) ^ (bb & cc))
                .wrapping_add(aa.rotate_right(2) ^ aa.rotate_right(13) ^ aa.rotate_right(22));
            let e_next = t.wrapping_add(dd);
            (hh, gg, ff, ee) = (gg, ff, ee, e_next);
            (dd, cc, bb, aa) = (cc, bb, aa, a_next);
        }
        [ff, ee, bb, aa]
    }
    fn msg1(a: M, b: M) -> M {
        let w = [a[0], a[1], a[2], a[3], b[0]];
        core::array::from_fn(|i| w[i].wrapping_add(crate::cpu::sig0(w[i + 1])))
    }
    fn msg2(a: M, b: M) -> M {
        let w16 = a[0].wrapping_add(crate::cpu::sig1(b[2]));
        let w17 = a[1].wrapping_add(crate::cpu::sig1(b[3]));
        let w18 = a[2].wrapping_add(crate::cpu::sig1(w16));
        let w19 = a[3].wrapping_add(crate::cpu::sig1(w17));
        [w16, w17, w18, w19]
    }
    fn shuf(x: M, imm: u32) -> M {
        core::array::from_fn(|i| x[((imm >> (2 * i)) & 3) as usize])
    }
    fn shuf_0e(x: M) -> M {
        shuf(x, 0x0E)
    }
    fn shuf_1b(x: M) -> M {
        shuf(x, 0x1B)
    }
    fn shuf_b1(x: M) -> M {
        shuf(x, 0xB1)
    }
    /// `(a:b) >> (bytes * 8)`, low 128 bits.
    fn alignr(a: M, b: M, lanes: usize) -> M {
        let both = [b[0], b[1], b[2], b[3], a[0], a[1], a[2], a[3]];
        core::array::from_fn(|i| both[i + lanes])
    }
    fn alignr4(a: M, b: M) -> M {
        alignr(a, b, 1)
    }
    fn alignr8(a: M, b: M) -> M {
        alignr(a, b, 2)
    }
    fn blend_f0(a: M, b: M) -> M {
        [a[0], a[1], b[2], b[3]]
    }
    fn lane(x: M, i: usize) -> u32 {
        x[i]
    }

    pub fn search(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
        let p: Pre<M> = shani_prepare!(job);
        for i in 0..count {
            let nonce = start.wrapping_add(i);
            let h7: u32 = shani_h7!(&p, nonce);
            if h7.swap_bytes() <= job.target_top {
                out.push(nonce);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn emulated_intrinsics_match_reference() {
        super::super::testutil::check_search(super::emu::search);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn hardware_matches_reference() {
        if !super::detected() {
            eprintln!("skipping: no SHA-NI on this CPU");
            return;
        }
        super::super::testutil::check_search(super::search);
    }
}
