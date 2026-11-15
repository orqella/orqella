# Licensing

Orqella uses a split model: the core and the native clients are open source,
while the hosted service and its production infrastructure remain proprietary.

## Core — MIT

The Orqella core is licensed under the MIT License. See [`LICENSE`](../LICENSE).

This covers the daemon, dashboard, desktop client, CLI tools, module runtime,
local installers, packaging, core documentation, and the shared client-side Link
and tunnel transport in `crates/orqella-link-core` and
`crates/orqella-tunnel-core`.

Native mobile application shells are maintained in a separate private
repository.

## Hosted platform — proprietary

The hosted Orqella Link service is proprietary and confidential. Copyright (c)
2026 Griffin O'Neill. All rights reserved. It is not part of this repository.

The private platform includes:

- the Link account, device, ticket, entitlement, and billing services;
- managed relay credential issuance and the relay broker;
- hosted-service terms, privacy, and acceptable-use policies;
- deployment configuration, production infrastructure, and release signing
  material.

The website source is licensed separately under the MIT License. Production
deployment automation and cloud infrastructure are not covered by the core MIT
license.

## First-party modules

First-party Orqella modules are separate repositories licensed under the MIT
License:

- `system`
- `opencode`
- `vm-lab`
- `voice-assistant`
- `terminal`
- `home-assistant`
- `workspace-picker`

Each module repository carries its own `LICENSE` file so it remains
self-contained when cloned or installed independently of the core. The shared
MIT text is also available at [`LICENSES/MIT.txt`](../LICENSES/MIT.txt).

Third-party modules, dependencies, vendored libraries, and assets remain under
their own licenses. Redistributing a module must preserve its license notice.

## Contributions

Contributions to this repository are accepted under the MIT License unless a
specific file says otherwise. Do not contribute code you do not have the right
to license under these terms.
