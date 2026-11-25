use serde::Serialize;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use sysinfo::{
    Components, CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System,
};

#[derive(Debug, Clone, Serialize)]
pub struct DiskSnapshot {
    pub name: String,
    pub mount: String,
    pub filesystem: String,
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub percent: f32,
    pub removable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkSnapshot {
    pub interface: String,
    pub received_total: u64,
    pub transmitted_total: u64,
    pub received_rate: f64,
    pub transmitted_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TemperatureSnapshot {
    pub label: String,
    pub current: f32,
    pub max: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuSnapshot {
    pub name: String,
    pub driver: String,
    pub busy: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatterySnapshot {
    pub name: String,
    pub status: String,
    pub percent: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemSnapshot {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub uptime_secs: u64,
    pub cpu_percent: f32,
    pub cpu_cores: Vec<f32>,
    pub cpu_count: usize,
    pub load_one: f64,
    pub load_five: f64,
    pub load_fifteen: f64,
    pub memory_total: u64,
    pub memory_used: u64,
    pub memory_percent: f32,
    pub swap_total: u64,
    pub swap_used: u64,
    pub swap_percent: f32,
    pub processes: usize,
    pub disks: Vec<DiskSnapshot>,
    pub networks: Vec<NetworkSnapshot>,
    pub temperatures: Vec<TemperatureSnapshot>,
    pub gpus: Vec<GpuSnapshot>,
    pub batteries: Vec<BatterySnapshot>,
    pub captured_at: String,
}

struct Cache {
    system: System,
    disks: Disks,
    networks: Networks,
    components: Components,
    hostname: String,
    last: Instant,
    process_count: usize,
    process_count_at: Instant,
    snapshot: Option<SystemSnapshot>,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static ASYNC_SNAPSHOT_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[cfg(target_os = "linux")]
fn process_count() -> usize {
    std::fs::read_dir("/proc")
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
                })
                .count()
        })
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
fn process_count() -> usize {
    let mut system = System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    system.processes().len()
}

pub fn snapshot() -> SystemSnapshot {
    let mutex = CACHE.get_or_init(|| {
        Mutex::new(Cache {
            system: System::new_with_specifics(
                RefreshKind::nothing()
                    .with_cpu(CpuRefreshKind::everything())
                    .with_memory(MemoryRefreshKind::everything()),
            ),
            disks: Disks::new_with_refreshed_list(),
            networks: Networks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            hostname: hostname::get()
                .ok()
                .and_then(|name| name.into_string().ok())
                .unwrap_or_else(|| "unknown".into()),
            last: Instant::now(),
            process_count: process_count(),
            process_count_at: Instant::now(),
            snapshot: None,
        })
    });
    let mut cache = mutex.lock().expect("metrics cache lock");
    if cache.last.elapsed() < Duration::from_secs(1)
        && let Some(snapshot) = &cache.snapshot
    {
        return snapshot.clone();
    }
    if cache.snapshot.is_none() {
        cache.system.refresh_cpu_usage();
        cache.system.refresh_memory();
        std::thread::sleep(Duration::from_millis(120));
    }
    let elapsed = cache.last.elapsed();
    cache.system.refresh_cpu_usage();
    cache.system.refresh_memory();
    if cache.process_count_at.elapsed() >= Duration::from_secs(30) {
        cache.process_count = process_count();
        cache.process_count_at = Instant::now();
    }
    cache.disks.refresh(true);
    cache.networks.refresh(true);
    cache.components.refresh(true);
    let load = System::load_average();
    let total_memory = cache.system.total_memory();
    let used_memory = cache.system.used_memory();
    let total_swap = cache.system.total_swap();
    let used_swap = cache.system.used_swap();
    let elapsed_secs = if elapsed.as_secs_f64() > 0.0 {
        elapsed.as_secs_f64()
    } else {
        1.0
    };
    let networks = cache
        .networks
        .list()
        .iter()
        .filter(|(name, _)| name.as_str() != "lo" && !name.starts_with("Loopback"))
        .map(|(name, data)| NetworkSnapshot {
            interface: name.clone(),
            received_total: data.total_received(),
            transmitted_total: data.total_transmitted(),
            received_rate: data.received() as f64 / elapsed_secs,
            transmitted_rate: data.transmitted() as f64 / elapsed_secs,
        })
        .collect();
    let disks = {
        let mut seen = std::collections::HashSet::new();
        let mut grouped: std::collections::BTreeMap<String, DiskSnapshot> =
            std::collections::BTreeMap::new();
        for disk in cache.disks.list() {
            let name = disk.name().to_string_lossy().to_string();
            if cfg!(target_os = "linux") && !name.starts_with("/dev/") {
                continue;
            }
            if !seen.insert(name.clone()) {
                continue;
            }
            let physical = physical_disk(&name);
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total.saturating_sub(available);
            let mount = disk.mount_point().to_string_lossy().to_string();
            let entry = grouped
                .entry(physical.clone())
                .or_insert_with(|| DiskSnapshot {
                    name: physical,
                    mount: mount.clone(),
                    filesystem: disk.file_system().to_string_lossy().to_string(),
                    total: 0,
                    used: 0,
                    available: 0,
                    percent: 0.0,
                    removable: false,
                });
            entry.total += total;
            entry.used += used;
            entry.available += available;
            entry.removable |= disk.is_removable();
            if mount == "/" || (entry.mount != "/" && mount.len() < entry.mount.len()) {
                entry.mount = mount;
                entry.filesystem = disk.file_system().to_string_lossy().to_string();
            }
        }
        grouped
            .into_values()
            .map(|mut disk| {
                disk.percent = if disk.total > 0 {
                    (disk.used as f64 * 100.0 / disk.total as f64) as f32
                } else {
                    0.0
                };
                disk
            })
            .collect()
    };
    let temperatures = cache
        .components
        .list()
        .iter()
        .filter_map(|component| {
            component.temperature().map(|current| TemperatureSnapshot {
                label: component.label().to_string(),
                current,
                max: component.max(),
            })
        })
        .collect();
    let snapshot = SystemSnapshot {
        hostname: cache.hostname.clone(),
        os: System::long_os_version().unwrap_or_else(|| std::env::consts::OS.into()),
        kernel: System::kernel_version().unwrap_or_default(),
        arch: System::cpu_arch(),
        uptime_secs: System::uptime(),
        cpu_percent: cache.system.global_cpu_usage(),
        cpu_cores: cache
            .system
            .cpus()
            .iter()
            .map(|cpu| cpu.cpu_usage())
            .collect(),
        cpu_count: cache.system.cpus().len(),
        load_one: load.one,
        load_five: load.five,
        load_fifteen: load.fifteen,
        memory_total: total_memory,
        memory_used: used_memory,
        memory_percent: if total_memory > 0 {
            (used_memory as f64 * 100.0 / total_memory as f64) as f32
        } else {
            0.0
        },
        swap_total: total_swap,
        swap_used: used_swap,
        swap_percent: if total_swap > 0 {
            (used_swap as f64 * 100.0 / total_swap as f64) as f32
        } else {
            0.0
        },
        processes: cache.process_count,
        disks,
        networks,
        temperatures,
        gpus: linux_gpus(),
        batteries: linux_batteries(),
        captured_at: crate::util::now_iso(),
    };
    cache.last = Instant::now();
    cache.snapshot = Some(snapshot.clone());
    snapshot
}

/// Coalesce concurrent metrics requests behind one blocking refresh. Every
/// module/system poll used to spawn its own blocking task, which could grow the
/// blocking pool and allocator arenas under a burst.
pub async fn snapshot_async() -> Result<SystemSnapshot, String> {
    let lock = ASYNC_SNAPSHOT_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = lock.lock().await;
    tokio::task::spawn_blocking(snapshot)
        .await
        .map_err(|error| format!("metrics task failed: {error}"))
}

#[cfg(target_os = "linux")]
fn physical_disk(device: &str) -> String {
    let base = device.rsplit('/').next().unwrap_or(device);
    let sys = std::path::PathBuf::from("/sys/class/block").join(base);
    if sys.join("partition").exists()
        && let Ok(canonical) = sys.canonicalize()
        && let Some(parent) = canonical.parent().and_then(|path| path.file_name())
    {
        return format!("/dev/{}", parent.to_string_lossy());
    }
    device.to_string()
}

#[cfg(not(target_os = "linux"))]
fn physical_disk(device: &str) -> String {
    device.to_string()
}

#[cfg(target_os = "linux")]
fn linux_gpus() -> Vec<GpuSnapshot> {
    let mut gpus = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return gpus;
    };
    let mut cards: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("card") && !name.contains('-')
        })
        .collect();
    cards.sort_by_key(|entry| entry.file_name());
    for card in cards {
        let device = card.path().join("device");
        let uevent = std::fs::read_to_string(device.join("uevent")).unwrap_or_default();
        let driver = uevent
            .lines()
            .find_map(|line| line.strip_prefix("DRIVER="))
            .unwrap_or("")
            .to_string();
        let busy = std::fs::read_to_string(device.join("gpu_busy_percent"))
            .ok()
            .and_then(|value| value.trim().parse().ok());
        let temperature = std::fs::read_dir(device.join("hwmon"))
            .ok()
            .and_then(|mut entries| entries.next())
            .and_then(|entry| entry.ok())
            .and_then(|entry| std::fs::read_to_string(entry.path().join("temp1_input")).ok())
            .and_then(|value| value.trim().parse::<f32>().ok())
            .map(|value| value / 1000.0);
        if !driver.is_empty() || busy.is_some() || temperature.is_some() {
            gpus.push(GpuSnapshot {
                name: card.file_name().to_string_lossy().to_string(),
                driver,
                busy,
                temperature,
            });
        }
    }
    gpus
}

#[cfg(not(target_os = "linux"))]
fn linux_gpus() -> Vec<GpuSnapshot> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn linux_batteries() -> Vec<BatterySnapshot> {
    let mut batteries = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return batteries;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim() != "Battery" {
            continue;
        }
        let status = std::fs::read_to_string(path.join("status"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let percent = std::fs::read_to_string(path.join("capacity"))
            .ok()
            .and_then(|value| value.trim().parse().ok());
        batteries.push(BatterySnapshot {
            name: entry.file_name().to_string_lossy().to_string(),
            status,
            percent,
        });
    }
    batteries
}

#[cfg(not(target_os = "linux"))]
fn linux_batteries() -> Vec<BatterySnapshot> {
    Vec::new()
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn snapshot_is_serializable_and_internally_consistent() {
        let snapshot = snapshot();
        let value = serde_json::to_value(&snapshot).expect("snapshot serializes");
        assert!(value.is_object());
        assert!(!snapshot.hostname.trim().is_empty());
        assert!(!snapshot.os.trim().is_empty());
        assert!(!snapshot.captured_at.trim().is_empty());
        assert_eq!(snapshot.cpu_count, snapshot.cpu_cores.len());
        assert!(snapshot.memory_percent >= 0.0 && snapshot.memory_percent <= 100.0);
        assert!(snapshot.swap_percent >= 0.0 && snapshot.swap_percent <= 100.0);
        for disk in &snapshot.disks {
            assert!(disk.percent >= 0.0 && disk.percent <= 100.0, "{disk:?}");
            assert_eq!(disk.total, disk.used.saturating_add(disk.available));
        }
        for network in &snapshot.networks {
            assert!(!network.interface.is_empty());
            assert!(network.received_rate >= 0.0);
            assert!(network.transmitted_rate >= 0.0);
        }
        for temperature in &snapshot.temperatures {
            assert!(!temperature.label.is_empty());
            if let Some(max) = temperature.max {
                assert!(max >= temperature.current || max == 0.0);
            }
        }
    }

    #[test]
    fn repeated_snapshots_use_the_one_second_cache() {
        let first = snapshot();
        let second = snapshot();
        assert_eq!(first.captured_at, second.captured_at);
        assert_eq!(first.cpu_count, second.cpu_count);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn partition_names_are_grouped_under_their_physical_disk() {
        // `/dev/sda` is returned unchanged whether or not it exists in sysfs.
        assert_eq!(physical_disk("/dev/sda"), "/dev/sda");
        assert_eq!(physical_disk("/dev/nvme0n1"), "/dev/nvme0n1");
    }
}
