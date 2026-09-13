//! Work as devices see it.
//!
//! The engine turns a Stratum `mining.notify` or a `getblocktemplate` result
//! into a [`Work`], and publishes it in a [`WorkCell`]. Devices watch the cell's
//! generation, build headers from the work with an extranonce unique to them,
//! and send back a [`Found`] for every hash that meets the share target.
//!
//! # Keeping devices out of each other's search space
//!
//! Every header has a 32-bit nonce, which a CPU core exhausts in seconds and a
//! GPU in well under one. Beyond that the search space is the coinbase
//! extranonce: [`EXTRANONCE_LEN`] bytes, built by [`Work::extranonce`] from a
//! *lane* and a *roll*.
//!
//! - The lane is unique per worker thread or GPU queue: `(device_index << 16) | sub_lane`.
//! - The roll counts up each time that lane exhausts its nonces.
//!
//! Two lanes never produce the same header, so no hash is ever computed twice.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use crate::sha::sha256d;
use crate::target::Target;

/// Bytes of extranonce this miner inserts into the coinbase.
pub const EXTRANONCE_LEN: usize = 8;

#[derive(Clone, Debug)]
pub struct Work {
    /// Unique, increasing id assigned by the engine.
    pub id: u64,
    /// The pool's job id, or a template identifier in node mode.
    pub job_id: String,
    pub version: u32,
    /// Previous block hash in header (internal, little-endian) byte order.
    pub prev_hash: [u8; 32],
    pub bits: u32,
    pub time: u32,
    /// Coinbase bytes before the extranonce (Stratum: `coinb1 || extranonce1`).
    pub coinbase_prefix: Vec<u8>,
    /// Coinbase bytes after the extranonce (Stratum: `coinb2`).
    pub coinbase_suffix: Vec<u8>,
    /// Merkle branch for the coinbase, in internal byte order.
    pub merkle_branch: Vec<[u8; 32]>,
    /// What a device must beat to report a hash at all.
    pub share_target: Target,
    pub height: Option<u64>,
    /// Pool asked miners to drop old work (`clean_jobs`), or a new block arrived.
    pub clean: bool,
}

impl Work {
    /// The extranonce for a lane and roll. See the module docs.
    #[inline]
    pub fn extranonce(lane: u32, roll: u32) -> [u8; EXTRANONCE_LEN] {
        let mut out = [0u8; EXTRANONCE_LEN];
        out[..4].copy_from_slice(&lane.to_be_bytes());
        out[4..].copy_from_slice(&roll.to_be_bytes());
        out
    }

    /// The network target this work's block must meet.
    pub fn network_target(&self) -> Target {
        Target::from_compact(self.bits)
    }

    /// The full coinbase transaction (non-witness serialisation) for an extranonce.
    pub fn coinbase(&self, extranonce: &[u8]) -> Vec<u8> {
        let mut tx = Vec::with_capacity(
            self.coinbase_prefix.len() + extranonce.len() + self.coinbase_suffix.len(),
        );
        tx.extend_from_slice(&self.coinbase_prefix);
        tx.extend_from_slice(extranonce);
        tx.extend_from_slice(&self.coinbase_suffix);
        tx
    }

    /// The merkle root for an extranonce, internal byte order.
    pub fn merkle_root(&self, extranonce: &[u8]) -> [u8; 32] {
        let mut root = sha256d(&self.coinbase(extranonce));
        let mut pair = [0u8; 64];
        for branch in &self.merkle_branch {
            pair[..32].copy_from_slice(&root);
            pair[32..].copy_from_slice(branch);
            root = sha256d(&pair);
        }
        root
    }

    /// An 80-byte header for an extranonce, with the nonce set to zero.
    pub fn header(&self, extranonce: &[u8]) -> [u8; 80] {
        let mut header = [0u8; 80];
        header[0..4].copy_from_slice(&self.version.to_le_bytes());
        header[4..36].copy_from_slice(&self.prev_hash);
        header[36..68].copy_from_slice(&self.merkle_root(extranonce));
        header[68..72].copy_from_slice(&self.time.to_le_bytes());
        header[72..76].copy_from_slice(&self.bits.to_le_bytes());
        header
    }
}

/// What a device sends back: a header whose hash met the share target.
#[derive(Clone, Debug)]
pub struct Found {
    pub work_id: u64,
    /// Index of the device that found it.
    pub device: usize,
    pub extranonce: [u8; EXTRANONCE_LEN],
    /// The complete header, nonce included.
    pub header: [u8; 80],
    /// Raw SHA-256d output.
    pub hash: [u8; 32],
}

impl Found {
    pub fn nonce(&self) -> u32 {
        u32::from_le_bytes(self.header[76..80].try_into().expect("4 bytes"))
    }
}

/// The latest work, shared between the engine and every device.
///
/// Devices check [`generation`](WorkCell::generation) between batches — an
/// atomic load — and only take the lock when it has moved.
#[derive(Default)]
pub struct WorkCell {
    generation: AtomicU64,
    current: RwLock<Option<Arc<Work>>>,
}

impl WorkCell {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    pub fn get(&self) -> Option<Arc<Work>> {
        self.current.read().clone()
    }

    /// Publishes new work; `None` tells devices to idle (disconnected, stopped).
    pub fn set(&self, work: Option<Work>) {
        *self.current.write() = work.map(Arc::new);
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merkle_root_without_branch_is_coinbase_txid() {
        let work = Work {
            id: 1,
            job_id: "1".into(),
            version: 0x2000_0000,
            prev_hash: [0; 32],
            bits: 0x1d00ffff,
            time: 0,
            coinbase_prefix: vec![1, 2, 3],
            coinbase_suffix: vec![4, 5],
            merkle_branch: vec![],
            share_target: Target::MAX,
            height: None,
            clean: false,
        };
        let ex = Work::extranonce(7, 9);
        let mut cb = vec![1, 2, 3];
        cb.extend_from_slice(&ex);
        cb.extend_from_slice(&[4, 5]);
        assert_eq!(work.merkle_root(&ex), sha256d(&cb));
        assert_eq!(&work.header(&ex)[36..68], &sha256d(&cb));
    }
}
