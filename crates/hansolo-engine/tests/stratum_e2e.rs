//! End to end against a fake Stratum pool on localhost.
//!
//! The pool is written from the pool's point of view: it rebuilds each
//! submitted header from its own coinb1/extranonce1/coinb2 and checks the hash,
//! so a wrong submit format or byte order shows up as rejected shares.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{ScalarDevice, report, wait_for};
use hansolo_core::snapshot::{MinerStatus, ShareResult};
use hansolo_core::target::hash_difficulty;
use hansolo_core::{Config, WorkSource, sha};
use hansolo_engine::Miner;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

// Mainnet block 250000 cut into a job (see the protocol unit tests).
const COINB1: &str =
    "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff130390d003";
const EXTRANONCE1: &str = "0447f9fc";
const COINB2: &str =
    "010000000000000154b8ad95000000001976a914ce2daea72b5b48fc85d9bba2263225cbe98985e088ac00000000";
const PREVHASH: &str = "b72c8f022b6b0b5eef83bacaafb64ca34ec07b4ac2e82d880000000900000000";
const BRANCH: [&str; 8] = [
    "d24625388664a797e13b1598c178b8f6f5605cf44087ad1adc907a357d9a2acf",
    "5ec17819f15fc62752cdd7187629b0e28adfff50d81b1117b1747674af3c8495",
    "6bba0d192ae3b2948e7647d157ea9c8fbd8b6137b0b115c6d1dc1c71b53368c9",
    "90922eb6e38c909afacad5a1ccc20f0ed475c65a38bc4e181fcaeabc2d457f63",
    "b156a0154dd6b6f73078c6a7a5cf3c2f9feb856085b481574010752c77c30b5a",
    "653722d897dbb6aca751c9bbf87f2b2a29482f29552eee1f555b35ba688291b1",
    "3a234583ab4a8726e730081f805b4dc6660dd8d74260819a749445ded1eceae4",
    "761cb3eb1cd4e3fb9257478ad1e5243deabc569bf9711e1723edac20e1324ee6",
];
const ADDRESS: &str = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";

#[derive(Default)]
struct PoolStats {
    connections: AtomicUsize,
    valid: AtomicUsize,
    invalid: AtomicUsize,
    bad_format: AtomicUsize,
}

fn notify(job: &str, clean: bool) -> Value {
    json!({"id": null, "method": "mining.notify",
        "params": [job, PREVHASH, COINB1, COINB2, BRANCH, "2", "1972dbf2", "51fcf947", clean]})
}

/// What the pool thinks the share's difficulty is, or `None` for a malformed submit.
fn check_submit(params: &Value, user: &str) -> Option<f64> {
    let p = params.as_array()?;
    if p.len() != 5 || p[0] != user || p[1] != "1" {
        return None;
    }
    let extranonce2 = hex::decode(p[2].as_str()?).ok()?;
    if extranonce2.len() != 8 || p[3].as_str()?.len() != 8 || p[4].as_str()?.len() != 8 {
        return None;
    }
    let ntime = u32::from_str_radix(p[3].as_str()?, 16).ok()?;
    let nonce = u32::from_str_radix(p[4].as_str()?, 16).ok()?;

    let mut coinbase = hex::decode(COINB1).unwrap();
    coinbase.extend(hex::decode(EXTRANONCE1).unwrap());
    coinbase.extend(extranonce2);
    coinbase.extend(hex::decode(COINB2).unwrap());
    let mut root = sha::sha256d(&coinbase);
    for b in BRANCH {
        let mut pair = root.to_vec();
        pair.extend(hex::decode(b).unwrap());
        root = sha::sha256d(&pair);
    }
    let prev_swapped = hex::decode(PREVHASH).unwrap();
    let mut header = Vec::with_capacity(80);
    header.extend(2u32.to_le_bytes());
    for word in prev_swapped.chunks(4) {
        header.extend(word.iter().rev());
    }
    header.extend(root);
    header.extend(ntime.to_le_bytes());
    header.extend(0x1972dbf2u32.to_le_bytes());
    header.extend(nonce.to_le_bytes());
    Some(hash_difficulty(&sha::sha256d(&header)))
}

const POOL_DIFFICULTY: f64 = 0.0001;

async fn serve_connection(stream: tokio::net::TcpStream, stats: Arc<PoolStats>, connection: usize) {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut submits = 0usize;
    let mut user = String::new();
    while let Ok(Some(line)) = lines.next_line().await {
        let msg: Value = serde_json::from_str(&line).unwrap();
        let id = msg["id"].clone();
        let mut out: Vec<Value> = Vec::new();
        match msg["method"].as_str().unwrap_or_default() {
            "mining.subscribe" => {
                assert_eq!(msg["params"][0], "hansolo/0.1.0");
                out.push(json!({"id": id, "result": [[["mining.notify", "s"]], EXTRANONCE1, 8], "error": null}));
            }
            "mining.authorize" => {
                user = msg["params"][0].as_str().unwrap().to_string();
                out.push(json!({"id": id, "result": true, "error": null}));
                out.push(json!({"id": null, "method": "mining.set_difficulty", "params": [POOL_DIFFICULTY]}));
                out.push(json!({"id": 99, "method": "mining.ping", "params": []}));
                out.push(
                    json!({"id": null, "method": "client.show_message", "params": ["hello miner"]}),
                );
                out.push(notify("1", true));
            }
            "mining.submit" => {
                submits += 1;
                match check_submit(&msg["params"], &user) {
                    None => {
                        stats.bad_format.fetch_add(1, Ordering::Relaxed);
                        out.push(
                            json!({"id": id, "result": null, "error": [20, "bad submit", null]}),
                        );
                    }
                    Some(d) if d < POOL_DIFFICULTY => {
                        stats.invalid.fetch_add(1, Ordering::Relaxed);
                        out.push(json!({"id": id, "result": null, "error": [23, "Low difficulty share", null]}));
                    }
                    Some(_) if submits == 2 => {
                        stats.valid.fetch_add(1, Ordering::Relaxed);
                        out.push(
                            json!({"id": id, "result": null, "error": [21, "Job not found", null]}),
                        );
                    }
                    Some(_) => {
                        stats.valid.fetch_add(1, Ordering::Relaxed);
                        out.push(json!({"id": id, "result": true, "error": null}));
                    }
                }
                // The first connection is dropped after a few shares.
                if connection == 1 && submits == 6 {
                    for m in out.drain(..) {
                        w.write_all(format!("{m}\n").as_bytes()).await.ok();
                    }
                    return;
                }
            }
            "mining.suggest_difficulty" => {
                // Like public-pool: answer with a set_difficulty notification, keeping ours.
                out.push(json!({"id": null, "method": "mining.set_difficulty", "params": [POOL_DIFFICULTY]}));
            }
            "" if id == 99 => {
                assert_eq!(msg["result"], "pong");
            }
            other => panic!("unexpected method {other}"),
        }
        for m in out {
            if w.write_all(format!("{m}\n").as_bytes()).await.is_err() {
                return;
            }
        }
    }
}

#[test]
fn mines_submits_reconnects_and_restarts() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let stats = Arc::new(PoolStats::default());
    let listener = runtime.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_stats = stats.clone();
    runtime.spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let n = server_stats.connections.fetch_add(1, Ordering::Relaxed) + 1;
            tokio::spawn(serve_connection(stream, server_stats.clone(), n));
        }
    });

    let config = Config {
        payout_address: ADDRESS.into(),
        worker_name: "test".into(),
        source: WorkSource::Stratum {
            url: format!("stratum+tcp://127.0.0.1:{port}"),
            username: None,
            password: "x".into(),
        },
        ..Config::default()
    };
    let liar = Box::new(ScalarDevice {
        name: "liar".into(),
        batch: 256,
        pause: Some(Duration::from_millis(20)),
        liar: true,
    });
    let devices = vec![
        ScalarDevice::boxed("cpu0"),
        ScalarDevice::boxed("cpu1"),
        liar,
    ];

    let miner = Miner::new();
    miner.set_best_ever(123.0);
    miner
        .start_with_devices(config.clone(), report(), devices)
        .unwrap();
    assert!(miner.is_running());

    let snap = wait_for(
        &miner,
        Duration::from_secs(60),
        "accepted shares and a reconnect",
        |s| s.shares.accepted >= 8 && s.connection.reconnects >= 1 && s.hashrate.current > 0.0,
    );
    assert_eq!(
        stats.bad_format.load(Ordering::Relaxed),
        0,
        "every submit is well-formed"
    );
    assert_eq!(
        stats.invalid.load(Ordering::Relaxed),
        0,
        "the liar's claims never reach the pool"
    );
    assert_eq!(snap.shares.rejected, 0);
    assert!(snap.shares.stale >= 1, "error 21 counts as stale");
    assert_eq!(snap.status, MinerStatus::Mining);
    assert_eq!(snap.connection.user, format!("{ADDRESS}.test"));
    assert_eq!(snap.connection.share_difficulty, POOL_DIFFICULTY);
    assert!(snap.shares.best_difficulty >= POOL_DIFFICULTY);
    assert_eq!(snap.shares.best_ever_difficulty, 123.0);
    assert!(snap.shares.best_hash.as_ref().unwrap().starts_with("0000"));
    assert!(
        snap.shares
            .recent
            .iter()
            .any(|r| r.result == ShareResult::Accepted)
    );
    assert!(
        snap.shares
            .recent
            .iter()
            .any(|r| r.result == ShareResult::Stale)
    );
    assert_eq!(snap.devices.len(), 3);
    assert!(
        snap.devices[2].errors > 0,
        "liar is counted as device errors"
    );
    assert!(snap.log.iter().any(|l| l.message.contains("hello miner")));
    let job = snap.job.as_ref().unwrap();
    assert_eq!(job.height, Some(250_000));
    // Display order: the internal bytes (each Stratum word swapped back) reversed.
    let swapped = hex::decode(PREVHASH).unwrap();
    let mut display: Vec<u8> = swapped
        .chunks(4)
        .flat_map(|w| w.iter().rev().copied())
        .collect();
    display.reverse();
    assert_eq!(job.prev_hash, hex::encode(display));
    assert!(job.prev_hash.starts_with("00000000000000"));
    assert_eq!(snap.network.height, Some(250_000));
    assert_eq!(
        snap.network.difficulty,
        hansolo_core::Target::from_compact(0x1972dbf2).difficulty()
    );
    assert!((snap.network.difficulty / 37_392_766.0 - 1.0).abs() < 1e-3);
    println!("{}", hansolo_engine::format_status_line(&snap));

    miner.stop();
    assert!(!miner.is_running());
    let stopped = miner.snapshot();
    assert_eq!(stopped.status, MinerStatus::Stopped);
    assert!(!stopped.connection.connected);

    // Restart twice in quick succession; the last one must mine.
    miner
        .start_with_devices(config.clone(), report(), vec![ScalarDevice::boxed("cpu0")])
        .unwrap();
    miner
        .start_with_devices(config, report(), vec![ScalarDevice::boxed("cpu0")])
        .unwrap();
    let snap = wait_for(
        &miner,
        Duration::from_secs(30),
        "shares after restart",
        |s| s.shares.accepted >= 2 && s.devices.len() == 1,
    );
    assert_eq!(snap.shares.rejected, 0);
    miner.stop();
    assert_eq!(miner.snapshot().status, MinerStatus::Stopped);
    assert!(stats.connections.load(Ordering::Relaxed) >= 3);
}

#[test]
fn rejects_bad_config() {
    let miner = Miner::new();
    let mut config = Config {
        payout_address: "not-an-address".into(),
        ..Config::default()
    };
    assert!(miner.start(config.clone()).is_err());
    config.payout_address = ADDRESS.into();
    config.source = WorkSource::Stratum {
        url: "stratum+tcp://nohost".into(),
        username: None,
        password: "x".into(),
    };
    assert!(miner.start(config).is_err());
    assert!(!miner.is_running());
}
