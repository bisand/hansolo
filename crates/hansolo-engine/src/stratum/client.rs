//! The Stratum connection loop.
//!
//! One task owns the socket. It interleaves three event sources with
//! `select!`: lines from the pool, verified solutions from the found thread,
//! and an inactivity deadline. Pool requests are matched to responses by id in
//! `pending`, which is also how submit latency is measured.
//!
//! On any failure the session ends, devices are idled (their work would use a
//! stale extranonce1), and the loop reconnects with exponential backoff. The
//! backoff resets once a session has produced work, so a pool that drops us
//! every few hours reconnects immediately while a dead pool is not hammered.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use hansolo_core::Target;
use hansolo_core::snapshot::{LogLevel, MinerStatus, ShareResult};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use super::protocol::{self, Notify, StratumEndpoint, Verdict};
use crate::net;
use crate::run::{JobExtra, RunCtx, Submission, difficulty_for_interval};

/// Reconnect when nothing (no notify, no response) arrives for this long.
const INACTIVITY: Duration = Duration::from_secs(150);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Longest line accepted from a pool; a notify for a big template is ~10 KiB.
const MAX_LINE: usize = 1 << 20;
/// Aim for a pool share about this often when suggesting a difficulty.
const SHARE_INTERVAL_SECS: f64 = 15.0;

/// Distinguishes connections, so a solution for the previous connection's
/// extranonce1 is never submitted on the next one.
static SESSIONS: AtomicU64 = AtomicU64::new(1);

enum End {
    Lost {
        error: String,
        had_work: bool,
    },
    Redirect {
        endpoint: StratumEndpoint,
        wait: Duration,
    },
}

enum Pending {
    Subscribe,
    Authorize,
    SuggestDifficulty,
    Submit {
        share_id: u64,
        sent: Instant,
        is_block: bool,
    },
}

pub(crate) async fn run(
    ctx: Arc<RunCtx>,
    url: String,
    password: String,
    mut submissions: mpsc::UnboundedReceiver<Submission>,
) {
    let Ok(mut endpoint) = protocol::parse_stratum_url(&url) else {
        return ctx.fail(format!("invalid pool URL {url:?}"));
    };
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
        ctx.log(
            LogLevel::Info,
            format!(
                "Connecting to {}:{}{}",
                endpoint.host,
                endpoint.port,
                if endpoint.tls { " (TLS)" } else { "" }
            ),
        );
        let end = session(&ctx, &endpoint, &password, &mut submissions).await;
        ctx.clear_work();
        match end {
            End::Lost { error, had_work } => {
                backoff = if had_work {
                    Duration::from_secs(1)
                } else {
                    (backoff * 2).min(MAX_BACKOFF)
                };
                ctx.update(|st| {
                    st.push_log(
                        LogLevel::Warning,
                        format!(
                            "Pool connection lost: {error}; retrying in {}s",
                            backoff.as_secs()
                        ),
                    );
                    st.snap.connection.connected = false;
                    st.snap.connection.last_error = Some(error);
                });
            }
            End::Redirect { endpoint: to, wait } => {
                ctx.log(
                    LogLevel::Info,
                    format!(
                        "Pool asked to reconnect to {}:{} in {}s",
                        to.host,
                        to.port,
                        wait.as_secs()
                    ),
                );
                ctx.update(|st| st.snap.connection.connected = false);
                endpoint = to;
                backoff = wait.max(Duration::from_millis(100));
            }
        }
    }
}

struct Session<'a> {
    ctx: &'a RunCtx,
    id: u64,
    user: String,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    extranonce1: Option<Vec<u8>>,
    extranonce2_hint: Option<usize>,
    difficulty: f64,
    last_notify: Option<Notify>,
    had_work: bool,
    suggested: bool,
    out: Vec<Value>,
}

async fn session(
    ctx: &RunCtx,
    endpoint: &StratumEndpoint,
    password: &str,
    submissions: &mut mpsc::UnboundedReceiver<Submission>,
) -> End {
    let io = match net::connect(&endpoint.host, endpoint.port, endpoint.tls, CONNECT_TIMEOUT).await
    {
        Ok(io) => io,
        Err(e) => {
            return End::Lost {
                error: format!("connect: {e}"),
                had_work: false,
            };
        }
    };
    let (reader, mut writer) = tokio::io::split(io);
    let mut reader = BufReader::new(reader);

    let mut s = Session {
        ctx,
        id: SESSIONS.fetch_add(1, Ordering::Relaxed),
        user: ctx.user.clone(),
        next_id: 1,
        pending: HashMap::new(),
        extranonce1: None,
        extranonce2_hint: None,
        difficulty: 1.0,
        last_notify: None,
        had_work: false,
        suggested: false,
        out: Vec::new(),
    };
    ctx.update(|st| {
        let c = &mut st.snap.connection;
        c.connected = true;
        c.connected_since = Some(SystemTime::now());
        c.last_error = None;
    });

    let subscribe_id = s.request(Pending::Subscribe);
    s.out.push(protocol::subscribe(subscribe_id));
    let authorize_id = s.request(Pending::Authorize);
    s.out
        .push(protocol::authorize(authorize_id, &s.user, password));
    let subscribe_sent = Instant::now();

    let mut deadline = tokio::time::Instant::now() + INACTIVITY;
    let mut line = Vec::new();
    loop {
        if let Err(e) = flush(&mut writer, &mut s.out).await {
            return s.lost(format!("write: {e}"));
        }
        tokio::select! {
            read = read_line(&mut reader, &mut line) => {
                match read {
                    Ok(false) => return s.lost("closed by pool".into()),
                    Ok(true) => {}
                    Err(e) => return s.lost(format!("read: {e}")),
                }
                deadline = tokio::time::Instant::now() + INACTIVITY;
                let text = String::from_utf8_lossy(&line).trim().to_string();
                line.clear();
                if text.is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        ctx.log(LogLevel::Warning, format!("Unparseable pool message ({e}): {}", truncate(&text, 200)));
                        continue;
                    }
                };
                if let Some(end) = s.handle(msg, subscribe_sent) {
                    let _ = flush(&mut writer, &mut s.out).await;
                    return end;
                }
            }
            Some(sub) = submissions.recv() => s.submit(sub),
            () = tokio::time::sleep_until(deadline) => {
                return s.lost(format!("no messages for {}s", INACTIVITY.as_secs()));
            }
        }
    }
}

async fn flush<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    out: &mut Vec<Value>,
) -> std::io::Result<()> {
    if out.is_empty() {
        return Ok(());
    }
    let mut buf = String::new();
    for msg in out.drain(..) {
        buf.push_str(&msg.to_string());
        buf.push('\n');
    }
    writer.write_all(buf.as_bytes()).await?;
    writer.flush().await
}

/// Appends to `line` until a `\n` arrives (`Ok(true)`) or the pool closes the
/// connection (`Ok(false)`). Cancel-safe: bytes are moved into `line` as soon as
/// they are read, so a `select!` branch winning mid-line loses nothing; the
/// caller clears `line` after handling it.
async fn read_line<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<bool> {
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Ok(false);
        }
        let (take, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        line.extend_from_slice(&buf[..take]);
        reader.consume(take);
        if done {
            return Ok(true);
        }
        if line.len() > MAX_LINE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line too long",
            ));
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

impl Session<'_> {
    fn request(&mut self, kind: Pending) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, kind);
        id
    }

    fn lost(&self, error: String) -> End {
        End::Lost {
            error,
            had_work: self.had_work,
        }
    }

    /// Handles one message from the pool. `Some` ends the session.
    fn handle(&mut self, msg: Value, subscribe_sent: Instant) -> Option<End> {
        let ctx = self.ctx;
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        if let Some(method) = msg.get("method").and_then(Value::as_str) {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            return self.handle_method(method, &params, id);
        }

        let pending = id.as_u64().and_then(|id| self.pending.remove(&id))?;
        let result = msg.get("result").cloned().unwrap_or(Value::Null);
        let error = msg.get("error").cloned().unwrap_or(Value::Null);
        match pending {
            Pending::Subscribe => {
                let latency = subscribe_sent.elapsed().as_millis().min(u32::MAX as u128) as u32;
                ctx.update(|st| st.snap.connection.latency_ms = Some(latency));
                if !error.is_null() {
                    return Some(self.lost(format!("subscribe failed: {error}")));
                }
                // [[subscriptions], extranonce1, extranonce2_size?]
                match result.get(1).and_then(Value::as_str).map(hex::decode) {
                    Some(Ok(e1)) => self.extranonce1 = Some(e1),
                    _ => {
                        ctx.log(LogLevel::Warning, "Pool sent no extranonce1; using none");
                        self.extranonce1 = Some(Vec::new());
                    }
                }
                self.extranonce2_hint = result.get(2).and_then(Value::as_u64).map(|n| n as usize);
                self.republish();
            }
            Pending::Authorize => {
                if result.as_bool() == Some(true) && error.is_null() {
                    ctx.log(LogLevel::Success, format!("Authorized as {}", self.user));
                } else {
                    let why = if error.is_null() {
                        "refused".to_string()
                    } else {
                        error.to_string()
                    };
                    ctx.update(|st| {
                        st.push_log(
                            LogLevel::Error,
                            format!("Pool did not authorize {}: {why}", self.user),
                        );
                        st.snap.connection.last_error = Some(format!("authorization: {why}"));
                    });
                }
                self.maybe_suggest_difficulty();
            }
            Pending::SuggestDifficulty => {
                if !error.is_null() {
                    ctx.log(
                        LogLevel::Debug,
                        format!("suggest_difficulty not accepted: {error}"),
                    );
                }
            }
            Pending::Submit {
                share_id,
                sent,
                is_block,
            } => {
                let latency = sent.elapsed().as_millis().min(u32::MAX as u128) as u32;
                let verdict = protocol::submit_verdict(&result, &error);
                ctx.share_result(
                    share_id,
                    match &verdict {
                        Verdict::Accepted => ShareResult::Accepted,
                        Verdict::Stale(_) => ShareResult::Stale,
                        Verdict::Rejected(_) => ShareResult::Rejected,
                    },
                );
                ctx.update(|st| {
                    st.snap.connection.latency_ms = Some(latency);
                    let shares = &mut st.snap.shares;
                    match &verdict {
                        Verdict::Accepted => {
                            shares.accepted += 1;
                            if is_block {
                                shares.blocks_found += 1;
                                st.push_log(
                                    LogLevel::Success,
                                    "!!! The pool accepted a BLOCK solution !!! It builds and broadcasts the block.".into(),
                                );
                            }
                        }
                        Verdict::Stale(why) => {
                            shares.stale += 1;
                            st.push_log(LogLevel::Warning, format!("Share stale: {why}"));
                        }
                        Verdict::Rejected(why) => {
                            shares.rejected += 1;
                            st.push_log(LogLevel::Warning, format!("Share rejected: {why}"));
                        }
                    }
                });
            }
        }
        None
    }

    fn handle_method(&mut self, method: &str, params: &Value, id: Value) -> Option<End> {
        let ctx = self.ctx;
        match method {
            "mining.notify" => match protocol::parse_notify(params) {
                Ok(notify) => {
                    let new_block = self
                        .last_notify
                        .as_ref()
                        .is_none_or(|n| n.prev_hash != notify.prev_hash);
                    let clean = notify.clean_jobs || new_block;
                    if new_block && self.last_notify.is_some() {
                        let height = protocol::coinbase_height(&notify.coinb1);
                        ctx.log(
                            LogLevel::Info,
                            format!(
                                "New block; now mining height {}",
                                height.map_or_else(|| "?".into(), |h| h.to_string())
                            ),
                        );
                    }
                    self.last_notify = Some(notify);
                    return self.publish(clean);
                }
                Err(e) => ctx.log(
                    LogLevel::Warning,
                    format!("Ignoring bad mining.notify: {e}"),
                ),
            },
            "mining.set_difficulty" => {
                let d = params
                    .get(0)
                    .and_then(Value::as_f64)
                    .or_else(|| params.as_f64());
                match d {
                    Some(d) if d > 0.0 && d.is_finite() => {
                        self.difficulty = d;
                        ctx.update(|st| {
                            st.snap.connection.share_difficulty = d;
                            st.push_log(
                                LogLevel::Info,
                                format!("Pool difficulty set to {}", crate::format_difficulty(d)),
                            );
                        });
                        // Keep the job, change what devices report.
                        return self.publish(false);
                    }
                    _ => ctx.log(
                        LogLevel::Warning,
                        format!("Ignoring bad set_difficulty {params}"),
                    ),
                }
            }
            "mining.set_extranonce" => {
                if let Some(Ok(e1)) = params.get(0).and_then(Value::as_str).map(hex::decode) {
                    self.extranonce1 = Some(e1);
                    self.extranonce2_hint =
                        params.get(1).and_then(Value::as_u64).map(|n| n as usize);
                    return self.publish(true);
                }
            }
            "client.reconnect" => {
                let host = params
                    .get(0)
                    .and_then(Value::as_str)
                    .filter(|h| !h.is_empty())
                    .map(str::to_string);
                let port = params
                    .get(1)
                    .and_then(|p| {
                        p.as_u64()
                            .or_else(|| p.as_str().and_then(|s| s.parse().ok()))
                    })
                    .and_then(|p| u16::try_from(p).ok());
                let wait = params
                    .get(2)
                    .and_then(|w| {
                        w.as_u64()
                            .or_else(|| w.as_str().and_then(|s| s.parse().ok()))
                    })
                    .unwrap_or(0)
                    .min(600);
                // Only honour a host change within the same domain: a
                // hijacked pool must not redirect our hashrate elsewhere.
                let current = ctx.config.source.clone();
                let hansolo_core::WorkSource::Stratum { url, .. } = current else {
                    return None;
                };
                let mut endpoint = protocol::parse_stratum_url(&url).ok()?;
                if let Some(host) = host
                    && host != endpoint.host
                {
                    if same_site(&host, &endpoint.host) {
                        endpoint.host = host;
                    } else {
                        ctx.log(
                            LogLevel::Warning,
                            format!(
                                "Ignoring redirect to foreign host {host}; reconnecting to {}",
                                endpoint.host
                            ),
                        );
                    }
                }
                if let Some(port) = port {
                    endpoint.port = port;
                }
                return Some(End::Redirect {
                    endpoint,
                    wait: Duration::from_secs(wait),
                });
            }
            "client.get_version" => {
                self.out
                    .push(json!({"id": id, "result": protocol::USER_AGENT, "error": null}));
            }
            "client.show_message" => {
                let text = params.get(0).and_then(Value::as_str).unwrap_or_default();
                ctx.log(
                    LogLevel::Info,
                    format!("Pool says: {}", truncate(text, 500)),
                );
            }
            "mining.ping" | "client.ping" => {
                self.out
                    .push(json!({"id": id, "result": "pong", "error": null}));
            }
            other => {
                if !id.is_null() {
                    self.out.push(json!({"id": id, "result": null, "error": [20, "unsupported method", null]}));
                }
                ctx.log(LogLevel::Debug, format!("Unhandled pool method {other}"));
            }
        }
        None
    }

    fn maybe_suggest_difficulty(&mut self) {
        if self.suggested {
            return;
        }
        let d = difficulty_for_interval(self.ctx.hashrate_estimate(), SHARE_INTERVAL_SECS);
        if d > 0.0 {
            self.suggested = true;
            let id = self.request(Pending::SuggestDifficulty);
            self.out.push(protocol::suggest_difficulty(id, d));
            self.ctx.log(
                LogLevel::Debug,
                format!("Suggested difficulty {}", crate::format_difficulty(d)),
            );
        }
    }

    /// Re-publishes the last notify (after subscribe completes).
    fn republish(&mut self) {
        if self.last_notify.is_some()
            && let Some(End::Lost { error, .. }) = self.publish(true)
        {
            self.ctx.log(LogLevel::Error, error);
        }
    }

    /// Turns the last notify into work for the devices.
    fn publish(&mut self, clean: bool) -> Option<End> {
        let ctx = self.ctx;
        let (Some(notify), Some(e1)) = (&self.last_notify, &self.extranonce1) else {
            return None; // subscribe response not in yet; republished when it is
        };
        let Some(size) =
            protocol::extranonce2_size(&notify.coinb1, e1, &notify.coinb2, self.extranonce2_hint)
                .or(self.extranonce2_hint)
        else {
            ctx.log(
                LogLevel::Warning,
                "Could not parse the pool's coinbase; assuming an 8-byte extranonce2",
            );
            return self.publish_sized(clean, hansolo_core::work::EXTRANONCE_LEN);
        };
        self.publish_sized(clean, size)
    }

    fn publish_sized(&mut self, clean: bool, size: usize) -> Option<End> {
        let ctx = self.ctx;
        let (notify, e1) = (self.last_notify.as_ref()?, self.extranonce1.as_ref()?);
        let (work, pad) = match protocol::build_work(
            ctx.next_work_id(),
            notify,
            e1,
            size,
            self.difficulty,
            clean,
        ) {
            Ok(w) => w,
            Err(e) => {
                ctx.fail(format!("Pool incompatible: {e}"));
                return Some(self.lost(e));
            }
        };
        let network = work.network_target();
        let height = work.height;
        // Stratum says nothing about the network; derive what the job implies.
        ctx.update(|st| {
            let net = &mut st.snap.network;
            net.difficulty = network.difficulty();
            net.hashrate =
                Some(net.difficulty * hansolo_core::target::HASHES_PER_DIFFICULTY / 600.0);
            if height.is_some() {
                net.height = height;
            }
        });
        if !self.had_work {
            ctx.log(
                LogLevel::Success,
                format!(
                    "Mining job {} at height {} (share difficulty {})",
                    work.job_id,
                    height.map_or_else(|| "?".into(), |h| h.to_string()),
                    crate::format_difficulty(Target::difficulty(&work.share_target))
                ),
            );
        }
        self.had_work = true;
        ctx.publish(
            work,
            JobExtra::Stratum {
                session: self.id,
                pad,
            },
            None,
            None,
        );
        None
    }

    fn submit(&mut self, sub: Submission) {
        let ctx = self.ctx;
        let JobExtra::Stratum { session, pad } = sub.entry.extra else {
            return;
        };
        if session != self.id {
            // Built on a previous connection's extranonce1; the pool cannot use it.
            ctx.share_result(sub.share_id, ShareResult::Stale);
            ctx.update(|st| st.snap.shares.stale += 1);
            return;
        }
        let mut extranonce2 = vec![0u8; pad];
        extranonce2.extend_from_slice(&sub.found.extranonce);
        let ntime = u32::from_le_bytes(sub.found.header[68..72].try_into().expect("4 bytes"));
        let id = self.request(Pending::Submit {
            share_id: sub.share_id,
            sent: Instant::now(),
            is_block: sub.is_block,
        });
        self.out.push(protocol::submit(
            id,
            &self.user,
            &sub.entry.work.job_id,
            &extranonce2,
            ntime,
            sub.found.nonce(),
        ));
        ctx.update(|st| st.snap.shares.submitted += 1);
    }
}

/// Whether two hosts share a registrable-looking suffix (last two labels), or
/// are identical IPs. Deliberately conservative.
fn same_site(a: &str, b: &str) -> bool {
    let tail = |h: &str| {
        let labels: Vec<&str> = h.trim_end_matches('.').rsplit('.').take(2).collect();
        labels.join(".").to_ascii_lowercase()
    };
    let is_ip = |h: &str| h.parse::<std::net::IpAddr>().is_ok();
    if is_ip(a) || is_ip(b) {
        return a == b;
    }
    tail(a) == tail(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_sites() {
        assert!(same_site("eu.public-pool.io", "public-pool.io"));
        assert!(!same_site("evil.example", "public-pool.io"));
        assert!(!same_site("10.0.0.1", "10.0.0.2"));
    }
}
