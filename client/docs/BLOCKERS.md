# Remaining blockers

Everything below needs a resource that is not available in this Linux
development environment. Code paths are implemented unless stated otherwise.

## macOS signing/notarization and Apple Silicon

The Intel macOS 12.7.6 host builds and runs the app; WKWebView app origins are
verified. Still needed:

- M6-02/M6-11: Developer ID signing, hardened runtime, notarization, stapling,
  and Apple Silicon coverage.

Required: an Apple Developer team, a Developer ID Application certificate,
notarization credentials, and Apple Silicon hardware.

## Windows Authenticode certificate

M6-03's installer, WebView2 bootstrapper, silent install/launch/uninstall, and
daemon-data preservation are verified. Signing is not.

Required: an Authenticode code-signing certificate and timestamp service.

## Signed update/beta hosting

M5-09's check, notes, download/install, progress, and relaunch are implemented.
M6-05/M6-12 need a hosted `latest.json`, signed platform artifacts, and a beta
channel. The private updater key is intentionally absent from the repository.

## Network matrix

M7-02 needs real LAN/NAT/VPN/cellular transitions and suspend/resume on physical
hardware. Interface-change and wake-gap detection are implemented and unit
tested, but the matrix itself is environmental.

## Independent security review

M7-01 needs an external reviewer for gateway binding, WebView IPC isolation,
deep links, keychain use, and the update chain. Internal notes and tests are in
`docs/SECURITY.md`, `docs/APP_SURFACES.md`, and `docs/UPDATES.md`.

## Remote Link and revocation tests

M3-10 needs an automated Link test account/device pair. M3-07's device
revocation UI needs an account-service revocation endpoint; the current core
Link API only rejects revoked devices, it does not expose a revoke call to the
client.

## Ubuntu/Fedora runtime matrix

M6-04 needs clean Ubuntu and Fedora machines (or a container runtime this
environment can access; Docker is permission-denied here). The `.deb` and
AppImage build and pass on the current Arch/WebKitGTK host.

## Local reconnect test

M2-10 automates fresh-profile startup, repeated proxying, and quit cleanup.
Reconnect-after-daemon-restart still needs a controlled stop/start of the local
daemon so the test does not disturb a user's active hub.

## Microphone hardware

M5-04's global permission handler and macOS usage string/entitlement are in
place. Voice Assistant recording needs a microphone and a running module in the
test environment.
