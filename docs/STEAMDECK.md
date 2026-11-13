# Steam Deck / immutable Linux setup

This is the tested path for putting an Orqella node on a Steam Deck, with the
System, Terminal, and OpenCode modules. It is intentionally written as both a
runbook and a list of product gaps; several steps should become one command
before this is ready for non-technical users.

Tested on:

- SteamOS `7.2.7-valve2-2-neptune-72` (Arch-based, read-only root)
- glibc `2.43`
- Python `3.14.6`
- Orqella `0.1.0`
- OpenCode `2.1.3`

## What works

- Orqella runs as a per-user systemd service from `$HOME`.
- `system` and `terminal` install and run without root.
- OpenCode runs from a standalone Linux binary and serves the module.
- The OpenCode module can use copied config/auth from another machine.
- The host daemon can connect to the Deck over SSH as a peer.

## Roadblocks and product gaps

| Gap | Why it happens | Product fix |
| --- | --- | --- |
| No standalone Orqella installer for SteamOS | `install-linux.sh` requires Cargo/Rust; the Deck has neither, and `/usr` is read-only | Publish a prebuilt Linux x86_64 archive and an installer that writes to `$HOME` |
| No global pip | System Python 3.14 on SteamOS has no pip outside a venv; the terminal module needs a venv | Keep module dependency installs inside the managed venv; don't assume `pip` on `PATH` |
| Linger disabled by default | User services stop after logout unless linger is enabled | Installer should offer/configure `loginctl enable-linger` |
| No module catalog/package | Modules were copied from source tarballs; private repos require credentials | Publish signed module packages or a catalog the daemon can install by ID |
| OpenCode binary is ~230 MB | There is no Node/npm on the Deck; OpenCode is a standalone binary | Download the standalone build into `$HOME`, not `/usr`; make this a module setup step |
| OpenCode has no packaged setup | The deck has no Node/npm; OpenCode must be supplied as a standalone binary | Bundle/download the OpenCode binary, or add a module setup action |
| OpenCode needs `--service` | `opencode serve` alone prints a temporary password; the module then gets 401 because `service.json` is not the service password | The module starts the shared service with `opencode service start`; if a unit is used, it must pass `--service` |
| Module install leaves sensitive permissions pending | `backend`, `agent`, `node.proxy:*`, and `app.open` are intentionally not auto-granted | Add an install preset/CLI `--grant` for trusted local modules, or surface a review step in setup |
| Empty `proxy = []` conflicts with `[[proxy]]` | The config writer emits `proxy = []`; appending an array-of-tables then fails with `duplicate key proxy` | Omit empty arrays, or update the config writer/API when adding a proxy |
| Orqella service PATH does not include `~/.local/bin` | Generated unit uses `/usr/local/bin:/usr/bin`; the OpenCode agent command then cannot be found | Include `%h/.local/bin` in generated units, or use absolute agent paths |
| Config import is manual | OpenCode config, auth, and state are spread across three directories; copying the DB is unnecessary and large (1.3 GB) | Planned: `orqella setup opencode --import-config` with a safe allowlist |
| No automatic peer/firewall setup | Host SSH is reachable from the Deck only if the host firewall permits it; the host can reach the Deck | Add guided peer setup and a connectivity check |

## 1. Install the Orqella binary

The Deck has a read-only root and no Rust toolchain. Build or obtain a Linux
x86_64 binary on another machine and copy only the daemon binary:

```bash
# on the build machine
scp target/release/orqella deck@<deck-host>:/home/deck/orqella

# on the Deck
mkdir -p ~/.local/bin
install -m 0755 ~/orqella ~/.local/bin/orqella
~/.local/bin/orqella service install
systemctl --user enable --now orqella.service
loginctl enable-linger "$USER"   # keep the node available without a login shell
```

Add `~/.local/bin` to the interactive shell path if `orqella` should be a
short command:

```bash
printf '\nexport PATH="$HOME/.local/bin:$PATH"\n' >> ~/.bashrc
```

The current release does **not** need a system package or a write to `/usr`.
Do not copy a `target/` checkout to the Deck just to install it; the Deck has
about 837 MB free on `/` and plenty in `/home`.

## 2. Install the modules

For the current temporary module flow, copy the module trees and install them
locally:

```bash
for module in system terminal opencode; do
  tar -C "$module" -czf "/tmp/$module.tgz" --exclude .git --exclude data .
  scp "/tmp/$module.tgz" deck@<deck-host>:
done

# on the Deck
mkdir -p ~/orqella-module-src
for module in system terminal opencode; do
  mkdir -p ~/orqella-module-src/$module
  tar -xzf ~/$module.tgz -C ~/orqella-module-src/$module
  ~/.local/bin/orqella module install ~/orqella-module-src/$module
done
```

After installing, review and grant permissions in the dashboard. The
high-value permissions for these modules are:

- `system`: `node.info`, `node.metrics`, `nodes.read`, `backend`; opt into
  `agent` only if you want **Identify with AI**.
- `terminal`: `backend`, `storage`.
- `opencode`: `backend`, `storage`, `node.proxy:opencode`, `app.open`.

There is currently no `orqella module permissions` CLI, so this is a UI/API
step. For local automation, the API is:

```bash
curl -sS -X POST -H 'X-Orqella: 1' -H 'Content-Type: application/json' \
  -d '{"permissions":["backend","storage"]}' \
  http://127.0.0.1:9477/api/v1/modules/terminal/permissions
```

## 3. Install the OpenCode service

The module is only a client. It needs a local OpenCode server and its
`service.json` credential.

The Deck has no Node/npm. Use the standalone x86_64 OpenCode binary (about
230 MB), or an official standalone installer if one is available. Install it
under the user's home:

```bash
# on a machine that already has OpenCode
install -m 0755 /path/to/opencode deck@<deck-host>:/home/deck/opencode

# on the Deck
install -m 0755 ~/opencode ~/.local/bin/opencode
~/.local/bin/opencode --version
```

Create the user service (optional). The OpenCode module now starts the shared
service with `opencode service start` when needed, so a unit is only useful if
you want the service running at login. If you do use one, the important flag is
`--service`; without it the server creates a temporary password and the OpenCode
module receives 401 errors.

```ini
# ~/.config/systemd/user/opencode.service
[Unit]
Description=OpenCode API and web server
Documentation=https://opencode.ai
After=network.target

[Service]
Type=simple
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin
ExecStart=%h/.local/bin/opencode serve --service --hostname 127.0.0.1 --port 49374
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now opencode.service
```

The module reuses this service when it is already running; do not start a
second `opencode serve` on the same port.

Verify the service before touching the module:

```bash
pass=$(python3 -c 'import json, pathlib; print(json.loads(pathlib.Path.home().joinpath(".config/opencode/service.json").read_text())["password"])')
curl -u "opencode:$pass" http://127.0.0.1:49374/api/model
```

## 4. Copy OpenCode config from another machine

This is the part that should become a first-class setup action. Copy only:

- `~/.config/opencode/` — config, `service.json`, UI settings, agents, plugins
- `~/.local/share/opencode/auth.json` — provider authentication (secret)
- `~/.local/state/opencode/` — small model/UI/session preferences

Do **not** copy:

- `~/.local/share/opencode/opencode.db*` (about 1.3 GB)
- `~/.local/share/opencode/log/`
- `~/.local/share/opencode/tool-output/`

Example:

```bash
tar -C ~/.config -czf /tmp/opencode-config.tgz opencode
tar -C ~/.local/share/opencode -czf /tmp/opencode-auth.tgz auth.json
tar -C ~/.local/state -czf /tmp/opencode-state.tgz opencode
scp /tmp/opencode-*.tgz deck@<deck-host>:

# on the Deck
mkdir -p ~/.config ~/.local/share/opencode ~/.local/state
tar -xzf ~/opencode-config.tgz -C ~/.config
tar -xzf ~/opencode-auth.tgz -C ~/.local/share/opencode
tar -xzf ~/opencode-state.tgz -C ~/.local/state
chmod 700 ~/.config/opencode ~/.local/share/opencode ~/.local/state/opencode
chmod 600 ~/.config/opencode/service.json ~/.local/share/opencode/auth.json
rm -f ~/opencode-*.tgz
```

`auth.json` contains provider credentials. Transfer it only over a trusted SSH
connection and remove the temporary archives afterward. The Deck now has a new
OpenCode database; copied preferences do not include old session history.

## 5. Configure the OpenCode proxy mount

The module page works without the proxy, but the OpenCode app handoff needs a
proxy mount. Add this to `~/.config/orqella/config.toml`:

```toml
[[proxy]]
id = "opencode"
title = "OpenCode"
base_url = "http://127.0.0.1:49374"
host = "opencode.orqella.localhost"
websocket = true
frame = false
```

**Current footgun:** if the generated config already contains:

```toml
proxy = []
```

remove that line first. TOML treats `proxy = []` and `[[proxy]]` as the same
key; appending the table without removing the empty array causes:

```text
duplicate key `proxy` in document root
```

Then restart:

```bash
systemctl --user restart orqella.service
```

## 6. Make the agent command available

The OpenCode module can use OpenCode directly. Orqella's own AI actions
(`agent.prompt`, System **Identify with AI**, module builder) also need the
`opencode` command on the daemon's `PATH`. The generated Orqella unit currently
does not include `~/.local/bin`. Add a drop-in so the fix survives a future
`orqella service install`:

```bash
mkdir -p ~/.config/systemd/user/orqella.service.d
cat > ~/.config/systemd/user/orqella.service.d/path.conf <<'EOF'
[Service]
Environment=PATH=/home/<user>/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/bin
EOF
systemctl --user daemon-reload
```

Then:

```bash
systemctl --user restart orqella.service
```

Module builds can legitimately take much longer than 15 minutes on the Deck.
Keep `[agent].timeout_secs = 0` (the default) so Orqella waits for the agent
instead of reporting a timeout while the work is still running. When the agent
exposes a session id, the Build page opens the OpenCode conversation; job
history and the session link are retained across daemon restarts. If you set a
positive timeout, OpenCode's default background service may keep running the
session after Orqella stops the CLI, so use `--standalone` in the agent args if
the timeout must cancel the work itself.

## 7. Verify

```bash
# Daemon
curl -fsS http://127.0.0.1:9477/healthz

# Modules
curl -sS http://127.0.0.1:9477/api/v1/modules

# OpenCode service and agent
~/.local/bin/opencode run --auto --format json "Reply with exactly: ok"
curl -sS -X POST -H 'X-Orqella: 1' -H 'Content-Type: application/json' \
  -d '{"method":"backend.request","params":{"method":"GET","path":"/api/models"}}' \
  http://127.0.0.1:9477/api/v1/modules/opencode/rpc
```

The OpenCode module should list models and expose its proxy at
`http://opencode.orqella.localhost:9477/` locally.

## Wake on LAN

The Deck supports WoWLAN when the network permits directed broadcasts. Orqella
learns its Wi-Fi MAC automatically while the Deck is online and caches the
interface for later wake attempts. Selecting the sleeping Deck in the machine
selector sends magic packets and falls back to the previously selected machine
if it does not come online.

## Suggested one-command end-user path

The eventual (not yet implemented) `orqella setup steamdeck` flow should:

1. Install a prebuilt Orqella binary to `~/.local/bin` and create the user unit.
2. Enable linger and add `~/.local/bin` to the daemon `PATH`.
3. Install the selected modules from a signed catalog.
4. Install or download the OpenCode standalone binary. The module starts the
   shared service on demand; optionally create `opencode.service` with
   `--service`.
5. Offer to import OpenCode config/auth from another machine, or start sign-in.
6. Add the `opencode` proxy mount safely, without emitting `proxy = []`.
7. Grant the module permissions with an explicit trusted-install confirmation.
8. Start the services, verify the module APIs, and print a single health summary.
