//! Numbers as people read them.

use std::time::{Duration, SystemTime};

const SI: [&str; 9] = ["", "k", "M", "G", "T", "P", "E", "Z", "Y"];

/// `412.3 MH/s`.
pub fn hashrate(hps: f64) -> String {
    format!("{}H/s", si(hps))
}

/// `1.24 M`, `126.7 T`, `0.0012`.
pub fn difficulty(d: f64) -> String {
    if d > 0.0 && d < 1.0 {
        return format!("{d:.4}");
    }
    si(d).trim_end().to_string()
}

/// A value with an SI prefix and a trailing space when there is no prefix, so
/// units line up: `412.3 M`, `12.0 `.
pub fn si(value: f64) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "0 ".into();
    }
    let mut v = value;
    let mut i = 0;
    while v >= 1000.0 && i < SI.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    let digits = if v >= 100.0 {
        0
    } else if v >= 10.0 {
        1
    } else {
        2
    };
    format!("{v:.digits$} {}", SI[i])
}

/// `3d 04h`, `12m 05s`, `42s`.
pub fn duration(d: Duration) -> String {
    let s = d.as_secs();
    let (days, hours, minutes, seconds) = (s / 86_400, s / 3600 % 24, s / 60 % 60, s % 60);
    if days > 0 {
        format!("{days}d {hours:02}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// `12s ago`, or `never`.
pub fn ago(at: Option<SystemTime>) -> String {
    match at.and_then(|t| SystemTime::now().duration_since(t).ok()) {
        Some(d) => format!("{} ago", duration(d)),
        None => "never".into(),
    }
}

/// Seconds as a span a person can picture: `41 minutes`, `5,812 years`,
/// `3.1 million years`.
pub fn long_span(seconds: f64) -> String {
    const YEAR: f64 = 365.25 * 86_400.0;
    if !seconds.is_finite() {
        return "forever".into();
    }
    let (value, unit) = if seconds < 120.0 {
        (seconds, "seconds")
    } else if seconds < 7_200.0 {
        (seconds / 60.0, "minutes")
    } else if seconds < 172_800.0 {
        (seconds / 3600.0, "hours")
    } else if seconds < 2.0 * YEAR {
        (seconds / 86_400.0, "days")
    } else {
        (seconds / YEAR, "years")
    };
    format!("{} {unit}", grouped(value))
}

/// `1 in 2.4 million`, for a probability.
pub fn odds(probability: f64) -> String {
    if probability <= 0.0 || !probability.is_finite() {
        return "—".into();
    }
    if probability >= 0.5 {
        return format!("{:.0}%", probability * 100.0);
    }
    format!("1 in {}", grouped(1.0 / probability))
}

/// Large counts in words past a million, thousands separators below.
pub fn grouped(value: f64) -> String {
    const WORDS: [(f64, &str); 5] = [
        (1e18, "quintillion"),
        (1e15, "quadrillion"),
        (1e12, "trillion"),
        (1e9, "billion"),
        (1e6, "million"),
    ];
    for (scale, word) in WORDS {
        if value >= scale {
            return format!("{:.1} {word}", value / scale);
        }
    }
    let whole = value.round() as u64;
    let digits = whole.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `3.125 BTC` from satoshis.
pub fn btc(sats: u64) -> String {
    let whole = sats / 100_000_000;
    let frac = sats % 100_000_000;
    let text = format!("{whole}.{frac:08}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text} BTC")
}

/// Local wall-clock `HH:MM:SS` without a time-zone database: UTC offset is not
/// knowable portably from std, so this prints UTC and says so once in the UI.
pub fn clock(at: SystemTime) -> String {
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        assert_eq!(hashrate(412_300_000.0), "412 MH/s");
        assert_eq!(hashrate(12_340.0), "12.3 kH/s");
        assert_eq!(difficulty(126.7e12), "127 T");
        assert_eq!(difficulty(0.5), "0.5000");
    }

    #[test]
    fn words() {
        assert_eq!(grouped(12_345.0), "12,345");
        assert_eq!(grouped(2_400_000.0), "2.4 million");
        assert_eq!(odds(1.0 / 2_400_000.0), "1 in 2.4 million");
        assert_eq!(btc(312_500_000), "3.125 BTC");
    }
}
