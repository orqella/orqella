# Orqella MCP gateway

`orqella mcp` exposes the running daemon to coding agents through the
[Model Context Protocol](https://modelcontextprotocol.io). It is a local,
read-first gateway: the daemon remains the source of truth for permissions,
audit, module lifecycle, and peer routing.

The gateway speaks MCP over stdio. It supports both the legacy `initialize`
handshake and the stateless `2026-07-28` discovery lifecycle (protocol versions
`2024-11-05` through `2026-07-28`). It is a privileged local client of the same
loopback API the CLI uses, so it does not open a second port and does not change
the one-port architecture. Every write tool is audited by the daemon.

```bash
orqella mcp                     # read-only tools (default)
orqella mcp --profile dev       # module install/dev/config/build tools
orqella mcp --profile admin     # updates, wake, unsigned installs
```

The default profile is `read`. A tool that is not in the active profile is
neither listed nor callable.

## Profiles

| Profile | Adds | Intended use |
| --- | --- | --- |
| `read` | Status, nodes, metrics, modules, module detail, logs, catalog, builds, dashboards, resources | Diagnostics and ordinary questions |
| `dev` | Module install/control/configure, build start, dashboard edit | Building and iterating on modules |
| `admin` | Module update, wake-on-LAN, daemon update check/apply, unsigned git installs | Explicit operator work |

Profiles are capability filters, not separate credentials. All three use the
private `<data>/admin.token` capability. Use `ORQELLA_MCP_TOKEN` to point the
gateway at a different daemon with `--url`.

## Client configuration

Print a ready-to-paste OpenCode snippet:

```bash
orqella mcp --print-config
orqella mcp --profile dev --print-config
```

OpenCode (`~/.config/opencode/opencode.json`):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "orqella": {
        "type": "local",
        "command": ["orqella", "mcp"]
      }
    }
  }
}
```

Claude Code:

```bash
claude mcp add orqella -- orqella mcp
```

Codex:

```bash
codex mcp add orqella -- orqella mcp
```

For module development, use `orqella mcp --profile dev` in the client command
so the agent can install a staging module, read its log, and verify the result.
Use `--profile admin` only for deliberate update or fleet work.

ACP clients can pass the same server in `session/new`:

```json
{
  "name": "orqella",
  "command": "/absolute/path/to/orqella",
  "args": ["mcp", "--profile", "dev"],
  "env": [
    { "name": "HOME", "value": "/home/user" }
  ]
}
```

The `HOME` override matters for an Agent Harness sidecar: the sidecar's `HOME`
is sandboxed, while `orqella mcp` must resolve the real daemon config/data
directories. A sidecar can use `ORQELLA_HOST_HOME` for this value, and
`ORQELLA_BIN` for the exact daemon binary.

## Streamable HTTP

The daemon can also serve MCP at `/mcp` on its existing loopback listener. It is
opt-in and off by default:

```toml
[mcp]
enabled = true
profile = "read"   # read | dev | admin
```

Restart the daemon after changing the section. On the first start with
`enabled = true`, Orqella creates a dedicated bearer token at
`<data>/mcp.token` with owner-only permissions. It is separate from the admin
token, so revoking HTTP MCP does not rotate the CLI capability. Delete the file
and restart to rotate it.

Print a ready-to-paste remote client snippet (it includes the token):

```bash
orqella mcp --print-http-config
```

OpenCode:

```json
{
  "mcp": {
    "servers": {
      "orqella": {
        "type": "remote",
        "url": "http://orqella.localhost:9477/mcp",
        "headers": { "Authorization": "Bearer <token>" }
      }
    }
  }
}
```

Claude Code:

```bash
claude mcp add --transport http orqella http://orqella.localhost:9477/mcp \
  --header "Authorization: Bearer <token>"
```

The endpoint supports the stateless `2026-07-28` lifecycle and legacy
session-based clients. It uses the configured profile for every HTTP client;
there is one token/profile pair per daemon. The endpoint is loopback-only in
the normal configuration. If `[server] allow_remote = true` exposes the daemon
beyond loopback, `/mcp` is exposed with it; use the dedicated token and a
trusted tunnel, and prefer Link for remote access. Requests must pass the
daemon's normal `Host` check and the MCP service's host validation, and are
limited to the configured 4 MiB request body.

The HTTP handler reuses the same tool/resource/prompt implementation as the
stdio server. Its internal self-client uses the private admin token; external
clients never receive that token.

## Core tools

### Read profile

| Tool | Purpose |
| --- | --- |
| `orqella_status` | Version, machine, theme, agent, Link state, and counts. Call first. |
| `orqella_nodes` | Local machine and Link peers with online state, latency, wake support, and errors. |
| `orqella_metrics` | CPU, memory, load, disks, network, temperatures, GPUs, and batteries. `node` selects a peer; `sections` narrows the result. |
| `orqella_modules` | Installed modules with version, enabled state, sidecar state, page path, and pending permissions. |
| `orqella_module` | One module's manifest, permissions, config, sidecar, git, signature, and contributed MCP tools. |
| `orqella_module_log` | Bounded sidecar log tail. Logs may contain sensitive data. |
| `orqella_catalog` | Curated module index with install/update state, permissions, and compatibility. |
| `orqella_builds` | Build/edit jobs, status, module id, session id, workspace, and error. |
| `orqella_dashboards` | Shared dashboards, card visibility, and layouts. |

### Dev profile

| Tool | Purpose |
| --- | --- |
| `orqella_module_install` | Install from an index id, local folder, or git URL; returns the module summary and page path. Relative local paths resolve against the directory where the MCP client launched `orqella mcp`; use an absolute path if the client starts it elsewhere. |
| `orqella_module_control` | `enable`, `disable`, `restart`, or `checkpoint` a module. |
| `orqella_module_configure` | Save declared module settings after schema validation. |
| `orqella_build_start` | Start the configured headless agent on a build or edit job; returns a job handle. |
| `orqella_dashboard_edit` | Create/rename a dashboard or show/hide a card. Raw layout editing stays in the UI. |

### Admin profile

| Tool | Purpose |
| --- | --- |
| `orqella_module_update` | Update from the git upstream (signature-gated). |
| `orqella_node_wake` | Send wake-on-LAN to a peer. |
| `orqella_update` | Check for a release, or apply one with `confirm: true`. |

## Module-contributed tools

A module can contribute tools to the same gateway. Add the `mcp` permission and
a `[mcp]` section to `module.toml`; the module must use a sidecar backend:

```toml
permissions = ["backend", "mcp"]

[mcp]
enabled = true

[[mcp.tools]]
name = "production_status"
title = "Production status"
description = "Read the production health summary."
input_schema = "mcp/production_status.schema.json"
path = "/api/mcp/production_status"
mutating = false
```

`input_schema` is a module-relative JSON Schema file (a JSON object, at most
64 KiB). `path` is a root-relative sidecar endpoint that accepts a JSON body and
returns JSON. Tool names are lowercase letters, digits, `.`, `_`, or `-`;
they are exposed as `module.<module-id>.<tool>`.

Rules:

- `[mcp]` requires `backend.kind = "sidecar"`, the `mcp` permission, and the
  `backend` grant.
- Read-only tools (`mutating = false`) appear in the `read` profile. Mutating
  tools require `dev`. The flag is a declaration used to choose the profile;
  the `backend`/`mcp` grants remain the security boundary, and every call is
  audited.
- The user sees the tools and descriptions in the module permission review.
  Tools stay hidden until `mcp` is granted.
- The sidecar must be running. The daemon forwards arguments to the declared
  path with the existing sidecar token, timeout, and audit.
- Tool names are namespaced to avoid collisions. The full qualified name must be
  at most 128 characters.

The daemon endpoint used by the gateway is privileged and not part of the module
host API:

```
GET  /api/v1/mcp/tools
POST /api/v1/mcp/module-tools/<module>/<tool>
```

Both require a privileged local session or the admin token.

## Resources

| URI | Content |
| --- | --- |
| `orqella://status` | Daemon info snapshot (JSON). |
| `orqella://skill` | The Orqella agent skill. |
| `orqella://docs/module-agent` | Module build guide. |
| `orqella://docs/host-api` | Host API contract. |
| `orqella://docs/theme` | Theme contract. |
| `orqella://catalog` | Module index. |
| `orqella://modules` | Installed module summaries. |
| `orqella://modules/{id}` | One module detail. |
| `orqella://modules/{id}/log` | Recent sidecar log. |
| `orqella://nodes` | Machine inventory. |
| `orqella://dashboards` | Dashboard document. |
| `orqella://builds/{id}` | One build job. |
| `orqella://mcp/tools` | Module-contributed tool catalog. |

Resources are better than tools for static docs and snapshots: clients can read
them without expanding the model's tool list.

## Prompts

- `orqella.module_build` — injects the module guide and the live feedback loop
  (`install` → `module` → `log` → `builds`). Arguments: `request`, optional
  `module_id`.
- `orqella.module_diagnose` — reads one module's manifest, permissions, sidecar
  state, and log and proposes the smallest fix. Argument: `module_id`.

## Security

The gateway follows the daemon trust model:

- It runs as the logged-in user and reads the private admin token. It does not
  expose the token, bootstrap token, peer tokens, Link credentials, sidecar
  WebSocket tokens, backend commands, or module runtime data.
- Read tools have no side effects. Write tools are audited by the daemon and
  carry MCP `readOnlyHint` / `destructiveHint` annotations.
- The gateway intentionally does **not** expose module removal, permission
  grants, trusted-key changes, Link sign-in/out, backup restore, raw config
  editing, a generic HTTP client, or arbitrary command execution.
- Module logs and module tool output are untrusted context. A module can return
  text that attempts prompt injection; hosts should treat it as data.
- `orqella_module_install` with `allow_unsigned = true` requires the admin
  profile. Git installs and updates are signature-gated by default.
- `orqella_update` requires `confirm: true` to apply a release.

## Troubleshooting

- **“Orqella is not reachable”**: run `orqella serve` or
  `orqella service install`; then retry.
- **“could not read …/admin.token”**: the daemon has not started yet. Run
  `orqella open` once, or start the service.
- **A tool is missing**: check the profile. `orqella mcp --profile dev` adds
  install/control/build tools; `admin` adds updates and wake.
- **A module tool is missing**: confirm the module declares `permissions =
  ["backend", "mcp"]`, has `[mcp] enabled = true`, is enabled, and has both
  grants in Settings → Modules.
- **A module tool fails with “sidecar is not running”**: restart the module
  sidecar in Settings → Modules or with `orqella_module_control` (`restart`).
- **The MCP client shows no tools**: the stdio server writes logs to stderr;
  check the client's MCP log. The gateway needs a valid daemon token before it
  can list module tools; core tools still list when the daemon is down, but
  calls return an actionable error.

## Development

```bash
cargo build
cargo test mcp::tests http::mcp::tests --lib
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"orqella_status","arguments":{}}}' \
  | ./target/debug/orqella mcp --profile read
```

Use the [MCP Inspector](https://modelcontextprotocol.io/docs/tools/inspector)
or a real client to exercise resources and prompts.
