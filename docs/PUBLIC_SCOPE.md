# Public scope

This repository contains the open, local-first part of Orqella. It is the
canonical source for the daemon, dashboard, module runtime, browser transport,
desktop client, and the shared client-side Link transport.

## Included

- The Rust daemon in `src/`.
- The dashboard shell and module assets in `web/`.
- The browser WebRTC client in `web-client/`.
- The native desktop client in `client/`.
- Shared client transport crates in `crates/orqella-link-core/` and
  `crates/orqella-tunnel-core/`.
- The module SDK, host API, theme contract, installers, examples, and public
  documentation.
- Local backups, support bundles, module packaging, signing verification, and
  the local AI build flow.

## Not included

The following are maintained separately and are not part of the MIT-licensed
core:

- The hosted Orqella Link account, device, ticket, entitlement, and billing
  services.
- Managed relay credential issuance and the private relay broker.
- The GitHub App publisher bridge.
- Deployment automation, infrastructure-as-code, production configuration,
  databases, and hosted-service operations.
- Release signing, notarization, App Store/Play credentials, and update-signing
  keys.
- Native iOS and Android application shells.
- The public website source and its deployment pipeline.
- Internal security operations, runbooks, and non-public planning documents.

The shared Rust tunnel core is public because the desktop client uses it.
Native mobile application shells and their platform packaging live in the
private mobile repository.

## Link and relays

The public client connects to the Orqella Link service. This repository does
not expose a public configuration surface for alternate account-service or relay
infrastructure.

Local Orqella does not require an account or the Link service.
