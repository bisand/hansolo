//! The contract every other HanSolo crate is written against.
//!
//! - [`sha`] — portable SHA-256, midstates and double hashing. The reference that
//!   every accelerated backend in `hansolo-hash` is tested against.
//! - [`target`] — compact bits, 256-bit targets and difficulty.
//! - [`work`] — a unit of work as the engine hands it to devices, and what a
//!   device hands back when it finds something.
//! - [`device`] — the trait a hashing device implements, and its live counters.
//! - [`config`] — what the user configures.
//! - [`snapshot`] — everything the UI shows, as plain data.

pub mod config;
pub mod device;
pub mod sha;
pub mod snapshot;
pub mod target;
pub mod work;

pub use config::{Config, ThemePreference, WorkSource};
pub use device::{Device, DeviceCtx, DeviceInfo, DeviceKind, DeviceStats};
pub use snapshot::MinerSnapshot;
pub use target::Target;
pub use work::{Found, Work, WorkCell};
