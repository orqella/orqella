# Orqella Link

Orqella is local-first: the dashboard, modules, and local app proxies run on
your machine without an account. **Orqella Link** is the paid managed service
for reaching another machine.

## What it does

- Signs a device in to your Orqella account.
- Enrolls machines and exposes them in the dashboard machine selector.
- Issues short-lived, device-scoped tickets for peer connections.
- Connects only over direct peer paths. Relay mode is disabled in the daemon,
  browser bridge, and native tunnel; Orqella does not carry application data,
  SSH, or peer API traffic through a relay.
- Keeps application data on the direct peer path. A peer that cannot establish
  a direct path is reported unreachable.

The clients are another Orqella daemon, a browser, the desktop app, and the
mobile app. The browser bridge is opt-in; the native clients use the shared
tunnel; peer daemons use the peer API. All of them are direct-only. A future
metered relay offering would be a separate, explicit opt-in; it is not part of
this build.

## Link a machine

An account owns devices. A **hub** is a computer running the Orqella daemon; a
phone, browser, or desktop app is a **client**. Clients can only see hubs that
are already enrolled. Signing in on a phone does not enroll the computer.

On the computer you want to reach:

1. Open Orqella and go to **Settings → General**.
2. Select **Sign In** in the Orqella Link section.
3. Approve the displayed user code at
   <https://link.orqella.io/login/cli>.
4. Wait for **Connected**. The machine now appears in the Link account page,
   the mobile machine list, the browser client, and the desktop client.

Repeat this on every hub. If the Link control plane is rebuilt with a fresh
database, every machine's saved token is invalid; the daemon reports
**Sign-in needs attention** and must be re-enrolled. `orqella doctor` prints
the live Link state and the next action.

Local unlock is not enrollment. The dashboard's **Sign in with Link** banner
hands a one-time code to a machine that is already linked. Use
`orqella open` and Settings to link the machine first. The handoff uses the
canonical `http://orqella.localhost:9477/` origin; `http://127.0.0.1:9477/`
remains a read-only dashboard fallback, and the banner points back to the
canonical origin when it is needed.

## Direct paths and firewalls

Link's SSH stream is direct-only, continuously. If the direct path disappears
mid-session, the SSH connection is closed instead of falling back to a relay.

The hub listens on UDP 7843 for iroh/QUIC by default. A fixed port keeps the
firewall rule valid across restarts; set `port = 0` only if you deliberately
want an ephemeral port:

```toml
[link]
port = 7843
```

Then add a LAN-scoped rule:

```bash
orqella firewall install
```

The platform installers ask once and run this automatically. The command detects
UFW or firewalld on Linux, the application firewall on macOS, or Windows
Defender Firewall, and adds a rule scoped to the local network. On Linux/macOS
it may prompt for sudo; on Windows, run it in an elevated terminal.
`orqella firewall status` shows the current state, `orqella firewall status
--check` exits non-zero when the rule is missing, and `orqella firewall
uninstall` removes the rule. The configured port only carries authenticated
Link/iroh traffic; do not expose it to the public internet.

If the port changes, the firewall rule must match it. Link tries direct
addresses and normal hole punching, but never falls back to a relay data path.
Devices that cannot reach each other directly are reported offline.

On a host running firewalld, inspecting rich rules requires elevation, so
`orqella firewall status` may report `rule state unknown` even when firewalld is
active. Run `orqella firewall install` from an interactive terminal (it invokes
`sudo firewall-cmd`) or add the LAN-scoped UDP rule manually. Without that rule,
peers on the LAN will show offline because relay fallback is disabled.

## What stays local

The Link control plane handles identity, device enrollment, tickets, and
entitlement. It is not an application data path. The local daemon keeps its
configuration, module data, audit history, and backups on the machine.

## Availability

The public client connects only to the Orqella Link service. The control plane,
relay broker, billing integration, and deployment infrastructure are proprietary
and maintained separately.

Local Orqella remains free.
