# Deferred and post-v1 work

These items are intentionally outside the first desktop release.

## Native transport

The current remote transport is the core-owned direct-only SSH/iroh forward.
A future `orqella/tcp/1` ALPN could carry HTTP/WebSocket streams directly and
remove SSH channel overhead. The gateway's upstream abstraction should remain
unchanged so that this is an internal transport change.

## Bundled daemon

The client is deliberately client-only in v1. A future installer could offer a
sidecar/local daemon, but that requires process supervision, data migration,
sidecar policy, and independent update handling.

## Mobile gateway reuse

The loopback origin gateway is a separate crate so other native shells can eventually
adopt the same app-origin and streaming behavior. Mobile remains on its existing
foreground tunnel until that path is verified.

## Linux packaging

Flatpak and sandboxed packages need portal, filesystem, DBus, and secret-service
validation. The current Linux artifacts are `.deb` and AppImage.

## App surfaces

If Tauri multiwebview stabilizes, revisit embedding app surfaces as dedicated
child webviews with per-app data stores. Keep the current loopback-origin
gateway as the stable contract.

## Security review

Before a public beta, review:

- gateway Host/Origin/path handling and cookie rewriting;
- remote-content Tauri IPC boundaries;
- deep-link parsing and command execution;
- keychain access and lock-screen behavior;
- updater signing and rollback;
- Linux WebKitGTK permission behavior.

## Platform validation

macOS and Windows bundle builds, signing, notarization, WebView2 runtime
handling, and real sleep/wake/network tests remain platform work.
