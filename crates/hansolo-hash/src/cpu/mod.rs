//! CPU hashing backends.
//!
//! Every backend answers one question for a range of nonces: which of them
//! *might* meet the share target? They do it by computing only the most
//! significant 32 bits of each SHA-256d result (the outer hash's last state
//! word, byte-swapped) and comparing that with [`Target::top_word`]. Anything
//! at or below the top word is a *candidate*; the caller recomputes candidates
//! with the portable reference and applies the full 256-bit comparison. Two
//! consequences:
//!
//! - The outer compression can stop after round 60: `state[7]` is fixed by then
//!   (h after round 63 is e after round 60), saving three rounds and the
//!   message words they use.
//! - Share targets easier than 32 leading zero bits (pool difficulty below 1,
//!   node-mode local share targets) still work, they just produce more
//!   candidates; nothing assumes the top word is zero.
//!
//! Per header, everything that does not depend on the nonce is computed once in
//! [`Job`]: the midstate, the first three inner rounds, the constant message
//! words, and so on. The portable and SIMD backends share one generic round
//! function ([`sha_top!`]) written against a handful of word operations, so the
//! scalar tests also exercise the logic the AVX2 and AVX-512 paths run.

use hansolo_core::Target;
use hansolo_core::sha::{IV, K, midstate};

pub mod device;
pub mod scalar;

#[cfg(target_arch = "aarch64")]
pub mod armv8;
#[cfg(target_arch = "aarch64")]
pub mod neon;

#[cfg(target_arch = "x86_64")]
pub mod avx2;
#[cfg(target_arch = "x86_64")]
pub mod avx512;
#[cfg(any(target_arch = "x86_64", test))]
pub mod shani;

use std::time::{Duration, Instant};

/// The padding word that follows the message.
pub(crate) const PAD: u32 = 0x8000_0000;

/// A header prepared for nonce search: everything that doesn't change with the nonce.
#[derive(Clone, Copy, Debug)]
pub struct Job {
    pub midstate: [u32; 8],
    /// Big-endian words of header bytes 64..76: merkle root tail, time, bits.
    pub tail: [u32; 3],
    /// Candidates are nonces whose hash's top 32 bits are `<=` this.
    pub target_top: u32,
    /// Inner state after round 3, minus the nonce word `n` (the nonce
    /// byte-swapped, since the header stores it little-endian):
    /// `a4 = n + state4[0]`, `e4 = n + state4[4]`; the other six don't depend on it.
    pub(crate) state4: [u32; 8],
    pub(crate) w16: u32,
    pub(crate) w17: u32,
    /// `w18 = w18_base + σ0(n)`.
    pub(crate) w18_base: u32,
    /// `w19 = n + w19_base`.
    pub(crate) w19_base: u32,
}

#[inline(always)]
pub(crate) const fn sig0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}
#[inline(always)]
pub(crate) const fn sig1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

impl Job {
    pub fn new(header: &[u8; 80], target: &Target) -> Job {
        let word = |o: usize| u32::from_be_bytes(header[o..o + 4].try_into().expect("4 bytes"));
        Job::from_parts(
            midstate(header),
            [word(64), word(68), word(72)],
            target.top_word(),
        )
    }

    pub fn from_parts(midstate: [u32; 8], tail: [u32; 3], target_top: u32) -> Job {
        // Rounds 0..=2 use only tail words, and round 3 adds the nonce to a
        // value that's otherwise constant.
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = midstate;
        for (i, &w) in tail.iter().enumerate() {
            let t1 = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w);
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        let t1_base = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(K[3]);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
            .wrapping_add((a & b) ^ (a & c) ^ (b & c));
        let state4 = [
            t1_base.wrapping_add(t2),
            a,
            b,
            c,
            d.wrapping_add(t1_base),
            e,
            f,
            g,
        ];

        // Message schedule of the second header block:
        // [t0, t1, t2, nonce, PAD, 0 × 10, 640].
        let w16 = tail[0].wrapping_add(sig0(tail[1]));
        let w17 = tail[1].wrapping_add(sig0(tail[2])).wrapping_add(sig1(640));
        let w18_base = tail[2].wrapping_add(sig1(w16));
        let w19_base = sig0(PAD).wrapping_add(sig1(w17));

        Job {
            midstate,
            tail,
            target_top,
            state4,
            w16,
            w17,
            w18_base,
            w19_base,
        }
    }

    /// The job's constants as SIMD (or scalar) words.
    #[inline(always)]
    pub(crate) fn consts<W: Copy>(&self, splat: impl Fn(u32) -> W) -> Consts<W> {
        Consts {
            mid: self.midstate.map(&splat),
            state4: self.state4.map(&splat),
            w16: splat(self.w16),
            w17: splat(self.w17),
            w18_base: splat(self.w18_base),
            w19_base: splat(self.w19_base),
            k: K.map(&splat),
            iv: IV.map(&splat),
            pad: splat(PAD),
            zero: splat(0),
            len_inner: splat(640),
            len_outer: splat(256),
        }
    }
}

/// [`Job`] constants widened to a lane type.
#[derive(Clone, Copy)]
pub(crate) struct Consts<W> {
    pub mid: [W; 8],
    pub state4: [W; 8],
    pub w16: W,
    pub w17: W,
    pub w18_base: W,
    pub w19_base: W,
    pub k: [W; 64],
    pub iv: [W; 8],
    pub pad: W,
    pub zero: W,
    pub len_inner: W,
    pub len_outer: W,
}

/// Computes `state[7]` of the outer hash (IV added) for nonce word lane `$n`
/// (the byte-swapped header nonce).
///
/// Expects these functions in scope for the lane type `W`: `add`, `sig0`,
/// `sig1`, `big_sig0` (Σ0 on a), `big_sig1` (Σ1 on e), `ch`, `maj`. Each backend
/// defines them with its own `#[target_feature]`, so they inline into the
/// backend's search loop.
macro_rules! sha_top {
    ($c:expr, $n:expr) => {{
        let c = $c;
        let n = $n;
        // ---- inner compression: block 2 of the header, from the midstate ----
        let z = c.zero;
        let mut w = [z; 64];
        w[4] = c.pad;
        w[15] = c.len_inner;
        w[16] = c.w16;
        w[17] = c.w17;
        w[18] = add(c.w18_base, sig0(n));
        w[19] = add(n, c.w19_base);
        for i in 20..64 {
            // w[0..4] only feed w16..w19, which are set above; the zero words
            // in w[4..16] constant-fold away.
            w[i] = add(
                add(add(w[i - 16], sig0(w[i - 15])), w[i - 7]),
                sig1(w[i - 2]),
            );
        }
        let s = c.state4;
        let (mut a, mut b, mut cc, mut d) = (add(n, s[0]), s[1], s[2], s[3]);
        let (mut e, mut f, mut g, mut h) = (add(n, s[4]), s[5], s[6], s[7]);
        for i in 4..64 {
            let t1 = add(add(add(add(h, big_sig1(e)), ch(e, f, g)), c.k[i]), w[i]);
            let t2 = add(big_sig0(a), maj(a, b, cc));
            h = g;
            g = f;
            f = e;
            e = add(d, t1);
            d = cc;
            cc = b;
            b = a;
            a = add(t1, t2);
        }
        let inner = [
            add(a, c.mid[0]),
            add(b, c.mid[1]),
            add(cc, c.mid[2]),
            add(d, c.mid[3]),
            add(e, c.mid[4]),
            add(f, c.mid[5]),
            add(g, c.mid[6]),
            add(h, c.mid[7]),
        ];

        // ---- outer compression, rounds 0..=60 only ----
        let mut w = [z; 61];
        w[..8].copy_from_slice(&inner);
        w[8] = c.pad;
        w[15] = c.len_outer;
        for i in 16..61 {
            w[i] = add(
                add(add(w[i - 16], sig0(w[i - 15])), w[i - 7]),
                sig1(w[i - 2]),
            );
        }
        let iv = c.iv;
        let (mut a, mut b, mut cc, mut d) = (iv[0], iv[1], iv[2], iv[3]);
        let (mut e, mut f, mut g, mut h) = (iv[4], iv[5], iv[6], iv[7]);
        for i in 0..60 {
            let t1 = add(add(add(add(h, big_sig1(e)), ch(e, f, g)), c.k[i]), w[i]);
            let t2 = add(big_sig0(a), maj(a, b, cc));
            h = g;
            g = f;
            f = e;
            e = add(d, t1);
            d = cc;
            cc = b;
            b = a;
            a = add(t1, t2);
        }
        let _ = (a, b);
        // Round 60's e becomes h after round 63.
        let t1 = add(add(add(add(h, big_sig1(e)), ch(e, f, g)), c.k[60]), w[60]);
        add(add(d, t1), iv[7])
    }};
}
pub(crate) use sha_top;

/// A CPU hashing path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Backend {
    Scalar,
    ShaNi,
    Armv8Sha2,
    Neon,
    Avx2,
    Avx512,
}

impl Backend {
    pub const ALL: [Backend; 6] = [
        Backend::Scalar,
        Backend::ShaNi,
        Backend::Armv8Sha2,
        Backend::Neon,
        Backend::Avx2,
        Backend::Avx512,
    ];

    /// The configuration name (`CpuConfig::backend`).
    pub fn name(self) -> &'static str {
        match self {
            Backend::Scalar => "scalar",
            Backend::ShaNi => "sha-ni",
            Backend::Armv8Sha2 => "armv8-sha2",
            Backend::Neon => "neon",
            Backend::Avx2 => "avx2",
            Backend::Avx512 => "avx512",
        }
    }

    /// The human name.
    pub fn label(self) -> &'static str {
        match self {
            Backend::Scalar => "Portable",
            Backend::ShaNi => "SHA-NI",
            Backend::Armv8Sha2 => "ARMv8 SHA2",
            Backend::Neon => "NEON 4-lane",
            Backend::Avx2 => "AVX2 8-lane",
            Backend::Avx512 => "AVX-512 16-lane",
        }
    }

    pub fn from_name(name: &str) -> Option<Backend> {
        let name = name.trim().to_ascii_lowercase().replace('_', "-");
        Backend::ALL.into_iter().find(|b| {
            b.name() == name
                || (name == "portable" && *b == Backend::Scalar)
                || (name == "shani" && *b == Backend::ShaNi)
        })
    }

    /// Whether this backend can run on this machine (compiled in and the CPU has it).
    pub fn is_supported(self) -> bool {
        match self {
            Backend::Scalar => true,
            #[cfg(target_arch = "x86_64")]
            Backend::ShaNi => shani::detected(),
            #[cfg(target_arch = "x86_64")]
            Backend::Avx2 => std::arch::is_x86_feature_detected!("avx2"),
            #[cfg(target_arch = "x86_64")]
            Backend::Avx512 => std::arch::is_x86_feature_detected!("avx512f"),
            #[cfg(target_arch = "aarch64")]
            Backend::Armv8Sha2 => std::arch::is_aarch64_feature_detected!("sha2"),
            #[cfg(target_arch = "aarch64")]
            Backend::Neon => std::arch::is_aarch64_feature_detected!("neon"),
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    pub fn supported() -> Vec<Backend> {
        Backend::ALL
            .into_iter()
            .filter(|b| b.is_supported())
            .collect()
    }

    /// Nonces the backend hashes per call, ideally; batch sizes should be multiples.
    pub fn lanes(self) -> u32 {
        match self {
            Backend::Neon => 4,
            Backend::Avx2 => 8,
            Backend::Avx512 => 16,
            _ => 1,
        }
    }

    /// Hashes `count` nonces starting at `start` (wrapping past `u32::MAX`), and
    /// appends every candidate nonce to `out`. See the module docs.
    ///
    /// Falls back to the portable path if the backend isn't supported here, so
    /// this is always safe to call.
    #[inline]
    pub fn search(self, job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
        match self {
            #[cfg(target_arch = "x86_64")]
            Backend::ShaNi if shani::detected() => shani::search(job, start, count, out),
            #[cfg(target_arch = "x86_64")]
            Backend::Avx2 if std::arch::is_x86_feature_detected!("avx2") => {
                avx2::search(job, start, count, out)
            }
            #[cfg(target_arch = "x86_64")]
            Backend::Avx512 if std::arch::is_x86_feature_detected!("avx512f") => {
                avx512::search(job, start, count, out)
            }
            #[cfg(target_arch = "aarch64")]
            Backend::Armv8Sha2 if std::arch::is_aarch64_feature_detected!("sha2") => {
                armv8::search(job, start, count, out)
            }
            #[cfg(target_arch = "aarch64")]
            Backend::Neon => neon::search(job, start, count, out),
            _ => scalar::search(job, start, count, out),
        }
    }

    /// Single-threaded hashes per second over roughly `duration`.
    pub fn benchmark(self, duration: Duration) -> f64 {
        let header = bench_header();
        // Target top word 0, as for any share of difficulty >= 1: candidates are
        // rare, as in real mining.
        let job = Job::new(&header, &Target([0; 32]));
        let mut out = Vec::new();
        let batch = 1u32 << 14;
        // Warm up caches and frequency scaling a little first.
        self.search(&job, 0, batch, &mut out);
        let started = Instant::now();
        let mut hashes = 0u64;
        let mut nonce = batch;
        while started.elapsed() < duration {
            self.search(&job, nonce, batch, &mut out);
            out.clear();
            nonce = nonce.wrapping_add(batch);
            hashes += batch as u64;
        }
        hashes as f64 / started.elapsed().as_secs_f64()
    }
}

/// A fixed, arbitrary header for benchmarks.
pub(crate) fn bench_header() -> [u8; 80] {
    let mut header = [0u8; 80];
    for (i, b) in header.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(97).wrapping_add(13);
    }
    header
}

/// The top 32 bits of a raw SHA-256d output read as a number.
#[inline]
pub fn hash_top_word(hash: &[u8; 32]) -> u32 {
    u32::from_le_bytes([hash[28], hash[29], hash[30], hash[31]])
}

#[cfg(test)]
pub(crate) mod testutil {
    use hansolo_core::sha::{header_hash_from_midstate, midstate};

    use super::*;

    /// xorshift64*: deterministic, dependency-free test randomness.
    pub struct Rng(pub u64);
    impl Rng {
        pub fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        pub fn next_u32(&mut self) -> u32 {
            (self.next_u64() >> 32) as u32
        }
        pub fn header(&mut self) -> [u8; 80] {
            let mut h = [0u8; 80];
            for b in h.iter_mut() {
                *b = self.next_u64() as u8;
            }
            h
        }
    }

    pub fn genesis() -> [u8; 80] {
        hex_header(
            "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c",
        )
    }

    fn hex_header(s: &str) -> [u8; 80] {
        let mut out = [0u8; 80];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    /// The candidates the reference says a search must return.
    pub fn reference_candidates(
        header: &[u8; 80],
        start: u32,
        count: u32,
        target_top: u32,
    ) -> Vec<u32> {
        let mid = midstate(header);
        let mut h = *header;
        (0..count)
            .map(|i| start.wrapping_add(i))
            .filter(|&nonce| {
                h[76..80].copy_from_slice(&nonce.to_le_bytes());
                hash_top_word(&header_hash_from_midstate(&mid, &h)) <= target_top
            })
            .collect()
    }

    /// Checks a search function against the reference on random headers, nonce
    /// ranges (including ones that wrap past `u32::MAX`), target top words, and
    /// on the genesis block.
    pub fn check_search(search: impl Fn(&Job, u32, u32, &mut Vec<u32>)) {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut out = Vec::new();
        for round in 0..48 {
            let header = rng.header();
            // A top word that lets ~1/16 of hashes through exercises the compare
            // on every lane position; u32::MAX checks everything passes.
            let target_top = match round % 4 {
                0 => u32::MAX,
                1 => 0x0FFF_FFFF,
                2 => rng.next_u32(),
                _ => 0x00FF_FFFF,
            };
            let mut job = Job::new(&header, &Target([0; 32]));
            job.target_top = target_top;
            let start = match round % 3 {
                0 => rng.next_u32(),
                1 => u32::MAX - 20, // wraps
                _ => 0,
            };
            let count = 1 + rng.next_u32() % 700;
            out.clear();
            search(&job, start, count, &mut out);
            let expected = reference_candidates(&header, start, count, target_top);
            assert_eq!(
                out, expected,
                "round {round}: start {start:#x} count {count} top {target_top:#x}"
            );
        }

        // Genesis: nonce 0x7c2bac1d has 32+ leading zero bits; nothing near it does.
        let header = genesis();
        let job = Job::new(&header, &Target([0; 32]));
        out.clear();
        search(&job, 0x7c2b_ac1d - 1000, 2000, &mut out);
        assert_eq!(out, vec![0x7c2b_ac1d]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for b in Backend::ALL {
            assert_eq!(Backend::from_name(b.name()), Some(b));
        }
        assert_eq!(Backend::from_name("PORTABLE"), Some(Backend::Scalar));
        assert_eq!(Backend::from_name("nope"), None);
    }

    #[test]
    fn every_supported_backend_via_dispatch() {
        for b in Backend::supported() {
            testutil::check_search(|j, s, c, o| b.search(j, s, c, o));
        }
    }

    #[test]
    fn genesis_top_word_is_zero() {
        let header = testutil::genesis();
        let mut h = header;
        h[76..80].copy_from_slice(&0x7c2b_ac1du32.to_le_bytes());
        let hash = hansolo_core::sha::header_hash_from_midstate(&midstate(&header), &h);
        assert_eq!(hash_top_word(&hash), 0);
        let mut display = hash;
        display.reverse();
        assert_eq!(
            display
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
    }
}
