//! Collecte CPU/RAM/disques/réseau.

use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use sysinfo::{DiskRefreshKind, Disks, MINIMUM_CPU_UPDATE_INTERVAL, Networks, System};
use tokio::{
    sync::watch,
    time::{self, MissedTickBehavior},
};

const KEPT_FS: &[&str] = &["ext4", "xfs", "btrfs", "vfat", "exfat", "zfs"];
const EXCLUDED_MOUNTS: &[&str] = &["/var/lib/docker", "/run", "/snap"];
const EXCLUDED_IFACE_PREFIXES: &[&str] = &["docker", "veth", "br-", "virbr", "tun", "wg"];

#[derive(Serialize)]
pub struct Stats<'a> {
    ts: u64,
    hostname: &'a str,
    uptime_s: u64,
    cpu: Cpu,
    mem: Mem,
    disks: Vec<Disk>,
    net: Net,
}

#[derive(Serialize)]
struct Cpu {
    usage: f32,
    cores: Vec<f32>,
}

#[derive(Serialize)]
struct Mem {
    used: u64,
    total: u64,
    swap_used: u64,
    swap_total: u64,
}

#[derive(Serialize)]
struct Disk {
    device: String,
    mount: String,
    fs: String,
    used: u64,
    total: u64,
}

#[derive(Serialize)]
struct Net {
    rx_bps: u64,
    tx_bps: u64,
}

struct Collector {
    sys: System,
    disks: Disks,
    networks: Networks,
    hostname: String,
    last_net: Instant,
}

impl Collector {
    /// Premier échantillon CPU invalide : refresh initial puis attente minimale.
    async fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        let collector = Self {
            sys,
            disks: Disks::new_with_refreshed_list_specifics(disk_refresh_kind()),
            networks: Networks::new_with_refreshed_list(),
            hostname: System::host_name().unwrap_or_else(|| "unknown".into()),
            last_net: Instant::now(),
        };
        time::sleep(MINIMUM_CPU_UPDATE_INTERVAL).await;
        collector
    }

    fn sample(&mut self) -> Stats<'_> {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.disks.refresh_specifics(true, disk_refresh_kind());
        self.networks.refresh(true);

        let now = Instant::now();
        let elapsed = now.duration_since(self.last_net).as_secs_f64();
        self.last_net = now;

        let (rx, tx) = self
            .networks
            .iter()
            .filter(|(name, _)| is_kept_iface(name))
            .fold((0u64, 0u64), |(rx, tx), (_, data)| {
                (rx + data.received(), tx + data.transmitted())
            });

        Stats {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            hostname: &self.hostname,
            uptime_s: System::uptime(),
            cpu: Cpu {
                usage: percent(self.sys.global_cpu_usage()),
                cores: self
                    .sys
                    .cpus()
                    .iter()
                    .map(|c| percent(c.cpu_usage()))
                    .collect(),
            },
            mem: Mem {
                used: self.sys.used_memory(),
                total: self.sys.total_memory(),
                swap_used: self.sys.used_swap(),
                swap_total: self.sys.total_swap(),
            },
            disks: select_disks(self.disks.iter().map(|d| Disk {
                device: d.name().to_string_lossy().into_owned(),
                mount: d.mount_point().to_string_lossy().into_owned(),
                fs: d.file_system().to_string_lossy().into_owned(),
                used: d.total_space().saturating_sub(d.available_space()),
                total: d.total_space(),
            })),
            net: Net {
                rx_bps: rate(rx, elapsed),
                tx_bps: rate(tx, elapsed),
            },
        }
    }

    fn sample_json(&mut self) -> Option<Arc<str>> {
        match serde_json::to_string(&self.sample()) {
            Ok(json) => Some(json.into()),
            Err(e) => {
                eprintln!("sysdash: sérialisation des stats impossible: {e}");
                None
            }
        }
    }
}

fn disk_refresh_kind() -> DiskRefreshKind {
    DiskRefreshKind::nothing().with_storage()
}

/// Arrondi à 0,1 et borné à 0–100.
fn percent(v: f32) -> f32 {
    ((v * 10.0).round() / 10.0).clamp(0.0, 100.0)
}

fn rate(bytes: u64, elapsed_s: f64) -> u64 {
    if elapsed_s > 0.0 {
        (bytes as f64 / elapsed_s).round() as u64
    } else {
        0
    }
}

fn is_kept_iface(name: &str) -> bool {
    name != "lo" && !EXCLUDED_IFACE_PREFIXES.iter().any(|p| name.starts_with(p))
}

fn is_kept_disk(disk: &Disk) -> bool {
    let mount = Path::new(&disk.mount);
    KEPT_FS.contains(&disk.fs.as_str()) && !EXCLUDED_MOUNTS.iter().any(|m| mount.starts_with(m))
}

/// Filtre, déduplique par device (point de montage le plus court), trie par point de montage.
fn select_disks(disks: impl Iterator<Item = Disk>) -> Vec<Disk> {
    let mut by_device: HashMap<String, Disk> = HashMap::new();
    for disk in disks.filter(is_kept_disk) {
        match by_device.get(&disk.device) {
            Some(kept) if kept.mount.len() <= disk.mount.len() => {}
            _ => {
                by_device.insert(disk.device.clone(), disk);
            }
        }
    }
    let mut disks: Vec<Disk> = by_device.into_values().collect();
    disks.sort_by(|a, b| a.mount.cmp(&b.mount));
    disks
}

/// Lance l'unique tâche de collecte et renvoie le dernier snapshot JSON publié.
pub async fn spawn_collector(interval: Duration) -> watch::Receiver<Arc<str>> {
    let mut collector = Collector::new().await;
    let first = collector.sample_json().unwrap_or_else(|| "{}".into());
    let (tx, rx) = watch::channel(first);

    tokio::spawn(async move {
        let mut ticker = time::interval(interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Some(json) = collector.sample_json() {
                tx.send_replace(json);
            }
        }
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(device: &str, mount: &str, fs: &str) -> Disk {
        Disk {
            device: device.into(),
            mount: mount.into(),
            fs: fs.into(),
            used: 0,
            total: 0,
        }
    }

    #[test]
    fn iface_filter() {
        for name in ["eth0", "enp3s0", "wlan0", "bond0"] {
            assert!(is_kept_iface(name), "{name}");
        }
        for name in [
            "lo", "docker0", "veth12ab", "br-1234", "virbr0", "tun0", "wg0",
        ] {
            assert!(!is_kept_iface(name), "{name}");
        }
    }

    #[test]
    fn disk_selection() {
        let disks = select_disks(
            [
                disk("/dev/sda2", "/home", "btrfs"),
                disk("/dev/sda2", "/", "btrfs"),
                disk("/dev/sda1", "/boot/efi", "vfat"),
                disk("tmpfs", "/tmp", "tmpfs"),
                disk("/dev/sdb1", "/var/lib/docker", "ext4"),
                disk("/dev/sdb2", "/var/lib/docker/overlay", "ext4"),
                disk("/dev/sdc1", "/run/media/usb", "exfat"),
                disk("/dev/loop0", "/snap/core", "squashfs"),
                disk("/dev/sdd1", "/srv", "xfs"),
                disk("/dev/sde1", "/runner", "ext4"),
            ]
            .into_iter(),
        );
        let mounts: Vec<&str> = disks.iter().map(|d| d.mount.as_str()).collect();
        assert_eq!(mounts, ["/", "/boot/efi", "/runner", "/srv"]);
    }

    #[test]
    fn percent_rounding() {
        assert_eq!(percent(12.44), 12.4);
        assert_eq!(percent(-1.0), 0.0);
        assert_eq!(percent(100.04), 100.0);
    }
}
