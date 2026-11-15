# Clean-machine validation guide

Use this guide before the first public release. Run it on a clean Linux, macOS,
and Windows machine or VM. The goal is to prove that a new user can build,
install, run, and remove Orqella without an existing checkout, Cargo cache,
private credentials, or Orqella config.

## 1. Prepare the VM

Use a disposable VM or a fresh OS install with a snapshot taken before the
first Orqella command.

| Platform | Suggested image | Notes |
| --- | --- | --- |
| Linux | Ubuntu 24.04 LTS | Test systemd user service and browser dashboard |
| Windows | Windows 11 | Test WebView2 and the PowerShell installer |
| macOS (VM not supported) | macOS 14 or newer | Test the no-setup loopback path, optional Safari host entries, and desktop client dependencies |

Install OS updates and take a snapshot. Do not copy an existing
`~/.config/orqella`, `~/.local/share/orqella`, `%APPDATA%\orqella`, or
`%LOCALAPPDATA%\orqella` into the VM.

Confirm the starting state:

```bash
# Linux/macOS
test ! -e ~/.config/orqella && echo "no config"
test ! -e ~/.local/share/orqella && echo "no data"
```

```powershell
# Windows
Test-Path "$env:APPDATA\orqella"
Test-Path "$env:LOCALAPPDATA\orqella"
```

Both should report `False`.

## 2. Install prerequisites

Install the build and test prerequisites:

```bash
# Linux
sudo apt-get update
sudo apt-get install -y git curl build-essential pkg-config python3 python3-venv nodejs npm
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
```

On Debian/Ubuntu for the desktop client, also install:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev patchelf file libssl-dev
```

macOS:

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
```

Windows PowerShell:

```powershell
winget install Rustlang.Rustup
winget install Git.Git
winget install Python.Python.3.12
winget install OpenJS.NodeJS.LTS
```

Open a new terminal after installing so `cargo`, `git`, and `python` are on
`PATH`.

## 3. Clone and verify the source tree

```bash
git clone https://github.com/orqella/orqella.git
cd orqella
git status --short
```

`git status` should be clean. Then run the public checks:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node --check web/app.js
node --test web/tests/*.test.mjs web-client/tests/*.test.mjs
```

Expected: all tests pass and no check requires a private repository or
credential.

## 4. Build and run from source

```bash
cargo build --release
./target/release/orqella serve
```

In another terminal:

```bash
curl -fsS http://orqella.localhost:9477/healthz
```

Expected: `{"ok":true,...}` and the dashboard opens at
`http://orqella.localhost:9477/`.

Check:

- The Overview dashboard renders.
- Settings opens and shows a version.
- The machine selector shows the local machine.
- A local example module can be installed:
  ```bash
  orqella module install ./examples/sidecar-module
  ```
  Review and grant its `backend` permission in Settings → Modules, open the
  module page, call its backend, then remove it.
- `orqella doctor` reports a healthy local setup.
- `orqella backup` creates a zip; `orqella support` creates a redacted zip.

Stop the foreground process after this step.

## 5. Test the installers and service

### Linux

```bash
./install-linux.sh
systemctl --user status orqella.service --no-pager
orqella open
orqella service uninstall
```

### macOS

```bash
./install-macos.sh
launchctl print "gui/$UID/com.orqella.daemon" | head
orqella hosts print          # optional Safari check
orqella service uninstall
```

Verify `http://127.0.0.1:9477/` works in Safari with no `/etc/hosts` entries
and that the installer did not prompt for sudo. Then, only if testing the
optional Safari path, run `orqella hosts install` and verify
`http://orqella.localhost:9477/` plus a configured app host.

### Windows

```powershell
.\install-windows.ps1
Get-Process orqella -ErrorAction SilentlyContinue
orqella open
orqella service uninstall
```

The Windows installer should not require a separate Visual C++ runtime.

## 6. Test the desktop client

Follow `client/README.md`. On Linux:

```bash
cd client
cargo build -p orqella-desktop
./target/debug/orqella-desktop
```

Check:

- The local daemon opens in the native window.
- Settings opens from the app menu and `Ctrl+,` / `Cmd+,`.
- A full app surface opens in the window.
- Quitting and relaunching restores the last local or remote session.

Do not test signed updates or App Store/Play distribution here; those use
private release credentials.

## 7. Test a clean restart and no-cloud behavior

1. Stop the daemon/service.
2. Disconnect the VM from the internet.
3. Start Orqella again.
4. Confirm the local dashboard, local modules, Settings, backups, and doctor
   still work without network access.
5. Reconnect the network only when you intentionally test Link.

Link is a managed hosted service and is not required for local use.

## 8. Test module installation

When the first-party module repositories are published after review, test one
public module URL:

```bash
orqella module trust <publisher-public-key> --label "orqella"
orqella module install https://github.com/orqella/orqella-system
orqella module list
```

Git installs verify the publisher signature. If the repository is unsigned, pass
`--allow-unsigned` for a one-off validation only.

Before they are published, use the bundled `examples/sidecar-module` and a
local module checkout instead. Do not use a private module URL in a public
validation run.

## 9. Record the result

For each platform, record:

- OS image and version
- Rust, Python, and Node versions
- Exact commands run
- Which checks passed or failed
- Any manual UI issues
- Whether the install and uninstall left stale services, optional hosts entries,
  or config/data directories
- Screenshots of the dashboard and desktop client if useful

Add the platform result to the maintainer release checklist or the release
issue. Do not mark the release ready until all three platforms have a
recorded result.

## 10. Clean up

Restore the VM snapshot or remove the test config/data and service:

```bash
orqella service uninstall
orqella hosts remove       # where applicable
rm -rf ~/.config/orqella ~/.local/share/orqella              # Linux
rm -rf ~/Library/Application\ Support/orqella                # macOS
```

On Windows, remove the per-user service entry and the
`%APPDATA%\orqella` / `%LOCALAPPDATA%\orqella` directories.
