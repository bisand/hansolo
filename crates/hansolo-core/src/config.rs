//! What the user configures. Persisted as TOML by the application.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    /// Follow the operating system; dark when it cannot be told.
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WorkSource {
    /// A solo pool speaking Stratum v1, e.g. `stratum+tcp://public-pool.io:21496`.
    Stratum {
        url: String,
        /// Defaults to `<payout_address>.<worker_name>`.
        #[serde(default)]
        username: Option<String>,
        #[serde(default = "default_password")]
        password: String,
    },
    /// Your own Bitcoin Core node, via `getblocktemplate` and `submitblock`.
    Node {
        rpc_url: String,
        #[serde(default)]
        rpc_user: String,
        #[serde(default)]
        rpc_password: String,
        /// `.cookie` file; used instead of user/password when set.
        #[serde(default)]
        cookie_file: Option<String>,
        #[serde(default = "default_poll")]
        poll_secs: u64,
    },
}

fn default_password() -> String {
    "x".into()
}

fn default_poll() -> u64 {
    5
}

impl Default for WorkSource {
    fn default() -> Self {
        WorkSource::Stratum {
            url: "stratum+tcp://public-pool.io:21496".into(),
            username: None,
            password: default_password(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuConfig {
    pub enabled: bool,
    /// `None` uses every logical core.
    pub threads: Option<usize>,
    /// Force a backend by name (`"scalar"`, `"sha-ni"`, `"armv8-sha2"`, `"avx2"`, …).
    /// `None` benchmarks and picks the fastest.
    pub backend: Option<String>,
    /// Run hashing threads at the lowest scheduling priority.
    pub low_priority: bool,
}

impl Default for CpuConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threads: None,
            backend: None,
            low_priority: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuConfig {
    pub enabled: bool,
    /// 1 (gentle, desktop stays responsive) to 10 (flat out).
    pub intensity: u8,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            intensity: 6,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AsicConfig {
    pub enabled: bool,
    /// Probe USB serial ports for known miners.
    pub usb: bool,
    /// Network miners running AxeOS (Bitaxe, NerdQAxe…), as `host` or `host:port`.
    pub network_devices: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: ThemePreference,
    /// Overrides the display's scale factor.
    pub scale: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where a found block pays. Required before mining starts.
    pub payout_address: String,
    pub worker_name: String,
    pub source: WorkSource,
    pub cpu: CpuConfig,
    pub gpu: GpuConfig,
    pub asic: AsicConfig,
    pub ui: UiConfig,
    /// Start mining as soon as the application opens.
    pub autostart: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            payout_address: String::new(),
            worker_name: "hansolo".into(),
            source: WorkSource::default(),
            cpu: CpuConfig::default(),
            gpu: GpuConfig::default(),
            asic: AsicConfig::default(),
            ui: UiConfig::default(),
            autostart: false,
        }
    }
}
