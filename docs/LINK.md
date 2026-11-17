# Orqella Link

Orqella is local-first: the dashboard, modules, and local app proxies run on
your machine without an account. **Orqella Link** is the paid managed service
for reaching another machine.

## What it does

- Signs a device in to your Orqella account.
- Enrolls machines and exposes them in the dashboard machine selector.
- Issues short-lived, device-scoped tickets for peer connections.
- Provides managed relay credentials so peers can establish a path through
  difficult NATs.
- Keeps application data on the direct peer path. SSH access is not opened over
  a relayed path.

## Host firewall and direct paths

Link's SSH stream is direct-only. The hub listens on a UDP port for iroh/QUIC.
By default the port is ephemeral, which makes a firewall rule unstable. Set a
fixed port:

```toml
[link]
port = 7843
```

Then add a LAN-scoped rule:

```bash
orqella firewall install
```

The command detects UFW or firewalld on Linux, the application firewall on
macOS, or Windows Defender Firewall, and adds a rule scoped to the local network.
On Linux/macOS it may prompt for sudo; on Windows, run it in an elevated
terminal. `orqella firewall status` shows the current state and
`orqella firewall remove` removes the rule. The configured port only carries
authenticated Link/iroh traffic; do not expose it to the public internet.

If the port changes, the firewall rule must match it. A fixed `[link] port` is
the recommended setting for any hub that will accept direct connections.

## What stays local

The Link control plane handles identity, device enrollment, tickets, and
entitlement. It is not an application data path. The local daemon keeps its
configuration, module data, audit history, and backups on the machine.

## Availability

The public client connects only to the Orqella Link service. The control plane,
relay broker, billing integration, and deployment infrastructure are proprietary
and maintained separately.

Local Orqella remains free.
