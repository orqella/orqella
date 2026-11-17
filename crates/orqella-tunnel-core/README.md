# orqella-tunnel-core

Shared Rust SSH tunnel core for the native Orqella clients.

- requires an explicit SHA256 host-key fingerprint;
- generates an Ed25519 device key on request, or accepts a private key;
- opens a loopback listener and forwards each connection over SSH direct-tcpip;
- is foreground-only by design;
- exposes a UniFFI API for Swift and Kotlin.

## CLI harness

```bash
cargo run --bin orqella-tunnel -- \
  --host studio.local --user user --key ~/.ssh/id_ed25519 \
  --fingerprint SHA256:... --remote-port 9477 --local-port 7790
```

Then browse `http://127.0.0.1:7790`.

## Mobile API

`MobileTunnel` owns one foreground tunnel. Key material is passed as bytes so
the native app can keep it in Keychain/Keystore:

```text
MobileTunnel()
  start(MobileTunnelConfig) -> loopback URL
  startLink(MobileLinkTunnelConfig) -> loopback URL
  appOrigin(node, mount, host) -> isolated app origin
  stop()
  isRunning()
  localUrl()

generateSshKey(comment) -> { privateKey, publicKey, fingerprint }
```

`generateSshKey` creates an Ed25519 key in OpenSSH format. Store `privateKey`
immediately in platform secure storage; only `publicKey` should appear in the
UI. `MobileTunnelConfig.expected_host_fingerprint` is required. Pass a stable
`local_port` so the WebView origin, and its localStorage, survives app
launches; `0` asks the OS for an ephemeral port and the core falls back to one
when the preferred port is busy. Call `stop()` when the app backgrounds.

`appOrigin` allocates a second loopback listener on a free `127.0.0.x` address
while the tunnel is running. The listener speaks HTTP to a WebView, rewrites the
Host header to the canonical app host, strips the tunnel credential, and
forwards over the same SSH session. Each app gets a distinct origin so app
cookies and storage stay out of the shell and other apps. App origins stop with
the tunnel.

## Bindings

Generate bindings from an unstripped host library. Release builds inherit the
workspace strip setting and do not contain the metadata bindgen reads.

```bash
cargo build --lib
cargo run --features cli --bin uniffi-bindgen -- generate \
  --library target/debug/liborqella_tunnel_core.so \
  --language kotlin --out-dir ../android/app/src/main/java
cargo run --features cli --bin uniffi-bindgen -- generate \
  --library target/debug/liborqella_tunnel_core.so \
  --language swift --out-dir ../ios/Generated
```

Use the platform build scripts in the private mobile repository instead of
copying these commands into an app checkout.
