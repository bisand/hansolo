//! The WGSL compute shader, generated.
//!
//! The shader computes SHA-256d of a header from its midstate for nonce
//! `base + index` in each invocation and, like the CPU backends, only the outer
//! hash's `state[7]` (rounds 0..=60). Nonces whose top word is at or below the
//! target's go into the results buffer through an atomic counter.
//!
//! It is written out round by round, with the round constants and the known
//! zero and padding message words inlined as literals, rather than as loops
//! over constant arrays. The loop version is shorter but ran at 469 MH/s on an
//! Apple M5 Pro under Metal; this one runs at about 1 GH/s, because the shader
//! compiler folds constants and schedules the arithmetic far better when it
//! sees it flat. Generating the text keeps that flat form readable here.

use std::fmt::Write;

use hansolo_core::sha::{IV, K};

use crate::cpu::PAD;

/// Layout of the `params` storage array; the host writes it in [`super::GpuMiner::dispatch`].
pub(crate) mod params {
    /// 0..8: the midstate.
    pub const MIDSTATE: usize = 0;
    /// 8..16: inner state after round 3 without the nonce word (`a` and `e` get `+ word`).
    pub const STATE4: usize = 8;
    pub const W16: usize = 16;
    pub const W17: usize = 17;
    /// `w18 = this + sig0(word)`.
    pub const W18_BASE: usize = 18;
    /// `w19 = this + word`.
    pub const W19_BASE: usize = 19;
    pub const BASE_NONCE: usize = 20;
    pub const COUNT: usize = 21;
    /// Invocations per dispatch row, for 2-D dispatches.
    pub const ROW: usize = 22;
    pub const TARGET_TOP: usize = 23;
    pub const CAPACITY: usize = 24;
    pub const LEN: usize = 25;
}

/// A message word in the generated code: a known constant, or a variable name.
fn word(name: &str, index: usize, known: &dyn Fn(usize) -> Option<u32>) -> String {
    match known(index) {
        Some(value) => format!("{value}u"),
        None => format!("{name}{index}"),
    }
}

/// `let {name}{i} = w[i-16] + sig0(w[i-15]) + w[i-7] + sig1(w[i-2]);`, skipping zero terms.
fn expansion(out: &mut String, name: &str, i: usize, known: &dyn Fn(usize) -> Option<u32>) {
    let mut terms = Vec::new();
    for (j, f) in [(i - 16, ""), (i - 15, "sig0"), (i - 7, ""), (i - 2, "sig1")] {
        if known(j) == Some(0) {
            continue;
        }
        let w = word(name, j, known);
        terms.push(if f.is_empty() { w } else { format!("{f}({w})") });
    }
    writeln!(out, "    let {name}{i} = {};", terms.join(" + ")).unwrap();
}

fn round(out: &mut String, i: usize, w: &str) {
    writeln!(
        out,
        "    t1 = h + (rotr(e, 6u) ^ rotr(e, 11u) ^ rotr(e, 25u)) + (g ^ (e & (f ^ g))) + {}u + {w};\n    \
         t2 = (rotr(a, 2u) ^ rotr(a, 13u) ^ rotr(a, 22u)) + ((a & b) | (c & (a | b)));\n    \
         h = g; g = f; f = e; e = d + t1; d = c; c = b; b = a; a = t1 + t2;",
        K[i]
    )
    .unwrap();
}

/// The shader source for a workgroup size.
pub(crate) fn source(workgroup_size: u32) -> String {
    use params::*;
    let mut s = String::with_capacity(32_000);
    writeln!(
        s,
        "@group(0) @binding(0) var<storage, read> params: array<u32, {LEN}>;
struct Results {{
    count: atomic<u32>,
    nonces: array<u32>,
}}
@group(0) @binding(1) var<storage, read_write> results: Results;

fn rotr(x: u32, n: u32) -> u32 {{ return (x >> n) | (x << (32u - n)); }}
fn sig0(x: u32) -> u32 {{ return rotr(x, 7u) ^ rotr(x, 18u) ^ (x >> 3u); }}
fn sig1(x: u32) -> u32 {{ return rotr(x, 17u) ^ rotr(x, 19u) ^ (x >> 10u); }}
fn bswap(x: u32) -> u32 {{
    return (x >> 24u) | ((x >> 8u) & 0x0000ff00u) | ((x << 8u) & 0x00ff0000u) | (x << 24u);
}}

@compute @workgroup_size({workgroup_size})
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let index = gid.y * params[{ROW}] + gid.x;
    if (index >= params[{COUNT}]) {{
        return;
    }}
    let nonce = params[{BASE_NONCE}] + index;
    // The header stores the nonce little-endian; SHA reads big-endian words.
    let word = bswap(nonce);
    var t1: u32;
    var t2: u32;"
    )
    .unwrap();

    // ---- inner compression: message [tail0, tail1, tail2, word, PAD, 0 × 10, 640] ----
    let inner_known = |i: usize| match i {
        4 => Some(PAD),
        5..=14 => Some(0),
        15 => Some(640),
        _ => None,
    };
    writeln!(
        s,
        "    let w16 = params[{W16}];\n    let w17 = params[{W17}];\n    \
         let w18 = params[{W18_BASE}] + sig0(word);\n    let w19 = word + params[{W19_BASE}];"
    )
    .unwrap();
    for i in 20..64 {
        expansion(&mut s, "w", i, &inner_known);
    }
    for (k, v) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
        let plus_word = if k == 0 || k == 4 { " + word" } else { "" };
        writeln!(s, "    var {v} = params[{}]{plus_word};", STATE4 + k).unwrap();
    }
    for i in 4..64 {
        round(&mut s, i, &word("w", i, &inner_known));
    }

    // ---- outer compression: message [inner state, PAD, 0 × 6, 256], rounds 0..=60 ----
    let outer_known = |i: usize| match i {
        8 => Some(PAD),
        9..=14 => Some(0),
        15 => Some(256),
        _ => None,
    };
    for (k, v) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
        writeln!(s, "    let v{k} = {v} + params[{}];", MIDSTATE + k).unwrap();
    }
    for i in 16..61 {
        expansion(&mut s, "v", i, &outer_known);
    }
    for (k, v) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
        writeln!(s, "    {v} = {}u;", IV[k]).unwrap();
    }
    for i in 0..60 {
        round(&mut s, i, &word("v", i, &outer_known));
    }
    // Round 60's new e is the final h.
    writeln!(
        s,
        "    t1 = h + (rotr(e, 6u) ^ rotr(e, 11u) ^ rotr(e, 25u)) + (g ^ (e & (f ^ g))) + {}u + v60;
    let top = bswap(d + t1 + {}u);
    if (top <= params[{TARGET_TOP}]) {{
        let slot = atomicAdd(&results.count, 1u);
        if (slot < params[{CAPACITY}]) {{
            results.nonces[slot] = nonce;
        }}
    }}
}}",
        K[60], IV[7]
    )
    .unwrap();
    s
}
