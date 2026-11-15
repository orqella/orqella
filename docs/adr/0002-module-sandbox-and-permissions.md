# ADR 0002: Server-enforced module permissions

Status: accepted.

Module UI runs in an opaque-origin iframe. It receives no shell cookie and no
sidecar module token (`ORQELLA_MODULE_TOKEN`). It does receive a per-module
`ws_token` for module WebSocket routes. All other host access goes through the
shell bridge to the daemon.

The daemon checks the module's granted permissions on every RPC. A module update
preserves previous grants and turns new manifest permissions into pending
requests. Pending permissions cannot be used until the user saves the permission
sheet. A local install grants non-sensitive permissions immediately and leaves
sensitive capabilities (`backend`, `agent`, `media.record`, `app.open`, and
`node.proxy:*`) pending for explicit review. Archive and peer imports start with
all permissions pending.

Consequences:

- The UI is not a security boundary.
- Permission changes are visible and reviewable.
- A failed update restores the previous module directory.
