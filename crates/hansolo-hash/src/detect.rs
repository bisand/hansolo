//! CPU and system facts for the hardware report.

use hansolo_core::snapshot::HardwareReport;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

/// Fills the OS, CPU and memory fields of `report`. Takes a few milliseconds:
/// only the static CPU list and RAM total are read, no usage sampling.
pub(crate) fn system(report: &mut HardwareReport) {
    let sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing())
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    report.os = match (System::name(), System::os_version()) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        (Some(name), None) => name,
        _ => std::env::consts::OS.to_string(),
    };
    report.arch = std::env::consts::ARCH.to_string();
    report.cpu_brand = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "Unknown CPU".to_string());
    let parallelism = std::thread::available_parallelism().map_or(1, |n| n.get());
    report.logical_cores = if sys.cpus().is_empty() {
        parallelism
    } else {
        sys.cpus().len()
    };
    report.physical_cores = System::physical_core_count().unwrap_or(report.logical_cores);
    report.memory_bytes = sys.total_memory();
    report.cpu_features = cpu_features();
}

/// The CPU features that matter for SHA-256, detected at run time.
pub fn cpu_features() -> Vec<(String, bool)> {
    #[allow(unused_mut)]
    let mut features: Vec<(String, bool)> = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::is_x86_feature_detected as has;
        features.push(("SHA-NI".into(), has!("sha")));
        features.push(("SSSE3".into(), has!("ssse3")));
        features.push(("SSE4.1".into(), has!("sse4.1")));
        features.push(("AVX2".into(), has!("avx2")));
        features.push(("AVX-512F".into(), has!("avx512f")));
    }
    #[cfg(target_arch = "aarch64")]
    {
        use std::arch::is_aarch64_feature_detected as has;
        features.push(("NEON".into(), has!("neon")));
        features.push(("SHA2".into(), has!("sha2")));
    }
    features
}
