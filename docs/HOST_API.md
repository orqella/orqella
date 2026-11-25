# Orqella host API (v1)

All calls from a module page go through the shell bridge:

```js
window.orqella.ready(async (context) => {
  const metrics = await window.orqella.call("system.metrics", {
    node: context.node?.id || "local",
  });
});
```

The init context contains `module`, `node`, `nodes`, `surface`, `config`, `theme`,
`theme_id`, `origin`, and `ws_token`. `theme` is `dark` or `light`; `theme_id`
is the selected theme id (built-in or custom). `ws_token` is the per-module token
required by `/m/<id>/ws/...`; the SDK adds it automatically, but modules that
create sockets in a Worker must append it as `__orqella_ws`. Use the other
fields for non-CSS visual decisions.

## `module.info`

No permission. Returns the module ID, name, version, and node scope.

## `system.info`

Requires `node.info`. Returns local hostname, OS, and architecture.

## `system.metrics`

Requires `node.metrics`.

Params:

```json
{ "node": "local" }
```

Returns:

```json
{
  "hostname": "studio",
  "os": "Linux",
  "kernel": "6.12",
  "arch": "x86_64",
  "uptime_secs": 123456,
  "cpu_percent": 4.2,
  "cpu_cores": [3.1, 8.0, 1.2],
  "cpu_count": 16,
  "load_one": 0.4,
  "load_five": 0.3,
  "load_fifteen": 0.2,
  "memory_total": 34359738368,
  "memory_used": 8589934592,
  "memory_percent": 25.0,
  "swap_total": 0,
  "swap_used": 0,
  "swap_percent": 0.0,
  "processes": 412,
  "disks": [{ "name": "/dev/nvme0n1", "mount": "/", "filesystem": "btrfs", "total": 0, "used": 0, "available": 0, "percent": 0.0, "removable": false }],
  "networks": [{ "interface": "enp6s0", "received_total": 0, "transmitted_total": 0, "received_rate": 0.0, "transmitted_rate": 0.0 }],
  "temperatures": [{ "label": "k10temp Tctl", "current": 44.0, "max": 90.0 }],
  "gpus": [{ "name": "card0", "driver": "amdgpu", "busy": 4, "temperature": 38.0 }],
  "batteries": [{ "name": "BAT0", "status": "Discharging", "percent": 74.0 }],
  "captured_at": "2026-11-01T20:00:00.000Z"
}
```

`cpu_cores`, `disks`, `networks`, `temperatures`, `gpus`, and `batteries` may be empty when the platform cannot report them. Network rates are bytes per second since the previous sample.

The daemon forwards the request to the selected peer's authenticated peer API.

## `proxy.list` / `services.list`

Requires `node.services`. Returns configured proxy mounts and basic metadata.
`proxy.list` is the current name; `services.list` remains as a compatibility
alias. There is no separate local-service inventory API.

## `nodes.list`

Requires `nodes.read`. Returns the local machine plus peers with online state, latency, and last error.

## `storage.get` / `storage.set`

Requires `storage`.

```json
{ "key": "settings" }
```

```json
{ "key": "settings", "value": { "compact": true } }
```

Storage is namespaced to the module ID and kept under the daemon's data directory. It is not a secret store.

## `backend.request`

Requires `backend` and a running sidecar.

```json
{
  "method": "GET",
  "path": "/api/items",
  "body": { "optional": "JSON body" }
}
```

The daemon adds the sidecar module token and forwards to `127.0.0.1:<sidecar-port>`.

## `proxy.status`

Requires `node.proxy:<mount>`.

```json
{ "mount": "myapp", "node": "studio" }
```

Returns whether the target is reachable and the isolated app URL the shell
should use for the dedicated app surface. A mount without a configured `host`
returns an empty URL; full apps require a host.

## `proxy.configure`

Requires both `node.proxy:<mount>` and `node.proxy.manage:<mount>`. Creates or
updates a proxy mount for this module and applies it immediately; no daemon
restart is needed.

```json
{
  "mount": "myapp",
  "base_url": "http://127.0.0.1:8080",
  "title": "My App",
  "websocket": true,
  "frame": false
}
```

`mount` must be a lowercase host label. The host is derived as
`<mount>.<canonical-host>` and cannot be overridden. `base_url` must be an
HTTP(S) URL with a host and no embedded credentials, query, or fragment.
`frame` relaxes framing protection for a trusted upstream and should stay off
unless the app is meant to run embedded (Home Assistant needs it). The
operation is audited and writes the mount to `config.toml`.

## `app.open`

Requires `app.open`. Asks the shell to display a full app surface.

```json
{ "mount": "myapp", "node": "studio" }
```

The mount must have a dedicated `host` configured. The URL is therefore a
distinct app origin and the app does not share the shell origin. `app.open`
returns an error for a mount without a host; use `proxy.configure` or add a
host such as `myapp.orqella.localhost` to `config.toml` and restart. The
path-only fallback is intentionally not supported for full apps.

Optional `path` and `handoff` values are validated same-origin paths. The
shell loads `handoff` first, then `path`; a module can use this for a one-time
app login or pairing hop without any host-specific routing in Orqella. A path
may contain `{server_key}`, which the daemon replaces with the URL-safe base64
of the final app origin; this keeps app routes that encode their origin correct
when the app is opened on a peer.

Native clients may define `window.__ORQELLA_NATIVE_APP_OPEN`. When present, the
shell passes `{url, handoff, title}` after resolving the app origin and skips
the `appFrame` iframe. Returning `false` falls back to the iframe. The mobile
shells use this to open full apps in a top-level WebView on an isolated
`127.0.0.x` origin, which preserves first-party app cookies and avoids frame
restrictions. This hook is optional; browser and desktop clients do not need it.

The browser client uses a separate origin per app and maps that origin’s HTTP,
WebSocket, and SSE traffic through one authenticated WebRTC transport. App data
is **direct WebRTC only**. Relay mode is disabled on both the hub and the
browser client, so there is no relay signaling or application path. Apps must be
root-relative and same-origin on their app origin; apps requiring their own
service worker are not currently supported. App origin assignment is deployment
configuration (`__ORQELLA_APP_ORIGINS` or `__ORQELLA_APP_ORIGIN_SUFFIX`), not
module code.

## `module.open`

No permission. Asks the shell to open this module's own page at a path:

```json
{ "path": "/?session=abc" }
```

The path must be root-relative, must not begin with `//`, and can only target
the calling module. The shell switches to the module's page and, for a module
installed on a peer, switches the machine selector first. This is the supported
way for an overview card to hand off to its full page; it does not grant access
to any other module or to shell settings.

## `system.pick_directory`

Requires `fs.pick`. Opens the trusted shell's folder picker and returns the
folder the user chose:

```json
{ "title": "Choose a workspace", "start": "/home/me/Projects", "node": "local" }
```

Result:

```json
{ "path": "/home/me/Projects/app" }
```

The user can cancel; the result is then `{ "cancelled": true }`. The shell browses
directories only, hides dot-directories, and never exposes file contents. For a
module installed on a peer, pass that node's id; the shell browses the peer's
filesystem through the peer API. A module never receives a directory listing,
only the one path the user selected. Older shells do not implement this method;
fall back to a text field in that case.

## Hooks

Hooks are named actions Orqella provides to modules. They are discoverable and
permission-gated:

```js
const hooks = await window.orqella.hooks.list();
const result = await window.orqella.hooks.run("agent.prompt", {
  prompt: "Add a route for …",
  root: "~/Projects/project",
  open: true,
});
```

`hooks.list` returns the available hooks, their required permission, and their
parameter schema. `hooks.run` takes `{ name, params }` and returns the hook
result. Unknown hooks or missing permissions fail the call.

### `agent.prompt`

Requires `agent`.

```json
{
  "prompt": "Add a route for …",
  "root": "~/Projects/project",
  "wait_ms": 6000,
  "open": false,
  "node": "local"
}
```

Starts the configured default agent, waits up to `wait_ms` (default 6000,
maximum 15000) for the harness to expose a conversation id, and returns the
agent job plus `session_id` and the resolved `viewer`. With `open: true`, the
result includes an `open` payload and the shell opens the conversation viewer
immediately. The job continues in the background if no session id appears
within the wait.

### `conversation.open`

Requires `agent`.

```json
{ "job": "agent-job-id", "node": "local" }
```

or pass a known conversation id:

```json
{ "session": "ses_…", "node": "local" }
```

Returns an `open` payload for the configured viewer. The shell opens app
viewers in the full app surface and external viewers in a new tab. Module
payloads carry the `node` where the agent ran, so the shell switches the
machine selector before opening the module page.

The viewer is configured under `[agent.viewer]`:

```toml
[agent.viewer]
kind = "module"                 # module | app | url | none
module = "opencode"             # an installed Orqella module page
path = "/?session={session}"    # module or app path template
title = "OpenCode"

# Or a proxy-mounted web app:
# kind = "app"
# mount = "opencode"
# path = "/session/{session}"

# Or an external viewer:
# kind = "url"
# url = "https://example.com/conversation/{session}"
```

If `[agent.viewer]` is empty, Orqella defaults to the Agent Harness module for
the OpenCode, Codex, Claude, Gemini, and custom presets, with a path such as
`/?harness=codex&session={session}`. The module owns the session index and
routes to the right conversation. All presets, including OpenCode, render in
the module; a session with a workspace is adopted and replayed. An OpenCode
session without a workspace, or one that cannot load, falls back to the native
workspace after pairing. Set `kind = "none"` to disable the fallback. Other
harnesses can use a module page, a proxy-mount app, or an external URL; use
`{session}` (or `{session_id}`) where the conversation id belongs. Module
viewers may also use `{root}`: the daemon substitutes the job workspace,
URL-encoded, so a conversation UI can adopt a session started by Build even when
that session is not in the module's own index.

## `agent.start` / `agent.status`

Requires `agent`. Starts the configured default agent in the background:

```json
{ "prompt": "Add a route for …", "root": "~/Projects/project" }
```

Returns a job with `id`. Poll `agent.status` with `{ "id": "…" }` for `status`,
`log`, `error`, `workspace`, `session_id`, and timestamps. This runs the
configured agent command with write access to `root`; it is not sandboxed like
a sidecar. For conversation-aware flows, prefer the `agent.prompt` hook.

Agent jobs are persisted, so `agent.status` still returns the last known state
after a daemon restart. A job that was queued or running during the restart is
reported as `failed` with an explanatory error and its last known `session_id`;
the underlying harness session may still be running. The agent timeout is
configured by `[agent].timeout_secs`; `0` (the default) disables it.

## `media.record.start` / `media.record.stop`

Requires `media.record`. These calls are intercepted by the trusted shell, not the daemon API. `start` asks for microphone permission; `stop` returns base64 audio:

```json
{ "audio": "…", "mime": "audio/webm", "size": 12345 }
```

Module iframes must use this bridge because their opaque origin cannot call `getUserMedia` itself.

## Permissions and node context

- Pass `node` explicitly for any node-aware operation.
- Do not hard-code a peer ID or machine name.
- Read the selected machine from `orqella.context.node`.
- Use `nodes.list` if the page needs to compare machines.
