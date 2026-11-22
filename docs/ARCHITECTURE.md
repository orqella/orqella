# Orqella architecture

## Runtime

One Rust binary, one loopback listener. The default role is both dashboard and node; `--no-dashboard` disables the shell but keeps the API and peer endpoints available.

```text
browser / native WebView
        │  http://orqella.localhost:<port>
        ▼
┌─────────────────────────────────────────────┐
│ Orqella daemon                              │
│  shell + assets + SDK                       │
│  /api/v1/* core API                         │
│  /m/<id>/* sandboxed module assets           │
│  /api/v1/modules/<id>/rpc bridge             │
│  /p/<mount>/* local reverse proxy            │
│  /n/<node>/p/<mount>/* remote proxy          │
│  Link peer API + peer health                 │
│  build jobs + sidecar supervisor             │
└───────────────┬─────────────────────────────┘
                │ Link: authenticated iroh peer-forward
                ▼
        another Orqella daemon
```

## Canonical origin

- Shell: `http://orqella.localhost:<port>` (loopback aliases also work).
- Local full app: `http://<mount>.orqella.localhost:<port>`. Optional
  same-origin `path` and `handoff` values let modules deep-link or perform a
  one-time app handoff.
- Remote full app: `http://<mount>.<node>.orqella.localhost:<port>`.

The top-level host rewrite maps an app host to `/p/<mount>/...` or `/n/<node>/p/<mount>/...` before Axum routing. This gives full apps a distinct browser origin instead of sharing the shell origin.

## Browser WebRTC bridge

`src/web.rs` optionally starts a second iroh endpoint with the
`iroh-webrtc-transport` custom transport. It accepts `orqella/web/1`,
authenticates with an authorized Orqella Link browser session, and proxies HTTP
and WebSocket traffic to the loopback daemon. The endpoint never binds a public
TCP port. A browser client under `web-client/` owns the WebRTC connection; a
service worker and WebSocket shim make the existing dashboard shell
transport-aware.

The browser endpoint can use endpoint-bound managed relay credentials issued by
Link. The browser signs in to Link, selects a machine, and receives its own
relay credentials before connecting. Hub signaling uses the managed relay set.

## Release updates

Installed daemons update from GitHub Releases, not from a git checkout. The
updater queries `[update] repository` for the newest published stable
`v<version>` release, selects the archive for the host platform, and verifies
the matching `SHA256SUMS.txt` entry. When a public key is configured it also
verifies a signed `ORQELLA_UPDATE.json` manifest. It replaces the running
binary, restarts the per-user service (or starts the daemon when none is
installed), and records the applied version in `update-state.json` so rollbacks
are refused. No Cargo, installer script, or checkout is required.

## Client paths

The daemon is the destination. Link helps four kinds of clients find a direct
path to it:

- **Another Orqella daemon** uses the Link peer forward and peer API; the
  dashboard machine selector switches the page context.
- **A browser** uses the optional WebRTC bridge in `src/web.rs`, authorized by
  an Orqella Link browser session. The browser client lives in `web-client/`.
- **The desktop client** in `client/` loads the same shell through a local
  loopback gateway, or reaches a remote machine through Link's native direct
  tunnel.
- **The mobile shells** use the shared tunnel core in
  `crates/orqella-tunnel-core`, keep the tunnel in the foreground, and load the
  dashboard in a native WebView.

Application data stays on the direct path for every client. Link relays assist
path setup and signaling; relayed application connections are rejected. See
[`LINK.md`](LINK.md) for the managed-service boundary.

## Module bridge

The module page is served under `/m/<id>/`. The daemon injects a base URL and the SDK into HTML. The iframe is sandboxed without `allow-same-origin`; it posts requests to its parent. The shell validates the source frame, calls `/api/v1/modules/<id>/rpc`, and returns the result. The daemon checks the module's granted permissions.

## State and supervisors

`AppState` is owned in `src/state.rs`: config, module store, sidecars, builder, Link manager, peer health, and the shared HTTP client. `src/supervisors.rs` owns the peer-health and sidecar-supervision loops. This keeps proxy and HTTP code from owning process lifecycles.

## Sidecars

`ModuleStore` owns module metadata. `SidecarManager` starts sidecar processes, allocates a loopback port, injects a sidecar module token, waits for health, and proxies `backend.request` calls. Sandboxing and memory limits are daemon defaults under `[sidecar]`; a module can opt out of filesystem sandboxing with `[backend] sandbox = false`, which requires an explicit acknowledgment in the permission review. A failed update restores the previous module directory.

## Machines and Link

Link is the only transport for reaching another Orqella daemon. The daemon signs
in to the Link account, receives short-lived device tickets, and opens
token-authenticated loopback forwards to each peer's Orqella port.
`peers::probe` checks each Link-managed peer through `/api/v1/peer/info`; the
dashboard node selector uses this inventory. The System module can request metrics from a peer.
`/n/<node>/p/<mount>` forwards app requests to the peer's
`/api/v1/peer/proxy/<mount>` endpoint. Manual direct-URL and static SSH peer
configuration is removed; legacy entries are pruned from config on load.
A background watchdog compares wall-clock and monotonic time; after a system
suspend/resume it restarts the Link manager so the iroh endpoint, SSH bridge,
and peer forwards are rebound instead of staying stale. It reacts only to a
sleep-sized gap, at most once per minute, and retries a failed rebuild twice.
Each peer-forward connection also fetches a fresh ticket and target descriptor,
so a peer that restarts onto a new direct address is reachable without
restarting Link on the other machine. The Link iroh endpoint uses UDP 7843 by
default; `[link] port` changes it and `port = 0` requests an ephemeral port.
SSH remains direct-only and is closed if the selected path becomes relay-only.

## Current test surface

- Host/Origin/CSRF checks.
- Module manifest validation and safe path resolution.
- Module store loads installed modules as independent git checkouts from the platform data directory; first-party modules live in separate repositories under the `orqella` GitHub organization, not in this checkout.
- Module lifecycle, permission gating, and config updates.
- Live configured app host proxy.
- Sidecar supervision and permission gating.
- Link peer health and remote metrics.
- Link resume detection and peer-forward rebinding.
- Remote app host HTML proxy.
- Remote module pages, assets, RPC, and WebSocket bridges.

## Deferred

Native mobile shells are maintained in a separate private repository.
Production account/relay deployment, a signed module index, Windows filesystem
isolation for sidecars, and full re-consent/permission upgrade UX remain
deferred. Linux bubblewrap, macOS sandbox-exec, and Windows Job Object
containment exist; see `SECURITY.md` for the per-platform guarantees. The
client-side Link transport lives in `crates/orqella-link-core`; the hosted
control plane and managed relay service are not part of this repository.
