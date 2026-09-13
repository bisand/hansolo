//! Portable backend: one nonce at a time with plain `u32` arithmetic.
//!
//! Runs everywhere and is the baseline the benchmark compares against. It is the
//! same round function the SIMD paths instantiate, specialised to `u32`.

use super::{Job, sha_top};

#[inline(always)]
fn add(a: u32, b: u32) -> u32 {
    a.wrapping_add(b)
}
#[inline(always)]
fn sig0(x: u32) -> u32 {
    super::sig0(x)
}
#[inline(always)]
fn sig1(x: u32) -> u32 {
    super::sig1(x)
}
#[inline(always)]
fn big_sig0(a: u32) -> u32 {
    a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)
}
#[inline(always)]
fn big_sig1(e: u32) -> u32 {
    e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25)
}
#[inline(always)]
fn ch(e: u32, f: u32, g: u32) -> u32 {
    g ^ (e & (f ^ g))
}
#[inline(always)]
fn maj(a: u32, b: u32, c: u32) -> u32 {
    (a & b) | (c & (a | b))
}

pub fn search(job: &Job, start: u32, count: u32, out: &mut Vec<u32>) {
    let c = job.consts(|x| x);
    for i in 0..count {
        let nonce = start.wrapping_add(i);
        // The header stores the nonce little-endian; SHA reads big-endian words.
        let h7: u32 = sha_top!(&c, nonce.swap_bytes());
        if h7.swap_bytes() <= job.target_top {
            out.push(nonce);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn matches_reference() {
        super::super::testutil::check_search(super::search);
    }
}
