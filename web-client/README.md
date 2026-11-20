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
root-relative asset paths do not collide. The included `Dockerfile` builds an
nginx image from the static files; `cloud/deploy/fly/web-client/fly.toml` and
`cloud/deploy/fly/staging/web-client/fly.toml` deploy it to Fly.

The staging hostname selects `https://link.staging.orqella.io` automatically;
`window.__ORQELLA_LINK_ACCOUNT_URL` overrides the Link account origin. The Link
account page opens `/client/?machine=<web_endpoint_id>` and hands the Link token
to the client with `postMessage`, so the user does not sign in twice.

For local testing, the included server starts all three origins (ports `8801`,
`8810`, and `8811`):

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

## Module origins

Module pages run on a dedicated origin per module:

```text
https://m-<hash>.modules.orqella.io
https://m-<hash>.modules.staging.orqella.io
```

Each origin serves the same generic `/module-bootstrap.html`,
`/module-bootstrap.js`, `/module-sw.js`, and `/module-bridge.js`. Module HTML,
CSS, JavaScript, images, HTTP, and WebSockets are relayed through the shell
broker over the direct WebRTC transport. The module frame may use
`allow-same-origin` because its origin is unique to that module, not because it
shares the shell origin.

The shell remains the only owner of the Link token, device token, relay
credentials, shell cookie, and shell storage. Module host APIs still go through
the daemon's permission-checked RPC. Module WebSockets are opened by the shell
with the module `ws_token` injected shell-side.

The client sets `__ORQELLA_REQUIRE_MODULE_ORIGINS`; it never falls back to an
opaque module frame. Until wildcard DNS and TLS exist, module origins are
opt-in with `?moduleOrigins=1` on the client URL. Local `serve.py` uses
`m-<hash>.localhost:<port>` for that flag.

Wildcard deployment requirement:

```text
*.modules.orqella.io          -> orqella-web.fly.dev
*.modules.staging.orqella.io  -> orqella-web-staging.fly.dev
*.apps.orqella.io             -> orqella-web.fly.dev
*.apps.staging.orqella.io     -> orqella-web-staging.fly.dev
```

Add them as Fly custom domains with wildcard certificates, then enable the
suffix by default in `client/config.js`. The design is documented in
[ADR 0006](../../docs/adr/0006-browser-module-capability-broker.md).

Full applications use separate app origins. Link browser sessions and relay
tokens are bearer credentials. Do not log them. The hub's relay token is never
exposed to the browser or modules.

The client has not yet been validated on cellular, CGNAT, symmetric NAT, or
IPv6-only networks. Direct WebRTC works on localhost/LAN; remote success still
depends on NAT traversal. The managed relay is used only for signaling.

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
