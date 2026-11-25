# Changelog

All notable changes to the open-source Orqella core, dashboard, browser client,
and desktop client are recorded here.

## 0.8.4 — 2026-11-25

- The Link heartbeat now carries the current entitlement, so a running daemon
  sees a trial start, conversion, or expiry without a restart. Settings shows
  the trial end date when the account is trialing.

## 0.8.3 — 2026-11-24

- Link device sign-in now shows one 8-character code. The redundant 6-digit
  confirmation code was removed from the daemon API, web settings, desktop
  transport and settings UI, and docs. The approval page now closes itself
  after the device is approved, with the existing success fallback when the
  browser owns the tab.
- The Link API already accepts and ignores the old confirmation field; this
  release aligns the clients and removes the misleading second code.

## 0.8.2 — 2026-11-24

- The shell now unloads hidden full-app and module iframes when you navigate
  back to a dashboard or another page. Previously a full app such as OpenCode
  kept running behind the visible page, including its timers, WebSockets,
  media, and animations; on WebKitGTK that could pin a web process at a full
  core and eventually abort it. Frame documents reload cleanly when reopened.
- Agent Harness 0.1.2 is the current first-party module. It coalesces streamed
  markdown rendering instead of rebuilding a whole message for every delta,
  groups discovered history under a collapsed rail section, reads live session
  status from the OpenCode service, defaults new-session folders to the current
  or last workspace, and exposes the composer hotkeys as tappable actions.

## 0.8.1 — 2026-11-24

- Fixed Settings → General → Install module reporting "Module index
  unavailable" with `HTTP 404` on every machine. The module index and both
  install actions requested a node-scoped path that repeated `/api/v1`, so the
  request matched no daemon route. The page now uses
  `GET /api/v1/nodes/<node>/module-index` and
  `POST /api/v1/nodes/<node>/modules/install`, restoring index, URL, and
  local-path installs on the local machine and linked peers. Regression in
  0.8.0.

## 0.8.0 — 2026-11-24

- Settings → General → Install module now follows the machine selector. The
  module index shows installed status for the selected machine, and index,
  URL, and local-path installs run on that machine. A hub forwards these
  requests to a linked peer with its peer token.
- Settings → Modules now manages the selected machine. Enable/disable,
  permissions, generated settings, custom settings frames, remove, export,
  sidecar restart and log, and git update/checkpoint run on that machine,
  including a linked peer.
- Fixed Link peer requests failing with `invalid peer token` after the first
  request on a reused HTTP connection. Peer forwards now disable keep-alive for
  ordinary HTTP requests on both ends, while preserving WebSocket upgrades, so
  every request goes through target credential injection.

## 0.7.0 — 2026-11-23

- Added the `node.proxy.manage:<mount>` permission and `proxy.configure` host
  API. A module can create or update its own proxy mount from its settings UI
  without editing `config.toml` or restarting the daemon. The module must hold
  both `node.proxy:<mount>` and the sensitive `node.proxy.manage:<mount>`
  grant; the public host is fixed to `<mount>.<canonical-host>`, upstream URLs
  cannot embed credentials or include a query/fragment, and every change is
  audited.
- Moved the module settings URL/local-path installer above the curated module
  index so it is available before the index finishes loading.
- Release hardening: a daemon release now fails closed when
  `ORQELLA_UPDATE_PUBLIC_KEY` is configured but the tag has no signed update
  manifest. Added `scripts/attach-update-manifest.sh` for attaching a manifest
  to an already-published release, pinned NSIS 3.10 and Tauri CLI 2.12.6, and
  limited workflow write permissions to the jobs that publish.
- The macOS installer no longer removes the Gatekeeper quarantine attribute
  from the desktop app.
- Updated `url` to 2.5.9 to address RUSTSEC-2026-0592, a stack overflow when
  parsing origins from deeply nested `blob:` URLs.
- Rotated the daemon update signing key after the original key could not be
  recovered. The new public key is published in `release/orqella-update.pub`;
  daemons that pin the old key must update `[update] public_key` before
  installing updates from this release onward.

## 0.6.0 — 2026-11-23

- Release builds no longer include the staging module/app origins in the shell
  or per-module-origin CSP. Staging origins remain available in debug builds.
- Desktop releases now require a Tauri updater signing key, publish
  `latest.json` plus signatures, and poll the stable
  `https://orqella.io/desktop/latest.json` endpoint. The website build copies
  the newest published desktop manifest.
- The Linux/macOS installer and the Windows setup now install the first-party
  Agent Harness workspace by default. Both detect an existing OpenCode, Claude
  Code, Codex, or Gemini CLI and configure it immediately; when no coding agent
  is present, the Linux/macOS installer offers OpenCode and the Windows
  components page preselects it. OpenCode installs select a free default model
  from its live model catalog. Opt out with `--no-agent-harness` or the Windows
  component checkbox; control the OpenCode offer with `--opencode` and
  `--no-opencode`.
- Added `orqella setup`, `orqella setup --status --json`, and
  `orqella setup --install-opencode`. Setup installs Agent Harness from the
  curated module index, grants only that module's declared first-party access,
  detects coding agents, enables the npx adapter fallback when Node is present,
  sets the default conversation and headless agent, and adds the OpenCode app
  surface. It restarts a managed per-user service automatically after changing
  its config. Settings → Agents also has an **Install OpenCode** action for
  machines that skipped the offer during installation.
- Archive-backed module index installs no longer require Git. Module installs
  still initialize a git checkout when Git is available; without it the module
  runs as a plain directory and updates/checkpoints explain that Git is needed.
  When an index entry is a GitHub git distribution and Git is missing, setup
  downloads the pinned commit archive and verifies the index tree digest.
- Added a Gemini CLI headless-agent preset.

## 0.5.0 — 2026-11-22

- Added a one-command installer for Linux and macOS. `curl -fsSL
  https://orqella.io/install.sh | sh` resolves the latest daemon and desktop
  releases, verifies their SHA-256 checksums, installs both apps for the
  current user, installs the per-user service, and waits for the dashboard.
  The installer needs no Rust, Git, sudo, or source checkout, and supports
  version pinning, `--dry-run`, `--no-desktop`, and `--no-service`. It detects
  an already-running manual daemon and leaves the service setup to the user
  instead of starting a conflicting unit.
- Added a combined per-user Windows setup that bundles the daemon with the
  latest published Orqella Desktop setup. It installs the CLI and desktop app,
  adds the CLI to the user PATH, registers the startup entry, stops a running
  daemon before replacing it, and removes both apps on uninstall.
- Reworked the website install and download flow around the one-command
  installer: a copyable command in the hero, a platform-aware install panel,
  Windows setup detection, clearer manual archive cards, and generated release
  metadata for the installer.

## 0.4.0 — 2026-11-22

- Made Link connection state visible and recoverable. Settings now reports the
  live device name, entitlement, and any startup error, offers **Sign In Again**
  when Link rejects a saved credential, and no longer treats a stored token as
  proof that the machine is connected. A 401 during startup clears the stale
  credential and stops the retry loop instead of leaving the machine silently
  unregistered. `orqella doctor` prints the live Link state and next action.
  The read-only dashboard banner now says when the machine is not linked, and
  local unlock requires a running Link instance with a clearer error. Desktop
  local-mode Link controls authenticate with the local bootstrap token.
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
