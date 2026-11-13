# Orqella security notes

## Default posture

- Bind to `127.0.0.1` only.
- Remote access is expected through Link or a private network.
- The shell and module pages do not share an authenticated browser context.

## Browser protections

- `Host` must be loopback, the canonical host, or a configured app subdomain.
- State-changing `/api/` requests require `X-Orqella: 1`.
- Browser `Origin`, when present, must be loopback or the canonical/app host.
- Module pages are sandboxed with an opaque origin.
- The shell keeps the module token; it is not passed into the iframe.
- Module API calls are permission-checked by the daemon.
- Module WebSocket paths accept `Origin: null` because sandboxed module frames
  have an opaque origin. That exception is limited to `/m/.../ws/...` and peer
  module paths; configured full-app origins are rejected on those routes. The
  module backend must still authenticate browser clients. The Terminal module
  adds a short `ws_token` that is only delivered through the authenticated
  backend bridge. A module must also have the `backend` permission before the
  daemon proxies its WebSocket.
- Module runtime files under `data/` are never served by the static module
  asset routes. They may contain credentials, models, and user data.
- Module icons are validated as images with safe relative paths and escaped by
  the shell before rendering.
- Sidecar logs are stored in the daemon data directory, not in the module
  writable tree, and are opened without following symlinks.
- `.git` and `module.toml` are mounted read-only inside the sandbox. Daemon-side
  git commands disable repository hooks, fsmonitor, credential helpers, and
  `ext::` transports. A sandbox-policy change on update revokes the `backend`
  grant until the user reviews it again.
- Link peer forward listeners require the peer token, so an unrelated local
  process cannot use the loopback forward as a bridge to another machine.

## Browser WebRTC access

Browser access is opt-in (`[web] enabled = true`) and never binds a public TCP
port. Clients connect through iroh and must present an authorized Orqella Link
browser session before the bridge proxies any loopback HTTP or WebSocket
request. Browser relay tokens and the hub relay token are bearer credentials
and must not be logged.

The browser client's service-worker architecture requires module iframes to run
with `allow-same-origin` so their assets and WebSockets can be proxied. This
grants a module more access to the client origin than a normal Orqella module
iframe has. The client displays a persistent **Trusted modules only** warning.
Treat the browser client as a trusted transport host until transport authority
is moved into a dedicated worker or isolated origin; this remains an accepted
spike-time risk.

## Proxy

- Only configured mounts can be proxied. There is no arbitrary URL proxy.
- The proxy normalizes paths and strips hop-by-hop headers.
- Host-based app origins provide a distinct browser origin for full applications.
- A proxy mount can set `frame = true` to relax `X-Frame-Options` and CSP
  `frame-ancestors` from a trusted upstream so it can run in the Orqella app
  iframe. The proxy preserves framing protection by allowing only the shell
  origin. Keep this off unless the upstream is explicitly intended to be
  embedded (Home Assistant needs it).
- Link-managed peers are reached through their own authenticated peer token.
- WebSocket and HTTP header handling should be covered by upstream-specific tests.

## Sidecars

A sidecar is explicit code execution on the host. It starts only after the
module has been granted `backend`; the same grant is required for its WebSocket
bridge. Sandboxing and limits are daemon defaults (`[sidecar]`). A module may
set `[backend] sandbox = false` to run unsandboxed when it needs desktop,
keyring, model, or setup access; that is a trusted-code decision. The permission
review requires an explicit unsandboxed acknowledgment before granting
`backend`. Linux uses
bubblewrap with a read-only system filesystem, writable module directory,
private `/tmp`, and an address-space limit. Memory limits are applied to
unsandboxed Linux sidecars through `prlimit` as well. macOS uses a sandbox-exec
profile for filesystem isolation but has no per-sidecar memory cap. Windows
uses a Job Object for lifecycle and memory containment for sandboxed and
unsandboxed sidecars; Windows filesystem isolation is not yet implemented.
Every current first-party module sets `[backend] sandbox = false`, so the
filesystem sandbox protects third-party modules that keep the default, not the
shipped first-party set; those modules are trusted local applications. A sidecar's
`HOME` is `data/home`, not the module root; legacy home directories are moved
there once on first start.

## Machines and Link

- Each daemon has an internal peer token in its config; the config is user-only
  on Unix.
- Link devices must present a valid signed ticket before a peer forward is
  created.
- Link's SSH bridge verifies the device ticket and SSH host-key fingerprint
  before opening SSH.
- The peer API is reachable only through the loopback listener or a Link-managed
  loopback forward, not a public bind. The forward itself also requires the peer
  token on every local connection.
- Peer tokens are never sent to module pages or embedded applications.
- Trust-store changes (`orqella module trust`) require the private
  `<data>/admin.token` capability and are not available to the shell or module
  API.

## Audit

Audit is opt-in (`[audit] enabled = true`). When enabled, module installs, imports, enables/disables/removals, Link machine changes, and builds are appended to `data/audit.jsonl`. The audit file is pruned to the latest 2,000 lines when it exceeds 2 MB; the API returns the latest 200 events.

## Backup and support artifacts

`orqella backup` is a local restore artifact and may contain internal peer
tokens, Link credentials, module state, and module data. Treat it as sensitive.
`orqella support` redacts peer tokens, Link tokens, and agent environment
values, and omits module runtime `data/`, `state.json`, and audit history. It is
the artifact to share when asking for help. Neither command uploads anything.
Module packages (`orqella module export`) omit top-level `data/`; runtime data
is not part of the published module contract.

## Module repositories and GitHub publishing

Every installed module is its own git checkout. `data/`, caches, virtualenvs,
and logs are excluded through the checkout's local `.git/info/exclude`, so
updates can fast-forward without dirtying tracked files.

The GitHub App private key lives only in the hosted publisher bridge, which is
not part of this repository. The daemon stores a bridge token in
`<data>/github/connection.json` with mode `0600`. A publish requests a
one-hour installation token, passes it to git through a temporary `GIT_ASKPASS`
script, and never writes it to the remote URL, config, module files, or logs.
Disconnecting revokes the bridge token. Publishing to a repository other than
the one recorded at install time requires an explicit user confirmation in the
browser dialog (or an explicit `--repo` on the CLI); a module cannot redirect a
publish by rewriting its own `.git/config`. A future production pass should add
per-installation token rotation/audit and a user-facing bridge token list.

## Module updates

Module installation preserves existing grants. If an update adds permissions, they appear as pending and the module cannot use them until the user reviews and saves the permission sheet. A failed update restores the previous module directory from `data/backups/modules`.

## Known gaps

- Linux has bubblewrap sidecar sandboxing. macOS has sandbox-exec filesystem isolation. Windows has Job Object lifecycle/memory containment but no filesystem isolation. None of these is a network namespace, and first-party modules currently opt out.
- Sidecars share the host network namespace; network isolation is a future hardening step.
- No signed module registry yet; portable package signing is implemented with explicit publisher trust. Git repositories are trust-on-first-use and do not carry package signatures.
- Dependency audit runs in CI. `zip` is current; `rsa 0.10.0-rc.18` is pinned by
  `russh` and covered by a documented advisory ignore. The Link SSH host key is
  Ed25519, so the vulnerable RSA decryption path is not used.
- Bridge tokens are bearer credentials; there is no per-device token list or expiry UI yet.
- No automatic secret store integration yet.
- Remote WebSocket proxying is locally tested; each upstream still needs a live compatibility check.
- LAN exposure/TLS is intentionally not enabled. `[server] allow_remote` is only an explicit opt-in guard; non-loopback binds still have no authentication.
- The local control API requires either the browser's automatic session cookie
  or the private admin token. A same-user process that can read the daemon data
  directory or forge browser headers remains trusted; network-namespace/process
  isolation for unsandboxed sidecars is still future work.
- Full app mounts must configure a dedicated `host`; path-only app surfaces are
  rejected. The app origin is distinct from the shell origin.
- Module WebSocket routes require a per-module token delivered through the frame
  context. Custom modules that create sockets in a Worker or bypass the SDK must
  add `__orqella_ws` themselves.

## Updates

The current update path executes the configured git checkout and its platform installer with the daemon user's privileges. Local updates require the shell CSRF header; remote updates require the peer token and are limited to the peer's own `[update]` repository configuration. A future release channel must add signed artifacts, signed metadata, and rollback before replacing this temporary git-based flow.
