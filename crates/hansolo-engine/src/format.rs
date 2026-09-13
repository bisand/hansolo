//! Human-readable numbers, shared by the headless status line and the app.

use hansolo_core::MinerSnapshot;

const SI: [&str; 7] = ["", "k", "M", "G", "T", "P", "E"];

/// Three significant digits with an SI suffix: `1.23k`, `45.6M`, `789G`.
fn si(value: f64) -> (String, &'static str) {
    if !value.is_finite() || value <= 0.0 {
        return ("0".into(), "");
    }
    let mut v = value;
    let mut i = 0;
    while v >= 999.5 && i < SI.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    let digits = if v >= 99.95 {
        0
    } else if v >= 9.995 {
        1
    } else {
        2
    };
    (format!("{v:.digits$}"), SI[i])
}

/// `"123 H/s"`, `"4.56 MH/s"`, `"1.20 TH/s"`.
pub fn format_hashrate(hashes_per_second: f64) -> String {
    let (n, unit) = si(hashes_per_second);
    format!("{n} {unit}H/s")
}

/// `"512"`, `"1.23k"`, `"98.7T"`. Difficulties below 1 (common for CPU shares)
/// keep three significant digits: `"0.00123"`.
pub fn format_difficulty(difficulty: f64) -> String {
    if difficulty.is_finite() && difficulty > 0.0 && difficulty < 1.0 {
        let magnitude = difficulty.log10().floor() as i32; // -1 for 0.5, -3 for 0.00123
        let decimals = (2 - magnitude).clamp(0, 12) as usize;
        return format!("{difficulty:.decimals$}");
    }
    let (n, unit) = si(difficulty);
    format!("{n}{unit}")
}

/// One compact line for logs and headless runs:
/// `Mining | 12.3 MH/s | shares 5/1 | best 1.23k | height 840000`.
pub fn format_status_line(s: &MinerSnapshot) -> String {
    let status = match &s.status {
        hansolo_core::snapshot::MinerStatus::Error(e) => format!("Error: {e}"),
        other => other.label().to_string(),
    };
    let height = s
        .network
        .height
        .or(s.job.as_ref().and_then(|j| j.height))
        .map_or_else(|| "-".to_string(), |h| h.to_string());
    let mut line = format!(
        "{status} | {} | shares {}/{} | best {} | height {height}",
        format_hashrate(s.hashrate.current),
        s.shares.accepted,
        s.shares.rejected,
        format_difficulty(s.shares.best_difficulty),
    );
    if s.shares.blocks_found > 0 {
        line.push_str(&format!(" | BLOCKS {}", s.shares.blocks_found));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashrates() {
        assert_eq!(format_hashrate(0.0), "0 H/s");
        assert_eq!(format_hashrate(12.0), "12.0 H/s");
        assert_eq!(format_hashrate(1234.0), "1.23 kH/s");
        assert_eq!(format_hashrate(45_600_000.0), "45.6 MH/s");
        assert_eq!(format_hashrate(999_700.0), "1.00 MH/s");
        assert_eq!(format_hashrate(1.2e18), "1.20 EH/s");
        assert_eq!(format_hashrate(1.2e21), "1200 EH/s");
    }

    #[test]
    fn difficulties() {
        assert_eq!(format_difficulty(0.0), "0");
        assert_eq!(format_difficulty(0.5), "0.500");
        assert_eq!(format_difficulty(0.00123), "0.00123");
        assert_eq!(format_difficulty(512.0), "512");
        assert_eq!(format_difficulty(1234.0), "1.23k");
        assert_eq!(format_difficulty(98.7e12), "98.7T");
        assert_eq!(format_difficulty(1.5e15), "1.50P");
    }

    #[test]
    fn status_line() {
        let mut s = MinerSnapshot::default();
        s.hashrate.current = 12_300_000.0;
        s.shares.accepted = 5;
        s.shares.rejected = 1;
        s.shares.best_difficulty = 1234.0;
        s.network.height = Some(840_000);
        assert_eq!(
            format_status_line(&s),
            "Stopped | 12.3 MH/s | shares 5/1 | best 1.23k | height 840000"
        );
    }
}
