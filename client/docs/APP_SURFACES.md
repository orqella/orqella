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
- The shell receives an injected resolver and iframe shim.
- Upstream `Host`, `Origin`, `Referer`, `Location`, and cookie `Domain` values
  are rewritten.
- The shell CSP `frame-src` is extended with the active app origins.
- HTTP, WebSocket, and SSE are streamed through the gateway.

## Verified

- OpenCode loads on `127.0.0.2` with its project/session list.
- Home Assistant loads on `127.0.0.3` and reaches its login form.
- Terminal WebSocket works through the gateway.
- OpenCode SSE returns `text/event-stream` through the gateway.
- Cookies and localStorage are isolated between two app origins and the shell.

## Known limitations

- An app that hard-codes the canonical hub host in absolute URLs may need
  rewriting or a dedicated app window.
- Apps that require their own service worker are untested in the native WebView.
- Apps with unusual streaming protocols beyond HTTP/WS/SSE need explicit tests.
- App origins are allocated dynamically on demand through the authenticated
  native endpoint; the pool is bounded and reset UI is still pending.
- If an app does not work embedded, the shell exposes **Open in window**. The
  gateway authenticates the request and the Tauri shell opens the same isolated
  app origin in a dedicated native window.
- Verified with OpenCode: the fallback window loaded the app origin independently
  of the shell iframe.
