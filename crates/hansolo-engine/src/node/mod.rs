//! Bitcoin Core: true solo mining against your own node.
//!
//! - [`rpc`] — a minimal JSON-RPC-over-HTTP/1.1 client (Bitcoin Core only speaks
//!   plain HTTP, so no TLS or HTTP library is needed).
//! - [`template`] — `getblocktemplate` parsing, coinbase construction, merkle
//!   branch and block assembly. Pure functions, tested against the `bitcoin`
//!   crate's own validation.
//! - [`client`] — the polling loop and `submitblock`.

pub(crate) mod client;
pub(crate) mod rpc;
pub mod template;
