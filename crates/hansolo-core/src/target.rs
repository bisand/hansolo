//! Targets and difficulty.
//!
//! A [`Target`] is a 256-bit number stored big-endian, so the byte order reads
//! the way block explorers print hashes. A header hash comes out of SHA-256d in
//! the opposite order; [`Target::is_met_by`] takes it as it comes.

use serde::{Deserialize, Serialize};

/// A 256-bit target, big-endian.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Target(pub [u8; 32]);

impl core::fmt::Debug for Target {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Target({})", hex::encode(self.0))
    }
}

/// The difficulty-1 target: `0x00000000FFFF` followed by zeros.
pub const DIFF1: Target = {
    let mut t = [0u8; 32];
    t[4] = 0xFF;
    t[5] = 0xFF;
    Target(t)
};

/// 2^32, the expected number of hashes per unit of difficulty (near enough;
/// the exact figure is 2^48 / 0xFFFF).
pub const HASHES_PER_DIFFICULTY: f64 = 4_295_032_833.0;

impl Target {
    pub const MAX: Target = Target([0xFF; 32]);

    /// Expands compact `nBits`.
    pub fn from_compact(bits: u32) -> Target {
        let exponent = (bits >> 24) as usize;
        let mantissa = bits & 0x00FF_FFFF;
        let mut out = [0u8; 32];
        if mantissa & 0x0080_0000 != 0 {
            // Negative in Bitcoin's encoding; never valid for a target.
            return Target(out);
        }
        let bytes = mantissa.to_be_bytes(); // [0, m2, m1, m0]
        for (i, &byte) in bytes[1..].iter().enumerate() {
            // Byte i of the mantissa sits at 256^(exponent - 1 - i).
            let power = exponent as isize - 1 - i as isize;
            if (0..32).contains(&power) {
                out[31 - power as usize] = byte;
            }
        }
        Target(out)
    }

    /// The target for a pool or network difficulty.
    pub fn from_difficulty(difficulty: f64) -> Target {
        if !(difficulty.is_finite()) || difficulty <= 0.0 {
            return Target::MAX;
        }
        // DIFF1 = 0xFFFF * 2^208, so target = (0xFFFF / d) * 2^208.
        let value = 65535.0 / difficulty;
        if value <= 0.0 {
            return Target([0; 32]);
        }
        let (mantissa, exponent) = decompose(value);
        let shift = exponent + 208;
        let mut out = [0u8; 32];
        if shift >= 256 {
            return Target::MAX;
        }
        // mantissa is < 2^53; place it shifted left by `shift` bits (may be negative).
        let m = mantissa as u128;
        for bit in 0..64i32 {
            if m >> bit & 1 == 1 {
                let pos = bit + shift;
                if (0..256).contains(&pos) {
                    let pos = pos as usize;
                    out[31 - pos / 8] |= 1 << (pos % 8);
                }
            }
        }
        Target(out)
    }

    /// Whether a raw SHA-256d output (little-endian number) is at or below this target.
    #[inline]
    pub fn is_met_by(&self, hash: &[u8; 32]) -> bool {
        for i in 0..32 {
            let h = hash[31 - i];
            let t = self.0[i];
            if h != t {
                return h < t;
            }
        }
        true
    }

    /// The difficulty this target represents.
    pub fn difficulty(&self) -> f64 {
        DIFF1.as_f64() / self.as_f64().max(1.0)
    }

    /// The value as a float, for ratios.
    pub fn as_f64(&self) -> f64 {
        self.0.iter().fold(0.0, |acc, &b| acc * 256.0 + b as f64)
    }

    /// The most significant 32 bits of the target — what a GPU shader compares
    /// against first.
    pub fn top_word(&self) -> u32 {
        u32::from_be_bytes([self.0[0], self.0[1], self.0[2], self.0[3]])
    }
}

/// The difficulty a raw SHA-256d output would have satisfied.
pub fn hash_difficulty(hash: &[u8; 32]) -> f64 {
    let mut be = *hash;
    be.reverse();
    Target(be).difficulty()
}

/// Leading zero bits of a raw SHA-256d output read as a number.
pub fn leading_zero_bits(hash: &[u8; 32]) -> u32 {
    let mut zeros = 0;
    for &byte in hash.iter().rev() {
        if byte == 0 {
            zeros += 8;
        } else {
            return zeros + byte.leading_zeros();
        }
    }
    zeros
}

fn decompose(value: f64) -> (u64, i32) {
    let bits = value.to_bits();
    let exp = ((bits >> 52) & 0x7FF) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    if exp == 0 {
        (frac, -1074)
    } else {
        (frac | (1u64 << 52), exp - 1075)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_genesis() {
        let t = Target::from_compact(0x1d00ffff);
        assert_eq!(t, DIFF1);
        assert!((t.difficulty() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn difficulty_round_trip() {
        for d in [1.0, 2.0, 1024.0, 65536.0, 1.5e12, 0.001] {
            let got = Target::from_difficulty(d).difficulty();
            assert!((got / d - 1.0).abs() < 1e-9, "{d} -> {got}");
        }
    }

    #[test]
    fn meets_compares_as_numbers() {
        let t = Target::from_difficulty(1.0);
        let mut hash = [0xFFu8; 32];
        assert!(!t.is_met_by(&hash));
        hash[31] = 0;
        hash[30] = 0;
        hash[29] = 0;
        hash[28] = 0;
        hash[27] = 0x00;
        assert!(t.is_met_by(&hash));
        assert_eq!(leading_zero_bits(&hash), 40);
    }
}
