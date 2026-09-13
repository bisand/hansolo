//! The Bitcoin Core polling loop.
//!
//! `getblocktemplate` is polled every `poll_secs` (longpoll is not used: it
//! would hold one connection open per poll and the fixed poll already bounds
//! how stale the work can get). New work is published when the template
//! changed — tip, transaction count, reward, or the local share difficulty —
//! and at least every 30 s so `curtime` keeps moving.
//!
//! Local "shares" are statistics only and never leave the process; a solution
//! meeting the network target is assembled into a block and sent with
//! `submitblock` immediately, followed by a fresh template.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use hansolo_core::snapshot::{LogLevel, MinerStatus};
use hansolo_core::{Target, WorkSource};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::rpc::{RpcClient, RpcError};
use super::template::{self, Template};
use crate::address::network_from_chain;
use crate::run::{JobExtra, RunCtx, Submission};

const REPUBLISH_AFTER: Duration = Duration::from_secs(30);
const INFO_REFRESH: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

pub(crate) async fn run(ctx: Arc<RunCtx>, mut submissions: mpsc::UnboundedReceiver<Submission>) {
    let WorkSource::Node {
        rpc_url,
        rpc_user,
        rpc_password,
        cookie_file,
        poll_secs,
    } = ctx.config.source.clone()
    else {
        return;
    };
    let rpc = match RpcClient::new(&rpc_url, &rpc_user, &rpc_password, cookie_file.as_deref()) {
        Ok(rpc) => rpc,
        Err(e) => return ctx.fail(e),
    };
    let poll = Duration::from_secs(poll_secs.max(1));
    let mut backoff = Duration::from_secs(1);
    let mut attempts = 0u32;
    loop {
        if attempts > 0 {
            ctx.update(|st| {
                st.snap.status = MinerStatus::Reconnecting;
                st.snap.connection.reconnects += 1;
            });
            tokio::time::sleep(backoff).await;
        } else {
            ctx.set_status(MinerStatus::Connecting);
        }
        attempts += 1;
        let mut had_work = false;
        let error = serve(&ctx, &rpc, poll, &mut submissions, &mut had_work).await;
        ctx.clear_work();
        backoff = if had_work {
            Duration::from_secs(1)
        } else {
            (backoff * 2).min(MAX_BACKOFF)
        };
        ctx.update(|st| {
            st.push_log(
                LogLevel::Warning,
                format!("Node: {error}; retrying in {}s", backoff.as_secs()),
            );
            st.snap.connection.connected = false;
            st.snap.connection.last_error = Some(error);
        });
    }
}

/// What the last published work was built from.
struct Published {
    prev_hash: [u8; 32],
    tx_count: usize,
    coinbase_value: u64,
    share_difficulty: f64,
    at: Instant,
}

/// Serves work until an error; returns the error.
async fn serve(
    ctx: &RunCtx,
    rpc: &RpcClient,
    poll: Duration,
    submissions: &mut mpsc::UnboundedReceiver<Submission>,
    had_work: &mut bool,
) -> String {
    let chain = match refresh_info(ctx, rpc).await {
        Ok(chain) => chain,
        Err(e) => return e,
    };
    if let Some(network) = network_from_chain(&chain)
        && !ctx.payout.is_valid_for(network)
    {
        ctx.fail(format!(
            "The payout address {} is not valid on the node's chain ({chain})",
            ctx.config.payout_address
        ));
        return std::future::pending::<String>().await;
    }
    let rules = if chain == "signet" {
        json!(["segwit", "signet"])
    } else {
        json!(["segwit"])
    };

    let mut last: Option<Published> = None;
    let mut last_info = Instant::now();
    let mut ticker = tokio::time::interval(poll);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let gbt = match rpc.call("getblocktemplate", json!([{"rules": rules}])).await {
                    Ok(v) => v,
                    Err(RpcError::Rpc { code: -10, message }) => {
                        return format!("node is not ready to mine ({message})");
                    }
                    Err(e) => return format!("getblocktemplate: {e}"),
                };
                let template = match template::parse_template(&gbt) {
                    Ok(t) => Arc::new(t),
                    Err(e) => return format!("getblocktemplate: {e}"),
                };
                let network_difficulty = Target::from_compact(template.bits).difficulty();
                let share_difficulty = template::local_share_difficulty(ctx.hashrate_estimate(), network_difficulty);
                let new_tip = last.as_ref().is_none_or(|p| p.prev_hash != template.prev_hash);
                let changed = new_tip
                    || last.as_ref().is_none_or(|p| {
                        p.tx_count != template.transactions.len()
                            || p.coinbase_value != template.coinbase_value
                            || p.share_difficulty != share_difficulty
                            || p.at.elapsed() >= REPUBLISH_AFTER
                    });
                if new_tip && last.is_some() {
                    ctx.log(LogLevel::Info, format!("New block; now mining height {}", template.height));
                }
                if new_tip || last_info.elapsed() >= INFO_REFRESH {
                    if let Err(e) = refresh_info(ctx, rpc).await {
                        return e;
                    }
                    last_info = Instant::now();
                }
                if changed {
                    publish(ctx, template.clone(), share_difficulty, new_tip);
                    if !*had_work {
                        ctx.log(
                            LogLevel::Success,
                            format!(
                                "Mining height {} with {} transactions (local share difficulty {})",
                                template.height,
                                template.transactions.len(),
                                crate::format_difficulty(share_difficulty)
                            ),
                        );
                    }
                    *had_work = true;
                    last = Some(Published {
                        prev_hash: template.prev_hash,
                        tx_count: template.transactions.len(),
                        coinbase_value: template.coinbase_value,
                        share_difficulty,
                        at: Instant::now(),
                    });
                }
            }
            Some(sub) = submissions.recv() => {
                if sub.is_block {
                    submit_block(ctx, rpc, sub).await;
                    // Whatever happened, the template is now stale.
                    ticker.reset_immediately();
                }
            }
        }
    }
}

fn publish(ctx: &RunCtx, template: Arc<Template>, share_difficulty: f64, clean: bool) {
    let work = template::build_work(
        ctx.next_work_id(),
        &template,
        ctx.payout.script_pubkey.as_bytes(),
        share_difficulty,
        clean,
    );
    ctx.update(|st| st.snap.network.block_reward = Some(template.coinbase_value));
    let (tx_count, value) = (template.transactions.len(), template.coinbase_value);
    ctx.publish(work, JobExtra::Node(template), Some(tx_count), Some(value));
}

/// Chain, height, difficulty, network hashrate and version. Returns the chain name.
async fn refresh_info(ctx: &RunCtx, rpc: &RpcClient) -> Result<String, String> {
    let info = rpc
        .call("getblockchaininfo", json!([]))
        .await
        .map_err(|e| format!("getblockchaininfo: {e}"))?;
    let chain = info
        .get("chain")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    if info.get("initialblockdownload").and_then(Value::as_bool) == Some(true) {
        ctx.log(
            LogLevel::Warning,
            "The node is still in initial block download",
        );
    }
    let mining = rpc.call("getmininginfo", json!([])).await.ok();
    let network = rpc.call("getnetworkinfo", json!([])).await.ok();
    let subversion = network
        .as_ref()
        .and_then(|n| n.get("subversion"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut first = false;
    ctx.update(|st| {
        let net = &mut st.snap.network;
        net.chain = Some(chain.clone());
        if let Some(blocks) = info.get("blocks").and_then(Value::as_u64) {
            // The height being mined, like the Stratum side reports.
            net.height = Some(blocks + 1);
        }
        if let Some(d) = info.get("difficulty").and_then(Value::as_f64) {
            net.difficulty = d;
        }
        net.hashrate = mining
            .as_ref()
            .and_then(|m| m.get("networkhashps"))
            .and_then(Value::as_f64)
            .or(net.hashrate);
        let c = &mut st.snap.connection;
        first = !c.connected;
        if first {
            c.connected = true;
            c.connected_since = Some(SystemTime::now());
            c.last_error = None;
        }
        c.server = subversion.clone().or(c.server.take());
    });
    if first {
        ctx.log(
            LogLevel::Success,
            format!(
                "Connected to Bitcoin Core {} on {chain}",
                subversion.as_deref().unwrap_or("(unknown version)")
            ),
        );
    }
    Ok(chain)
}

async fn submit_block(ctx: &RunCtx, rpc: &RpcClient, sub: Submission) {
    let JobExtra::Node(template) = &sub.entry.extra else {
        return;
    };
    let coinbase = sub.entry.work.coinbase(&sub.found.extranonce);
    let block = template::assemble_block_hex(&sub.found.header, &coinbase, template);
    let mut hash = hansolo_core::sha::sha256d(&sub.found.header);
    hash.reverse();
    let hash = hex::encode(hash);
    ctx.log(
        LogLevel::Success,
        format!(
            "Submitting block {hash} at height {} ({} bytes)",
            template.height,
            block.len() / 2
        ),
    );

    let started = Instant::now();
    let mut result = rpc.call("submitblock", json!([block])).await;
    if let Err(RpcError::Transport(_)) = &result {
        // A block is worth one retry on a hiccup.
        tokio::time::sleep(Duration::from_millis(500)).await;
        result = rpc.call("submitblock", json!([block])).await;
    }
    let latency = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    ctx.update(|st| st.snap.connection.latency_ms = Some(latency));
    match result {
        Ok(Value::Null) => {
            ctx.update(|st| {
                st.snap.shares.submitted += 1;
                st.snap.shares.accepted += 1;
                st.snap.shares.blocks_found += 1;
                st.push_log(
                    LogLevel::Success,
                    format!(
                        "!!! BLOCK ACCEPTED BY THE NODE !!! {hash} at height {}",
                        template.height
                    ),
                );
            });
        }
        Ok(Value::String(reason)) if reason == "inconclusive" || reason == "duplicate" => {
            ctx.update(|st| {
                st.snap.shares.submitted += 1;
                st.push_log(
                    LogLevel::Warning,
                    format!("Node answered {reason:?} for block {hash}"),
                );
            });
        }
        Ok(other) => {
            let reason = other
                .as_str()
                .map_or_else(|| other.to_string(), str::to_string);
            ctx.block_rejected(
                sub.share_id,
                format!("Node rejected block {hash}: {reason}"),
            );
        }
        Err(e) => ctx.block_rejected(sub.share_id, format!("submitblock failed for {hash}: {e}")),
    }
}
