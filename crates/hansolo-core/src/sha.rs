//! Portable SHA-256.
//!
//! Deliberately plain: this is the reference implementation that accelerated
//! backends are checked against, and the fallback on hardware with nothing better.

pub const IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

pub const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Compresses one 64-byte block into `state`.
#[inline]
pub fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (word, chunk) in w.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*chunk);
    }
    compress_words(state, &mut w);
}

/// Compresses a block already split into big-endian words. `w[16..]` is scratch.
#[inline]
pub fn compress_words(state: &mut [u32; 8], w: &mut [u32; 64]) {
    for i in 16..64 {
        w[i] = w[i - 16]
            .wrapping_add(w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3))
            .wrapping_add(w[i - 7])
            .wrapping_add(
                w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10),
            );
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let t1 = h
            .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
            .wrapping_add((e & f) ^ (!e & g))
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
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
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

/// SHA-256 of an arbitrary message.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state = IV;
    let (blocks, rest) = data.as_chunks::<64>();
    for block in blocks {
        compress(&mut state, block);
    }
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let blocks = if rest.len() < 56 { 1 } else { 2 };
    let bits = (data.len() as u64).wrapping_mul(8);
    tail[blocks * 64 - 8..blocks * 64].copy_from_slice(&bits.to_be_bytes());
    for block in tail[..blocks * 64].as_chunks::<64>().0 {
        compress(&mut state, block);
    }
    digest(&state)
}

/// SHA-256(SHA-256(data)), the hash Bitcoin uses for headers, txids and merkle nodes.
pub fn sha256d(data: &[u8]) -> [u8; 32] {
    sha256(&sha256(data))
}

/// The state after the first 64 bytes of an 80-byte header.
///
/// Those bytes (version, previous hash, 28 bytes of merkle root) do not change
/// while the nonce rolls, so every device computes this once per header.
pub fn midstate(header: &[u8; 80]) -> [u32; 8] {
    let mut state = IV;
    compress(&mut state, header[..64].try_into().expect("64 bytes"));
    state
}

/// Serialises a state as the big-endian digest bytes.
#[inline]
pub fn digest(state: &[u32; 8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *chunk = word.to_be_bytes();
    }
    out
}

/// Double SHA-256 of an 80-byte header, starting from its midstate.
///
/// This is the per-nonce work: two compressions for the second half of the
/// header, one for the outer hash. `header[64..80]` must be current.
#[inline]
pub fn header_hash_from_midstate(midstate: &[u32; 8], header: &[u8; 80]) -> [u8; 32] {
    let mut w = [0u32; 64];
    for (word, chunk) in w.iter_mut().zip(header[64..].as_chunks::<4>().0) {
        *word = u32::from_be_bytes(*chunk);
    }
    w[4] = 0x8000_0000;
    w[15] = 640;
    let mut inner = *midstate;
    compress_words(&mut inner, &mut w);

    let mut w2 = [0u32; 64];
    w2[..8].copy_from_slice(&inner);
    w2[8] = 0x8000_0000;
    w2[15] = 256;
    let mut outer = IV;
    compress_words(&mut outer, &mut w2);
    digest(&outer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex32(s: &str) -> [u8; 32] {
        hex::decode(s).unwrap().try_into().unwrap()
    }

    #[test]
    fn empty_and_abc() {
        assert_eq!(
            sha256(b""),
            hex32("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(
            sha256(b"abc"),
            hex32("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn two_block_message() {
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            hex32("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
        );
    }

    #[test]
    fn genesis_header_via_midstate() {
        let header: [u8; 80] = hex::decode(
            "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c",
        )
        .unwrap()
        .try_into()
        .unwrap();
        let mut hash = header_hash_from_midstate(&midstate(&header), &header);
        assert_eq!(hash, sha256d(&header));
        hash.reverse();
        assert_eq!(
            hex::encode(hash),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
    }
}
