//! End to end against a fake Bitcoin Core (regtest difficulty) on localhost.
//!
//! The fake node validates every submitted block with the `bitcoin` crate:
//! merkle root, witness commitment, BIP34 height, proof of work, and that the
//! coinbase pays the configured address the full template value.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bitcoin::consensus::{deserialize, serialize};
use bitcoin::hashes::Hash;
use bitcoin::{Amount, Block, ScriptBuf, Transaction, TxOut, Witness};
use common::{ScalarDevice, report, wait_for};
use hansolo_core::snapshot::{MinerStatus, ShareResult};
use hansolo_core::{Config, WorkSource};
use hansolo_engine::Miner;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const ADDRESS: &str = "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080";
const AUTH: &str = "Basic dXNlcjpwYXNz"; // user:pass

struct Chain {
    tip: [u8; 32],
    height: u64,
    blocks_ok: usize,
    stale: usize,
}

fn tx(seed: u8) -> Transaction {
    use bitcoin::{OutPoint, Sequence, TxIn, Txid, absolute::LockTime, transaction};
    Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([seed; 32]), 1),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[vec![seed; 72], vec![3; 33]]),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(5000),
            script_pubkey: ScriptBuf::from_bytes([&[0x00, 0x14][..], &[seed; 20]].concat()),
        }],
    }
}

fn template(chain: &Chain) -> Value {
    let txs = [tx(1), tx(2)];
    let mut wtxids = vec![bitcoin::Wtxid::all_zeros()];
    wtxids.extend(txs.iter().map(Transaction::compute_wtxid));
    let root = bitcoin::merkle_tree::calculate_root(wtxids.into_iter()).unwrap();
    let mut data = root.to_byte_array().to_vec();
    data.extend([0u8; 32]);
    let commitment = hansolo_core::sha::sha256d(&data);
    let mut prev = chain.tip;
    prev.reverse();
    json!({
        "version": 0x2000_0000,
        "previousblockhash": hex::encode(prev),
        "transactions": txs.iter().map(|t| json!({
            "data": hex::encode(serialize(t)),
            "txid": t.compute_txid().to_string(),
            "hash": t.compute_wtxid().to_string(),
        })).collect::<Vec<_>>(),
        "coinbasevalue": 50_0000_0000u64 + 1234,
        "default_witness_commitment": format!("6a24aa21a9ed{}", hex::encode(commitment)),
        "curtime": 1_726_000_000u64,
        "bits": "207fffff",
        "height": chain.height + 1,
    })
}

fn submit_block(chain: &mut Chain, block_hex: &str) -> Value {
    let block: Block = deserialize(&hex::decode(block_hex).unwrap()).unwrap();
    assert!(block.check_merkle_root(), "merkle root");
    assert!(block.check_witness_commitment(), "witness commitment");
    let target = block.header.target();
    block.header.validate_pow(target).expect("proof of work");
    let payout = hansolo_engine::parse_payout_address(ADDRESS)
        .unwrap()
        .script_pubkey;
    let coinbase = &block.txdata[0];
    assert_eq!(coinbase.output[0].script_pubkey, payout);
    assert_eq!(
        coinbase.output[0].value,
        Amount::from_sat(50_0000_0000 + 1234)
    );
    assert_eq!(block.txdata.len(), 3);
    if block.header.prev_blockhash.to_byte_array() != chain.tip {
        chain.stale += 1;
        return json!("inconclusive");
    }
    assert_eq!(block.bip34_block_height().unwrap(), chain.height + 1);
    chain.tip = block.block_hash().to_byte_array();
    chain.height += 1;
    chain.blocks_ok += 1;
    Value::Null
}

async fn handle(
    mut stream: tokio::net::TcpStream,
    chain: Arc<Mutex<Chain>>,
    calls: Arc<AtomicUsize>,
) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 65536];
    let (head_end, length) = loop {
        let n = stream.read(&mut buf).await.unwrap();
        assert!(n > 0, "client closed early");
        raw.extend_from_slice(&buf[..n]);
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..i]).to_string();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .unwrap()
                .parse::<usize>()
                .unwrap();
            break (i + 4, len);
        }
    };
    while raw.len() < head_end + length {
        let n = stream.read(&mut buf).await.unwrap();
        raw.extend_from_slice(&buf[..n]);
    }
    let head = String::from_utf8_lossy(&raw[..head_end]).to_string();
    let reply = |status: &str, body: String| {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    if !head.contains(&format!("Authorization: {AUTH}")) {
        stream
            .write_all(reply("401 Unauthorized", String::new()).as_bytes())
            .await
            .unwrap();
        return;
    }
    calls.fetch_add(1, Ordering::Relaxed);
    let req: Value = serde_json::from_slice(&raw[head_end..head_end + length]).unwrap();
    let result = {
        let mut chain = chain.lock();
        match req["method"].as_str().unwrap() {
            "getblockchaininfo" => {
                json!({"chain": "regtest", "blocks": chain.height, "difficulty": 4.656542373906925e-10, "initialblockdownload": false})
            }
            "getmininginfo" => json!({"networkhashps": 12.5}),
            "getnetworkinfo" => json!({"subversion": "/Satoshi:28.0.0/"}),
            "getblocktemplate" => {
                assert_eq!(req["params"][0]["rules"], json!(["segwit"]));
                template(&chain)
            }
            "submitblock" => submit_block(&mut chain, req["params"][0].as_str().unwrap()),
            other => panic!("unexpected {other}"),
        }
    };
    let body = json!({"result": result, "error": null, "id": req["id"]}).to_string();
    stream
        .write_all(reply("200 OK", body).as_bytes())
        .await
        .unwrap();
}

#[test]
fn mines_blocks_on_a_fake_regtest_node() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let chain = Arc::new(Mutex::new(Chain {
        tip: [7; 32],
        height: 100,
        blocks_ok: 0,
        stale: 0,
    }));
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (server_chain, server_calls) = (chain.clone(), calls.clone());
    runtime.spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(handle(stream, server_chain.clone(), server_calls.clone()));
        }
    });

    let config = Config {
        payout_address: ADDRESS.into(),
        source: WorkSource::Node {
            rpc_url: format!("http://127.0.0.1:{port}"),
            rpc_user: "user".into(),
            rpc_password: "pass".into(),
            cookie_file: None,
            poll_secs: 1,
        },
        ..Config::default()
    };
    // Regtest: about every other hash is a block, so hash slowly.
    let device = Box::new(ScalarDevice {
        name: "slow".into(),
        batch: 1,
        pause: Some(Duration::from_millis(150)),
        liar: false,
    });
    let miner = Miner::new();
    miner
        .start_with_devices(config.clone(), report(), vec![device])
        .unwrap();

    let snap = wait_for(
        &miner,
        Duration::from_secs(30),
        "two accepted blocks",
        |s| s.shares.blocks_found >= 2,
    );
    assert!(chain.lock().blocks_ok >= 2);
    assert_eq!(snap.status, MinerStatus::Mining);
    assert_eq!(snap.connection.mode, "Bitcoin Core");
    assert_eq!(snap.connection.server.as_deref(), Some("/Satoshi:28.0.0/"));
    assert_eq!(snap.network.chain.as_deref(), Some("regtest"));
    assert_eq!(snap.network.hashrate, Some(12.5));
    assert_eq!(snap.network.block_reward, Some(50_0000_0000 + 1234));
    assert!(
        snap.shares
            .recent
            .iter()
            .any(|r| r.result == ShareResult::Block)
    );
    let job = snap.job.as_ref().unwrap();
    assert_eq!(job.tx_count, Some(2));
    assert!(job.height.unwrap() >= 102);
    assert!(job.coinbase_hex.contains(&hex::encode(b"/HanSolo/")));
    miner.stop();

    // Wrong credentials: retries, reports the error, never mines.
    let mut bad = config;
    if let WorkSource::Node { rpc_password, .. } = &mut bad.source {
        *rpc_password = "wrong".into();
    }
    miner
        .start_with_devices(bad, report(), vec![ScalarDevice::boxed("cpu")])
        .unwrap();
    let snap = wait_for(&miner, Duration::from_secs(10), "an auth error", |s| {
        s.connection
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("401"))
    });
    assert!(snap.job.is_none());
    miner.stop();

    // A mainnet address on a regtest node is refused outright.
    let mut wrong_chain = Config {
        payout_address: "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq".into(),
        ..Config::default()
    };
    wrong_chain.source = WorkSource::Node {
        rpc_url: format!("http://127.0.0.1:{port}"),
        rpc_user: "user".into(),
        rpc_password: "pass".into(),
        cookie_file: None,
        poll_secs: 1,
    };
    miner
        .start_with_devices(wrong_chain, report(), vec![ScalarDevice::boxed("cpu")])
        .unwrap();
    wait_for(
        &miner,
        Duration::from_secs(10),
        "a chain mismatch error",
        |s| matches!(&s.status, MinerStatus::Error(e) if e.contains("regtest")),
    );
    assert!(!miner.is_running());
}
