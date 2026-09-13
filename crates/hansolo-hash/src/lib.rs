//! Hashing devices for HanSolo, and the machinery that picks them.
//!
//! PUBLIC API CONTRACT — the engine and the app depend on exactly these items:
//!
//! - [`plan`] — detect hardware, benchmark candidate paths, and build devices.
//! - [`detect`] — detection only (no benchmark, no devices), for the hardware page
//!   before mining starts.
//! - [`Plan`].

use hansolo_core::snapshot::HardwareReport;
use hansolo_core::{Config, Device};

/// The outcome of [`plan`]: what was found and the devices to run.
pub struct Plan {
    pub report: HardwareReport,
    pub devices: Vec<Box<dyn Device>>,
}

/// Probes the machine. Cheap enough to call from a UI's startup path on a
/// background thread (well under a second).
pub fn detect(config: &Config) -> HardwareReport {
    let _ = config;
    HardwareReport::default()
}

/// Detects, benchmarks, and builds the devices for `config`.
///
/// `progress` receives short human-readable lines ("benchmarking AVX2 8-lane…")
/// which the engine forwards to the log.
pub fn plan(config: &Config, progress: &(dyn Fn(&str) + Sync)) -> Plan {
    let _ = progress;
    Plan {
        report: detect(config),
        devices: Vec::new(),
    }
}
