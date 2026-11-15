<p align="center">
  <a href="https://orqella.io">
    <img src="logo.png" alt="Orqella" width="96">
  </a>
</p>

<h1 align="center">Orqella</h1>

<p align="center">
  <strong>Your machines, one calm surface.</strong><br>
  A local-first, modular, multi-machine dashboard.
</p>

<p align="center">
  <a href="https://orqella.io">Website</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#modules">Modules</a> ·
  <a href="#desktop-client">Desktop client</a> ·
  <a href="docs/">Documentation</a>
</p>

Orqella runs on your own machines. One small Rust daemon serves the dashboard,
the module pages, and configured local apps on a single loopback port. Your data
stays local by default; there is no account requirement, no hosted database, and
no cloud round-trip for the dashboard itself.

Add pages for the things you actually use — system metrics, a terminal, Home
Assistant, OpenCode, a VM lab, or something you build yourself. Pages are
self-contained modules, sandboxed by the shell, with permissions enforced by the
daemon.

## Why Orqella

- **Local first.** The dashboard is served from `orqella.localhost` on your
  machine. Local use never needs a cloud account.
- **Modular.** A module is a small package with a page, an overview card, or
  both. Modules are ordinary git repositories, not a closed plugin format.
- **Composable.** Use a module as-is, configure it in Settings, or describe a
  page you want and let an AI agent build and install it.
- **Multi-machine.** Switch between enrolled machines from the dashboard.
  Remote access is provided by **Orqella Link**, our managed account and relay
  service.
- **One port.** The shell, modules, local app proxies, and remote machine
  routes all live behind one user-facing listener.

## Quick start

Requirements: [Rust](https://rustup.rs/) stable, Git, and Python 3.11+ if you
plan to use modules with Python sidecars. Linux, macOS, and Windows are
supported.

```bash
git clone https://github.com/orqella/orqella.git
cd orqella
cargo build --release
./target/release/orqella serve
```

Open **http://orqella.localhost:9477/**.

If you prefer the numeric loopback address, **http://127.0.0.1:9477/** serves
the same dashboard.

### Install as a user service

```bash
cargo install --path . --locked
orqella service install
orqella open
```

This installs the per-user service (systemd on Linux, a LaunchAgent on macOS, or
a startup entry on Windows) and starts Orqella at login.

Platform installers that build, install, register the service, and start it are
also available:

```bash
./install-linux.sh
./install-macos.sh
```

```powershell
.\install-windows.ps1
```

On macOS, the installer adds the required `orqella.localhost` entries for Safari
(one `sudo` prompt). Chrome and Firefox do not need those entries.

### Configuration

Orqella creates its config on first run:

| Platform | Config | Data |
| --- | --- | --- |
| Linux | `~/.config/orqella/config.toml` | `~/.local/share/orqella/` |
| macOS | `~/Library/Application Support/orqella/` | `~/Library/Application Support/orqella/` |
| Windows | `%APPDATA%\orqella\` | `%LOCALAPPDATA%\orqella\` |

Set `ORQELLA_HOME` for a portable layout with `config/`, `data/`, and `run/`
under one directory.

## Modules

First-party modules for **System**, **OpenCode**, **VM Lab**, **Voice
Assistant**, **Terminal**, and **Home Assistant** are published under the
[orqella organization](https://github.com/orqella) as they pass review. Modules
are installed as independent git repositories from **Settings → Modules**, or
from the CLI:

```bash
orqella module install https://github.com/orqella/orqella-system
orqella module check ./my-module
orqella module dev ./my-module
```

A module can provide:

- `ui/index.html` — a full page
- `ui/overview.html` — a dashboard card
- `ui/settings.html` — custom module settings
- `backend/` — an optional sidecar for server-side work

Start with the [module specification](docs/MODULE_SPEC.md), the
[host API](docs/HOST_API.md), and the [module author guide](docs/MODULE_AGENT.md).
The shell serves module pages in a sandboxed iframe and enforces every declared
permission on the daemon, not in the browser.

## Remote machines and Orqella Link

Orqella is local-first. When you want to reach another machine, **Orqella Link**
provides managed, account-assisted connectivity: sign in once, enroll your
machines, and select them from the dashboard. Link is a paid hosted service;
local Orqella remains free.

The Link control plane issues short-lived tickets and managed relay credentials
to entitled devices. Application traffic stays direct whenever a path can be
established.

See [docs/LINK.md](docs/LINK.md) for the managed-service boundary.

## Desktop client

The native desktop client lives in [`client/`](client/). It loads the same
Orqella dashboard in a Tauri window, adds native app windows, notifications, and
updates, and uses the shared Link transport for remote machines.

```bash
cd client
cargo build -p orqella-desktop
```

Signed desktop releases are not published yet. See the
[client README](client/README.md) for platform dependencies and packaging
commands.

## Development

Core checks run from the repository root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Desktop client checks run from `client/`:

```bash
cd client
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The dashboard is plain HTML/CSS/JavaScript in `web/`. The browser transport
client is in `web-client/`. The native desktop client is in `client/`, with the
shared Link and tunnel crates in `crates/`. See [the architecture
notes](docs/ARCHITECTURE.md) for the daemon boundaries, [the public
scope](docs/PUBLIC_SCOPE.md) for what is and is not in this repository, and
[the release checklist](docs/RELEASE.md) for packaging.

## Security

Orqella assumes modules are user-installed code, not trusted by default. Sidecars
run sandboxed where the platform supports it, permissions are enforced
server-side, module iframes do not receive the shell session, and support
bundles redact secrets. Read [docs/SECURITY.md](docs/SECURITY.md) for the threat
model and platform guarantees.

To report a vulnerability, please use GitHub's private security advisory flow
rather than a public issue.

## Status

Orqella is in active early development. The local dashboard, module runtime,
browser client, and desktop source build are usable today; APIs, packaging, and
the hosted service are still evolving before a 1.0 release. Expect changes, and
pin to a tag if you need stability. See
[docs/CLEAN_VM_VALIDATION.md](docs/CLEAN_VM_VALIDATION.md) for the clean-machine
release validation guide.

## Community

- [Support](SUPPORT.md)
- [Public scope](docs/PUBLIC_SCOPE.md)
- [Security policy](SECURITY.md)
- [Trademark policy](TRADEMARK.md)

## License

The Orqella core is licensed under the MIT License. See [LICENSE](LICENSE) and
[docs/LICENSING.md](docs/LICENSING.md).

First-party modules are separate repositories under the
[orqella organization](https://github.com/orqella) and are licensed
independently. The hosted Orqella Link service, its control plane, managed
relays, and deployment infrastructure are proprietary and are not part of this
repository.
