use anyhow::{Context, Result, bail};
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use tokio::net::UdpSocket;
use tokio::time::{Duration, sleep};

const DEFAULT_PORT: u16 = 9;
const DEFAULT_REPEAT: u8 = 3;
const REPEAT_DELAY: Duration = Duration::from_millis(200);
const CACHE_FRESH_SECS: i64 = 15 * 60;
const CACHE_MAX_AGE_SECS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeInterface {
    pub name: String,
    pub mac: String,
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub broadcast: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeTarget {
    pub mac: String,
    pub broadcast: Ipv4Addr,
    pub port: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WakeCache {
    #[serde(default)]
    pub nodes: BTreeMap<String, WakeCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeCacheEntry {
    pub name: String,
    pub updated_at: i64,
    #[serde(default)]
    pub interfaces: Vec<WakeInterface>,
}

impl WakeCache {
    pub fn load(path: &Path) -> Self {
        let mut cache: WakeCache = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        cache.prune();
        cache
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_vec_pretty(self)?;
        crate::util::write_atomic(path, &raw)?;
        let _ = crate::util::restrict_file(path);
        Ok(())
    }

    pub fn record(&mut self, node: &str, name: &str, interfaces: Vec<WakeInterface>) {
        self.nodes.insert(
            node.to_string(),
            WakeCacheEntry {
                name: name.to_string(),
                updated_at: chrono::Utc::now().timestamp(),
                interfaces,
            },
        );
    }

    pub fn entry(&self, node: &str, name: &str) -> Option<&WakeCacheEntry> {
        self.nodes.get(node).or_else(|| {
            self.nodes.values().find(|entry| {
                !entry.name.is_empty() && !name.is_empty() && entry.name.eq_ignore_ascii_case(name)
            })
        })
    }

    pub fn interfaces(&self, node: &str, name: &str) -> Vec<WakeInterface> {
        self.entry(node, name)
            .map(|entry| entry.interfaces.clone())
            .unwrap_or_default()
    }

    pub fn is_fresh(&self, node: &str) -> bool {
        self.nodes
            .get(node)
            .map(|entry| chrono::Utc::now().timestamp() - entry.updated_at < CACHE_FRESH_SECS)
            .unwrap_or(false)
    }

    fn prune(&mut self) {
        let now = chrono::Utc::now().timestamp();
        self.nodes
            .retain(|_, entry| now - entry.updated_at < CACHE_MAX_AGE_SECS);
    }
}

/// Report the LAN interfaces this machine can be woken on. Loopback, virtual,
/// and link-local interfaces are omitted. Platforms without interface data
/// return an empty list; wake-on-LAN is then simply unavailable for this node.
pub fn local_interfaces() -> Vec<WakeInterface> {
    let Ok(interfaces) = NetworkInterface::show() else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for interface in interfaces {
        if interface.internal || is_virtual(&interface.name) {
            continue;
        }
        let Some(mac) = interface
            .mac_addr
            .as_deref()
            .and_then(|mac| parse_mac(mac).ok())
        else {
            continue;
        };
        let mac = format_mac(mac);
        for address in interface.addr {
            let Addr::V4(address) = address else {
                continue;
            };
            if address.ip.is_loopback() || address.ip.is_link_local() || address.ip.is_unspecified()
            {
                continue;
            }
            let Some(netmask) = address.netmask else {
                continue;
            };
            if netmask == Ipv4Addr::UNSPECIFIED {
                continue;
            }
            let broadcast = address
                .broadcast
                .filter(|broadcast| !broadcast.is_unspecified())
                .unwrap_or_else(|| broadcast_for(address.ip, netmask));
            result.push(WakeInterface {
                name: interface.name.clone(),
                mac: mac.clone(),
                ip: address.ip,
                netmask,
                broadcast,
            });
        }
    }
    result.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.ip.cmp(&right.ip))
            .then(left.mac.cmp(&right.mac))
    });
    result.dedup();
    result
}

/// Intersect the cached target interfaces with this machine's local subnets.
/// Only interfaces on a shared L2 segment can be reached with a magic packet.
pub fn matching_targets(cached: &[WakeInterface], local: &[WakeInterface]) -> Vec<WakeTarget> {
    let mut targets = Vec::new();
    for target in cached {
        if !local.iter().any(|interface| same_subnet(target, interface)) {
            continue;
        }
        let candidate = WakeTarget {
            mac: target.mac.clone(),
            broadcast: target.broadcast,
            port: DEFAULT_PORT,
        };
        if !targets.iter().any(|existing: &WakeTarget| {
            existing.mac == candidate.mac && existing.broadcast == candidate.broadcast
        }) {
            targets.push(candidate);
        }
    }
    targets
}

pub async fn send_targets(targets: &[WakeTarget], repeat: u8) -> Result<usize> {
    if targets.is_empty() {
        bail!("no wake-on-LAN targets available");
    }
    let repeat = if repeat == 0 { DEFAULT_REPEAT } else { repeat }.clamp(1, 10) as usize;
    let packets = targets
        .iter()
        .map(|target| parse_mac(&target.mac).map(|mac| (target, magic_packet(mac))))
        .collect::<Result<Vec<_>>>()?;
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .context("could not open the wake-on-LAN socket")?;
    socket
        .set_broadcast(true)
        .context("could not enable broadcast on the wake-on-LAN socket")?;
    let mut sent = 0;
    for index in 0..repeat {
        for (target, packet) in &packets {
            let destination = SocketAddr::new(IpAddr::V4(target.broadcast), target.port);
            socket
                .send_to(packet, destination)
                .await
                .with_context(|| format!("could not send wake-on-LAN packet to {destination}"))?;
            sent += 1;
        }
        if index + 1 < repeat {
            sleep(REPEAT_DELAY).await;
        }
    }
    Ok(sent)
}

fn same_subnet(target: &WakeInterface, local: &WakeInterface) -> bool {
    let target_mask = u32::from(target.netmask);
    if target_mask != 0
        && (u32::from(target.ip) & target_mask) == (u32::from(local.ip) & target_mask)
    {
        return true;
    }
    let local_mask = u32::from(local.netmask);
    local_mask != 0 && (u32::from(target.ip) & local_mask) == (u32::from(local.ip) & local_mask)
}

fn broadcast_for(ip: Ipv4Addr, netmask: Ipv4Addr) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(ip) | !u32::from(netmask))
}

fn is_virtual(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    const VIRTUAL_PREFIXES: &[&str] = &[
        "lo",
        "docker",
        "veth",
        "virbr",
        "br-",
        "bridge",
        "vmnet",
        "vboxnet",
        "utun",
        "tun",
        "tap",
        "wg",
        "tailscale",
        "zt",
        "ham",
        "awdl",
        "llw",
        "anpi",
        "p2p",
        "vethernet",
        "wsl",
    ];
    VIRTUAL_PREFIXES
        .iter()
        .any(|prefix| name == *prefix || name.starts_with(prefix))
}

/// Parse a wake-on-LAN MAC address. Accepts colon, hyphen, or bare hex.
pub fn parse_mac(value: &str) -> Result<[u8; 6]> {
    let cleaned: String = value
        .trim()
        .chars()
        .filter(|character| *character != ':' && *character != '-')
        .collect();
    if cleaned.len() != 12 {
        bail!("MAC address must contain 12 hexadecimal digits");
    }
    if !cleaned
        .chars()
        .all(|character| character.is_ascii_hexdigit())
    {
        bail!("MAC address contains a non-hexadecimal character");
    }
    let mut mac = [0u8; 6];
    for (index, byte) in mac.iter_mut().enumerate() {
        let start = index * 2;
        *byte = u8::from_str_radix(&cleaned[start..start + 2], 16)
            .context("MAC address contains an invalid byte")?;
    }
    Ok(mac)
}

pub fn format_mac(mac: [u8; 6]) -> String {
    mac.iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// A wake-on-LAN magic packet: six 0xff bytes followed by sixteen copies of
/// the target MAC address.
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0xff; 102];
    for chunk in packet[6..].as_chunks_mut::<6>().0 {
        chunk.copy_from_slice(&mac);
    }
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface(name: &str, mac: &str, ip: &str, netmask: &str) -> WakeInterface {
        let ip: Ipv4Addr = ip.parse().unwrap();
        let netmask: Ipv4Addr = netmask.parse().unwrap();
        WakeInterface {
            name: name.into(),
            mac: mac.into(),
            ip,
            netmask,
            broadcast: broadcast_for(ip, netmask),
        }
    }

    #[test]
    fn parses_mac_addresses() {
        let expected = [0xb4, 0x8c, 0x9d, 0x07, 0xe9, 0x55];
        assert_eq!(parse_mac("b4:8c:9d:07:e9:55").unwrap(), expected);
        assert_eq!(parse_mac("B4-8C-9D-07-E9-55").unwrap(), expected);
        assert_eq!(parse_mac("b48c9d07e955").unwrap(), expected);
        assert_eq!(parse_mac(" b4:8c:9d:07:e9:55 ").unwrap(), expected);
    }

    #[test]
    fn rejects_invalid_mac_addresses() {
        assert!(parse_mac("").is_err());
        assert!(parse_mac("b4:8c:9d:07:e9").is_err());
        assert!(parse_mac("b4:8c:9d:07:e9:5z").is_err());
        assert!(parse_mac("b4:8c:9d:07:e9:55:00").is_err());
    }

    #[test]
    fn builds_the_magic_packet() {
        let mac = [0xb4, 0x8c, 0x9d, 0x07, 0xe9, 0x55];
        let packet = magic_packet(mac);
        assert_eq!(packet.len(), 102);
        assert!(packet[..6].iter().all(|byte| *byte == 0xff));
        for chunk in packet[6..].as_chunks::<6>().0 {
            assert_eq!(chunk, &mac);
        }
    }

    #[test]
    fn matches_only_targets_on_a_shared_subnet() {
        let local = vec![interface(
            "enp6s0",
            "aa:bb:cc:dd:ee:ff",
            "192.168.68.10",
            "255.255.255.0",
        )];
        let same = vec![interface(
            "wlan0",
            "b4:8c:9d:07:e9:55",
            "192.168.68.18",
            "255.255.255.0",
        )];
        let other = vec![interface(
            "wlan0",
            "b4:8c:9d:07:e9:55",
            "10.0.0.18",
            "255.255.255.0",
        )];
        assert_eq!(matching_targets(&same, &local).len(), 1);
        assert!(matching_targets(&other, &local).is_empty());
    }

    #[test]
    fn matches_across_mismatched_netmasks() {
        let local = vec![interface(
            "enp6s0",
            "aa:bb:cc:dd:ee:ff",
            "192.168.68.10",
            "255.255.252.0",
        )];
        let target = vec![interface(
            "wlan0",
            "b4:8c:9d:07:e9:55",
            "192.168.69.18",
            "255.255.255.0",
        )];
        assert_eq!(matching_targets(&target, &local).len(), 1);
    }

    #[test]
    fn caches_interfaces_by_node_and_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wake.json");
        let mut cache = WakeCache::default();
        cache.record(
            "link-abc",
            "steamdeck",
            vec![interface(
                "wlan0",
                "b4:8c:9d:07:e9:55",
                "192.168.68.18",
                "255.255.255.0",
            )],
        );
        cache.save(&path).unwrap();
        let loaded = WakeCache::load(&path);
        assert_eq!(loaded.interfaces("link-abc", "").len(), 1);
        assert_eq!(loaded.interfaces("link-new", "SteamDeck").len(), 1);
        assert!(loaded.is_fresh("link-abc"));
    }

    #[test]
    fn local_interfaces_are_usable_candidates() {
        for interface in local_interfaces() {
            assert!(parse_mac(&interface.mac).is_ok());
            assert!(!interface.ip.is_loopback());
            assert!(!interface.ip.is_link_local());
        }
    }

    #[test]
    fn cache_misses_and_expiry_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wake.json");
        assert!(WakeCache::load(&path).interfaces("missing", "").is_empty());
        let mut cache = WakeCache::default();
        assert!(!cache.is_fresh("missing"));
        cache.record(
            "link-old",
            "Old",
            vec![interface(
                "wlan0",
                "b4:8c:9d:07:e9:55",
                "192.168.68.18",
                "255.255.255.0",
            )],
        );
        // A cache entry with a timestamp in the past is stale.
        cache.nodes.get_mut("link-old").unwrap().updated_at = 0;
        assert!(!cache.is_fresh("link-old"));
        let mut bad = path.clone();
        bad.set_extension("bad");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(WakeCache::load(&bad).interfaces("link-old", "").is_empty());
    }

    #[test]
    fn cache_prunes_old_entries_when_saving() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wake.json");
        let mut cache = WakeCache::default();
        cache.record(
            "link-old",
            "Old",
            vec![interface(
                "wlan0",
                "b4:8c:9d:07:e9:55",
                "192.168.68.18",
                "255.255.255.0",
            )],
        );
        cache.nodes.get_mut("link-old").unwrap().updated_at = 0;
        cache.record(
            "link-new",
            "New",
            vec![interface(
                "wlan0",
                "b4:8c:9d:07:e9:55",
                "192.168.68.19",
                "255.255.255.0",
            )],
        );
        cache.save(&path).unwrap();
        let loaded = WakeCache::load(&path);
        assert!(!loaded.is_fresh("link-old"));
        assert!(loaded.is_fresh("link-new"));
    }

    #[test]
    fn virtual_interfaces_are_not_wake_candidates() {
        assert!(is_virtual("lo"));
        assert!(is_virtual("docker0"));
        assert!(is_virtual("veth1234"));
        assert!(is_virtual("virbr0"));
        assert!(is_virtual("tailscale0"));
        assert!(!is_virtual("enp6s0"));
        assert!(!is_virtual("wlan0"));
    }

    #[tokio::test]
    async fn sends_a_magic_packet() {
        let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let targets = vec![WakeTarget {
            mac: "b4:8c:9d:07:e9:55".into(),
            broadcast: Ipv4Addr::LOCALHOST,
            port,
        }];
        assert_eq!(send_targets(&targets, 1).await.unwrap(), 1);
        let mut buffer = [0u8; 128];
        let (length, _) =
            tokio::time::timeout(Duration::from_secs(2), listener.recv_from(&mut buffer))
                .await
                .expect("wake packet timed out")
                .unwrap();
        assert_eq!(length, 102);
        assert_eq!(&buffer[6..12], &[0xb4, 0x8c, 0x9d, 0x07, 0xe9, 0x55]);
    }
}
