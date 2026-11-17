# Updates

## Flow

**Settings → Updates** checks the signed Tauri updater manifest configured in
`tauri.conf.json`. The commands are gated by the compile-time
`ORQELLA_DESKTOP_UPDATER` environment variable: preview builds leave it unset
and show "Release updates are not configured in this preview build." instead of
calling a dead endpoint. Release builds set it to `1`. When a newer version is available the client shows its
version and release notes and reveals **Install <version>**.

Installing:

1. The updater re-checks the manifest.
2. The artifact downloads with signature verification.
3. Progress is emitted to the settings page as
   `orqella-update-progress` (`downloaded`, optional `total` bytes).
4. The artifact is installed and the app emits `orqella-update-installed`.
5. The app relaunches into the new version.

On Windows the installer performs the relaunch; on macOS and Linux the client
calls `AppHandle::restart()` after installation.

## Verification

The settings page was previously exercised on Linux; re-run this checklist
after the in-window settings change:

- **Check for updates** in a preview build returns the explicit
  "Release updates are not configured" message instead of calling a dead
  endpoint or crashing.
- The update panel serializes version, release notes, and publish date.
- The install command compiles behind the same updater plugin and emits
  progress/installed events.

A full download/install/relaunch run requires a hosted, signed updater manifest
and a private signing key, which are not in this repository. That remains a
release-environment step.

## Requirements

- Keep the updater private key outside the repository.
- Publish `latest.json` plus platform artifacts at
  `https://releases.orqella.io/desktop/latest.json`.
- Build release binaries with `ORQELLA_DESKTOP_UPDATER=1`.
- Validate the signature before shipping a beta.
