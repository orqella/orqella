# ADR 0001: One user-facing port and canonical app origins

Status: accepted.

The daemon exposes one loopback listener. The shell lives at
`orqella.localhost`; a full embedded app gets a distinct host such as
`app.orqella.localhost`, and a remote app gets
`app.<node>.orqella.localhost`. The host is rewritten to the appropriate
`/p/...` or `/n/...` route before Axum routing.

Consequences:

- The browser never needs a second port.
- Full apps get a separate browser origin instead of sharing shell storage.
- Mobile clients must resolve `*.localhost` inside their WebView; otherwise a
  path-only full apps are not supported; every app mount must configure a host.
- Absolute URLs emitted by the daemon are not sufficient for a foreground
  tunnel; the WebView should prefer origin-relative URLs when the tunnel
  endpoint is `127.0.0.1:<ephemeral>`.
