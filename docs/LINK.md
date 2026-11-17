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

## Direct paths and firewalls

Link's SSH stream is direct-only, continuously. If the direct path disappears
mid-session, the SSH connection is closed instead of falling back to a relay.

The hub listens on a UDP port for iroh/QUIC, ephemeral by default. Orqella does
not modify host firewall rules, and no inbound port forward or firewall
exception is normally required. Both devices reach their managed relays
outbound, and iroh establishes a direct path through hole punching.

If you pin a fixed port:

```toml
[link]
port = 7843
```

and choose to add a manual firewall rule, scope it to the local network and
never expose it to the public internet. The port only carries authenticated
Link/iroh traffic.

## What stays local

The Link control plane handles identity, device enrollment, tickets, and
entitlement. It is not an application data path. The local daemon keeps its
configuration, module data, audit history, and backups on the machine.

## Availability

The public client connects only to the Orqella Link service. The control plane,
relay broker, billing integration, and deployment infrastructure are proprietary
and maintained separately.

Local Orqella remains free.
