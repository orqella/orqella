# macOS

Tested on an Intel MacBook Pro running macOS 12.7.6 with Command Line Tools
(Xcode full install is not required for this build).

## Build

```bash
cd desktop/src-tauri
MACOSX_DEPLOYMENT_TARGET=11.0 cargo tauri build --bundles app
```

The `.app` is unsigned when no Developer ID identity is installed. Tauri's
`bundle_dmg.sh` step can block on Finder/AppleScript automation in a
non-interactive session; create the DMG directly instead:

```bash
cd target/release/bundle
hdiutil create -volname "Orqella Desktop" \
  -srcfolder "macos/Orqella Desktop.app" \
  -ov -format UDZO "dmg/Orqella Desktop_0.2.1_x64.dmg"
```

Artifacts from the verified build:

```text
Orqella Desktop.app                         29 MB
Orqella Desktop_0.2.1_x64.dmg               13.2 MiB
sha256 386d47d365c1c87affb4e2783c47093f6318ba0f693db77dd87a3f172c15a339
```

No Apple Developer identity was present (`security find-identity -v -p
codesigning` returned `0 valid identities found`), so signing, hardened
runtime, notarization, and stapling are still pending.

## App origins (no sudo, no hosts file)

macOS only assigns `127.0.0.1` to `lo0` by default; binding `127.0.0.2` fails
with `Can't assign requested address`. The gateway therefore uses a per-app
`*.orqella.local` hostname plus a unique loopback port:

```text
upstream host: opencode.orqella.localhost
browser origin: http://opencode.orqella.local:<unique-port>/
```

Each app origin is registered over mDNS with the built-in `dns-sd` tool:

```text
dns-sd -P orqella-opencode-... _orqella._tcp local <port>   opencode.orqella.local 127.0.0.1
```

The registration is unprivileged and lives only while the client runs. The
gateway waits for the name to resolve before returning the origin. Cookies and
localStorage remain host-scoped per app.

**The desktop client does not modify `/etc/hosts` and requires no sudo.** Core
may still manage `/etc/hosts` for its Safari/browser dashboard; that is
independent of this client.

`Info.plist` includes ATS exceptions for `orqella.local` and
`orqella.localhost` plus `NSAllowsLocalNetworking`; without them WKWebView
blocks the cleartext app-origin HTTP requests.

## Verification

- `cargo check`, `cargo test -p orqella-desktop --lib` (7 tests), and
  `cargo test -p orqella-desktop-gateway` (14 tests) pass.
- The built app launched against the local daemon and served the dashboard
  gateway at `http://127.0.0.1:19477/` (HTTP 200).
- With **no `opencode.orqella.local` entry in `/etc/hosts`**, the shell shim
  exposed `http://opencode.orqella.local:<port>/`; `dscacheutil` resolved it to
  `127.0.0.1` through the client's `dns-sd` registration, and the gateway bound
  the port on `127.0.0.1`.
- The app origin loaded in WKWebView and opened multiple connections to the
  app-origin port. Before the ATS exception, the system log reported
  `App Transport Security has blocked a cleartext HTTP connection`.
