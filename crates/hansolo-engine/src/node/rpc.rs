//! JSON-RPC over HTTP/1.1, just enough for Bitcoin Core.
//!
//! Each call opens a connection, sends one POST with `Connection: close` and
//! reads to EOF. Calls are a few per poll interval, so keep-alive would buy
//! nothing and cost a lot of edge cases. The cookie file is re-read on every
//! call because Core rewrites it with a new password each time it restarts.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::net;

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RESPONSE: usize = 64 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RpcUrl {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl RpcUrl {
    /// `http://host:port/path`, or a bare `host[:port]` (default port 8332).
    pub(crate) fn parse(url: &str) -> Result<RpcUrl, String> {
        let url = url.trim();
        let rest = match url.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
            Some((scheme, _)) => {
                return Err(format!(
                    "unsupported RPC URL scheme {scheme:?}; Bitcoin Core RPC is plain http://"
                ));
            }
            None => url,
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') || authority.starts_with('[') => (
                h,
                p.parse::<u16>()
                    .ok()
                    .filter(|&p| p != 0)
                    .ok_or_else(|| format!("RPC URL {url:?} has an invalid port"))?,
            ),
            _ => (authority, 8332),
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return Err(format!("RPC URL {url:?} has no host"));
        }
        Ok(RpcUrl {
            host: host.to_string(),
            port,
            path: path.to_string(),
        })
    }
}

#[derive(Debug)]
pub(crate) enum RpcError {
    /// Could not talk to the node at all.
    Transport(String),
    /// HTTP-level failure without a JSON-RPC body (401, 403, 404…).
    Http(u16, String),
    /// The node answered with a JSON-RPC error.
    Rpc { code: i64, message: String },
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Transport(e) => write!(f, "{e}"),
            RpcError::Http(401, _) => write!(f, "HTTP 401: wrong RPC user/password or cookie"),
            RpcError::Http(code, body) => write!(f, "HTTP {code}: {body}"),
            RpcError::Rpc { code, message } => write!(f, "RPC error {code}: {message}"),
        }
    }
}

pub(crate) enum Auth {
    UserPass(String, String),
    Cookie(String),
}

pub(crate) struct RpcClient {
    url: RpcUrl,
    auth: Auth,
}

impl RpcClient {
    pub(crate) fn new(
        url: &str,
        user: &str,
        password: &str,
        cookie_file: Option<&str>,
    ) -> Result<Self, String> {
        let auth = match cookie_file.map(str::trim).filter(|c| !c.is_empty()) {
            Some(path) => Auth::Cookie(path.to_string()),
            None => Auth::UserPass(user.to_string(), password.to_string()),
        };
        Ok(RpcClient {
            url: RpcUrl::parse(url)?,
            auth,
        })
    }

    fn authorization(&self) -> Result<String, RpcError> {
        let credentials = match &self.auth {
            Auth::UserPass(u, p) => format!("{u}:{p}"),
            Auth::Cookie(path) => std::fs::read_to_string(path)
                .map(|s| s.trim().to_string())
                .map_err(|e| RpcError::Transport(format!("cannot read cookie file {path}: {e}")))?,
        };
        Ok(format!("Basic {}", base64(credentials.as_bytes())))
    }

    pub(crate) async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let body = json!({"jsonrpc": "1.0", "id": "hansolo", "method": method, "params": params})
            .to_string();
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\nUser-Agent: hansolo/0.1.0\r\n\r\n",
            path = self.url.path,
            host = self.url.host,
            port = self.url.port,
            auth = self.authorization()?,
            len = body.len(),
        );
        let exchange = async {
            let mut io = net::connect(
                &self.url.host,
                self.url.port,
                false,
                Duration::from_secs(10),
            )
            .await?;
            io.write_all(request.as_bytes()).await?;
            io.write_all(body.as_bytes()).await?;
            io.flush().await?;
            let mut response = Vec::new();
            (&mut io)
                .take(MAX_RESPONSE as u64)
                .read_to_end(&mut response)
                .await?;
            std::io::Result::Ok(response)
        };
        let response = tokio::time::timeout(TIMEOUT, exchange)
            .await
            .map_err(|_| RpcError::Transport(format!("{method}: timed out")))?
            .map_err(|e| {
                RpcError::Transport(format!("{}:{}: {e}", self.url.host, self.url.port))
            })?;
        let (status, body) = parse_http_response(&response).map_err(RpcError::Transport)?;

        // Core returns JSON-RPC errors with HTTP 500/404 and a JSON body.
        match serde_json::from_slice::<Value>(&body) {
            Ok(v) if v.get("error").is_some() || v.get("result").is_some() => {
                let error = &v["error"];
                if !error.is_null() {
                    return Err(RpcError::Rpc {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string(),
                    });
                }
                Ok(v.get("result").cloned().unwrap_or(Value::Null))
            }
            _ => Err(RpcError::Http(
                status,
                String::from_utf8_lossy(&body[..body.len().min(200)])
                    .trim()
                    .to_string(),
            )),
        }
    }
}

/// Splits a complete HTTP/1.1 response into status and (de-chunked) body.
pub(crate) fn parse_http_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("truncated HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or("bad HTTP status line")?;
    let mut chunked = false;
    let mut length = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            } else if name.eq_ignore_ascii_case("content-length") {
                length = value.parse::<usize>().ok();
            }
        }
    }
    if chunked {
        return Ok((status, dechunk(body)?));
    }
    let body = match length {
        Some(n) if n <= body.len() => body[..n].to_vec(),
        Some(_) => return Err("truncated HTTP body".into()),
        None => body.to_vec(),
    };
    Ok((status, body))
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("bad chunk")?;
        let size_text = String::from_utf8_lossy(&body[..eol]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "bad chunk size")?;
        body = &body[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            RpcUrl::parse("http://127.0.0.1:18443").unwrap(),
            RpcUrl {
                host: "127.0.0.1".into(),
                port: 18443,
                path: "/".into()
            }
        );
        assert_eq!(RpcUrl::parse("node.lan").unwrap().port, 8332);
        assert_eq!(
            RpcUrl::parse("http://node:8332/wallet/x").unwrap().path,
            "/wallet/x"
        );
        assert!(RpcUrl::parse("https://node:8332").is_err());
        assert!(RpcUrl::parse("http://node:notaport").is_err());
    }

    #[test]
    fn base64_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"__cookie__:abc"), "X19jb29raWVfXzphYmM=");
    }

    #[test]
    fn http_responses() {
        let (s, b) =
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}extra").unwrap();
        assert_eq!((s, b.as_slice()), (200, b"{}".as_slice()));
        let (s, b) =
            parse_http_response(b"HTTP/1.1 500 Internal\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n")
                .unwrap();
        assert_eq!((s, b.as_slice()), (500, b"abcde".as_slice()));
        assert!(parse_http_response(b"HTTP/1.1 200 OK\r\n").is_err());
    }
}
