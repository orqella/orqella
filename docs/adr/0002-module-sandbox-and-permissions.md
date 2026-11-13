# ADR 0002: Server-enforced module permissions

Status: accepted.

Module UI runs in an opaque-origin iframe. It receives no shell cookie and no
module token. All host access goes through the shell bridge to the daemon.

The daemon checks the module's granted permissions on every RPC. A module update
preserves previous grants and turns new manifest permissions into pending
requests. Pending permissions cannot be used until the user saves the permission
sheet. Local packages are installed with their permissions granted by the user's
local action; archive/peer imports start with pending permissions.

Consequences:

- The UI is not a security boundary.
- Permission changes are visible and reviewable.
- A failed update restores the previous module directory.
