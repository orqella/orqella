//! Host firewall assistance for Link's direct-only UDP listener.
//!
//! Link carries SSH only over direct iroh/QUIC paths. The hub therefore needs a
//! stable, reachable UDP port. This module detects the host firewall and can add
//! a LAN-scoped rule for the configured `[link] port`. It never opens the port
//! to the public internet.

use anyhow::{Context, Result, bail};
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use std::fmt::Write as _;
use std::net::Ipv4Addr;
use std::process::{Command, Stdio};

const RULE_NAME: &str = "Orqella Link";
const COMMENT: &str = "Orqella Link";

/// How widely a matching firewall rule exposes the Link UDP port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirewallScope {
    /// No matching rule.
    None,
    /// Matching rules are limited to the detected LAN.
    Lan,
    /// Matching rules are limited to sources outside the detected LAN.
    Restricted,
    /// At least one matching rule accepts traffic from any source.
    Public,
    /// A rule exists, but its scope could not be determined.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct FirewallReport {
    pub kind: &'static str,
    pub active: bool,
    pub allowed: Option<bool>,
    pub scope: FirewallScope,
    pub port: u16,
    pub networks: Vec<String>,
    pub detail: String,
}

impl FirewallReport {
    pub fn is_allowed(&self) -> bool {
        !self.active || self.allowed == Some(true)
    }

    pub fn summary(&self) -> String {
        let mut out = String::new();
        let _ = write!(
            out,
            "Link UDP port {} · {} ({})",
            self.port,
            self.kind,
            if self.active { "active" } else { "inactive" }
        );
        if !self.networks.is_empty() {
            let _ = write!(out, " · LAN {}", self.networks.join(", "));
        }
        if !self.active {
            out.push_str(" · not filtering");
        } else {
            match self.allowed {
                Some(true) => match self.scope {
                    FirewallScope::Lan => out.push_str(" · allowed from LAN only"),
                    FirewallScope::Restricted => out.push_str(" · allowed from restricted sources"),
                    FirewallScope::Public => out.push_str(" · allowed from anywhere"),
                    FirewallScope::None | FirewallScope::Unknown => {
                        out.push_str(" · allowed (scope unknown)")
                    }
                },
                Some(false) => out.push_str(" · no rule found"),
                None => out.push_str(" · rule state unknown"),
            }
        }
        if !self.detail.is_empty() {
            let _ = write!(out, " · {}", self.detail);
        }
        out
    }
}

/// Inspect the host firewall without elevating.
pub fn status(port: u16) -> FirewallReport {
    let networks = lan_networks();
    if cfg!(target_os = "linux") {
        linux_status(port, networks)
    } else if cfg!(target_os = "macos") {
        macos_status(port, networks)
    } else if cfg!(target_os = "windows") {
        windows_status(port, networks)
    } else {
        FirewallReport {
            kind: "unknown",
            active: false,
            allowed: None,
            scope: FirewallScope::Unknown,
            port,
            networks,
            detail: "unsupported platform; allow the Link UDP port from the LAN manually".into(),
        }
    }
}

/// Add a LAN-scoped rule for the configured Link port. May prompt for sudo/UAC.
pub fn install(port: u16) -> Result<FirewallReport> {
    if port == 0 {
        bail!(
            "[link] port is not set; choose a fixed UDP port (for example 7843) and restart the service"
        );
    }
    if cfg!(target_os = "linux") {
        linux_install(port)
    } else if cfg!(target_os = "macos") {
        macos_install(port)
    } else if cfg!(target_os = "windows") {
        windows_install(port)
    } else {
        bail!(
            "automatic firewall setup is not implemented on this platform; allow UDP {port} from the LAN manually"
        );
    }
}

/// Remove the rule added by [`install`].
pub fn remove(port: u16) -> Result<()> {
    if port == 0 {
        bail!("[link] port is not set");
    }
    if cfg!(target_os = "linux") {
        linux_remove(port)
    } else if cfg!(target_os = "macos") {
        macos_remove()
    } else if cfg!(target_os = "windows") {
        windows_remove()
    } else {
        bail!("automatic firewall removal is not implemented on this platform");
    }
}

fn linux_status(port: u16, networks: Vec<String>) -> FirewallReport {
    if has_command("ufw") {
        let active = systemd_active("ufw");
        if !active {
            return FirewallReport {
                kind: "ufw",
                active: false,
                allowed: Some(true),
                scope: FirewallScope::Unknown,
                port,
                networks,
                detail: "UFW is installed but inactive".into(),
            };
        }
        let (allowed, scope) = ufw_rule_status(port, &networks);
        return FirewallReport {
            kind: "ufw",
            active: true,
            allowed,
            scope,
            port,
            networks,
            detail: if allowed == Some(false) {
                "run `orqella firewall install` to add a LAN-scoped rule".into()
            } else {
                String::new()
            },
        };
    }
    if has_command("firewall-cmd") && systemd_active("firewalld") {
        let scope = firewalld_rule_scope(port, &networks);
        let allowed = scope.map(|scope| scope != FirewallScope::None);
        return FirewallReport {
            kind: "firewalld",
            active: true,
            allowed,
            scope: scope.unwrap_or(FirewallScope::Unknown),
            port,
            networks,
            detail: if allowed == Some(false) {
                "run `orqella firewall install` to add a LAN-scoped rule".into()
            } else {
                String::new()
            },
        };
    }
    if systemd_active("nftables") || systemd_active("iptables") {
        return FirewallReport {
            kind: "nftables",
            active: true,
            allowed: None,
            scope: FirewallScope::Unknown,
            port,
            networks,
            detail: format!(
                "allow UDP {port} from the LAN in your nftables/iptables ruleset; Orqella does not edit those automatically"
            ),
        };
    }
    FirewallReport {
        kind: "none",
        active: false,
        allowed: Some(true),
        scope: FirewallScope::Unknown,
        port,
        networks,
        detail: "no host firewall detected".into(),
    }
}

fn linux_install(port: u16) -> Result<FirewallReport> {
    let networks = lan_networks();
    if has_command("ufw") && systemd_active("ufw") {
        for network in &networks {
            run_elevated(
                "ufw",
                &[
                    "allow",
                    "from",
                    network,
                    "to",
                    "any",
                    "port",
                    &port.to_string(),
                    "proto",
                    "udp",
                    "comment",
                    COMMENT,
                ],
            )?;
        }
        return Ok(linux_status(port, networks));
    }
    if has_command("firewall-cmd") && systemd_active("firewalld") {
        for network in &networks {
            let rule = format!(
                "rule family=ipv4 source address={network} port port={port} protocol=udp accept"
            );
            run_elevated(
                "firewall-cmd",
                &["--permanent", &format!("--add-rich-rule={rule}")],
            )?;
        }
        run_elevated("firewall-cmd", &["--reload"])?;
        return Ok(linux_status(port, networks));
    }
    if systemd_active("nftables") || systemd_active("iptables") {
        bail!(
            "nftables/iptables is active; allow UDP {port} from {} manually. \
             Orqella does not edit a shared ruleset automatically.",
            networks.join(", ")
        );
    }
    Ok(linux_status(port, networks))
}

fn linux_remove(port: u16) -> Result<()> {
    let networks = lan_networks();
    if has_command("ufw") && systemd_active("ufw") {
        for network in &networks {
            run_elevated(
                "ufw",
                &[
                    "delete",
                    "allow",
                    "from",
                    network,
                    "to",
                    "any",
                    "port",
                    &port.to_string(),
                    "proto",
                    "udp",
                ],
            )?;
        }
        return Ok(());
    }
    if has_command("firewall-cmd") && systemd_active("firewalld") {
        for network in &networks {
            let rule = format!(
                "rule family=ipv4 source address={network} port port={port} protocol=udp accept"
            );
            run_elevated(
                "firewall-cmd",
                &["--permanent", &format!("--remove-rich-rule={rule}")],
            )?;
        }
        run_elevated("firewall-cmd", &["--reload"])?;
        return Ok(());
    }
    Ok(())
}

fn macos_status(port: u16, networks: Vec<String>) -> FirewallReport {
    let _ = port;
    let state = Command::new("/usr/libexec/ApplicationFirewall/socketfilterfw")
        .args(["--getglobalstate"])
        .output();
    let Ok(state) = state else {
        return FirewallReport {
            kind: "macos",
            active: false,
            allowed: Some(true),
            scope: FirewallScope::Unknown,
            port,
            networks,
            detail: "application firewall state unavailable".into(),
        };
    };
    let text = String::from_utf8_lossy(&state.stdout).to_ascii_lowercase();
    let active = text.contains("enabled");
    if !active {
        return FirewallReport {
            kind: "macos",
            active: false,
            allowed: Some(true),
            scope: FirewallScope::Unknown,
            port,
            networks,
            detail: "application firewall is disabled".into(),
        };
    }
    FirewallReport {
        kind: "macos",
        active: true,
        allowed: None,
        scope: FirewallScope::Unknown,
        port,
        networks,
        detail: "run `orqella firewall install` to allow the Orqella binary".into(),
    }
}

fn macos_install(port: u16) -> Result<FirewallReport> {
    let exe = std::env::current_exe().context("could not locate the Orqella binary")?;
    let exe = exe.to_string_lossy().to_string();
    run_elevated(
        "/usr/libexec/ApplicationFirewall/socketfilterfw",
        &["--add", &exe],
    )?;
    run_elevated(
        "/usr/libexec/ApplicationFirewall/socketfilterfw",
        &["--unblockapp", &exe],
    )?;
    Ok(macos_status(port, lan_networks()))
}

fn macos_remove() -> Result<()> {
    let exe = std::env::current_exe().context("could not locate the Orqella binary")?;
    run_elevated(
        "/usr/libexec/ApplicationFirewall/socketfilterfw",
        &["--remove", &exe.to_string_lossy()],
    )
}

fn windows_status(port: u16, networks: Vec<String>) -> FirewallReport {
    let profiles = Command::new("netsh")
        .args(["advfirewall", "show", "allprofiles", "state"])
        .output();
    let Ok(profiles) = profiles else {
        return FirewallReport {
            kind: "windows",
            active: false,
            allowed: Some(true),
            scope: FirewallScope::Unknown,
            port,
            networks,
            detail: "Windows Firewall state unavailable".into(),
        };
    };
    let text = String::from_utf8_lossy(&profiles.stdout).to_ascii_lowercase();
    if !text.contains("on") {
        return FirewallReport {
            kind: "windows",
            active: false,
            allowed: Some(true),
            scope: FirewallScope::Unknown,
            port,
            networks,
            detail: "Windows Firewall is off".into(),
        };
    }
    let rule = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={RULE_NAME}"),
        ])
        .output();
    let mut scope = FirewallScope::Unknown;
    let allowed = rule.ok().map(|output| {
        let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        let present = output.status.success() && text.contains("rule name");
        if present {
            scope = if text.contains("localsubnet") {
                FirewallScope::Lan
            } else if text.contains("remoteip")
                && (text.contains("any") || text.contains("0.0.0.0/0"))
            {
                FirewallScope::Public
            } else {
                FirewallScope::Unknown
            };
        }
        present
    });
    FirewallReport {
        kind: "windows",
        active: true,
        allowed,
        scope,
        port,
        networks,
        detail: if allowed == Some(false) {
            "run `orqella firewall install` in an elevated terminal".into()
        } else {
            String::new()
        },
    }
}

fn windows_install(port: u16) -> Result<FirewallReport> {
    let args = [
        "advfirewall",
        "firewall",
        "add",
        "rule",
        &format!("name={RULE_NAME}"),
        "dir=in",
        "action=allow",
        "protocol=UDP",
        &format!("localport={port}"),
        "profile=private",
        "remoteip=LocalSubnet",
    ];
    let _ = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            &format!("name={RULE_NAME}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if let Err(error) = run("netsh", &args) {
        bail!(
            "{error:#}; run this in an elevated PowerShell:\n  netsh {}",
            args.join(" ")
        );
    }
    Ok(windows_status(port, lan_networks()))
}

fn windows_remove() -> Result<()> {
    let args = [
        "advfirewall",
        "firewall",
        "delete",
        "rule",
        &format!("name={RULE_NAME}"),
    ];
    if let Err(error) = run("netsh", &args) {
        bail!(
            "{error:#}; run this in an elevated PowerShell:\n  netsh {}",
            args.join(" ")
        );
    }
    Ok(())
}

fn lan_networks() -> Vec<String> {
    let mut networks = Vec::new();
    if let Ok(interfaces) = NetworkInterface::show() {
        for interface in interfaces {
            if interface.internal || is_virtual_interface(&interface.name) {
                continue;
            }
            for addr in interface.addr {
                let Addr::V4(v4) = addr else { continue };
                if v4.ip.is_loopback() || v4.ip.is_link_local() || !is_lan_ip(v4.ip) {
                    continue;
                }
                let Some(mask) = v4.netmask else { continue };
                let prefix = u32::from(mask).count_ones();
                if !(8..=30).contains(&prefix) {
                    continue;
                }
                let network = Ipv4Addr::from(u32::from(v4.ip) & u32::from(mask));
                let cidr = format!("{network}/{prefix}");
                if !networks.contains(&cidr) {
                    networks.push(cidr);
                }
            }
        }
    }
    if networks.is_empty() {
        networks.extend(
            ["192.168.0.0/16", "10.0.0.0/8", "172.16.0.0/12"]
                .iter()
                .map(|value| value.to_string()),
        );
    }
    networks
}

fn is_lan_ip(ip: Ipv4Addr) -> bool {
    // RFC1918 plus the shared CGNAT/Tailscale range.
    let value = u32::from(ip);
    ip.is_private() || (value & 0xffc0_0000) == 0x6440_0000
}

fn is_virtual_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "docker",
        "br-",
        "veth",
        "virbr",
        "vmnet",
        "vboxnet",
        "tun",
        "tap",
        "tailscale",
        "wg",
        "zt",
        "utun",
        "awdl",
        "llw",
        "bridge",
        "ham",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

fn has_command(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn systemd_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ufw_rule_status(port: u16, networks: &[String]) -> (Option<bool>, FirewallScope) {
    let mut found = false;
    let mut scope = FirewallScope::None;
    for path in ["/etc/ufw/user.rules", "/etc/ufw/user6.rules"] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let public_source = if path.ends_with("user6.rules") {
            "::/0"
        } else {
            "0.0.0.0/0"
        };
        for line in text.lines() {
            if let Some(line_scope) = ufw_line_scope(line, port, networks, public_source) {
                found = true;
                scope = merge_scope(scope, line_scope);
            }
        }
    }
    if found {
        (Some(true), scope)
    } else if std::fs::metadata("/etc/ufw/user.rules").is_ok() {
        (Some(false), FirewallScope::None)
    } else {
        (None, FirewallScope::Unknown)
    }
}

fn ufw_line_scope(
    line: &str,
    port: u16,
    networks: &[String],
    public_source: &str,
) -> Option<FirewallScope> {
    if !rule_line_allows(line, port) {
        return None;
    }
    let source = line
        .split_whitespace()
        .skip_while(|token| *token != "-s")
        .nth(1);
    Some(match source {
        None => FirewallScope::Public,
        Some(value) if value == public_source => FirewallScope::Public,
        Some(value) if value == "0.0.0.0/0" || value == "::/0" => FirewallScope::Public,
        Some(value) if networks.iter().any(|network| network == value) => FirewallScope::Lan,
        Some(_) => FirewallScope::Restricted,
    })
}

fn merge_scope(current: FirewallScope, next: FirewallScope) -> FirewallScope {
    match (current, next) {
        (FirewallScope::Public, _) | (_, FirewallScope::Public) => FirewallScope::Public,
        (FirewallScope::Restricted, _) | (_, FirewallScope::Restricted) => {
            FirewallScope::Restricted
        }
        (FirewallScope::Lan, _) | (_, FirewallScope::Lan) => FirewallScope::Lan,
        _ => FirewallScope::None,
    }
}

fn rule_line_allows(line: &str, port: u16) -> bool {
    if !line.contains("ACCEPT") || !line.contains("-p udp") {
        return false;
    }
    let Some(idx) = line.find("--dport ") else {
        return false;
    };
    let spec = line[idx + "--dport ".len()..]
        .split_whitespace()
        .next()
        .unwrap_or_default();
    if let Some((start, end)) = spec.split_once(':') {
        let (Ok(start), Ok(end)) = (start.parse::<u16>(), end.parse::<u16>()) else {
            return false;
        };
        (start..=end).contains(&port)
    } else {
        spec.parse::<u16>() == Ok(port)
    }
}

fn firewalld_rule_scope(port: u16, networks: &[String]) -> Option<FirewallScope> {
    let output = Command::new("firewall-cmd")
        .args(["--list-rich-rules"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut found = false;
    let mut scope = FirewallScope::None;
    for line in text.lines() {
        if !(line.contains(&format!("port port=\"{port}\""))
            && line.contains("protocol=\"udp\"")
            && line.contains("accept"))
        {
            continue;
        }
        found = true;
        let source = line
            .split("source address=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next());
        let line_scope = match source {
            None => FirewallScope::Public,
            Some(value) if networks.iter().any(|network| network == value) => FirewallScope::Lan,
            Some(_) => FirewallScope::Restricted,
        };
        scope = merge_scope(scope, line_scope);
    }
    if found {
        Some(scope)
    } else {
        Some(FirewallScope::None)
    }
}

fn run_elevated(program: &str, args: &[&str]) -> Result<()> {
    if is_elevated() {
        return run(program, args);
    }
    if cfg!(unix) {
        let mut command = Command::new("sudo");
        command.arg(program).args(args);
        eprintln!("running: sudo {} {}", program, args.join(" "));
        let status = command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("could not run sudo {program}"))?;
        if !status.success() {
            bail!(
                "`sudo {program} {}` failed; run it manually if needed",
                args.join(" ")
            );
        }
        return Ok(());
    }
    run(program, args).with_context(|| {
        format!(
            "run this in an elevated terminal: {program} {}",
            args.join(" ")
        )
    })
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    eprintln!("running: {} {}", program, args.join(" "));
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("could not run {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

fn is_elevated() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ufw_rule_parser_handles_single_ports_and_ranges() {
        assert!(rule_line_allows(
            "-A ufw-user-input -p udp --dport 7843 -s 192.168.0.0/16 -j ACCEPT",
            7843
        ));
        assert!(rule_line_allows(
            "-A ufw-user-input -p udp --dport 32768:60999 -j ACCEPT",
            54633
        ));
        assert!(!rule_line_allows(
            "-A ufw-user-input -p udp --dport 7843 -s 192.168.0.0/16 -j DROP",
            7843
        ));
        assert!(!rule_line_allows(
            "-A ufw-user-input -p tcp --dport 7843 -j ACCEPT",
            7843
        ));
        assert!(!rule_line_allows(
            "-A ufw-user-input -p udp --dport 7843 -j ACCEPT",
            7844
        ));
    }

    #[test]
    fn ufw_rule_scope_distinguishes_lan_and_public() {
        let networks = vec!["192.168.68.0/22".to_string()];
        assert_eq!(
            ufw_line_scope(
                "-A ufw-user-input -p udp --dport 7843 -s 192.168.68.0/22 -j ACCEPT",
                7843,
                &networks,
                "0.0.0.0/0",
            ),
            Some(FirewallScope::Lan)
        );
        assert_eq!(
            ufw_line_scope(
                "-A ufw-user-input -p udp --dport 7843 -j ACCEPT",
                7843,
                &networks,
                "0.0.0.0/0",
            ),
            Some(FirewallScope::Public)
        );
        assert_eq!(
            ufw_line_scope(
                "-A ufw-user-input -p udp --dport 7843 -s 0.0.0.0/0 -j ACCEPT",
                7843,
                &networks,
                "0.0.0.0/0",
            ),
            Some(FirewallScope::Public)
        );
        assert_eq!(
            ufw_line_scope(
                "-A ufw-user-input -p udp --dport 7843 -s 203.0.113.0/24 -j ACCEPT",
                7843,
                &networks,
                "0.0.0.0/0",
            ),
            Some(FirewallScope::Restricted)
        );
        assert_eq!(
            ufw_line_scope(
                "-A ufw-user-input -p udp --dport 7844 -j ACCEPT",
                7843,
                &networks,
                "0.0.0.0/0",
            ),
            None
        );
    }

    #[test]
    fn firewall_summary_reports_rule_scope() {
        let lan = FirewallReport {
            kind: "ufw",
            active: true,
            allowed: Some(true),
            scope: FirewallScope::Lan,
            port: 7843,
            networks: vec!["192.168.68.0/22".into()],
            detail: String::new(),
        };
        assert!(lan.summary().contains("allowed from LAN only"));

        let public = FirewallReport {
            scope: FirewallScope::Public,
            ..lan
        };
        assert!(public.summary().contains("allowed from anywhere"));
    }

    #[test]
    fn virtual_interfaces_are_skipped() {
        assert!(is_virtual_interface("docker0"));
        assert!(is_virtual_interface("tailscale0"));
        assert!(is_virtual_interface("utun3"));
        assert!(!is_virtual_interface("enp6s0"));
        assert!(!is_virtual_interface("en0"));
        assert!(!is_virtual_interface("Ethernet"));
    }

    #[test]
    fn cgnat_is_treated_as_lan() {
        assert!(is_lan_ip("192.168.68.10".parse().unwrap()));
        assert!(is_lan_ip("100.76.91.26".parse().unwrap()));
        assert!(!is_lan_ip("8.8.8.8".parse().unwrap()));
    }
}
