---
name: orqella
description: >
  Use for any task involving Orqella: running or troubleshooting the local
  daemon, using the Orqella CLI, configuring machines or Link, building,
  editing, installing, or debugging modules and dashboards, styling a module
  for the active theme, sidecars, permissions, app handoffs, backups, and
  updates. Trigger on "Orqella", "orqella.localhost", "module.toml", Orqella
  module/page/dashboard/theme requests, or when asked to customize the
  Orqella dashboard. Not for unrelated generic web development unless it will
  be installed into Orqella.
---

# Orqella

Orqella is a local-first, modular, multi-machine dashboard. One Rust daemon
serves the shell, module pages, and configured apps on one loopback port.
Modules are independent git repositories; you add capability by building or
installing a module, not by modifying the shell.

Source and documentation: https://github.com/orqella/orqella. The docs live in
the `docs/` directory: https://github.com/orqella/orqella/tree/main/docs.

## Start here

- Dashboard: `http://orqella.localhost:9477/`
- Loopback alias: `http://127.0.0.1:9477/`
- Run in the foreground: `orqella serve`
- Run as a per-user service: `orqella service install`
- Open the dashboard: `orqella open`
- Check the installation: `orqella doctor`
- Print this guide: `orqella skill`
- CLI help: `orqella --help`, `orqella module --help`
- Native desktop client: build with `cargo build -p orqella-desktop` in the
  repo's `client/` directory:
  `https://github.com/orqella/orqella/tree/main/client`

Typical locations:

| Item | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Config | `~/.config/orqella/config.toml` | `~/Library/Application Support/orqella/config.toml` | `%APPDATA%\orqella\config.toml` |
| Data | `~/.local/share/orqella/` | `~/Library/Application Support/orqella/` | `%LOCALAPPDATA%\orqella\` |
| Installed modules | `<data>/modules/<module-id>/` | same | same |
| Theme override | `<config>/theme.css` | same | same |

`ORQELLA_HOME=<dir>` creates a portable `config/`, `data/`, and `run/` layout.

The repository is not the module store. Every module, including first-party
modules, is installed as its own git checkout under the data directory. There
is no `modules/` directory in the Orqella source checkout. First-party modules
live in separate repositories under the `orqella` GitHub organization
(https://github.com/orqella), such as `orqella-system`, `orqella-terminal`, and
`orqella-agent-harness`.

## Install

Requirements: Rust stable, Git, and Python 3.11+ if you use Python sidecars.

Build and run from a checkout:

```bash
git clone https://github.com/orqella/orqella.git
cd orqella
cargo build --release
./target/release/orqella serve
```

Install the binary and per-user service:

```bash
cargo install --path . --locked
orqella service install
orqella open
```

Platform installers build, install, restart the service, and check health:

```bash
./install-linux.sh
./install-macos.sh
```

```powershell
.\install-windows.ps1
```

The macOS installer does not edit `/etc/hosts`; Safari host entries are an
optional `orqella hosts install` step. `orqella update apply` pulls, rebuilds,
installs, and restarts the daemon from the configured checkout.

## Everyday CLI

Run the daemon and manage the service:

```bash
orqella serve                  # foreground on the configured port
orqella serve --port 9478      # temporary alternate port
orqella serve --no-dashboard   # API and peer endpoints only
orqella serve --web            # enable the browser WebRTC bridge
orqella open                   # print and open the dashboard URL
orqella skill                  # print this guide as text
orqella service install
orqella service status
orqella service uninstall
orqella doctor
```

`orqella skill` prints this guide verbatim; redirect it when setting up an agent:

```bash
orqella skill > orqella-skill.md
```

On macOS, Safari needs optional explicit host entries for the canonical origin
and full app surfaces; Chrome, Firefox, and the desktop client do not. The
dashboard also works at `http://127.0.0.1:9477/`:

```bash
orqella hosts print
orqella hosts install
orqella hosts remove
```

Backups, support, updates, and removal:

```bash
orqella backup                  # zip of config, state, modules, module data
orqella support                 # redacted troubleshooting bundle
orqella update check
orqella update configure <repo>
orqella update apply
orqella rotate-peer
orqella uninstall
orqella uninstall --purge       # also deletes config and data
```

`orqella backup` can contain Link credentials, peer tokens, and module data.
Treat it like a password. `orqella support` omits module runtime data and
redacts credentials; share that when asking for help. `orqella uninstall`
removes the service but keeps local state; `--purge` also deletes config and
data.

## Module commands

```bash
orqella module new ./my-page            # scaffold a module repo
orqella module check ./my-page          # validate the manifest and files
orqella module dev ./my-page            # install into the running daemon and open it
orqella module install ./my-page
orqella module install https://github.com/acme/orqella-weather --branch main
orqella module list
orqella module update my-page
orqella module checkpoint my-page "Add forecast"
orqella module remove my-page
orqella module export my-page ./my-page.zip
orqella module verify ./my-page.zip
orqella module import ./my-page.zip
orqella module push my-page <peer>
orqella module key
orqella module trust <public-key-hex> --label "Publisher"
orqella module sign ./my-page
orqella module guide
```

`module new`, `module dev`, `module check`, and `module guide` are the main
authoring commands. `module guide` prints the same full module-authoring guide
the AI builder receives. `module dev` installs the local folder into the running
daemon and opens the page; if the module declares a sensitive permission such
as `backend`, approve it in Settings → Modules before testing the backend.

Local folder installs are copied into the module store and initialized as a git
repository when needed; they are not signature-gated. Git URL installs and git
updates verify a trusted publisher signature by default; pass
`--allow-unsigned` only for code you trust. The dashboard asks for confirmation
before retrying an unsigned install or update.

Private git modules need credentials that git itself can use, such as an SSH
agent or a token embedded in the URL. The daemon disables credential helpers
and terminal prompts.

Updates fast-forward from the module's `origin`, refuse a dirty checkout, and
preserve the module's `data/` directory. A checkpoint commits locally without
pushing. `module remove` deletes the installed checkout, including its runtime
`data/`; back that up first if it matters.

## Build a module

A module is a self-contained git repository. The minimum package is
`module.toml` plus a page, an overview card, or both.

Typical loop:

```bash
orqella module new ./my-page
cd my-page
# edit module.toml and ui/
orqella module check .
orqella module dev .
```

`module new` creates a git repository and commits the scaffold. Commit later
source edits with ordinary git. `module dev` installs or updates the local
folder and opens the page; re-run it after edits, or refresh the page if only UI
assets changed. `module checkpoint <id>` commits the installed copy when you are
working through Orqella rather than the source folder.

```text
my-module/
├── module.toml
├── ui/
│   ├── index.html        # page (optional)
│   ├── overview.html     # overview card (optional)
│   ├── settings.html     # custom Modules-tab UI (optional)
│   └── app.js
├── backend/              # optional sidecar
├── README.md
└── AGENTS.md             # recommended for AI editing
```

Minimal manifest:

```toml
schema = 1
id = "com.example.my-page"
name = "My page"
version = "0.1.0"
api_version = "1"
description = "What this page does."
entry = "ui/index.html"          # optional if overview exists
overview = "ui/overview.html"    # optional card entry
settings = "ui/settings.html"    # optional custom settings UI
overview_default = true          # card starts visible
page_default = true              # page tab starts visible
icon = "ui/icon.svg"             # optional
node_scope = "node"              # node | aggregate | hub
permissions = ["node.metrics", "storage"]

[ui]
overview_width = 2               # preferred grid columns
overview_height = 3              # preferred grid rows

[backend]
kind = "none"                    # none | sidecar

[[config.fields]]
key = "refresh_ms"
type = "number"                  # boolean | number | text | select
label = "Refresh interval"
default = 3000
min = 1000
max = 60000
step = 500
help = "How often the page refreshes."
```

At least one of `entry` or `overview` must exist. Modules with only an overview
card do not appear in page navigation. `page_default = false` keeps a page out
of the tab list until the user enables it in Settings → Modules. Overview cards
declare a preferred size (default `2×3`); users can move and resize them from
**Edit** on the Overview or a custom dashboard. Entry, overview, settings, and
icon paths are relative to the module root and must exist; icons must be image
files.

`node_scope` declares how the page uses machines: `node` acts on the selected
machine, `aggregate` compares multiple machines, and `hub` is only meaningful
on the dashboard-serving machine. Module IDs are 1–96 characters, start with a
letter or digit, and may contain letters, digits, `.`, `_`, and `-`.

Declare settings with `[[config.fields]]`; the host renders the form and
delivers saved values as `context.config`. Supported types are `boolean`,
`number`, `text`, and `select` (with `options = [{ value, label }]`). The host
validates types, bounds, and select options before saving.

Declare interoperating modules under `[[dependencies]]` with `id`, `optional`,
and a short `reason`. This is metadata for install, update, and UI; a required
dependency uses `optional = false`, and missing optional dependencies must
degrade gracefully. Do not use dependencies to share source code.

### Module UI

- Pages are served under `/m/<id>/`, cards under `/m/<id>/overview`, and custom
  settings under `/m/<id>/settings`.
- The daemon injects `<base>` and `window.orqella`; do not add your own bridge.
- The page runs in a sandboxed iframe with an opaque origin. It does not receive
  the shell cookie or the sidecar module token.
- Use `window.orqella.ready(async (context) => ...)` and
  `window.orqella.call(method, params)`.
- `context` contains `module`, `node`, `nodes`, `surface`, `config`, `theme`,
  `theme_id`, `origin`, and `ws_token`. `surface` is `page`, `overview`, or
  `settings`; `config` is this module's saved settings.
- Persist module preferences with `storage.get` / `storage.set`, not shell
  localStorage.
- For node-aware calls, pass `context.node?.id || "local"`; never hard-code a
  machine name or peer ID.
- Provide loading, empty, error, and narrow-screen states.

Minimal page:

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>My page</title>
  <link rel="stylesheet" href="/assets/theme.css">
  <link rel="stylesheet" href="/assets/module.css">
  <link rel="stylesheet" href="app.css">
</head>
<body>
  <main class="stack">
    <header class="page-head"><div><p class="eyebrow">My module</p><h1>My page</h1></div></header>
    <article class="card"><p id="out" class="muted">Connecting…</p></article>
  </main>
  <script src="app.js"></script>
</body>
</html>
```

```js
window.orqella.ready(async (context) => {
  const metrics = await window.orqella.call("system.metrics", {
    node: context.node?.id || "local",
  });
  document.getElementById("out").textContent =
    `${context.node?.name || "local"}: CPU ${Math.round(metrics.cpu_percent)}%`;
});
```

For a module WebSocket, connect to
`ws://<host>/m/<id>/ws/<path>` (or `/n/<node>/m/<id>/ws/<path>` for a remote
module). The SDK adds `__orqella_ws` automatically. Modules that create sockets
inside a Worker or bypass the SDK must append `context.ws_token` themselves.
The daemon proxies the socket only when the module has the `backend` permission
and its sidecar is running.

### Styling

Load the theme and core module styles before the module's own stylesheet:

```html
<link rel="stylesheet" href="/assets/theme.css">
<link rel="stylesheet" href="/assets/module.css">
<link rel="stylesheet" href="app.css">
```

Use the shipped primitives before inventing new ones: `.card`, `.button`,
`.field`, `.chip`, `.empty`, `.eyebrow`, `.lede`, `.row`, `.spread`, `.wrap`,
`.muted`, `.small`, `.mono`, `.grid-2`, `.metric`, and `.icon-button`.
`module.css` is the module-facing subset of the shell's visual system; do not
re-create buttons, cards, forms, or typography in the module stylesheet, and do
not load the shell's `styles.css`.

Use theme tokens rather than fixed colors. Key tokens include `--bg`,
`--bg-soft`, `--panel`, `--ink`, `--muted`, `--line`, `--mint`, `--accent`,
`--amber`, `--red`, `--radius`, `--font-sans`, and `--font-mono`. The full
contract is in [`docs/THEME.md`](https://github.com/orqella/orqella/blob/main/docs/THEME.md).

Use `context.theme` (`dark` or `light`) and `context.theme_id` for non-CSS
decisions. For canvas, terminal, or chart surfaces, derive colors and
typography from the same tokens instead of shipping a fixed palette. This is a
soft default: if the user asks for a different visual direction, state the
divergence and keep the module usable in the selected theme.

### Permissions and host APIs

Declare every capability the module uses. Only implemented permissions are
accepted; reserved namespaces are rejected by the validator.

| Permission | Grants |
| --- | --- |
| `node.info` | Hostname, OS, architecture. |
| `node.metrics` | CPU, memory, load, uptime, processes, disks, networks, temperatures, GPUs. |
| `node.services` | List configured proxy mounts. |
| `nodes.read` | Local and peer machine list. |
| `node.proxy:<mount>` | A configured proxy mount. |
| `app.open` | Open a dedicated app surface. |
| `agent` | Run the configured default agent with write access to a requested directory. Sensitive. |
| `media.record` | Microphone recording through the trusted shell. |
| `fs.pick` | Open the trusted-shell folder picker and receive the chosen path. |
| `backend` | Call this module's sidecar. |
| `storage` | Module-private key/value storage. |

`fs.read:*`, `fs.write:*`, `net.fetch:*`, `secrets:*`, and `node.action:*` are
reserved and rejected until a scoped host API exists.

Host APIs:

| Call | Permission | Purpose |
| --- | --- | --- |
| `module.info` | none | Module ID, name, version, node scope. |
| `system.info` | `node.info` | Local hostname, OS, architecture. |
| `system.metrics` | `node.metrics` | Metrics; pass `node` for a peer. |
| `proxy.list` / `services.list` | `node.services` | Configured proxy mounts. |
| `nodes.list` | `nodes.read` | Local machine plus peers with online state. |
| `storage.get` / `storage.set` | `storage` | Module-private JSON storage. |
| `backend.request` | `backend` | Forward an HTTP request to the module sidecar. |
| `proxy.status` | `node.proxy:<mount>` | Reachability and app URL for a mount. |
| `app.open` | `app.open` | Open a full app surface; accepts optional `path` and `handoff`. |
| `module.open` | none | Open this module's own page at a root-relative path. |
| `system.pick_directory` | `fs.pick` | Trusted-shell folder picker; returns the chosen path. |
| `hooks.list` / `hooks.run` | none / hook-specific | Discover and run named hooks. |
| `agent.start` / `agent.status` | `agent` | Lower-level background agent jobs. |
| `media.record.start` / `media.record.stop` | `media.record` | Trusted-shell microphone bridge. |

Stable hooks are `agent.prompt` and `conversation.open`, both requiring `agent`.
`agent.prompt` starts the configured default agent, optionally waits for a
conversation ID, and can return an `open` payload. `conversation.open` reopens a
job or session in the configured viewer. Preset defaults point at the Agent
Harness module (`/?harness=<preset>&session={session}&cwd={root}`), which owns the session
index and routes to the right conversation. All presets render in the module;
a session with a workspace is adopted and replayed. An OpenCode session without
a workspace, or one that cannot load, falls back to the native workspace after
pairing.

`backend.request` takes `{ method?, path, body? }`; `path` is required and the
daemon forwards the request to the module sidecar.

Sensitive permissions (`backend`, `agent`, `media.record`, `app.open`, and
`node.proxy:*`) are not auto-granted. Install or update, then review and save
the permission sheet in Settings → Modules before the module can use them.

### Sidecars

Use a sidecar only when the host APIs cannot do the job. A sidecar is real code
execution as the logged-in user, sandboxed by daemon policy by default.

```toml
[backend]
kind = "sidecar"
command = ["python3", "backend/server.py"]
python = ">=3.11"
requirements = "backend/requirements.txt"
health_path = "/health"
ready_timeout_ms = 15000
# read_paths = ["~/.config/my-service"]
# sandbox = false
# platforms = ["linux", "macos"]
```

- The daemon allocates a loopback port and sets `ORQELLA_MODULE_PORT`,
  `ORQELLA_MODULE_TOKEN`, `ORQELLA_MODULE_ID`, `ORQELLA_NODE`,
  `ORQELLA_HOST_HOME`, `ORQELLA_MODULE_CONFIG`, and `HOME`
  (`<module>/data/home`).
- The sidecar must bind `127.0.0.1:<port>` and return 2xx from the configured
  `health_path`.
- `backend.request` adds `X-Orqella-Module-Token`; do not send the token
  yourself.
- `backend.read_paths` are bound read-only inside the sandbox. Do not write
  outside the module directory.
- `backend.network` is reserved; sidecars currently share the host network.
- `backend.platforms` is an optional allow-list (`linux`, `macos`, `windows`).
  On other hosts the sidecar is not started and non-backend surfaces still run.
- Python requirements create a managed venv under the daemon data directory at
  `sidecar-venvs/<module-id>` and refresh when the requirements file changes.
- Sidecar logs are in the daemon data directory. Use **Show log** in
  Settings → Modules when startup or `backend.request` fails.

`[backend] sandbox = false` opts out of filesystem sandboxing for modules that
need local desktop, keyring, model, or setup access. The permission review must
show an explicit warning and require an acknowledgment before granting
`backend`. Treat the sidecar as trusted code either way.

For a prebuilt sidecar, declare exactly one of `url` or `path` per artifact,
plus a SHA-256 digest:

```toml
[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/releases/my-sidecar-linux-x86_64"
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
args = ["--serve"]

[[backend.artifacts]]
platform = "macos-aarch64"
path = "backend/bin/my-sidecar-macos-aarch64"
sha256 = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
```

URL artifacts are cached under `data/.sidecar-artifacts/` and downloads are
capped at 256 MB. `path` artifacts stay in the module repository. Both are
checksum-verified on every start. `backend.command` is the fallback when no
artifact matches the host. Private modules should use `path` or a pre-signed
URL; artifact URLs are fetched without extra authentication headers.

## Install, update, and signing

- Local folder: `orqella module install ./my-module` — no signature required.
- Git URL: `orqella module install <git-url>` — the cloned tree must contain a
  trusted `ORQELLA_SIGNATURE.json` unless `--allow-unsigned` is passed.
- Git update: `orqella module update <id>` — same signature check after the
  fast-forward pull.
- Trust a publisher: `orqella module trust <public-key-hex> --label "Name"`.
- Show this daemon's public identity: `orqella module key`.
- Sign a directory: `orqella module sign ./my-module` writes
  `ORQELLA_SIGNATURE.json`.
- Export/import: `orqella module export <id> <file>` and
  `orqella module import <file>`; imports verify the publisher signature.
  `orqella module verify <file>` checks signature and trust without installing.
- Peer copy: `orqella module push <id> <peer>` exports a signed package to a
  Link machine.

`orqella build "..."` and the Build page run the configured default agent against
a staging directory and install a validated module. The Build page also edits
installed modules: edits run against a copy, must keep the module ID, and are
validated before replacing the installed module. Agent jobs persist across
daemon restarts. `[agent].timeout_secs = 0` (the default) means no timeout; a
positive value stops the agent process group, though a service-backed harness
may keep its session running.

## Dashboards, settings, and themes

Settings has four tabs:

- **General** — daemon settings, theme, Link sign-in, machines, and module
  installation/creation.
- **Agents** — the daemon's headless default agent, the default agent for new
  conversations, workspace behavior, and the embedded agent registry.
- **Dashboards** — create, rename, and delete custom dashboards.
- **Modules** — select a module, enable/disable it, open it, export or remove
  it, review permissions, edit generated settings, inspect repository state,
  restart the sidecar, and show its log.

The **Overview** dashboard is the protected default. It opens from the Orqella
brand in the top-left and cannot be deleted. Custom dashboards are shell pages,
not modules: they have no manifest, page iframe, sidecar, permissions, or
package. Dashboard definitions and selected cards are shared with every client
connected to the same machine; card positions and sizes stay local to each
device. **Edit** on a dashboard shows the card picker and grid controls. Page
tabs and dashboards are reordered by dragging their grips while editing; module
page visibility remains per device.

Built-in themes: `nocturne` (default), `signal`, `daybreak`, `foundry`, `cyber`,
`btop`, `catppuccin-latte`, and `catppuccin-mocha`. The picker is Settings →
General → Theme; the selected ID is stored as `theme = "..."` in `config.toml`.
Legacy IDs `tokyo-night` and `vscode-dark-modern` are accepted as aliases;
`catppuccin-latte` and `catppuccin-mocha` are now first-class themes.

To customize any theme, append CSS to `<config>/theme.css`. The server appends
it after the selected theme with `no-store` caching, so it can override tokens
and selectors. The Theme card in Settings shows the resolved path and has a
**Reload** button.

Custom themes are standalone files under `<config>/themes/<id>.css` with a
`<id>.json` metadata sidecar; they appear in the same picker. The Theme card's
prompt uses the configured default agent to create a complete new theme and
select it. Generating a theme does not modify the personal override, and each
generation creates a separate theme rather than editing an existing one.

### Revising a custom theme

A generated theme is installed as a snapshot. Editing the workshop copy after
the job completes does not change the installed theme, because the daemon only
reads `<config>/themes/<id>.css` and its sidecar; it does not watch the job
workspace.

To make theme edits live, write the complete CSS to
`<config>/themes/<id>.css` and update `<id>.json` (`name`, `description`,
`color_scheme`; keep `created_at`). Keep the id stable so the existing
`theme = "<id>"` selection keeps working, back up both files first, then reload
the dashboard. `/assets/theme.css` and the picker are read from disk with
`no-store`, so no daemon restart or re-generation is needed. Create a new id
only when the user wants a second theme; a new id leaves the old one in the
picker.

Treat the installed files as the source of truth. Whenever you revise a theme,
sync the installed CSS and sidecar as part of the edit and verify what the
daemon serves instead of leaving the change only in the workshop copy.

When promoting a custom theme to a built-in, move its `<id>.css` and
`<id>.json` out of `<config>/themes` (keep a backup) and point
`theme = "<builtin-id>"` at the built-in. Otherwise the picker shows two themes
with the same name.

Module CSS must follow the active theme unless the user explicitly asks
otherwise. Use tokens and primitives rather than hard-coded colors, and use
`context.theme` / `context.theme_id` for canvas or native-feeling surfaces.

## App surfaces

A full web app is exposed through a configured proxy mount, not a module-owned
reverse proxy. Add the mount to `config.toml`:

```toml
[[proxy]]
id = "myapp"
title = "My App"
base_url = "http://127.0.0.1:8080"
host = "myapp.orqella.localhost"
websocket = true
frame = false
```

Restart the daemon after editing `config.toml`. If the generated config already
contains `proxy = []`, remove that line first; TOML treats `proxy = []` and
`[[proxy]]` as the same key and reports a duplicate key error.

A dedicated `host` is required for `app.open`; path-only app surfaces are
rejected. The app runs on its own browser origin. Set `frame = true` only for a
trusted upstream that sends `X-Frame-Options` or CSP `frame-ancestors`, so the
proxy can allow the shell origin to embed it.

From a module:

```js
await window.orqella.call("app.open", {
  mount: "myapp",
  node: context.node?.id || "local",
  handoff: "/auth/connect/one-time-code", // optional, loaded first
  path: "/some/deep/link",                // optional, loaded after handoff
});
```

`path` and `handoff` are validated same-origin paths. `path` may contain
`{server_key}`, which the daemon replaces with the URL-safe base64 of the final
app origin. Apps must be root-relative and same-origin on their app origin.
Apps that require their own service worker are not currently supported.

Browser application data is direct WebRTC only. Managed Link credentials are
used for bootstrap/signaling, and relayed application connections are rejected.
The browser client supports full apps, not module pages; use a native client
when direct WebRTC is unavailable.

## Machines and Link

Orqella is local-first; local use is free and does not need an account. To
reach another machine, sign in to **Orqella Link** from Settings → General →
Sign In and enroll the machine. Link is the paid managed service; Link-managed
machines appear in the machine selector automatically.

- Manual direct-URL and static SSH peer configuration is not supported; Link is
  the only remote-machine transport.
- Peer tokens never reach module pages or embedded applications.
- `orqella rotate-peer` rotates this machine's peer secret and refreshes its
  Link registration.
- Wake-on-LAN is available for peers that support it; selecting a sleeping
  machine sends a magic packet and can fall back to the previous machine.
- Browser access is opt-in with `[web] enabled = true` or `orqella serve --web`
  and requires a Link browser session. It never binds a public TCP port.

## Troubleshooting

- **Dashboard does not resolve:** use `http://127.0.0.1:9477/`; on macOS run
  the optional `orqella hosts install` only if Safari needs the canonical
  origin or full app surfaces.
- **Daemon not responding:** run `orqella doctor`, then
  `orqella service status`; restart the service if needed.
- **Port already in use:** `orqella serve --port 9478`.
- **Module missing from navigation:** it may be overview-only, disabled, or
  `page_default = false`. Enable the page in Settings → Modules.
- **Permission errors:** review and save the module's permission sheet in
  Settings → Modules. Sensitive permissions start pending.
- **Sidecar will not start:** confirm `backend` is granted, the configured health
  path returns 2xx, Python meets `backend.python`, requirements installed, and
  `read_paths` are declared. Open **Show log** in Settings → Modules.
- **Unsigned install or update:** trust the publisher with
  `orqella module trust`, or use `--allow-unsigned` for code you trust.
- **App surface fails:** confirm the mount has a `host`, the upstream is
  reachable, and `frame = true` only when the upstream is meant to be embedded.
- **Config is invalid:** fix `config.toml`; restore from `orqella backup` if
  needed.
- **Need help:** run `orqella support` and share the redacted zip.

## Working in the Orqella source checkout

If the task is to change Orqella itself:

- Read the repository docs, starting with
  [`ARCHITECTURE.md`](https://github.com/orqella/orqella/blob/main/docs/ARCHITECTURE.md)
  and [`SECURITY.md`](https://github.com/orqella/orqella/blob/main/docs/SECURITY.md).
- Keep the daemon loopback-only by default and preserve the one-port model.
- Keep module integrations in modules; add reusable host actions to the hook
  registry rather than one-off daemon endpoints.
- Do not give module iframes the shell cookie or sidecar token. Enforce
  permissions server-side.
- Do not put secrets in the repo, module packages, config examples, or logs.
- Run `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace`.
- Update the relevant file under `docs/` in the repository when behavior or a
  contract changes.

Useful docs under `https://github.com/orqella/orqella/tree/main/docs`:

- [`MODULE_SPEC.md`](https://github.com/orqella/orqella/blob/main/docs/MODULE_SPEC.md)
- [`MODULE_AGENT.md`](https://github.com/orqella/orqella/blob/main/docs/MODULE_AGENT.md)
- [`HOST_API.md`](https://github.com/orqella/orqella/blob/main/docs/HOST_API.md)
- [`THEME.md`](https://github.com/orqella/orqella/blob/main/docs/THEME.md)
- [`MODULE_REPOS.md`](https://github.com/orqella/orqella/blob/main/docs/MODULE_REPOS.md)
- [`SECURITY.md`](https://github.com/orqella/orqella/blob/main/docs/SECURITY.md)
- [`SIGNING.md`](https://github.com/orqella/orqella/blob/main/docs/SIGNING.md)
- [`ARCHITECTURE.md`](https://github.com/orqella/orqella/blob/main/docs/ARCHITECTURE.md)
- [`RELEASE.md`](https://github.com/orqella/orqella/blob/main/docs/RELEASE.md)
- [`adr/`](https://github.com/orqella/orqella/tree/main/docs/adr)

## Output expectations

When asked to build or change a module, dashboard, theme, or app surface, write
the files instead of only describing them. Validate with
`orqella module check <path>` and try it with `orqella module dev <path>`.
Report what you verified and what still needs a desktop, mobile, or
platform-specific check.

Ask before destructive actions such as removing a module, deleting module
`data/`, or running `orqella uninstall --purge`. Keep user data in the module's
`data/` directory and never commit caches, virtualenvs, models, or logs.
