# Desktop release checklist

## Build

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd desktop/src-tauri
cargo tauri build --bundles deb,appimage
```

Expected Linux artifacts:

```text
target/release/bundle/deb/Orqella Desktop_0.2.1_amd64.deb
target/release/bundle/appimage/Orqella Desktop_0.2.1_amd64.AppImage
```

## Signing

- macOS: Developer ID certificate, hardened runtime, notarization, stapling.
- Windows: Authenticode certificate and signed installer.
- Linux: publish checksums and, if AppImage, a signed update manifest.
- Updater: private signing key must remain outside the repository; public key is
  in `tauri.conf.json`.

## Smoke matrix

1. Fresh profile -> local daemon -> shell -> Terminal -> app.
2. Link device-code approval -> remote hub -> direct tunnel -> shell.
3. OpenCode app (HTTP + SSE) and Home Assistant app (cookies + framing + WS).
4. App-origin isolation: cookie/storage leak test.
5. Sleep/wake and network change.
6. Disconnect, sign out, quit cleanup.
7. Deep link `orqella://settings` and `orqella://hub/<id>`.
8. Notification permission and test.
9. Microphone/Voice Assistant.
10. Update check and signed update install.

## Blocking release items

- macOS and Windows signing/notarization credentials.
- A hosted signed updater manifest.
- Platform WebView validation on macOS and Windows.
- Independent security review.
