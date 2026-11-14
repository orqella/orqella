# ADR 0001: Isolated loopback origins for desktop app surfaces

- Status: accepted
- Date: 2026-11-11
- Context: Orqella desktop client

## Decision

The desktop client keeps full applications inside the dashboard shell, but each
app is served on a dedicated loopback origin:

```text
shell:  http://127.0.0.1:<stable-port>/
app A:  http://127.0.0.2:<stable-port>/
app B:  http://127.0.0.3:<stable-port>/
```

The gateway rewrites the upstream `Host`, `Origin`, `Referer`, `Location`, and
cookie `Domain` values. It also rewrites the shell CSP `frame-src` directive to
include the allocated app origins.

The shell receives an injected resolver and an iframe `src` rewrite shim. The
resolver is the forward-compatible path; the iframe hook keeps older hub shells
working until the optional `window.__ORQELLA_APP_ORIGIN_RESOLVER` hook is
released in core.

## Why not `*.orqella.localhost`

WKWebView/Safari does not reliably resolve arbitrary `.localhost` subdomains on
current macOS without `/etc/hosts` entries. Requiring an administrator-level
hosts edit for every app is not acceptable for a desktop client.

## Why loopback aliases

- No DNS dependency.
- Distinct origin, cookie jar, localStorage, and IndexedDB per app.
- Root-relative app assets work unchanged.
- Loopback is a secure context for browser APIs.
- No public listener.
- No wildcard DNS or hosts-file management.

## Fallback

If a platform or app cannot run correctly in an iframe on a loopback alias,
open that app in a dedicated Tauri window using the same gateway and per-app
data store. Do not move apps onto the shell origin.

## Implementation notes

- macOS uses per-app `*.orqella.local` names and unique loopback ports because
  only `127.0.0.1` exists on `lo0`. Those origins share a registrable domain,
  so app responses are marked `Origin-Agent-Cluster: ?1` to keep them
  origin-keyed.
- The shell CSP lists the gateway's app-origin allocation namespace rather than
  only the current leases. A shell document cannot learn about a later lease
  without a reload, and peer apps are leased on demand.
- Path-based `/p/` and `/n/<node>/p/` URLs are compatibility input only: the
  shim rewrites them to an app origin, and the gateway rejects them if they
  reach the shell listener.

## Verified

- `127.0.0.2` and `127.0.0.3` app origins load OpenCode and Home Assistant.
- Cookies/localStorage set on one app origin are not visible on the other app
  origin or the shell origin.
- The shell CSP contains the allocated app origins.
- Terminal WebSocket, OpenCode SSE, and app HTTP traffic pass through the
  gateway.
