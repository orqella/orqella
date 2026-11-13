# Compatibility

## Hub API

The desktop client targets the Orqella hub HTTP/API **v1** surface exposed by
`/api/v1/info`. It checks `api_version` before starting a gateway. A hub that
does not report v1 is rejected with a visible error.

The shell, modules, app proxy, WebSocket, and SSE routes are the same routes the
browser dashboard uses. No daemon-specific desktop route is required.

## Link API

The client uses the core-owned `orqella-link-core` and the existing Link API:
device-code login, device registration, ticket issuance, SSH certificate
issuance, and device revocation. It does not implement a parallel account or
ticket protocol.

## Version policy

- Client version is independent of daemon and module versions.
- A future release should define a minimum hub version and show an update hint
  when connected to an older hub.
- Desktop updates must not restart or update the daemon.
- Link API changes must remain backward compatible for released clients or
  carry an explicit API version.
