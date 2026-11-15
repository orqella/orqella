//! Optional Safari-compatible `/etc/hosts` management for local Orqella names.
//!
//! Chrome and Firefox resolve `*.localhost` as loopback on their own, and the
//! desktop client resolves app origins without host entries. Safari relies on
//! the system resolver, so `orqella.localhost` and configured app hosts still
//! need explicit `/etc/hosts` entries. Installation is opt-in; the macOS
//! installer and `orqella service install` no longer call it automatically.

use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;

pub const BLOCK_START: &str = "# >>> orqella managed hosts >>>";
pub const BLOCK_END: &str = "# <<< orqella managed hosts <<<";

/// Hostnames Safari needs for the shell and every configured app surface.
pub fn hostnames(config: &Config) -> Vec<String> {
    let canonical = config.canonical_host().trim_end_matches('.');
    let mut hosts = BTreeSet::new();
    if !canonical.is_empty() {
        hosts.insert(canonical.to_string());
    }
    for mount in &config.proxy {
        if let Some(host) = mount
            .host
            .as_deref()
            .map(str::trim)
            .filter(|host| !host.is_empty())
        {
            hosts.insert(host.trim_end_matches('.').to_string());
        }
        for peer in &config.peers {
            if !canonical.is_empty() && !mount.id.is_empty() && !peer.id.is_empty() {
                hosts.insert(format!("{}.{}.{}", mount.id, peer.id, canonical));
            }
        }
    }
    hosts.into_iter().collect()
}

/// Address written for managed entries. Use an explicit loopback IPv4 bind
/// when configured; otherwise fall back to `127.0.0.1`.
pub fn address(config: &Config) -> String {
    let bind = config.server.bind.trim();
    if bind
        .parse::<std::net::Ipv4Addr>()
        .is_ok_and(|ip| ip.is_loopback())
    {
        bind.to_string()
    } else {
        "127.0.0.1".into()
    }
}

/// The complete managed block, including markers.
pub fn block(config: &Config) -> String {
    let mut output = String::from(BLOCK_START);
    output.push('\n');
    let address = address(config);
    for host in hostnames(config) {
        output.push_str(&address);
        output.push(' ');
        output.push_str(&host);
        output.push('\n');
    }
    output.push_str(BLOCK_END);
    output.push('\n');
    output
}

/// Replace our managed block while preserving all other host entries.
pub fn merge(existing: &str, replacement: &str) -> String {
    let mut kept = Vec::new();
    let mut skipping = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == BLOCK_START {
            skipping = true;
            continue;
        }
        if skipping {
            if trimmed == BLOCK_END {
                skipping = false;
            }
            continue;
        }
        kept.push(line);
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    let mut output = kept.join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    if !replacement.trim().is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(replacement.trim_end());
        output.push('\n');
    }
    output
}

#[cfg(unix)]
pub fn hosts_path() -> PathBuf {
    PathBuf::from("/etc/hosts")
}

pub fn install(config: &Config) -> Result<()> {
    write_managed(config, true)
}

pub fn remove(config: &Config) -> Result<()> {
    write_managed(config, false)
}

/// Install into a caller-provided file. Used by tests and useful for recovery.
pub fn install_to(config: &Config, path: &Path) -> Result<()> {
    write_file(path, &block(config))
}

pub fn remove_from(path: &Path) -> Result<()> {
    write_file(path, "")
}

/// Hostnames that Safari cannot resolve from the current managed block.
#[cfg(unix)]
pub fn missing(config: &Config) -> Vec<String> {
    let Ok(existing) = fs::read_to_string(hosts_path()) else {
        return hostnames(config);
    };
    let address = address(config);
    hostnames(config)
        .into_iter()
        .filter(|host| {
            !existing
                .lines()
                .any(|line| line.trim() == format!("{address} {host}"))
        })
        .collect()
}

#[cfg(not(unix))]
pub fn missing(_config: &Config) -> Vec<String> {
    Vec::new()
}

#[cfg(not(unix))]
fn write_managed(_config: &Config, _add: bool) -> Result<()> {
    bail!("automatic /etc/hosts management is available on macOS and Linux only")
}

#[cfg(unix)]
fn write_managed(config: &Config, add: bool) -> Result<()> {
    {
        let path = hosts_path();
        let replacement = if add { block(config) } else { String::new() };
        let existing = fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let next = merge(&existing, &replacement);
        if next == existing {
            println!("hosts: already current");
            return Ok(());
        }
        if is_root() {
            fs::write(&path, &next)
                .with_context(|| format!("could not write {}", path.display()))?;
            #[cfg(target_os = "macos")]
            flush_dns();
            println!("hosts: updated {}", path.display());
            return Ok(());
        }

        let mut temp = tempfile::Builder::new()
            .prefix("orqella-hosts-")
            .tempfile()
            .context("could not create a private hosts staging file")?;
        temp.write_all(next.as_bytes())
            .context("could not write the hosts staging file")?;
        temp.flush()
            .context("could not flush the hosts staging file")?;
        let temp_path = temp.path().to_path_buf();
        let status = Command::new("sudo")
            .args(["install", "-m", "0644"])
            .arg(&temp_path)
            .arg(&path)
            .status()
            .context("could not run sudo; run the same command from a terminal with administrator access")?;
        drop(temp);
        if !status.success() {
            bail!(
                "sudo could not update {}; run `sudo orqella hosts install` from a terminal",
                path.display()
            );
        }
        #[cfg(target_os = "macos")]
        flush_dns();
        println!("hosts: updated {}", path.display());
        Ok(())
    }
}

fn write_file(path: &Path, replacement: &str) -> Result<()> {
    let existing =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    let next = merge(&existing, replacement);
    if next != existing {
        fs::write(path, next).with_context(|| format!("could not write {}", path.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|uid| uid.trim() == "0")
}

#[cfg(target_os = "macos")]
fn flush_dns() {
    let _ = Command::new("dscacheutil").args(["-flushcache"]).status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PeerConfig, ProxyConfig};

    fn config() -> Config {
        let mut config = Config::default();
        config.proxy.push(ProxyConfig {
            id: "opencode".into(),
            title: "OpenCode".into(),
            base_url: "http://127.0.0.1:49374".into(),
            host: Some("opencode.orqella.localhost".into()),
            websocket: true,
            frame: false,
        });
        config.peers.push(PeerConfig {
            id: "link-laptop".into(),
            name: "Laptop".into(),
            url: None,
            token: "test".into(),
        });
        config
    }

    #[test]
    fn uses_a_loopback_bind_address_when_configured() {
        let mut config = config();
        config.server.bind = "127.0.0.5".into();
        assert_eq!(address(&config), "127.0.0.5");
        assert!(block(&config).contains("127.0.0.5 orqella.localhost"));
        config.server.bind = "0.0.0.0".into();
        assert_eq!(address(&config), "127.0.0.1");
    }

    #[test]
    fn includes_shell_proxy_and_peer_hosts() {
        let hosts = hostnames(&config());
        assert!(hosts.contains(&"orqella.localhost".to_string()));
        assert!(hosts.contains(&"opencode.orqella.localhost".to_string()));
        assert!(hosts.contains(&"opencode.link-laptop.orqella.localhost".to_string()));
    }

    #[test]
    fn merge_is_idempotent_and_preserves_unmanaged_entries() {
        let existing = "127.0.0.1 localhost\n# custom\n";
        let block = block(&config());
        let once = merge(existing, &block);
        let twice = merge(&once, &block);
        assert_eq!(once, twice);
        assert!(once.contains("# custom"));
        assert_eq!(once.matches(BLOCK_START).count(), 1);
    }

    #[test]
    fn remove_deletes_only_the_managed_block() {
        let existing = "127.0.0.1 localhost\n# custom\n";
        let with_block = merge(existing, &block(&config()));
        let removed = merge(&with_block, "");
        assert_eq!(removed, existing);
    }

    #[test]
    fn install_to_file_round_trips() {
        let path =
            std::env::temp_dir().join(format!("orqella-hosts-test-{}.txt", std::process::id()));
        fs::write(&path, "127.0.0.1 localhost\n").unwrap();
        install_to(&config(), &path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("opencode.orqella.localhost"));
        remove_from(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "127.0.0.1 localhost\n");
        let _ = fs::remove_file(&path);
    }
}
