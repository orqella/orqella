# Packaging notes

## Linux

Verified on this machine (WebKitGTK 2.54.1):

```bash
cd desktop/src-tauri
cargo tauri build --bundles deb
cargo tauri build --bundles appimage
```

Artifacts:

```text
target/release/bundle/deb/Orqella Desktop_0.1.0_amd64.deb
target/release/bundle/appimage/Orqella Desktop_0.1.0_amd64.AppImage
```

The binary needs WebKitGTK 4.1 and the usual GTK/AppIndicator runtime
libraries. On tiling compositors the in-window menu bar is hidden to preserve
screen space; Settings remains available via `Ctrl+,`, the tray, and
`orqella-desktop --settings`.

On Wayland the client sets `WEBKIT_DMABUF_RENDERER_FORCE_SHM=1` before
GTK/WebKit initializes. This selects the shared-memory transport without
emptying the renderer transport mode, avoiding both the WebKitGTK DMA-BUF
Wayland failures (`Gdk-Message: Error 71 (Protocol error)`, blank secondary
WebViews) and the WebKitGTK 2.54 web-process abort in
`PlatformDisplay::sharedDisplay()` when a page creates a WebCodecs H.264
decoder. Explicit values for either WebKit variable are respected; avoid
`WEBKIT_DISABLE_DMABUF_RENDERER=1` on 2.54 as it reintroduces the abort. The
client also marks pages with `data-orqella-motion="reduce"` when it selects
that transport, because the shared-memory path repaints the full window for
every animation frame; themes use that marker to freeze ambient animation while
keeping their static styling.

## macOS

Verified on an Intel Mac running macOS 12.7.6. See
[`MACOS.md`](MACOS.md) for build commands, artifact sizes, app-origin behavior,
and verification evidence.

```text
target/release/bundle/macos/Orqella Desktop.app
target/release/bundle/dmg/Orqella Desktop_0.1.0_x64.dmg
```

The unsigned `.app` builds and runs; the DMG is created with `hdiutil` because
Tauri's Finder-scripted DMG step can block in non-interactive SSH sessions.
`Info.plist` ships `NSMicrophoneUsageDescription`, an audio-input entitlement,
and ATS exceptions for `*.orqella.local`/`*.orqella.localhost`. App origins are
published over unprivileged mDNS (`dns-sd`), not `/etc/hosts`.

Before beta:

- sign with a Developer ID certificate and enable hardened runtime;
- notarize and staple the DMG;
- verify Apple Silicon.

## Windows

Verified in the Windows 11 VM:

```text
target\release\bundle\nsis\Orqella Desktop_0.1.0_x64-setup.exe   9.56 MiB
```

The NSIS per-user installer builds with the Tauri CLI, embeds the WebView2
bootstrapper, and installs under `%LOCALAPPDATA%\Orqella Desktop`. The product
name is deliberately distinct from the daemon's `%LOCALAPPDATA%\orqella` data
directory; installing as plain `Orqella` would place binaries beside the
daemon's `state.json`, modules, and workshop data on case-insensitive Windows.

Silent install (`/S`) created:

```text
%LOCALAPPDATA%\Orqella Desktop\orqella-desktop.exe
%LOCALAPPDATA%\Orqella Desktop\uninstall.exe
Start Menu\Programs\Orqella Desktop.lnk
```

The installed binary launched in the interactive session and rendered the
Orqella shell/disconnected page. Silent uninstall removed the install directory
and shortcut while leaving `%LOCALAPPDATA%\orqella\state.json` and `modules/`
intact.

Before beta:

- sign with an Authenticode certificate;
- verify deep-link registration and firewall behavior for direct iroh endpoints;
- validate the bootstrapper on a machine without WebView2.

## Updates

The Tauri updater plugin is initialized with a generated public key and the
intended GitHub release endpoint. The private signing key lives outside the
repository. `createUpdaterArtifacts` is intentionally disabled until the
release host, signing key handling, and rollback policy are finalized. Updater
artifacts must be signature-verified and published over HTTPS.
