//! Block templates, coinbase construction and block assembly.
//!
//! # The coinbase HanSolo builds
//!
//! ```text
//! version 2 | 1 input: null prevout, sequence 0xffffffff
//!   scriptSig: <BIP34 height push> <push 8: extranonce> <push "/HanSolo/">
//! outputs: payout script = coinbasevalue; OP_RETURN witness commitment (when given)
//! locktime 0
//! ```
//!
//! The non-witness serialisation is split around the 8 extranonce bytes into
//! `Work::coinbase_prefix`/`coinbase_suffix`; the txid (and so the merkle root)
//! is always the non-witness hash. Only the block sent to `submitblock` carries
//! the witness form: marker `00 01` and a single 32-byte zero witness reserved
//! value, which the commitment from `getblocktemplate` already assumes.

use hansolo_core::sha::sha256d;
use hansolo_core::target::HASHES_PER_DIFFICULTY;
use hansolo_core::work::EXTRANONCE_LEN;
use hansolo_core::{Target, Work};
use serde_json::Value;

/// Written into every coinbase scriptSig after the extranonce.
pub const COINBASE_TAG: &[u8] = b"/HanSolo/";

/// A template transaction: raw hex kept as-is (only needed for `submitblock`).
#[derive(Clone, Debug)]
pub struct TemplateTx {
    pub data_hex: String,
    /// Internal byte order.
    pub txid: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct Template {
    pub height: u64,
    pub version: u32,
    /// Internal byte order.
    pub prev_hash: [u8; 32],
    pub bits: u32,
    pub curtime: u32,
    pub coinbase_value: u64,
    /// The `default_witness_commitment` output script, when segwit is active.
    pub witness_commitment: Option<Vec<u8>>,
    pub transactions: Vec<TemplateTx>,
}

fn hash_from_display(s: &str) -> Result<[u8; 32], String> {
    let mut h: [u8; 32] = hex::decode(s)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("bad hash {s:?}"))?;
    h.reverse();
    Ok(h)
}

/// Parses a `getblocktemplate` result.
pub fn parse_template(v: &Value) -> Result<Template, String> {
    let u64_field = |name: &str| {
        v.get(name)
            .and_then(Value::as_u64)
            .ok_or(format!("template has no {name}"))
    };
    let bits = v
        .get("bits")
        .and_then(Value::as_str)
        .and_then(|b| u32::from_str_radix(b, 16).ok())
        .ok_or("template has no bits")?;
    let prev_hash = hash_from_display(
        v.get("previousblockhash")
            .and_then(Value::as_str)
            .ok_or("template has no previousblockhash")?,
    )?;
    let witness_commitment = match v.get("default_witness_commitment").and_then(Value::as_str) {
        Some(h) => Some(hex::decode(h).map_err(|_| "bad default_witness_commitment")?),
        None => None,
    };
    let transactions = v
        .get("transactions")
        .and_then(Value::as_array)
        .ok_or("template has no transactions")?
        .iter()
        .map(|tx| {
            let data_hex = tx
                .get("data")
                .and_then(Value::as_str)
                .ok_or("transaction without data")?;
            let txid = hash_from_display(
                tx.get("txid")
                    .and_then(Value::as_str)
                    .ok_or("transaction without txid")?,
            )?;
            Ok(TemplateTx {
                data_hex: data_hex.to_string(),
                txid,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Template {
        height: u64_field("height")?,
        // Signed in Core's JSON; the header stores the same 32 bits.
        version: v
            .get("version")
            .and_then(Value::as_i64)
            .ok_or("template has no version")? as u32,
        prev_hash,
        bits,
        curtime: u64_field("curtime")? as u32,
        coinbase_value: u64_field("coinbasevalue")?,
        witness_commitment,
        transactions,
    })
}

/// The scriptSig prefix BIP34 requires: `CScript() << height` in Core terms.
/// Heights 1–16 are a single `OP_1`..`OP_16`; others a minimal little-endian
/// number push, with a padding zero when the top bit would read as a sign.
pub fn bip34_height_push(height: u64) -> Vec<u8> {
    match height {
        0 => vec![0x00],
        1..=16 => vec![0x50 + height as u8],
        _ => {
            let mut num = Vec::new();
            let mut h = height;
            while h > 0 {
                num.push((h & 0xff) as u8);
                h >>= 8;
            }
            if num.last().is_some_and(|b| b & 0x80 != 0) {
                num.push(0);
            }
            let mut out = vec![num.len() as u8];
            out.extend_from_slice(&num);
            out
        }
    }
}

pub(crate) fn write_varint(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

/// The non-witness coinbase, split around the extranonce.
pub fn build_coinbase(
    height: u64,
    value: u64,
    payout_script: &[u8],
    witness_commitment: Option<&[u8]>,
) -> (Vec<u8>, Vec<u8>) {
    let height_push = bip34_height_push(height);
    let script_len = height_push.len() + 1 + EXTRANONCE_LEN + 1 + COINBASE_TAG.len();
    debug_assert!(script_len <= 100, "coinbase scriptSig limit");

    let mut prefix = Vec::with_capacity(64);
    prefix.extend_from_slice(&2u32.to_le_bytes());
    prefix.push(1); // one input
    prefix.extend_from_slice(&[0u8; 32]);
    prefix.extend_from_slice(&u32::MAX.to_le_bytes());
    write_varint(&mut prefix, script_len as u64);
    prefix.extend_from_slice(&height_push);
    prefix.push(EXTRANONCE_LEN as u8);

    let mut suffix = Vec::with_capacity(128);
    suffix.push(COINBASE_TAG.len() as u8);
    suffix.extend_from_slice(COINBASE_TAG);
    suffix.extend_from_slice(&u32::MAX.to_le_bytes()); // sequence
    write_varint(&mut suffix, 1 + u64::from(witness_commitment.is_some()));
    suffix.extend_from_slice(&value.to_le_bytes());
    write_varint(&mut suffix, payout_script.len() as u64);
    suffix.extend_from_slice(payout_script);
    if let Some(commitment) = witness_commitment {
        suffix.extend_from_slice(&0u64.to_le_bytes());
        write_varint(&mut suffix, commitment.len() as u64);
        suffix.extend_from_slice(commitment);
    }
    suffix.extend_from_slice(&0u32.to_le_bytes()); // locktime
    (prefix, suffix)
}

/// The witness serialisation of a single-input coinbase: marker and flag after
/// the version, one 32-byte zero witness item before the locktime.
pub fn coinbase_with_witness(non_witness: &[u8]) -> Vec<u8> {
    let (body, locktime) = non_witness.split_at(non_witness.len() - 4);
    let mut out = Vec::with_capacity(non_witness.len() + 36);
    out.extend_from_slice(&body[..4]);
    out.extend_from_slice(&[0x00, 0x01]);
    out.extend_from_slice(&body[4..]);
    out.extend_from_slice(&[0x01, 0x20]);
    out.extend_from_slice(&[0u8; 32]);
    out.extend_from_slice(locktime);
    out
}

/// The merkle branch for the transaction at index 0, given the txids of every
/// other transaction in block order.
pub fn merkle_branch(other_txids: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut level: Vec<[u8; 32]> = other_txids.to_vec();
    let mut branch = Vec::new();
    let mut pair = [0u8; 64];
    while !level.is_empty() {
        branch.push(level[0]);
        // The rest of this level pairs up on its own; index 0 pairs with level[0].
        let mut rest = level.split_off(1);
        if rest.len() % 2 == 1 {
            rest.push(*rest.last().expect("non-empty"));
        }
        level = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                pair[..32].copy_from_slice(&c[0]);
                pair[32..].copy_from_slice(&c[1]);
                sha256d(&pair)
            })
            .collect();
    }
    branch
}

/// Work for a template. `share_difficulty` is local; see [`local_share_difficulty`].
pub fn build_work(
    id: u64,
    template: &Template,
    payout_script: &[u8],
    share_difficulty: f64,
    clean: bool,
) -> Work {
    let (prefix, suffix) = build_coinbase(
        template.height,
        template.coinbase_value,
        payout_script,
        template.witness_commitment.as_deref(),
    );
    let txids: Vec<[u8; 32]> = template.transactions.iter().map(|t| t.txid).collect();
    Work {
        id,
        job_id: format!("{}-{}", template.height, id),
        version: template.version,
        prev_hash: template.prev_hash,
        bits: template.bits,
        time: template.curtime,
        coinbase_prefix: prefix,
        coinbase_suffix: suffix,
        merkle_branch: merkle_branch(&txids),
        share_target: Target::from_difficulty(share_difficulty),
        height: Some(template.height),
        clean,
    }
}

/// The complete block for `submitblock`, as hex.
pub fn assemble_block_hex(
    header: &[u8; 80],
    coinbase_non_witness: &[u8],
    template: &Template,
) -> String {
    let coinbase = if template.witness_commitment.is_some() {
        coinbase_with_witness(coinbase_non_witness)
    } else {
        coinbase_non_witness.to_vec()
    };
    let mut head = header.to_vec();
    write_varint(&mut head, template.transactions.len() as u64 + 1);
    head.extend_from_slice(&coinbase);
    let tx_len: usize = template.transactions.iter().map(|t| t.data_hex.len()).sum();
    let mut out = String::with_capacity(head.len() * 2 + tx_len);
    out.push_str(&hex::encode(head));
    for tx in &template.transactions {
        out.push_str(&tx.data_hex);
    }
    out
}

/// A local share difficulty giving roughly one share per 20 s at `hashrate`,
/// never harder than the network (so every block is also a share).
pub fn local_share_difficulty(hashrate: f64, network_difficulty: f64) -> f64 {
    let mut d = hashrate * 20.0 / HASHES_PER_DIFFICULTY;
    d = if d.is_finite() && d > 0.0 {
        2f64.powi(d.log2().round() as i32)
    } else {
        // Unknown hashrate: easy enough for a single CPU core.
        1.0 / 1024.0
    };
    if network_difficulty > 0.0 {
        d.min(network_difficulty)
    } else {
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::block::{Header, Version};
    use bitcoin::consensus::{deserialize, serialize};
    use bitcoin::hashes::Hash;
    use bitcoin::{
        Amount, Block, BlockHash, CompactTarget, ScriptBuf, Transaction, TxMerkleNode, TxOut,
        Witness,
    };

    #[test]
    fn bip34_pushes() {
        assert_eq!(bip34_height_push(1), [0x51]);
        assert_eq!(bip34_height_push(16), [0x60]);
        assert_eq!(bip34_height_push(17), [0x01, 0x11]);
        assert_eq!(bip34_height_push(127), [0x01, 0x7f]);
        assert_eq!(bip34_height_push(128), [0x02, 0x80, 0x00]);
        assert_eq!(bip34_height_push(255), [0x02, 0xff, 0x00]);
        assert_eq!(bip34_height_push(256), [0x02, 0x00, 0x01]);
        assert_eq!(bip34_height_push(840_000), [0x03, 0x40, 0xd1, 0x0c]);
        // Matches what the Stratum side reads back.
        for h in [1, 16, 17, 127, 128, 32_768, 840_000, 16_777_216] {
            assert_eq!(
                crate::stratum::protocol::parse_bip34_height(&bip34_height_push(h)),
                Some(h)
            );
        }
    }

    #[test]
    fn merkle_branch_matches_full_tree() {
        for n in 1..=12usize {
            let txids: Vec<[u8; 32]> = (0..n).map(|i| sha256d(&[i as u8])).collect();
            let branch = merkle_branch(&txids[1..]);
            let mut root = txids[0];
            for b in &branch {
                let mut pair = [0u8; 64];
                pair[..32].copy_from_slice(&root);
                pair[32..].copy_from_slice(b);
                root = sha256d(&pair);
            }
            let expected = bitcoin::merkle_tree::calculate_root(
                txids.iter().map(|t| bitcoin::Txid::from_byte_array(*t)),
            )
            .unwrap();
            assert_eq!(root, expected.to_byte_array(), "{n} transactions");
        }
    }

    /// A spend with a witness, so the witness commitment actually commits to something.
    fn segwit_tx(seed: u8) -> Transaction {
        use bitcoin::{OutPoint, Sequence, TxIn, Txid, absolute::LockTime, transaction};
        Transaction {
            version: transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(Txid::from_byte_array([seed; 32]), 0),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::from_slice(&[vec![seed; 71], vec![2; 33]]),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(1000 + seed as u64),
                script_pubkey: ScriptBuf::from_bytes([&[0x00, 0x14][..], &[seed; 20]].concat()),
            }],
        }
    }

    fn template_for(height: u64, txs: &[Transaction], with_commitment: bool) -> Template {
        let mut t = Template {
            height,
            version: 0x2000_0000,
            prev_hash: sha256d(b"prev"),
            bits: 0x207f_ffff,
            curtime: 1_700_000_000,
            coinbase_value: 50_0000_0000,
            witness_commitment: None,
            transactions: txs
                .iter()
                .map(|tx| TemplateTx {
                    data_hex: hex::encode(serialize(tx)),
                    txid: tx.compute_txid().to_byte_array(),
                })
                .collect(),
        };
        if with_commitment {
            // What Core computes: wtxid merkle root with the coinbase as zero,
            // hashed with a zero witness reserved value.
            let mut wtxids = vec![[0u8; 32]];
            wtxids.extend(txs.iter().map(|tx| tx.compute_wtxid().to_byte_array()));
            let root = bitcoin::merkle_tree::calculate_root(
                wtxids.into_iter().map(bitcoin::Wtxid::from_byte_array),
            )
            .unwrap();
            let mut data = root.to_byte_array().to_vec();
            data.extend_from_slice(&[0u8; 32]);
            let commitment = sha256d(&data);
            let mut script = vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
            script.extend_from_slice(&commitment);
            t.witness_commitment = Some(script);
        }
        t
    }

    #[test]
    fn assembled_block_validates() {
        let payout = crate::parse_payout_address("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080")
            .unwrap()
            .script_pubkey;
        let txs = vec![segwit_tx(1), segwit_tx(2), segwit_tx(3)];
        for (height, with_commitment) in [(1, true), (17, true), (128, false), (840_000, true)] {
            let mut txs = txs.clone();
            if !with_commitment {
                // Without a commitment the block may not carry witness data at all.
                txs.iter_mut().for_each(|tx| tx.input[0].witness.clear());
            }
            let template = template_for(height, &txs, with_commitment);
            let work = build_work(5, &template, payout.as_bytes(), 1.0, true);
            let extranonce = hansolo_core::Work::extranonce(0x0001_0002, 7);
            let coinbase_bytes = work.coinbase(&extranonce);

            // The coinbase parses, its txid is the non-witness hash, and it is shaped right.
            let coinbase: Transaction = deserialize(&coinbase_bytes).unwrap();
            assert_eq!(
                coinbase.compute_txid().to_byte_array(),
                sha256d(&coinbase_bytes)
            );
            assert!(coinbase.is_coinbase());
            assert!(coinbase.input[0].script_sig.len() <= 100);
            assert_eq!(coinbase.output[0].script_pubkey, payout);
            assert_eq!(
                coinbase.output[0].value,
                Amount::from_sat(template.coinbase_value)
            );
            assert_eq!(coinbase.output.len(), 1 + usize::from(with_commitment));

            let mut header = work.header(&extranonce);
            header[76..80].copy_from_slice(&42u32.to_le_bytes());
            let block_hex = assemble_block_hex(&header, &coinbase_bytes, &template);
            let block: Block = deserialize(&hex::decode(&block_hex).unwrap()).unwrap();

            assert_eq!(block.txdata.len(), 4);
            if height > 16 {
                assert_eq!(block.bip34_block_height().unwrap(), height);
            } else {
                // rust-bitcoin only reads data pushes; Core encodes 1–16 as OP_N.
                assert_eq!(
                    block.txdata[0].input[0].script_sig.as_bytes()[0],
                    0x50 + height as u8
                );
            }
            assert!(block.check_merkle_root());
            assert!(block.check_witness_commitment());
            if with_commitment {
                assert_eq!(
                    block.txdata[0].input[0].witness,
                    Witness::from_slice(&[[0u8; 32]])
                );
                let commitment = block.txdata[0].output.last().unwrap();
                assert_eq!(
                    commitment,
                    &TxOut {
                        value: Amount::ZERO,
                        script_pubkey: ScriptBuf::from_bytes(
                            template.witness_commitment.clone().unwrap()
                        ),
                    }
                );
            }
            assert_eq!(
                block.header.prev_blockhash,
                BlockHash::from_byte_array(template.prev_hash)
            );
            assert_eq!(
                block.header.bits,
                CompactTarget::from_consensus(template.bits)
            );
            assert_eq!(block.header.version, Version::from_consensus(0x2000_0000));
            assert_eq!(
                block.header.merkle_root,
                TxMerkleNode::from_byte_array(work.merkle_root(&extranonce))
            );
            assert_eq!(serialize(&block.header), header.to_vec());
            let _: Header = block.header;
        }
    }

    #[test]
    fn parses_core_template() {
        let v = serde_json::json!({
            "version": 536870912,
            "previousblockhash": "000000000000000000011f3ac9e6b3a1b1d8ac6f5c5f2a0e8e2c4d5b6a7f8e9d",
            "transactions": [{
                "data": "00", "txid": "0100000000000000000000000000000000000000000000000000000000000000",
                "hash": "0100000000000000000000000000000000000000000000000000000000000000", "fee": 1, "weight": 4
            }],
            "coinbasevalue": 312_500_000u64,
            "default_witness_commitment": "6a24aa21a9ed0000000000000000000000000000000000000000000000000000000000000000",
            "curtime": 1_726_000_000u64,
            "bits": "17034219",
            "height": 862_000u64,
        });
        let t = parse_template(&v).unwrap();
        assert_eq!(t.height, 862_000);
        assert_eq!(t.bits, 0x1703_4219);
        assert_eq!(t.prev_hash[31], 0x00);
        assert_eq!(t.prev_hash[0], 0x9d);
        assert_eq!(t.transactions[0].txid[31], 0x01);
        assert_eq!(t.witness_commitment.as_ref().unwrap().len(), 38);
    }

    #[test]
    fn share_difficulty() {
        // 10 MH/s for 20 s is ~0.047 → 2^-4.
        assert_eq!(local_share_difficulty(10e6, 1e14), 1.0 / 16.0);
        assert_eq!(local_share_difficulty(0.0, 1e14), 1.0 / 1024.0);
        // Regtest: never harder than the network.
        assert!(local_share_difficulty(1e12, 4.6e-10) <= 4.6e-10);
    }
}
