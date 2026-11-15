# Changelog

All notable changes to the open-source Orqella core, dashboard, browser client,
and desktop client are recorded here.

## Unreleased

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
