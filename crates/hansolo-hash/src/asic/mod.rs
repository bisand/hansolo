//! ASIC miners: detection, and network miners as monitored devices.
//!
//! Two very different kinds of hardware turn up here:
//!
//! - **Network miners running AxeOS** (Bitaxe, NerdQAxe and friends). They run
//!   their own firmware and their own Stratum connection, so HanSolo cannot
//!   hand them work. [`AxeOsDevice`] polls `GET /api/system/info` and reports
//!   their hashrate, temperature and power as a device, so the dashboard shows
//!   the whole fleet. It never sends them anything.
//! - **USB sticks** (GekkoScience Compac F, NewPac, 2Pac). These are bare BM13xx
//!   chips behind a USB-serial bridge and need a host-side driver. Detection
//!   recognises the bridges; driving them is not implemented. See
//!   [`usb_driver`] for the extension point.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use hansolo_core::config::AsicConfig;
use hansolo_core::snapshot::AsicReport;
use hansolo_core::{Device, DeviceCtx, DeviceInfo, DeviceKind};

/// Per-request timeout when probing a network miner during detection.
const PROBE_TIMEOUT: Duration = Duration::from_millis(900);
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// What an AxeOS miner said about itself.
#[derive(Clone, Debug, Default)]
pub struct AxeOsStatus {
    pub hostname: Option<String>,
    pub asic_model: Option<String>,
    pub device_model: Option<String>,
    pub firmware: Option<String>,
    /// Gigahashes per second.
    pub hashrate_ghs: Option<f64>,
    pub temperature_c: Option<f64>,
    pub power_w: Option<f64>,
}

impl AxeOsStatus {
    fn from_json(v: &serde_json::Value) -> AxeOsStatus {
        let s = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .filter(|x| !x.is_empty())
        };
        let f = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_f64().or_else(|| x.as_str()?.parse().ok()))
        };
        AxeOsStatus {
            hostname: s("hostname"),
            asic_model: s("ASICModel"),
            device_model: s("deviceModel")
                .or_else(|| s("boardVersion").map(|b| format!("board {b}"))),
            firmware: s("version"),
            hashrate_ghs: f("hashRate"),
            temperature_c: f("temp"),
            power_w: f("power"),
        }
    }

    pub fn display_name(&self) -> String {
        let host = self
            .hostname
            .clone()
            .unwrap_or_else(|| "AxeOS miner".into());
        match (&self.device_model, &self.asic_model) {
            (Some(model), Some(asic)) => format!("{host} ({model}, {asic})"),
            (None, Some(asic)) => format!("{host} ({asic})"),
            (Some(model), None) => format!("{host} ({model})"),
            (None, None) => host,
        }
    }
}

/// A network miner that answered during detection.
#[derive(Clone, Debug)]
pub struct NetworkMiner {
    pub url: String,
    pub status: AxeOsStatus,
}

/// `host`, `host:port` or a full URL, as the API endpoint.
pub fn info_url(entry: &str) -> String {
    let entry = entry.trim().trim_end_matches('/');
    let base = if entry.starts_with("http://") || entry.starts_with("https://") {
        entry.to_string()
    } else {
        format!("http://{entry}")
    };
    format!("{base}/api/system/info")
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(true)
        .build()
        .into()
}

/// One `GET /api/system/info`.
pub fn fetch_status(agent: &ureq::Agent, url: &str) -> Result<AxeOsStatus, String> {
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("not AxeOS JSON: {e}"))?;
    if !json.is_object() {
        return Err("not AxeOS JSON: expected an object".into());
    }
    Ok(AxeOsStatus::from_json(&json))
}

/// Detects configured network miners (in parallel) and, when enabled, USB miners.
pub fn probe(config: &AsicConfig) -> (Vec<AsicReport>, Vec<NetworkMiner>) {
    if !config.enabled {
        return (Vec::new(), Vec::new());
    }
    let mut reports = Vec::new();
    let mut miners = Vec::new();

    let results: Vec<(String, Result<AxeOsStatus, String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = config
            .network_devices
            .iter()
            .map(|entry| {
                let url = info_url(entry);
                s.spawn(move || {
                    let result = fetch_status(&agent(PROBE_TIMEOUT), &url);
                    (url, result)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| (String::new(), Err("probe thread failed".into())))
            })
            .collect()
    });
    for (url, result) in results {
        let location = url.trim_end_matches("/api/system/info").to_string();
        match result {
            Ok(status) => {
                reports.push(AsicReport {
                    name: status.display_name(),
                    location,
                    supported: true,
                    note: "runs its own firmware; monitored, not driven".into(),
                });
                miners.push(NetworkMiner { url, status });
            }
            Err(e) => reports.push(AsicReport {
                name: "AxeOS miner (not responding)".into(),
                location,
                supported: false,
                note: e,
            }),
        }
    }

    if config.usb {
        reports.extend(usb_reports());
    }
    (reports, miners)
}

/// Known USB-serial bridges used by USB miners.
struct KnownUsb {
    vid: u16,
    pid: u16,
    name: &'static str,
    note: &'static str,
}

const KNOWN_USB: &[KnownUsb] = &[
    KnownUsb {
        vid: 0x10c4,
        pid: 0xea60,
        name: "GekkoScience Compac F / NewPac (CP210x bridge)",
        note: "Silicon Labs CP210x, as used by GekkoScience BM13xx sticks (also by many non-miners); \
               the BM13xx USB driver isn't implemented yet",
    },
    KnownUsb {
        vid: 0x0403,
        pid: 0x6015,
        name: "GekkoScience 2Pac / Compac (FT230X bridge)",
        note: "FTDI FT230X, as used by older GekkoScience BM1384 sticks; the USB driver isn't implemented yet",
    },
    KnownUsb {
        vid: 0x303a,
        pid: 0x1001,
        name: "ESP32-S3 over USB (Bitaxe or NerdMiner?)",
        note: "boards like Bitaxe mine on their own; add them by IP address as a network device to monitor them",
    },
];

fn usb_reports() -> Vec<AsicReport> {
    // serialport scans sysfs on Linux (no libudev) and panics if it's missing,
    // which would abort a release build; check first.
    #[cfg(target_os = "linux")]
    if !std::path::Path::new("/sys/class/tty").is_dir() {
        return Vec::new();
    }
    let Ok(ports) = serialport::available_ports() else {
        return Vec::new();
    };
    let mut reports = Vec::new();
    for port in ports {
        let serialport::SerialPortType::UsbPort(usb) = &port.port_type else {
            continue;
        };
        let Some(known) = KNOWN_USB
            .iter()
            .find(|k| k.vid == usb.vid && k.pid == usb.pid)
        else {
            continue;
        };
        let product = usb
            .product
            .as_deref()
            .map(|p| format!(" — {p}"))
            .unwrap_or_default();
        reports.push(AsicReport {
            name: format!("{}{product}", known.name),
            location: format!("usb:{}", port.port_name),
            supported: usb_driver(&port).is_some(),
            note: format!("{:04x}:{:04x}: {}", usb.vid, usb.pid, known.note),
        });
    }
    reports
}

/// Extension point for USB-attached BM13xx miners. Returns `None`: no driver yet.
///
/// A driver would, for a GekkoScience Compac F (BM1397) or NewPac (BM1387):
///
/// 1. Open the port at 115200 baud, pulse the reset line via RTS/DTR, and set
///    the chip's PLL/baud registers (the Compac F moves to a higher baud rate
///    after init), plus the voltage and frequency for the stick.
/// 2. Enumerate chips with the "read address" command and assign addresses.
/// 3. For each [`hansolo_core::Work`], build headers per extranonce roll as the
///    CPU device does, and send midstate jobs (`job_id`, `midstate`, `tail`,
///    `nbits`, `ntime`, starting nonce) framed with the BM13xx preamble and CRC.
///    The BM1397 hashes version-rolled midstates too; start with one.
/// 4. Read 9-byte nonce responses, map them back to their job id, verify with
///    `hansolo_core::sha::header_hash_from_midstate` and report through
///    [`crate::verify_candidate`]'s path. Count hashes as found nonces times
///    the chip's ticket difficulty times 2^32.
/// 5. Watch `ctx.work.generation()` and `ctx.stopping()` between reads, with a
///    read timeout of ~100 ms.
///
/// The returned device would use `DeviceKind::Asic` and a `usb:` location.
/// Deliberately not written without a stick to test against: init sequences
/// differ per chip and board, and a wrong voltage setting is not harmless.
pub fn usb_driver(_port: &serialport::SerialPortInfo) -> Option<Box<dyn Device>> {
    None
}

/// A network AxeOS miner as a monitored [`Device`].
pub struct AxeOsDevice {
    url: String,
    name: String,
    detail: String,
}

impl AxeOsDevice {
    pub fn new(miner: &NetworkMiner) -> AxeOsDevice {
        let s = &miner.status;
        let mut detail = format!(
            "AxeOS at {}",
            miner.url.trim_end_matches("/api/system/info")
        );
        if let Some(fw) = &s.firmware {
            detail.push_str(&format!(", firmware {fw}"));
        }
        AxeOsDevice {
            url: miner.url.clone(),
            name: s.display_name(),
            detail,
        }
    }
}

fn format_ghs(ghs: f64) -> String {
    if ghs >= 1000.0 {
        format!("{:.2} TH/s", ghs / 1000.0)
    } else {
        format!("{ghs:.1} GH/s")
    }
}

impl Device for AxeOsDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            name: self.name.clone(),
            kind: DeviceKind::Asic,
            backend: "AxeOS (monitored)".into(),
            detail: self.detail.clone(),
        }
    }

    fn run(self: Box<Self>, ctx: DeviceCtx) {
        let agent = agent(Duration::from_secs(3));
        let mut last_poll: Option<Instant> = None;
        let mut rate_hps = 0.0f64;
        let mut credited = Instant::now();
        let mut carry = 0.0f64;

        while !ctx.stopping() {
            if last_poll.is_none_or(|t| t.elapsed() >= POLL_INTERVAL) {
                last_poll = Some(Instant::now());
                // The request blocks for at most the agent timeout; stopping
                // can lag by that much, which is acceptable for a monitor.
                match fetch_status(&agent, &self.url) {
                    Ok(status) => {
                        rate_hps = status.hashrate_ghs.unwrap_or(0.0).max(0.0) * 1e9;
                        if let Some(t) = status.temperature_c {
                            ctx.stats
                                .temperature_mc
                                .store((t * 1000.0) as i64, Ordering::Relaxed);
                        }
                        if let Some(p) = status.power_w {
                            ctx.stats
                                .power_mw
                                .store((p.max(0.0) * 1000.0) as u64, Ordering::Relaxed);
                        }
                        let rate = status
                            .hashrate_ghs
                            .map(format_ghs)
                            .unwrap_or_else(|| "hashrate unknown".into());
                        ctx.stats
                            .set_status(format!("{rate}; mines its own pool work, not HanSolo's"));
                    }
                    Err(e) => {
                        rate_hps = 0.0;
                        ctx.stats.errors.fetch_add(1, Ordering::Relaxed);
                        ctx.stats.set_status(format!("unreachable: {e}"));
                    }
                }
            }

            // Credit estimated hashes continuously so the engine's rate (from
            // counter deltas) is smooth rather than a spike every poll.
            let dt = credited.elapsed().as_secs_f64();
            credited = Instant::now();
            carry += rate_hps * dt;
            let whole = carry.floor();
            if whole >= 1.0 {
                ctx.stats.add_hashes(whole as u64);
                carry -= whole;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        ctx.stats.set_status("stopped");
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use hansolo_core::{DeviceStats, WorkCell};

    use super::*;

    const SAMPLE: &str = r#"{"power":14.2,"voltage":5100,"temp":58.5,"vrTemp":49,"hashRate":1123.4,
        "hostname":"bitaxe","ASICModel":"BM1370","deviceModel":"Gamma","version":"v2.9.0"}"#;

    /// A one-endpoint HTTP server answering every request with `SAMPLE`.
    fn serve() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{SAMPLE}",
                    SAMPLE.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        addr.to_string()
    }

    #[test]
    fn urls() {
        assert_eq!(
            info_url("192.168.1.50"),
            "http://192.168.1.50/api/system/info"
        );
        assert_eq!(
            info_url("bitaxe.local:8080/"),
            "http://bitaxe.local:8080/api/system/info"
        );
        assert_eq!(info_url("http://x"), "http://x/api/system/info");
    }

    #[test]
    fn probes_and_monitors_axeos() {
        let host = serve();
        let config = AsicConfig {
            enabled: true,
            usb: false,
            network_devices: vec![host.clone(), "127.0.0.1:1".into()],
        };
        let started = Instant::now();
        let (reports, miners) = probe(&config);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(reports.len(), 2);
        assert_eq!(miners.len(), 1);
        assert_eq!(reports[0].name, "bitaxe (Gamma, BM1370)");
        assert!(reports[0].supported);
        assert!(!reports[1].supported);
        assert_eq!(miners[0].status.hashrate_ghs, Some(1123.4));

        let (tx, _rx) = crossbeam_channel::unbounded();
        let stats = Arc::new(DeviceStats::default());
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = DeviceCtx {
            index: 0,
            work: Arc::new(WorkCell::new()),
            found: tx,
            stats: stats.clone(),
            stop: stop.clone(),
        };
        let device = Box::new(AxeOsDevice::new(&miners[0]));
        assert_eq!(device.info().kind, DeviceKind::Asic);
        let handle = std::thread::spawn(move || device.run(ctx));
        std::thread::sleep(Duration::from_millis(600));
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
        assert_eq!(stats.temperature_c(), Some(58.5));
        assert_eq!(stats.power_mw.load(Ordering::Relaxed), 14_200);
        // ~0.6 s at 1.1234 TH/s.
        let hashes = stats.hashes.load(Ordering::Relaxed) as f64;
        assert!(
            hashes > 0.3 * 1.1234e12 && hashes < 0.8 * 1.1234e12,
            "{hashes}"
        );
    }
}
