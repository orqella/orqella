# Orqella browser client

This is the transport-aware browser client for an Orqella machine. It is a
static app (with a service worker) that connects to a hub over WebRTC. The hub
must be running with browser access enabled:

```toml
[web]
enabled = true
```

Or start a test daemon with `orqella serve --web`. The daemon must also be
signed in to Orqella Link so it can authorize browser sessions. Browser access
authenticates through Orqella Link: sign in, select a machine, and connect.

## Build

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.118 --locked
./web-client/build.sh
```

Serve the `web-client/` directory from an HTTPS origin in production. The
shell and the full-screen applications must use separate origins so their
root-relative asset paths do not collide. For local testing, the included
server starts all three origins (ports `8801`, `8810`, and `8811`):

```bash
cd web-client
python3 serve.py
```

Open `http://127.0.0.1:8801/client/`.

The app origins are `http://127.0.0.1:8810` (Home Assistant) and
`http://127.0.0.1:8811` (OpenCode). `app-view-shell-shim.js` maps those
isolation origins from the hub shell's application surface. In production,
host each app at its own HTTPS origin and set the full origins in
`client/config.js`; do not put them on the shell origin:

```js
window.__ORQELLA_APP_ORIGINS = {
  homeassistant: 'https://ha.example',
  opencode: 'https://opencode.example',
};
```

The transport bar reports `Direct WebRTC`; relayed application connections are
rejected. The client heartbeats `/healthz` so sleep/wake and network changes can
trigger recovery.

### Relay policy

The browser client is **direct-WebRTC only** for application data. Managed
relay credentials are used solely for WebRTC bootstrap/signaling, and the hub
rejects any `orqella/web/1` application connection whose selected Iroh path is
a relay. That makes managed-relay data egress impossible through the browser
bridge, even if a client is modified to request it.

The browser client uses managed relay signaling for bootstrap. All application
data remains direct; there is no relayed application-data fallback.

## Security notes

This is a functional integration spike, but the client origin is trusted more
than a normal Orqella module origin. To make module iframes service-worker
controlled, the client adds `allow-same-origin` to their sandbox. A malicious
installed module could therefore reach the client page and its in-memory
transport. The client shows a persistent **Trusted modules only** warning, and
this is an accepted risk for the spike. Production should move transport
authority into a dedicated worker or otherwise isolate module execution before
shipping this to untrusted modules.

Link browser sessions and relay tokens are bearer credentials. Do not log them.
The hub's relay token is never exposed to the browser or to modules.

The client has not yet been validated on cellular, CGNAT, symmetric NAT, or
IPv6-only networks. Direct WebRTC works on localhost/LAN; remote success still
depends on NAT traversal. The managed relay is used only for signaling.

Full applications are isolated by origin, but the shell still uses the
documented `allow-same-origin` module-frame compromise. Do not run untrusted
modules with this spike client.

## Arbitrary app surfaces

Any configured proxy mount can use the same app client. The mount is the stable
contract; module code only calls `app.open` with its mount.

Production can derive an origin per mount with a wildcard suffix:

```js
window.__ORQELLA_APP_ORIGIN_SUFFIX = '.apps.example';
// mount "immich" -> https://immich.apps.example
```

Explicit origins override the suffix:

```js
window.__ORQELLA_APP_ORIGINS = {
  immich: 'https://immich.apps.example',
  media: 'https://media.apps.example',
};
```

Serve the same `app-client/`, `app-sw.js`, `event-source-shim.js`, and
`ws-shim.js` files on every app origin. The app origin must be a separate origin
from the shell because apps commonly use root-relative assets and APIs.

The hub requires a dedicated `host` for every full app mount; path-only
`/p/<mount>` app surfaces are rejected. The app worker still maps the app
origin’s traffic to `/p/<mount>` through the top transport, but `app.open`
only returns host-routed URLs.

The top client now:

- derives the app mount from the app origin;
- rejects HTTP paths outside `/p/<mount>`;
- rejects WebSocket/SSE paths outside `/p/<mount>`;
- tracks WebSocket ownership so one app cannot close another app’s sockets.

Apps with their own required service worker, absolute hub-host URLs, or unusual
streaming protocols still need explicit testing. The generic shims cover normal
same-origin HTTP, WebSocket, EventSource, and common GET SSE paths.
