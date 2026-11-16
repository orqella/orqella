# Changelog

All notable changes to the open-source Orqella core, dashboard, browser client,
and desktop client are recorded here.

## Unreleased

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
