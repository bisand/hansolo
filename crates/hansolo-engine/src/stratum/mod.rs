//! Stratum v1: mining through a (solo) pool.
//!
//! [`protocol`] holds the message formats and the notify → [`Work`](hansolo_core::Work)
//! conversion as pure functions; [`client`] runs the connection.

pub(crate) mod client;
pub mod protocol;
