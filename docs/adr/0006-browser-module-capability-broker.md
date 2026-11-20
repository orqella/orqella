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

Isolation does not remove module capabilities or integrations. Host APIs,
agent jobs, microphone recording, app hand-offs, and sidecar-to-sidecar
integrations keep working through separate planes; see
[Interaction planes](#interaction-planes). Only direct module network access is
restricted.

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

host.call      { id, method, params }
host.result    { id, ok, result, error }
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

### Interaction planes

The broker must not be modeled as a transparent HTTP proxy. Module interactions
split into four planes, and only the module data plane is path-allowlisted:

| Plane | Examples | Path through the browser |
| --- | --- | --- |
| Capability | `backend.request`, `agent.start`, `hooks.run`, `storage`, `proxy.status` | Broker `host.call` → shell → daemon RPC |
| Module data | `/m/<id>/...` pages, assets, HTTP, WS, SSE | Module origin → module SW → broker → transport |
| Shell transition | `app.open`, `module.open`, `fs.pick`, `media.record` | Shell handles it directly |
| Hub-local integration | Home Assistant ↔ Voice Assistant, Agent Harness ↔ OpenCode | Sidecar to sidecar; never enters the browser |

#### Capability plane

`window.orqella.call(...)` continues to work unchanged from the module's point
of view, but its wire format moves onto the broker port as `host.call`. The
shell forwards each call to the existing
`/api/v1/modules/<id>/rpc` endpoint using the module ID bound to that broker
port, never a module-supplied ID. The daemon keeps enforcing the manifest
permissions and `granted_permissions`.

This preserves the whole current host API surface:

- `backend.request`, including the sidecar module token injection;
- `agent.start`, `agent.status`, and `agent.prompt`;
- `hooks.list` and `hooks.run`, including `conversation.open`;
- `storage.get`/`storage.set`;
- `proxy.list`/`proxy.status`, `nodes.list`, `system.info`/`system.metrics`;
- `module.info`, `app.open`, and `fs.pick`.

The capability plane is not a way around the data-plane allowlist. A module
still cannot fetch `/api`, and the daemon still authorizes every method. The
broker adds identity binding, not new authority.

#### Agent interactions

Most first-party modules that act on the machine declare the `agent`
permission: Voice Assistant, System, and VM Lab. They call the default agent
through `agent.prompt`, `agent.start`, `agent.status`, or
`conversation.open`. Build uses the same daemon-side builder without a module
frame. None of those run in the browser: the daemon starts the configured agent
command on the hub, and the module only receives a job id and status. That
continues to work over the broker.

`conversation.open` returns a viewer payload. The shell resolves it as it does
today:

- a module viewer opens another module page under the same origin and broker
  rules;
- an app viewer opens the configured isolated app origin;
- an external URL opens a new top-level tab, which the module cannot do itself.

Only the daemon may select a different module as a conversation viewer.
`module.open` remains caller-only, so a module cannot use it to navigate another
module's frame.

Two browser-specific adjustments are required:

- `agent.status` is poll-based. Crossing a WebRTC link makes aggressive polling
  wasteful, so add an `agent.subscribe`-style event stream over the broker
  before relying on it for live progress. The Agent Harness module already
  streams over its module WebSocket and is unaffected.
- `app.open` payloads must be rewritten by the browser shell to configured app
  origins, exactly like local app surfaces. Otherwise an agent viewer or Home
  Assistant dashboard resolves to an `*.orqella.localhost` host that the phone
  cannot reach.

#### Voice and microphones

`media.record.start` and `media.record.stop` are shell capabilities, not module
network calls. The shell tab owns `getUserMedia` and `MediaRecorder`; the module
receives the encoded clip over the broker. Module origins never receive
microphone permission and never call `getUserMedia` themselves.

This is required on iOS, where microphone access inside a cross-origin iframe
is restricted, and it preserves the existing hold-to-talk gesture: pointer down
starts recording, pointer up stops and submits. Voice Assistant needs no change.

#### Home Assistant and sidecar-to-sidecar integrations

The Home Assistant ↔ Voice Assistant link is entirely hub-local. The bundled
Jarvis custom component runs inside Home Assistant and POSTs to the configured
Voice Assistant URL (default `127.0.0.1:8790`); the Whisper STT entity uses the
same endpoint. Agent Harness calls OpenCode through its own sidecar. None of
this traffic passes through the shell, the module origin, or the browser, so
the broker leaves it untouched.

The Home Assistant module page itself only checks sidecar and proxy status and
calls `app.open`. Its dashboard is a full app on its own origin, not a module
page. Browser module support does not replace app surfaces; the browser client
still needs a configured `homeassistant` app origin.

#### Long-running and binary capability calls

Some interactions do not fit a short request/response:

- Home Assistant integration setup and restart can run for minutes;
- Voice Assistant returns base64 audio that can be several megabytes;
- agent transcripts stream continuously.

The broker therefore needs:

- a per-call timeout override or background job handle for setup actions;
- binary payloads over `MessagePort` and explicit size limits;
- streaming events for agent progress, module WebSocket, and SSE;
- backpressure instead of unbounded buffering.

Phase 1 can ship the capability plane with a raised timeout and base64 JSON.
Phase 2 adds binary frames and event streams alongside module HTTP/WS.

#### WebSocket token exposure

Today the per-module `ws_token` is placed in module context and in the module
document URL. Once the broker owns module WebSockets, remove it from context:
the module asks the broker to open `/m/<id>/ws/...`, the shell injects the
module token, and module JavaScript never sees it. Workers that open sockets
directly must go through the SDK shim, or keep the token as a documented
exception until workers are brokered.

### Module SDK and network shims

`module-sdk.js` keeps the existing `window.orqella` host API surface. Host API
calls go through `host.call` on the broker port and then the shell's existing
permission-checked RPC path, not through the module `fetch` shim:

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
- pass the module context over the existing private context port;
- route `host.call` through the existing frame-request handler, so
  `media.record`, `system.pick_directory`, `module.open`, and `app.open` keep
  their shell-side handling while everything else goes to module RPC.

The shell remains responsible for:

- opening/closing module frames;
- assigning frame nonces;
- owning the WebRTC transport;
- brokering only allowlisted module traffic;
- applying app-origin mapping to `app.open` results in the browser client;
- clearing caches when a module is updated or removed.

Two non-RPC protocols need the same audit:

- The Agent Harness settings surface reports its agent registry with
  `orqella:agent-registry` and accepts `orqella:set-agent-default` directly
  from the shell. `settings.js` must check the expected module origin instead
  of the opaque `null` origin. Prefer moving this protocol onto `host.call` or
  a broker event stream.
- Nested frames inside a module (the VM Lab console and terminal, for example)
  remain same-origin with that module and keep using the module SDK or the
  module page as their relay. They are inside the same trust boundary and
  never receive a broker port of their own.

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

### Phase 1: Static module pages and capabilities

- Per-module origin mapping and frame creation.
- Bootstrap handshake, shell broker port, nonce validation.
- Module HTML/CSS/JS/images/fonts through the SW relay.
- `host.call` for the existing `window.orqella` host APIs.
- Shell-side `media.record`, `fs.pick`, `module.open`, and `app.open` with
  browser app-origin mapping.
- Prove Voice Assistant recording and an agent prompt from a browser module.
- No module HTTP/WebSocket yet.

### Phase 2: Module HTTP and WebSocket

- `fetch`/XHR/WebSocket/EventSource shims.
- Shell-side module `ws_token` injection (removed from context).
- Terminal and log-style modules.
- Binary payloads and agent progress event streams.
- Request/response size, timeout, and rate limits.

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
- `backend.request` reaches the module's own sidecar from a browser module.
- `agent.prompt` starts a hub-side agent job, and `conversation.open` opens the
  configured module, app, or external viewer.
- Voice Assistant hold-to-talk records through `media.record` on iOS Safari and
  submits to its sidecar.
- Home Assistant can run its integration setup from the browser and open its
  dashboard on a configured app origin.
- The module `ws_token`, sidecar token, Link token, device token, and relay
  token never appear in module context, storage, or network requests.
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
