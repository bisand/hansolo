//! No display at all: a server, a container, a board over SSH.
//!
//! Mines with the saved configuration and prints a status line every ten
//! seconds, plus every new log entry as it happens. Stop it with Ctrl-C.

use std::time::{Duration, SystemTime};

use hansolo_core::Config;
use hansolo_core::snapshot::LogLevel;
use hansolo_engine::Miner;

use crate::store::Store;

pub fn run(miner: Miner, config: Config, store: &Store) -> Result<(), Box<dyn std::error::Error>> {
    if config.payout_address.trim().is_empty() {
        return Err(format!(
            "no payout address configured. Set `payout_address` in {} (or run with a display once).",
            store.config_path().display()
        )
        .into());
    }
    let state = store.load_state();
    miner.set_best_ever(state.best_ever_difficulty);
    miner.start(config)?;

    let mut printed_until: Option<SystemTime> = None;
    let mut last_status = std::time::Instant::now() - Duration::from_secs(60);
    let mut best = state.best_ever_difficulty;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let snapshot = miner.snapshot();
        let since = printed_until;
        for entry in snapshot.log.iter().filter(|e| since.is_none_or(|t| e.at > t)) {
            let level = match entry.level {
                LogLevel::Debug => "debug",
                LogLevel::Info => "info ",
                LogLevel::Success => "ok   ",
                LogLevel::Warning => "warn ",
                LogLevel::Error => "error",
            };
            eprintln!("{} {level} {}", crate::format::clock(entry.at), entry.message);
            printed_until = Some(entry.at);
        }
        if last_status.elapsed() >= Duration::from_secs(10) {
            last_status = std::time::Instant::now();
            eprintln!("{}", hansolo_engine::format_status_line(&snapshot));
        }
        let session_best = snapshot.shares.best_difficulty;
        if session_best > best {
            best = session_best;
            let mut state = store.load_state();
            state.best_ever_difficulty = best;
            let _ = store.save_state(&state);
        }
    }
}
