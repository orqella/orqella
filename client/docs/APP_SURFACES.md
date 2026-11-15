# App surface behavior and limitations

The desktop client keeps the hub's existing app-surface contract: a module calls
`app.open` with a configured proxy mount, and the shell opens the full app.

## Native implementation

- Linux/Windows: the gateway allocates a loopback IP per app (`127.0.0.2`,
  `127.0.0.3`, ...) on the shell port.
- macOS: only `127.0.0.1` exists on `lo0` by default, so the gateway maps the
  canonical `*.orqella.localhost` host to `*.orqella.local`, binds a unique
  loopback port, and registers the name over mDNS with `dns-sd`. Cookies and
  localStorage remain host-scoped per app. The client needs no `/etc/hosts`
  entry and no sudo; core's Safari/browser hosts management is separate.
- The shell receives an injected resolver and iframe shim. The shim rewrites
  both the `src` property and `setAttribute("src", …)`, including legacy
  `/p/<mount>` and `/n/<node>/p/<mount>` URLs.
- Upstream `Host`, `Origin`, `Referer`, `Location`, and cookie `Domain` values
  are rewritten.
- The shell CSP `frame-src` covers the gateway's whole app-origin allocation
  namespace, so an origin leased after the shell document loaded is still
  framable.
- App responses carry `Origin-Agent-Cluster: ?1`; the app iframe delegates
  microphone, camera, display capture, fullscreen, and clipboard. Apps can send
  native notifications with
  `parent.postMessage({ type: "orqella:notify", title, body }, "*")` when
  embedded.
- HTTP, WebSocket, and SSE are streamed through the gateway.

## Verified

- OpenCode loads on `127.0.0.2` with its project/session list.
- Home Assistant loads on `127.0.0.3` and reaches its login form.
- Terminal WebSocket works through the gateway.
- OpenCode SSE returns `text/event-stream` through the gateway.
- Cookies and localStorage are isolated between two app origins and the shell.

## Known limitations

- An app that hard-codes the canonical hub host in absolute URLs may need
  rewriting to work embedded.
- Apps that require their own service worker are untested in the native WebView.
- App responses are origin-keyed (`Origin-Agent-Cluster: ?1`); apps that still
  use `document.domain` to relax across sibling hosts will fail.
- Apps with unusual streaming protocols beyond HTTP/WS/SSE need explicit tests.
- App origins are allocated dynamically on demand through the authenticated
  native endpoint; the pool is bounded and reset UI is still pending.
