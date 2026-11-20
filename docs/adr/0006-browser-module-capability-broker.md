# ADR 0006: Browser module surfaces through a capability broker

Status: proposed.

## Context

The browser WebRTC client can load the hub shell, but deliberately disables
module pages. The shell is served same-origin inside the browser client and is
controlled by the client service worker. Module pages are different:

- they run in an iframe sandbox without `allow-same-origin`, so their origin is
  opaque and the service worker does not control their requests;
- they are third-party code and must not get the Link account token, device
  token, relay credentials, or unrestricted `/api` access;
- they still need module HTML, CSS, JavaScript, images, fonts, HTTP, WebSocket,
  EventSource, host APIs, and (eventually) workers and dynamic imports;
- full applications already have a separate-origin design under
  `web-client/app-client/`, but modules need their own isolated origin because
  they are embedded inside the dashboard shell.

Simply adding `allow-same-origin` to the module frame is not acceptable. It
would let module JavaScript read the shell's Link session, call `/api`, and
reach other modules.

The iOS browser transport work also established two constraints that this
design must preserve:

- module surfaces must work on iOS Safari and Chrome (WebKit);
- the browser client is direct-WebRTC only for application data; Link is auth
  and signaling only.

## Decision

Use **one isolated browser origin per module** plus a **capability broker**
owned by the shell.

The shell owns the WebRTC transport and all Link credentials. A module origin
gets only a short-lived, module-scoped capability and a private `MessagePort`
to the shell broker. The module origin has no Link token, device token, relay
token, shell cookie, or shell storage.

The module origin is served by a generic bootstrap page and a generic service
worker. Module content and assets are relayed through the broker, not fetched
directly from the hub by the module frame.

### Origin scheme

Production:

```text
https://<module-origin-id>.modules.orqella.io
```

Staging:

```text
https://<module-origin-id>.modules.staging.orqella.io
```

`<module-origin-id>` is a stable URL-safe hash of the module ID, for example
`m-<first 16 hex characters of sha256(module_id)>`. Using a hash avoids module
IDs containing dots or characters that are invalid in a hostname.

This requires wildcard DNS and a wildcard certificate for the module host. The
same static bootstrap and service-worker files are served on every module
origin. This is the deployment cost of true module isolation.

A single shared module origin with per-module paths is rejected: modules would
share storage, caches, broadcast channels, and service-worker scope.
Per-module ports are rejected because public browsers cannot reach arbitrary
ports through the current hosting model.

### Frame model

The shell loads:

```html
<iframe
  sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
  src="https://m-abc123.modules.orqella.io/m/<module-id>/"
></iframe>
```

`allow-same-origin` is safe here because it refers to the unique module origin,
not the shell origin. The shell stays cross-origin and its Link session is not
readable from the module frame.

The module frame must not get `allow-top-navigation`, `allow-modals`, or
`allow-downloads` unless a specific module surface needs them.

### Bootstrap

Every module origin serves the same static bootstrap:

```text
/module-bootstrap.html
/module-bootstrap.js
/module-sw.js
/module-sdk.js
```

The sequence is:

1. The shell creates the module iframe with the module origin, module ID,
   surface, and a random frame nonce.
2. `module-bootstrap.js` registers `module-sw.js` at scope `/`.
3. The bootstrap page creates a `MessageChannel` and posts
   `orqella:module-bootstrap` to `window.parent` (the shell) with:
   - module ID
   - frame nonce
   - surface (`page`, `overview`, or `settings`)
   - one transferred port
4. The shell verifies `event.source` is the exact module iframe, the frame
   nonce matches, and the module origin is the expected origin for that module.
5. The shell stores the port in a per-module broker and sends the module
   document HTML and a module-scoped `ws_token` over it.
6. The bootstrap writes the module document into its own document with a
   `<base>` pointing at the module origin, so relative URLs and the service
   worker keep the module on its isolated origin.
7. The bootstrap injects `module-sdk.js` and the module bridge before the
   module's own scripts run.

The module frame's `window.parent` remains the shell, so the existing
`window.orqella` host API handshake can continue to use `postMessage` plus the
private context port.

### Module service worker

`module-sw.js` is generic and contains no module credentials. It handles:

- bootstrap/static files: network or cache;
- module navigation and subresources: relay through the module page bridge;
- GET caching keyed by module ID and module version/hash;
- `Clear-Site-Data`/cache invalidation on module update or removal.

For a module request that needs the hub, the service worker:

1. Finds the module page's `WindowClient`.
2. Opens a `MessageChannel` and posts a request to the bootstrap page.
3. The bootstrap page forwards the request over its shell broker port.
4. The shell fetches through the already-authenticated WebRTC transport.
5. The response is returned to the service worker and then to the module.

The service worker never gets the Link token or a raw transport port to the
top client. It can only ask the module page to relay.

### Capability broker

The broker lives in the shell and is keyed by module ID plus frame nonce.

Protocol messages:

```text
http.request   { id, method, path, headers, body }
http.response  { id, status, headers, body }

ws.open        { id, path, protocols }
ws.send        { id, kind, data }
ws.close       { id, code, reason }
ws.event       { id, type, data }

sse.open       { id, path }
sse.event      { id, event, data }
sse.close      { id }
```

The broker enforces:

- the request path starts with `/m/<module-id>/` (local module), or
  `/n/<node>/m/<module-id>/` when remote module support is enabled;
- exact allowlisting for shared static assets such as `/assets/theme.css` and
  `/assets/module.css` if they are not inlined;
- rejection of `/api`, `/v1`, `/v1/web`, `/v1/relay`, `/p`, `/n/.../p`, and
  every other module's path;
- rejection of requests from a frame other than the registered module frame or
  with the wrong nonce;
- stripping `Cookie`, `Authorization`, `Proxy-Authorization`, and
  `X-Orqella-*` headers from module-originated requests;
- response-size, request-rate, header-count, and WebSocket-count limits per
  module;
- no `Set-Cookie` propagation to the module origin.

The module's existing `ws_token` is module-scoped and may be injected into the
module WebSocket path by the bridge. The Link token is never placed in module
context or module storage.

### Module SDK and network shims

`module-sdk.js` keeps the existing `window.orqella` host API surface. Host API
calls continue to go through the shell's existing permission-checked RPC path:

- `window.orqella.call(...)`
- `window.orqella.ready(...)`
- `window.orqella.onContext(...)`
- hooks

The module origin additionally installs:

- `fetch`/XHR shims for same-module paths, backed by the broker;
- `WebSocket` and `EventSource` shims backed by the broker;
- a small `MessagePort` bridge to the bootstrap page.

External network access is denied by default. A module that needs a third-party
API must use a host API or a future declared external-network permission. Do
not silently allow arbitrary `connect-src` network from a module origin.

Static assets are handled by the module-origin service worker, so normal
`<script>`, `<link>`, `<img>`, `<font>`, dynamic imports, and workers continue
to work without HTML rewriting. This is the main reason to use a real
per-module origin instead of trying to inline an entire module into a sandboxed
`srcdoc`.

### Shell changes

`web/js/frames.js` currently accepts module messages only from the opaque
`null` origin. It will need to:

- accept the expected module origin for a given frame;
- keep the existing `event.source` and nonce checks;
- bind the broker port to the module frame and nonce;
- reject a module frame that navigates to an unexpected origin;
- pass the module context over the existing private context port.

The shell remains responsible for:

- opening/closing module frames;
- assigning frame nonces;
- owning the WebRTC transport;
- brokering only allowlisted module traffic;
- clearing caches when a module is updated or removed.

### Lifecycle and caching

- Module version/hash is part of the cache key.
- Update/uninstall sends a broker message and a service-worker message to
  clear the module origin's cache and, if necessary, `Clear-Site-Data`.
- The browser can evict inactive module caches under storage pressure.
- A failed SW registration or a missing module origin disables that module
  surface only; the shell and other modules remain usable.

## Alternatives considered

### Allow same-origin module frames

Rejected. Module code could read the Link session, call `/api`, and reach other
modules.

### Inline the module into the shell with `srcdoc`

Useful as a small-module fallback, but rejected as the primary design. It
requires rewriting/inlining CSS, JavaScript, images, fonts, dynamic imports,
workers, and module navigation, and it will never cover arbitrary modules.

### One shared module origin

Rejected for security. Modules would share storage, caches, broadcast channels,
and service-worker scope.

### Relay application data

Not part of this design. The browser client remains direct-WebRTC only. Module
traffic uses the same direct transport as the shell.

## Rollout plan

### Phase 0: Spike

- Choose the wildcard module host and certificate path.
- Serve generic bootstrap, service worker, and SDK files on one test origin.
- Load a trivial module and prove the shell frame, bridge, and SW work on
  desktop and iOS.

### Phase 1: Static module pages

- Per-module origin mapping and frame creation.
- Bootstrap handshake, shell broker port, nonce validation.
- Module HTML/CSS/JS/images/fonts through the SW relay.
- Existing `window.orqella` host APIs.
- No WebSocket yet.

### Phase 2: Module HTTP and WebSocket

- `fetch`/XHR/WebSocket/EventSource shims.
- Module `ws_token` injection.
- Terminal and log-style modules.
- Request/response size and rate limits.

### Phase 3: Compatibility

- Remote modules under `/n/<node>/m/<id>/`.
- Dynamic imports and workers.
- Module update/uninstall cache invalidation.
- Overview and settings surfaces.
- Storage quotas and eviction.

### Phase 4: Hardening

- Security tests for cross-module access, `/api`, Link-token access, nonce
  replay, origin confusion, and worker/import escapes.
- CSP and frame-ancestors headers per environment.
- Penetration test against a malicious module fixture.
- Remove the browser-client module warning only after this passes.

## Acceptance criteria

- A module can render and fetch only its own paths.
- A module cannot read the shell's Link token, cookies, localStorage, or
  sessionStorage.
- A module cannot fetch `/api`, `/v1`, `/p`, another module, or a remote node's
  private paths.
- A module cannot open a top-level navigation or escape its frame sandbox.
- Module WebSocket and EventSource traffic works for the module's own routes.
- Module host APIs still enforce daemon permissions.
- Module update invalidates old cached assets.
- The shell and other modules remain usable if one module origin fails.
- The design works in iOS Safari, iOS Chrome, desktop Chrome, Firefox, and
  Safari.

## Open questions

- Which wildcard host and certificate provider should be the long-term home:
  Cloudflare, Fly custom domains, or the future public website?
- Should module origins be scoped per hub (`<module>.<hub>.modules...`) or only
  per module? Per-module is sufficient for browser isolation; per-hub would
  further isolate the same module across accounts but is not needed for the
  current threat model.
- Do we support remote module pages in the first release, or local modules
  only?
- Which shared assets are allowlisted versus inlined by the bootstrap?
- What are the module cache and per-module request quotas?
- Should third-party module network access ever be a declared permission, or
  should all external access go through host APIs?
