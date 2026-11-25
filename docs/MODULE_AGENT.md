# Build an Orqella module

You are working inside an Orqella module staging directory. The user has described a dashboard page they want. Build it here.

## What you are building

An Orqella module is a self-contained dashboard page. It must work without modifying the Orqella daemon.

The minimum package is:

```text
module.toml
ui/index.html
```

A module may instead provide `ui/overview.html` as an overview card, or both a page and a card. The page gets a navigation entry; the card can appear on the protected Overview dashboard or any custom dashboard. The page may load local CSS and JavaScript from `ui/`. A sidecar backend is optional and should be used when the page needs real server-side work that the host APIs do not provide.

## Rules

- Keep all files inside this staging directory.
- Do not read or write outside it, except that you may run `orqella module install .`
  from this directory to install the finished module into the local daemon.
- Do not add secrets.
- Do not assume a specific user, hostname, port, or operating system.
- Use the `window.orqella` SDK for host access. Do not try to read the dashboard's cookies.
- Declare every host permission you use in `module.toml`.
- Prefer static HTML/CSS/JS. A sidecar must be explicitly configured.
- If a Python sidecar needs packages, declare `backend.python` and
  `backend.requirements`; Orqella creates and maintains the virtual environment.
  Do not assume global `pip` packages or a system Python version. Keep pure
  standard-library modules portable across supported Python versions.
- Declare `backend.platforms` when the sidecar depends on platform tools (for
  example PipeWire, systemd, or a desktop shell). Do not claim cross-platform
  support you have not implemented.
- The page must be useful on a narrow phone screen as well as a desktop.
- If the page needs data from another machine, accept the node from `orqella.context` or let the shell choose it. Do not hard-code a machine name.
- Start from the core Orqella visual system. Link `/assets/theme.css`, then `/assets/module.css`, then your own stylesheet. Use the shipped primitives (`.card`, `.button`, `.field`, `.chip`, `.empty`, `.row`, `.muted`, `.small`, `.mono`, `.grid-2`, `.metric`, `.icon-button`) before adding module-specific styles. This is a soft default: the user may ask for a different visual direction, and you may override a primitive when core does not provide an appropriate style. State the divergence.
- Respect `context.theme`/`context.theme_id` for non-CSS decisions. For canvas or terminal surfaces, derive colors and typography from the same tokens (`--bg`, `--ink`, `--mint`, `--font-mono`) instead of hard-coding a palette.
- If the module integrates with another Orqella module, declare it under
  `[[dependencies]]` with `id`, `optional`, and a short `reason`. Do not assume
  the dependency is installed; handle a missing optional dependency gracefully.
  Do not use dependencies as a source-code sharing mechanism.
- The module is its own git repository. Commit a coherent initial state so the user can roll back.
- Keep runtime data in `data/`; it is ignored. Do not commit caches, virtualenvs, model downloads, or logs.

## module.toml

```toml
schema = 1
id = "com.example.my-page"
name = "My page"
version = "0.1.0"
api_version = "1"
description = "What this page does."
entry = "ui/index.html"      # optional if overview is present
overview = "ui/overview.html" # optional card entry
settings = "ui/settings.html" # optional custom Modules-tab UI
overview_default = true       # whether the card starts visible
page_default = true           # whether the page tab starts visible
icon = "ui/icon.svg"
node_scope = "node"           # node | aggregate | hub
permissions = ["node.metrics", "storage"]

[ui]
surface = "page"              # page | app
overview_width = 2            # preferred card width in grid columns
overview_height = 3           # preferred card height in grid rows

[backend]
kind = "none"                 # none | sidecar

[[config.fields]]
key = "refresh_ms"
type = "number"               # boolean | number | text | select
label = "Refresh interval"
default = 3000
min = 1000
max = 60000
step = 500
```

Module settings are declared with `[[config.fields]]` and render automatically on the Settings page. Read the saved values from `context.config`; do not hard-code user preferences. A module must define `entry`, `overview`, or both.

Overview cards should declare a preferred size with `ui.overview_width` and `ui.overview_height` in dashboard grid cells (default `2x3`). Size the card for its normal content so it does not need an internal scrollbar; a dashboard uses this as the starting size, and users can move and resize cards from **Edit** on the Overview or any custom dashboard. Cards keep their positions; empty cells stay empty. Page and dashboard tabs are reordered by dragging their grips in the sidebar (or the mobile menu) while editing. Custom dashboard definitions, visible cards, and layouts are shared by every client of the daemon. Each dashboard keeps a desktop layout and a separate mobile order, and the desktop grid folds down to fit narrow windows without horizontal scrolling. Custom dashboards are created in **Settings → Dashboards**; they use the same cards and are not modules.

### Node scope

- `node`: the page acts on the machine selected in the dashboard.
- `aggregate`: the page compares or combines multiple machines.
- `hub`: the page is meaningful only where the dashboard is served.

### Permissions

Only ask for what you use:

| Permission | Meaning |
| --- | --- |
| `node.info` | Hostname, OS, architecture. |
| `node.metrics` | CPU, memory, load, uptime, process count, disks, networks, temperatures, and GPUs where available. |
| `node.services` | List configured proxy mounts. |
| `nodes.read` | List local and peer machines. |
| `storage` | Module-private key/value storage. |
| `backend` | Talk to this module's sidecar. |
| `node.proxy:<mount>` | Use a configured proxy mount such as `myapp`. |
| `node.proxy.manage:<mount>` | Create or update this module's own proxy mount. Sensitive. |
| `app.open` | Ask the shell to open a dedicated app surface. |
| `agent` | Run the configured default agent in a requested directory. Sensitive. |
| `media.record` | Record microphone audio through the trusted shell. |

## The page or overview card

`ui/index.html` or `ui/overview.html` is served inside a sandboxed frame. The daemon injects the SDK automatically. Do not add your own `<script src="/sdk/...">`.

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>My page</title>
  <link rel="stylesheet" href="/assets/theme.css">
  <link rel="stylesheet" href="/assets/module.css">
  <style>
    body { margin: 0; background: var(--bg); color: var(--ink); font: 15px var(--font-sans); }
    main { padding: 28px; max-width: 1000px; margin: auto; }
  </style>
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

## Host API

### `system.info`

Returns local host metadata. Requires `node.info`.

### `system.metrics`

Params: `{ node?: string }`. Returns CPU, memory, load, uptime, process count, OS and kernel information. Requires `node.metrics`.

### `nodes.list`

Returns the local machine and configured peers with online state. Requires `nodes.read`.

### `storage.get` / `storage.set`

Params: `storage.get { key }`, `storage.set { key, value }`. The store is private to this module. Requires `storage`.

### `backend.request`

Params: `{ method?, path, body? }`. Calls this module's sidecar. Requires `backend` and a sidecar backend. The host adds the sidecar module token; do not send one yourself.

### `proxy.status` / `app.open`

Params: `{ mount, node? }`. Use a declared `node.proxy:<mount>` permission for status. `app.open` asks the shell to show a full app surface; it requires `app.open` and accepts optional `node`, `path`, and `handoff` values.

If your module owns an app mount, declare
`node.proxy.manage:<mount>` and call `proxy.configure` from the module's
settings UI instead of asking the user to edit `config.toml`. Params:
`{ mount, base_url, title?, websocket?, frame? }`. The host is always
`<mount>.<canonical-host>`; `frame` is for trusted upstreams that are meant to
be embedded.

If your module wraps a web app, use the configured proxy mount; do not add a
second reverse proxy or product-specific core route. The shell assigns an
isolated app origin from deployment config or a wildcard suffix, and the app
must be root-relative on that origin. Current browser app data is direct WebRTC
only — managed relay is signaling only and relayed application connections are
rejected. Apps that require their own service worker are not supported yet.
Test the app surface on a dedicated origin, including any WebSocket/SSE flows.

`ready` runs once per page document. When a page opens an app surface, the
shell reloads that page the next time it is selected, so a launcher flow such as
"pair, then open the app" runs again on a fresh document.

## Sidecar backend

A sidecar is an ordinary local process started by Orqella. It is the default choice when the page needs to read files, call a non-HTTP API, run a command, or keep long-lived state.

```toml
[backend]
kind = "sidecar"
command = ["python3", "backend/server.py"]
health_path = "/health"
ready_timeout_ms = 15000
# read_paths = ["~/.config/my-service"]
# sandbox = false  # opt out of the sidecar sandbox
```

A sidecar can use a prebuilt executable for each host instead of an interpreter
command:

```toml
[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/releases/my-sidecar-linux-x86_64"
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
```

Private modules can ship their own binaries instead of using a public download
host:

```toml
[[backend.artifacts]]
platform = "linux-x86_64"
path = "backend/bin/my-sidecar-linux-x86_64"
sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
```

Use exactly one of `url` or `path`, plus the daemon's target names (`linux`,
`macos`, `windows`) and architecture (`x86_64`, `aarch64`, `arm`, `x86`).
Artifacts are checksum-verified on every start, marked executable, and run with
the artifact's `args`. If no artifact matches the host, `backend.command` is the
fallback. Keep `url` artifacts directly accessible without extra authentication
headers; use a public/internal host or a pre-signed URL.

The daemon sets:

- `ORQELLA_MODULE_ID`
- `ORQELLA_MODULE_PORT` — bind an HTTP server to `127.0.0.1:<port>`
- `ORQELLA_MODULE_TOKEN` — require it on every request via `X-Orqella-Module-Token`
- `ORQELLA_NODE`
- `ORQELLA_HOST_HOME`
- `ORQELLA_MODULE_CONFIG` — this module's saved settings as JSON
- `HOME` — `<module>/data/home`, with legacy home directories migrated once

The sidecar must expose the configured `backend.health_path` (default `/health`) and return 2xx when ready. It runs as the logged-in user under the daemon sidecar policy. On Linux the daemon may use a read-only system filesystem, a writable module directory, private `/tmp`, and a memory limit. Do not try to write outside the module directory. Declare any host files you need read-only in `backend.read_paths`; they are bound into the sandbox. Treat this as real code execution on the machine.

## Validation

Before finishing:

1. Run `orqella module check .` if the CLI is available.
2. Confirm the entry file exists and is relative.
3. Confirm every `orqella.call` method has a declared permission.
4. Confirm the page has no hard-coded `/home/...` or machine-specific URLs.
5. Confirm the page has a useful empty/error state.
6. Commit the module in its own repository.
7. Install the finished module with `orqella module install .` when the CLI is
   available. This is explicitly allowed: the CLI resolves `.` against this
   directory and writes the module into the daemon's own module store.

## Output

When done, the daemon will validate and install the module. You may also run
`orqella module install .` yourself to install the finished module. Do not stop
at a plan: create the files.
