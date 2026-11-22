# Changelog

All notable changes to the open-source Orqella core, dashboard, browser client,
and desktop client are recorded here.

## 0.4.0 — 2026-11-22

- Reworked daemon updates to use GitHub Releases instead of a git checkout.
  `orqella update check` finds the newest published stable `v<version>`
  release, selects the archive for the host platform, and reports the version.
  `orqella update apply` downloads it, verifies the matching `SHA256SUMS.txt`
  entry (and a signed `ORQELLA_UPDATE.json` when an update key is configured),
  replaces the running binary, and restarts the per-user service. The updater
  no longer uses `git`, Cargo, an installer script, `[update] repo/branch/
  remote/installer`, `allow_unsigned`, or `orqella update configure`.
- Moved the dashboard **Edit** control below the overview cards, aligned to the
  left edge, so the top of the dashboard stays free for content. The edit bar
  and card picker also render below the grid while editing.
- Added a fast reconnect path for mobile Link tunnels. The core now pings the
  live SSH transport instead of using an HTTP health request, reuses SSH
  certificates and relay credentials for their short TTLs, skips redundant
  device-token exchange, and uses one fewer ticket fetch when the certificate
  is cached. The mobile shells keep the dashboard WebView and open app surfaces
  mounted through a reconnect, show a non-blocking status banner, and replace
  failed connection attempts with a “Waiting for network…” state when the
  device is offline. The loopback listener is now owned by a durable core
  gateway: it stays bound while the SSH transport is replaced, app origins
  survive the swap, and requests accepted during the gap wait briefly for the
  new transport instead of failing with connection refused.
- Added browser-style page zoom to the desktop client. `Ctrl` + scroll (`Cmd` +
  scroll on macOS), `Ctrl`/`Cmd` + `+`/`-`, and `Ctrl`/`Cmd` + `0` now change
  and reset the native webview zoom, including content inside module and app
  frames. The dashboard receives only a narrowly scoped `zoom_page` capability.
- Restored `orqella firewall status|install|uninstall` for the Link direct UDP
  port. `status --check` exits non-zero when the rule is missing. The platform
  installers now ask once and install the LAN-scoped rule automatically, with
  `--firewall`/`--no-firewall` overrides for unattended installs. Link uses UDP
  7843 by default so the rule stays valid across restarts; set `[link] port = 0`
  for an ephemeral port. The helper detects UFW, firewalld, the macOS
  application firewall, or Windows Defender and never opens the port to the
  public internet. Link still attempts NAT hole punching when no LAN path is
  available, and SSH remains direct-only.
- Split backups into `orqella backup create` and `orqella backup restore`.
  Backups now include the theme override, custom themes, module signing key, and
  Link identity keys. Restore stages and validates the archive before replacing
  live files, refuses support bundles, unsafe paths, symlinks, and duplicate
  entries, and writes a pre-restore safety backup by default. `orqella backup`
  without a subcommand prints help, and restore works even when the current
  config is invalid.
- Fixed desktop-client external links doing nothing. `target="_blank"` and
  `window.open` requests now open in the system browser instead of silently
  creating no window. New-window requests to an allowlisted loopback origin
  navigate the existing window; the client never creates a second Tauri webview.
- Reworked dashboard layout. Dashboards now use the full content width, and the
  grid folds its column count down on narrower windows instead of producing
  horizontal scrolling. Desktop and mobile layouts are independent, with the
  phone using its own card order and heights. The daemon now stores layouts
  (including the Overview visibility and geometry) so every client connected to
  the same machine sees the same arrangement, and `orqella backup create` now
  includes the dashboard store. Card swap previews stay anchored instead of resetting
  their transition on every pointer move.
- Fixed the Link browser unlock always failing on a Link-signed-in hub. The
  daemon stores a device-scoped Link token, but the unlock compared accounts
  with `/v1/me`, which only accepts account session tokens. Unlock now proves
  ownership by checking that the browser's account owns this machine's
  registered Link device, and the shell shows the daemon's actual error text
  instead of a generic "Link unlock failed".
- Added `[link] port` to pin the hub's direct iroh UDP listener. Link SSH
  remains direct-only, and a connection is closed if its selected path becomes
  relay-only. Orqella does not modify host firewall rules; the UDP port is
  never exposed publicly.
- Added isolated loopback app origins for native mobile clients. The shared
  tunnel core can allocate a `127.0.0.x` listener per app, rewrite the Host
  header to the canonical app host, and keep app cookies out of the shell
  origin. The shell exposes `window.__ORQELLA_NATIVE_APP_OPEN` so mobile clients
  can present full apps in a top-level WebView; the canonical app iframe remains
  the fallback for older clients. The shell CSP advertises the mobile app-origin
  range, and the tunnel also injects those sources into the shell response so a
  newer phone works with an older hub shell.
- Fixed isolated mobile app origins dropping keep-alive requests after the
  first one. The tunnel only rewrote `Host` on the initial request, so later
  requests on the same connection reached the hub with a `127.0.0.x` host and
  fell back to the shell router; Home Assistant could then reload into a blank
  or black app surface. Every request head is now rewritten.
- Added microphone and camera capture support to the mobile shells for
  loopback shell and isolated app origins.
- Added `MobileTunnel.appOrigin(node, mount, host)` to the UniFFI surface.
- Added a Settings → Agents tab that separates the daemon's headless agent from
  the Agent Harness workspace default, puts both defaults and workspace behavior
  in one place, and embeds the agent registry. The old AI agent card moved out
  of General, and Agent Harness module settings now point to the new tab.
- Fixed btop overview cards losing their bottom border at fractional display
  scales when the card's bottom edge lands on the overview grid boundary.
- Added the btop built-in theme, modeled on btop++'s built-in "Default"
  terminal theme: black panes, monospace type, square corners, btop's four box
  border colors, titles inlaid in the top border, dark-red selection, and
  blocky green-yellow-red meters.
- Added first-party Catppuccin themes: `catppuccin-latte` and
  `catppuccin-mocha`. Both use the official Catppuccin palettes and style-guide
  mappings (mauve accent, peach secondary accent, rosewater cursor, overlay2
  selection, and 12px/6px corner radii). The `catppuccin-latte` and
  `catppuccin-mocha` ids were previously aliases for Daybreak and Nocturne and
  now resolve to these themes.
- Fixed module pages getting stuck on their boot screen when a WebView delivers
  the iframe load event after the page announces readiness. Returning to a page
  that opened an app surface now reloads it so the launch flow runs again.
- Added the Cyber built-in theme: a phosphor-green circuit grid with pulsing
  traces, faint rain, CRT scanlines, compositor-only motion, and
  reduced-motion fallbacks.
- macOS installers and `orqella service install` no longer edit `/etc/hosts`.
  Safari host entries are an opt-in `orqella hosts install` step; the dashboard
  also works at the loopback origin without them.
- Initial public repository preparation.
- Added `orqella skill`, which prints the bundled agent guide as plain text.
- The core, browser client, and desktop client share one repository and one
  release stream.
- Native mobile shells are maintained in a separate private repository.
- Orqella Link is a managed service; the public client has no account-service or
  relay configuration.
- Custom dashboard definitions and card selection are stored by the daemon and
  shared by every client connected to that machine. Card layout remains local to
  each device.
