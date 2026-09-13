//! Stratum v1 message formats, independent of any socket.
//!
//! # Byte orders, the part everyone gets wrong once
//!
//! - `prevhash` in `mining.notify` is the header's internal (little-endian)
//!   32 bytes with *each 4-byte word* byte-swapped — the way the original
//!   cpuminer printed it. Converting back swaps each word again.
//! - Merkle branch entries are internal byte order, as sent.
//! - `version`, `nbits` and `ntime` are hex of the numeric value.
//! - In `mining.submit`, `ntime` and `nonce` are 8 hex digits of the numeric
//!   value (big-endian text), and extranonce2 is the raw bytes in hex.
//!
//! # Extranonce layout
//!
//! The pool's coinbase is `coinb1 ‖ extranonce1 ‖ extranonce2 ‖ coinb2`, where
//! the scriptSig length in `coinb1` already counts extranonce2's bytes. HanSolo
//! devices always insert exactly [`EXTRANONCE_LEN`] (8) bytes. The size the pool
//! expects is taken from the subscribe response when given and otherwise found
//! by test-parsing the coinbase with each candidate size. Larger sizes are
//! handled by zero-padding in front of our 8 bytes; smaller ones cannot be
//! served without a variable-length extranonce in `hansolo-core`.

use bitcoin::Transaction;
use hansolo_core::work::EXTRANONCE_LEN;
use hansolo_core::{Target, Work};
use serde_json::{Value, json};

pub const USER_AGENT: &str = "hansolo/0.1.0";

/// A parsed pool URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StratumEndpoint {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

/// Accepts `stratum+tcp://host:port`, `stratum+ssl://` / `stratum+tls://`, and
/// a bare `host:port`.
pub fn parse_stratum_url(url: &str) -> Result<StratumEndpoint, String> {
    let url = url.trim();
    let (tls, rest) = if let Some((scheme, rest)) = url.split_once("://") {
        match scheme.to_ascii_lowercase().as_str() {
            "stratum+tcp" | "stratum" | "tcp" => (false, rest),
            "stratum+ssl" | "stratum+tls" | "ssl" | "tls" => (true, rest),
            other => return Err(format!("unsupported pool URL scheme {other:?}")),
        }
    } else {
        (false, url)
    };
    let rest = rest.trim_end_matches('/');
    let (host, port) = rest
        .rsplit_once(':')
        .ok_or_else(|| format!("pool URL {url:?} needs a port, e.g. stratum+tcp://host:3333"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.contains('/') {
        return Err(format!("pool URL {url:?} has no valid host"));
    }
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|&p| p != 0)
        .ok_or_else(|| format!("pool URL {url:?} has an invalid port"))?;
    Ok(StratumEndpoint {
        host: host.to_string(),
        port,
        tls,
    })
}

/// A decoded `mining.notify`.
#[derive(Clone, Debug)]
pub struct Notify {
    pub job_id: String,
    /// Internal byte order.
    pub prev_hash: [u8; 32],
    pub coinb1: Vec<u8>,
    pub coinb2: Vec<u8>,
    pub merkle_branch: Vec<[u8; 32]>,
    pub version: u32,
    pub bits: u32,
    pub time: u32,
    pub clean_jobs: bool,
}

fn hex_u32(v: &Value, what: &str) -> Result<u32, String> {
    match v {
        Value::String(s) => u32::from_str_radix(s.trim_start_matches("0x"), 16)
            .map_err(|_| format!("bad {what} {s:?}")),
        Value::Number(n) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| format!("bad {what} {n}")),
        _ => Err(format!("missing {what}")),
    }
}

fn hex_bytes(v: &Value, what: &str) -> Result<Vec<u8>, String> {
    v.as_str()
        .and_then(|s| hex::decode(s).ok())
        .ok_or_else(|| format!("bad {what}"))
}

/// Stratum's word-swapped prevhash to header byte order.
pub fn stratum_prevhash_to_internal(hex_str: &str) -> Result<[u8; 32], String> {
    let bytes: [u8; 32] = hex::decode(hex_str)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("bad prevhash {hex_str:?}"))?;
    let mut out = [0u8; 32];
    for (dst, src) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(bytes.as_chunks::<4>().0)
    {
        dst.copy_from_slice(&[src[3], src[2], src[1], src[0]]);
    }
    Ok(out)
}

pub fn parse_notify(params: &Value) -> Result<Notify, String> {
    let p = params.as_array().ok_or("notify params are not an array")?;
    if p.len() < 8 {
        return Err(format!(
            "notify has {} params, expected at least 8",
            p.len()
        ));
    }
    let job_id = match &p[0] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err("bad job id".into()),
    };
    let prev_hash = stratum_prevhash_to_internal(p[1].as_str().ok_or("bad prevhash")?)?;
    let merkle_branch = p[4]
        .as_array()
        .ok_or("bad merkle branch")?
        .iter()
        .map(|b| {
            hex_bytes(b, "merkle branch entry")?
                .try_into()
                .map_err(|_| "merkle branch entry is not 32 bytes".to_string())
        })
        .collect::<Result<Vec<[u8; 32]>, String>>()?;
    Ok(Notify {
        job_id,
        prev_hash,
        coinb1: hex_bytes(&p[2], "coinb1")?,
        coinb2: hex_bytes(&p[3], "coinb2")?,
        merkle_branch,
        version: hex_u32(&p[5], "version")?,
        bits: hex_u32(&p[6], "nbits")?,
        time: hex_u32(&p[7], "ntime")?,
        clean_jobs: p.get(8).and_then(Value::as_bool).unwrap_or(false),
    })
}

/// The extranonce2 size that makes `coinb1 ‖ e1 ‖ e2 ‖ coinb2` a well-formed
/// transaction. Prefers `hint` and then 8 when several would parse.
pub fn extranonce2_size(
    coinb1: &[u8],
    extranonce1: &[u8],
    coinb2: &[u8],
    hint: Option<usize>,
) -> Option<usize> {
    let parses = |n: usize| {
        let mut tx = Vec::with_capacity(coinb1.len() + extranonce1.len() + n + coinb2.len());
        tx.extend_from_slice(coinb1);
        tx.extend_from_slice(extranonce1);
        tx.resize(tx.len() + n, 0);
        tx.extend_from_slice(coinb2);
        bitcoin::consensus::deserialize::<Transaction>(&tx).is_ok()
    };
    hint.into_iter()
        .chain([EXTRANONCE_LEN])
        .chain(0..=32)
        .find(|&n| n <= 32 && parses(n))
}

/// Offset of the scriptSig in a serialised coinbase, and its declared length.
fn script_sig_span(tx: &[u8]) -> Option<(usize, usize)> {
    let mut i = 4;
    if tx.get(4) == Some(&0) && tx.get(5) == Some(&1) {
        i += 2; // segwit marker and flag
    }
    let (_, n) = read_varint(tx.get(i..)?)?; // input count
    i += n + 36;
    let (len, n) = read_varint(tx.get(i..)?)?;
    Some((i + n, len as usize))
}

pub(crate) fn read_varint(b: &[u8]) -> Option<(u64, usize)> {
    match *b.first()? {
        0xfd => Some((u16::from_le_bytes(b.get(1..3)?.try_into().ok()?) as u64, 3)),
        0xfe => Some((u32::from_le_bytes(b.get(1..5)?.try_into().ok()?) as u64, 5)),
        0xff => Some((u64::from_le_bytes(b.get(1..9)?.try_into().ok()?), 9)),
        v => Some((v as u64, 1)),
    }
}

/// The BIP34 height at the start of a scriptSig.
pub fn parse_bip34_height(script: &[u8]) -> Option<u64> {
    match *script.first()? {
        0x00 => Some(0),
        op @ 0x51..=0x60 => Some((op - 0x50) as u64),
        len @ 1..=8 => {
            let bytes = script.get(1..1 + len as usize)?;
            let mut v = 0u64;
            for (i, &b) in bytes.iter().enumerate() {
                v |= (b as u64) << (8 * i);
            }
            // A set top bit would make the number negative; not a height.
            (bytes.last()? & 0x80 == 0).then_some(v)
        }
        _ => None,
    }
}

/// The block height from the start of a coinbase (e.g. `coinb1 ‖ extranonce1`).
pub fn coinbase_height(coinbase_start: &[u8]) -> Option<u64> {
    let (offset, len) = script_sig_span(coinbase_start)?;
    let end = coinbase_start.len().min(offset + len);
    parse_bip34_height(coinbase_start.get(offset..end)?)
}

/// Builds work from a notify. Returns the work and the zero padding to put in
/// front of the device extranonce when submitting.
pub fn build_work(
    id: u64,
    notify: &Notify,
    extranonce1: &[u8],
    extranonce2_size: usize,
    share_difficulty: f64,
    clean: bool,
) -> Result<(Work, usize), String> {
    if extranonce2_size < EXTRANONCE_LEN {
        return Err(format!(
            "the pool expects a {extranonce2_size}-byte extranonce2; HanSolo needs at least {EXTRANONCE_LEN}"
        ));
    }
    let pad = extranonce2_size - EXTRANONCE_LEN;
    let mut prefix = Vec::with_capacity(notify.coinb1.len() + extranonce1.len() + pad);
    prefix.extend_from_slice(&notify.coinb1);
    prefix.extend_from_slice(extranonce1);
    prefix.resize(prefix.len() + pad, 0);
    let height = coinbase_height(&prefix);
    let work = Work {
        id,
        job_id: notify.job_id.clone(),
        version: notify.version,
        prev_hash: notify.prev_hash,
        bits: notify.bits,
        time: notify.time,
        coinbase_prefix: prefix,
        coinbase_suffix: notify.coinb2.clone(),
        merkle_branch: notify.merkle_branch.clone(),
        share_target: Target::from_difficulty(share_difficulty),
        height,
        clean,
    };
    Ok((work, pad))
}

pub fn subscribe(id: u64) -> Value {
    json!({"id": id, "method": "mining.subscribe", "params": [USER_AGENT]})
}

pub fn authorize(id: u64, user: &str, password: &str) -> Value {
    json!({"id": id, "method": "mining.authorize", "params": [user, password]})
}

pub fn suggest_difficulty(id: u64, difficulty: f64) -> Value {
    // Integer pools (ckpool) reject fractions; public-pool accepts either.
    let d = if difficulty >= 1.0 {
        json!(difficulty.round() as u64)
    } else {
        json!(difficulty)
    };
    json!({"id": id, "method": "mining.suggest_difficulty", "params": [d]})
}

/// `[worker, job_id, extranonce2_hex, ntime_hex, nonce_hex]`.
pub fn submit(
    id: u64,
    user: &str,
    job_id: &str,
    extranonce2: &[u8],
    ntime: u32,
    nonce: u32,
) -> Value {
    json!({
        "id": id,
        "method": "mining.submit",
        "params": [user, job_id, hex::encode(extranonce2), format!("{ntime:08x}"), format!("{nonce:08x}")],
    })
}

/// How a pool answered a submit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    Stale(String),
    Rejected(String),
}

/// Interprets a `mining.submit` response.
pub fn submit_verdict(result: &Value, error: &Value) -> Verdict {
    if error.is_null() {
        return if result.as_bool() == Some(true) {
            Verdict::Accepted
        } else {
            Verdict::Rejected("rejected".into())
        };
    }
    let (code, message) = match error {
        Value::Array(a) => (
            a.first().and_then(Value::as_i64),
            a.get(1).and_then(Value::as_str),
        ),
        Value::Object(o) => (
            o.get("code").and_then(Value::as_i64),
            o.get("message").and_then(Value::as_str),
        ),
        _ => (None, error.as_str()),
    };
    let message = message.unwrap_or("rejected").to_string();
    let text = match code {
        Some(c) => format!("{message} ({c})"),
        None => message.clone(),
    };
    if code == Some(21) || message.to_ascii_lowercase().contains("stale") {
        Verdict::Stale(text)
    } else {
        Verdict::Rejected(text)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hansolo_core::sha::sha256d;

    /// Mainnet block 250000, cut into a notify the way a pool would: coinb1 ends
    /// after the BIP34 height push, the next 4 scriptSig bytes play extranonce1
    /// and the 8 after that extranonce2.
    pub(crate) const COINB1: &str = "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff130390d003";
    pub(crate) const EXTRANONCE1: &str = "0447f9fc";
    pub(crate) const EXTRANONCE2: &str = "5108880055609a63";
    pub(crate) const COINB2: &str = "010000000000000154b8ad95000000001976a914ce2daea72b5b48fc85d9bba2263225cbe98985e088ac00000000";
    pub(crate) const PREVHASH: &str =
        "b72c8f022b6b0b5eef83bacaafb64ca34ec07b4ac2e82d880000000900000000";
    pub(crate) const BRANCH: [&str; 8] = [
        "d24625388664a797e13b1598c178b8f6f5605cf44087ad1adc907a357d9a2acf",
        "5ec17819f15fc62752cdd7187629b0e28adfff50d81b1117b1747674af3c8495",
        "6bba0d192ae3b2948e7647d157ea9c8fbd8b6137b0b115c6d1dc1c71b53368c9",
        "90922eb6e38c909afacad5a1ccc20f0ed475c65a38bc4e181fcaeabc2d457f63",
        "b156a0154dd6b6f73078c6a7a5cf3c2f9feb856085b481574010752c77c30b5a",
        "653722d897dbb6aca751c9bbf87f2b2a29482f29552eee1f555b35ba688291b1",
        "3a234583ab4a8726e730081f805b4dc6660dd8d74260819a749445ded1eceae4",
        "761cb3eb1cd4e3fb9257478ad1e5243deabc569bf9711e1723edac20e1324ee6",
    ];
    pub(crate) const NONCE: u32 = 9_533_025;
    pub(crate) const NTIME: &str = "51fcf947";

    pub(crate) fn notify_params(job_id: &str, clean: bool) -> Value {
        json!([
            job_id, PREVHASH, COINB1, COINB2, BRANCH, "2", "1972dbf2", NTIME, clean
        ])
    }

    #[test]
    fn urls() {
        assert_eq!(
            parse_stratum_url("stratum+tcp://public-pool.io:21496").unwrap(),
            StratumEndpoint {
                host: "public-pool.io".into(),
                port: 21496,
                tls: false
            }
        );
        assert!(
            parse_stratum_url("stratum+ssl://pool.example:443")
                .unwrap()
                .tls
        );
        assert!(
            parse_stratum_url("stratum+tls://pool.example:443/")
                .unwrap()
                .tls
        );
        assert_eq!(parse_stratum_url("10.0.0.2:3333").unwrap().host, "10.0.0.2");
        assert!(parse_stratum_url("stratum+tcp://pool.example").is_err());
        assert!(parse_stratum_url("http://pool.example:80").is_err());
        assert!(parse_stratum_url("pool:0").is_err());
    }

    #[test]
    fn real_notify_rebuilds_block_250000() {
        let notify = parse_notify(&notify_params("abc", true)).unwrap();
        assert!(notify.clean_jobs);
        let e1 = hex::decode(EXTRANONCE1).unwrap();
        let size = extranonce2_size(&notify.coinb1, &e1, &notify.coinb2, None).unwrap();
        assert_eq!(size, 8);
        let (work, pad) = build_work(7, &notify, &e1, size, 1.0, true).unwrap();
        assert_eq!(pad, 0);
        assert_eq!(work.height, Some(250_000));

        let e2 = hex::decode(EXTRANONCE2).unwrap();
        let mut header = work.header(&e2);
        header[76..80].copy_from_slice(&NONCE.to_le_bytes());
        let mut hash = sha256d(&header);
        assert!(work.network_target().is_met_by(&hash));
        hash.reverse();
        assert_eq!(
            hex::encode(hash),
            "000000000000003887df1f29024b06fc2200b55f8af8f35453d7be294df2d214"
        );
    }

    #[test]
    fn extranonce_size_inference() {
        let notify = parse_notify(&notify_params("abc", false)).unwrap();
        // Pretend the pool used a 2-byte extranonce1: then 10 bytes of extranonce2 fit.
        let e1 = &hex::decode(EXTRANONCE1).unwrap()[..2];
        assert_eq!(
            extranonce2_size(&notify.coinb1, e1, &notify.coinb2, None),
            Some(10)
        );
        let (work, pad) = build_work(1, &notify, e1, 10, 1.0, false).unwrap();
        assert_eq!(pad, 2);
        assert_eq!(work.coinbase_prefix.len(), notify.coinb1.len() + 4);
        // 6 bytes of extranonce1 leaves 6 for extranonce2: not servable.
        let e1 = [0u8; 6];
        assert_eq!(
            extranonce2_size(&notify.coinb1, &e1, &notify.coinb2, None),
            Some(6)
        );
        assert!(build_work(1, &notify, &e1, 6, 1.0, false).is_err());
    }

    #[test]
    fn heights() {
        assert_eq!(parse_bip34_height(&[0x51]), Some(1));
        assert_eq!(parse_bip34_height(&[0x60]), Some(16));
        assert_eq!(parse_bip34_height(&[0x01, 0x11]), Some(17));
        assert_eq!(parse_bip34_height(&[0x02, 0x80, 0x00]), Some(128));
        assert_eq!(parse_bip34_height(&[0x03, 0x40, 0xd1, 0x0c]), Some(840_000));
        assert_eq!(parse_bip34_height(&[0x01, 0x80]), None);
        assert_eq!(parse_bip34_height(&[]), None);
    }

    #[test]
    fn submit_format() {
        let msg = submit(9, "addr.w", "job1", &[0xab; 8], 0x51fcf947, 0x00917661);
        assert_eq!(
            msg["params"],
            json!(["addr.w", "job1", "abababababababab", "51fcf947", "00917661"])
        );
        assert_eq!(suggest_difficulty(1, 1024.4)["params"], json!([1024]));
        assert_eq!(suggest_difficulty(1, 0.5)["params"], json!([0.5]));
    }

    #[test]
    fn verdicts() {
        assert_eq!(
            submit_verdict(&json!(true), &Value::Null),
            Verdict::Accepted
        );
        assert!(matches!(
            submit_verdict(&json!(false), &Value::Null),
            Verdict::Rejected(_)
        ));
        assert!(matches!(
            submit_verdict(&Value::Null, &json!([21, "Job not found", ""])),
            Verdict::Stale(_)
        ));
        assert!(matches!(
            submit_verdict(&Value::Null, &json!({"code": 23, "message": "Low difficulty share"})),
            Verdict::Rejected(m) if m == "Low difficulty share (23)"
        ));
        assert!(matches!(
            submit_verdict(&Value::Null, &json!([20, "Stale share", null])),
            Verdict::Stale(_)
        ));
    }
}
