# Orqella desktop client

A native Tauri 2 desktop client for Orqella on macOS, Windows, and Linux.

The client loads your existing Orqella dashboard and modules in a native window.
It does not reimplement the dashboard. Local traffic goes through a loopback
origin gateway; remote application data uses Orqella Link's native direct-only
tunnel. Browser WebRTC remains a separate client.

This client lives in the Orqella repository under `client/`. See
[`docs/BLOCKERS.md`](docs/BLOCKERS.md) for items that still need external
signing or hardware.

## What you need

- An Orqella daemon running locally (`orqella serve`) **or** an Orqella Link
  account with a reachable machine.
- Linux: WebKitGTK 4.1, GTK 3, and libayatana-appindicator runtime libraries.
- macOS: 11 or newer.
- Windows: 10/11 with WebView2 (the installer embeds the bootstrapper).
- For source builds: a recent Rust stable toolchain (tested with 1.99) and the
  platform build tools.

The client does not bundle the daemon.

## Install

The Linux/macOS one-command installer and the Windows setup install the daemon,
CLI, and desktop client together:

```bash
curl -fsSL https://orqella.io/install.sh | sh
```

The signed desktop release channel is not published yet. For a manual install,
build from source, or use a development artifact produced by the packaging
commands below:

| Platform | Artifact |
| --- | --- |
| Linux (Debian/Ubuntu) | `Orqella Desktop_<version>_amd64.deb` |
| Linux (portable) | `Orqella Desktop_<version>_amd64.AppImage` |
| Windows | `Orqella Desktop_<version>_x64-setup.exe` |
| macOS | `Orqella Desktop_<version>_x64.dmg` |

The current artifacts are unsigned development builds. macOS signing/
notarization, Windows Authenticode, and the signed updater manifest are still
pending; see [`docs/BLOCKERS.md`](docs/BLOCKERS.md).

### Build from source

The client shares the Link transport and tunnel crates with the daemon, so keep
the repository layout intact:

```text
orqella/
  client/       (this client)
  crates/       (shared Link and tunnel crates)
  src/          (daemon)
```

Check the layout before building:

```bash
./scripts/check-source-checkout.sh
```

Install the Tauri CLI once:

```bash
cargo install tauri-cli --version 2.12.4 --locked
```

On Debian/Ubuntu, install the Tauri build dependencies:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev patchelf build-essential file libssl-dev
```

Then check and build:

```bash
cd client
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p orqella-desktop
```

Run the local development build:

```bash
./target/debug/orqella-desktop
```

On Wayland the client automatically sets
`WEBKIT_DMABUF_RENDERER_FORCE_SHM=1` before GTK/WebKit starts, which selects
the shared-memory transport and avoids the Gdk protocol error seen on some
compositor/driver combinations. It deliberately does not use
`WEBKIT_DISABLE_DMABUF_RENDERER=1`: on WebKitGTK 2.54 that can leave the web
process without a shared display and abort on the first WebCodecs H.264
decoder (for example the VM Lab console). You can still override either
variable explicitly if needed.

Build installers (requires `cargo-tauri` 2.12+):

```bash
cd desktop/src-tauri
cargo tauri build --bundles deb,appimage   # Linux
cargo tauri build --bundles nsis           # Windows
cargo tauri build --bundles app,dmg        # macOS
```

## First run

1. Start the daemon on the same machine:

   ```bash
   orqella serve
   ```

2. Launch Orqella Desktop. It connects to `http://127.0.0.1:9477` by default.
3. If no local daemon is found, the window shows the in-window connection
   screen with **Use Local Service** and **Connect with Link**. There is no
   close button until a connection exists.
4. After connecting, open **Settings** from the app menu, with `Ctrl+,`
   (`Cmd+,` on macOS), or from the tray. Settings opens in the main window and
   closes with the X or Escape.
5. Link sign-in is passkey-first. The desktop app never asks for your Link
   password directly; if you use the password fallback, the browser page
   requires an authenticator app or recovery code. Enrolled machines keep a
   device token, so normal launches do not ask for a second factor again.
6. To use another machine, use the dashboard's machine selector. In local mode,
   Settings shows the local daemon's Link account; its Link button signs the
   daemon in or out. When disconnected, the desktop client can sign in to Link
   directly and open a remote machine over the native direct tunnel.

The client remembers the last mode, hub, and shell port. If the preferred shell
port is occupied it falls back to the next free loopback port.

## Using the client

- The dashboard, modules, Terminal, OpenCode, and Home Assistant load through
  the loopback gateway.
- `F5` or `Ctrl+R` (`Cmd+R` on macOS) reloads the current window.
- `Ctrl` + scroll (`Cmd` + scroll on macOS) or `Ctrl`/`Cmd` + `+`/`-` changes
  the page zoom, including inside module and app frames. `Ctrl`/`Cmd` + `0`
  restores 100%.
- Native notifications can come from the dashboard via
  `window.__ORQELLA_NOTIFY(title, body)`; embedded app frames use
  `parent.postMessage({ type: "orqella:notify", title, body }, "*")`.
- Deep links: `orqella://settings` and `orqella://hub/<id>`.
- **Settings → Updates** checks the signed release channel and can download,
  install, and relaunch when a release is published.

## Platform notes

- **Linux:** `.deb` and AppImage are built and tested on Arch/WebKitGTK and
  native Wayland. The client selects `WEBKIT_DMABUF_RENDERER_FORCE_SHM=1`
  automatically on Wayland for the shared-memory transport. This avoids
  WebKitGTK DMA-BUF protocol failures without the WebKitGTK 2.54
  `WEBKIT_DISABLE_DMABUF_RENDERER=1` display-initialization crash.
  On tiling compositors (Hyprland, Sway, Niri, River, Wayfire) the in-window
  menu bar is omitted; use `Ctrl+,`, the tray, or bind
  `orqella-desktop --settings` in your compositor. GNOME/KDE keep the in-window
  menu. Ubuntu/Fedora runtime testing is still pending.
- **macOS:** Intel builds are tested on macOS 12. App origins use
  `*.orqella.local` plus unique loopback ports because macOS only has
  `127.0.0.1` on `lo0`. The client registers each app hostname over mDNS with
  `dns-sd`, so the desktop client needs **no `/etc/hosts` change and no sudo**.
  (Core may still offer hosts entries for its Safari/browser dashboard; that is
  separate from this client.) Apple Silicon and signing/notarization are still
  pending.
- **Windows:** NSIS per-user installer with embedded WebView2 bootstrapper;
  installs to `%LOCALAPPDATA%\Orqella Desktop` so it does not collide with the
  daemon's `%LOCALAPPDATA%\orqella` data. Authenticode signing is pending.

## Repository layout

```text
desktop/src-tauri/       Tauri app, native commands, download/update flow
desktop/ui/              Settings, startup, theme assets
crates/orqella-desktop-gateway/   Loopback HTTP/WS/SSE/app-origin gateway
crates/orqella-desktop-transport/ Link device login and direct-only tunnel
docs/                    Security, packaging, direct-path, and test notes
```

## Documentation

- [`docs/PACKAGING.md`](docs/PACKAGING.md) — bundle details and platform quirks
- [`docs/MACOS.md`](docs/MACOS.md) — macOS build, app origins, and verification
- [`docs/APP_SURFACES.md`](docs/APP_SURFACES.md) — app origins and limitations
- [`docs/DIRECT_PATH.md`](docs/DIRECT_PATH.md) — direct-only Link verification
- [`docs/DIAGNOSTICS.md`](docs/DIAGNOSTICS.md) — logs and diagnostics export
- [`docs/UPDATES.md`](docs/UPDATES.md) — updater flow and signing requirements
- [`docs/SECURITY.md`](docs/SECURITY.md) — gateway and WebView security model
- [`docs/BLOCKERS.md`](docs/BLOCKERS.md) — what still needs credentials/hardware

## Remote

`https://github.com/orqella/orqella`
