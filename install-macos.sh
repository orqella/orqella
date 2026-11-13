#!/usr/bin/env bash
# Build and install Orqella for the current user on macOS.
#
#   ./install-macos.sh
#
# This runs `cargo install --path .`, refreshes the per-user LaunchAgent
# (com.orqella.daemon) so the daemon starts at login, restarts it, and waits
# for the loopback listener to become healthy. Re-run it after `git pull`.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

LABEL="com.orqella.daemon"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "install-macos.sh supports macOS only." >&2
  echo "On Linux or Windows use: orqella service install" >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo was not found. Install Rust from https://rustup.rs and try again." >&2
  exit 1
fi

python_ok() {
  "$1" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' 2>/dev/null
}

# The built-in Voice Assistant sidecar imports tomllib, which is Python 3.11+.
# Sidecars inherit the daemon's PATH, which the LaunchAgent copies from this
# script's environment, so prefer a suitable interpreter that is already
# installed but missing from the current PATH.
if ! python_ok python3 && [[ -x "$HOME/.local/bin/python3" ]] && python_ok "$HOME/.local/bin/python3"; then
  export PATH="$HOME/.local/bin:$PATH"
fi
if ! python_ok python3; then
  echo "warning: python3 is missing or older than 3.11, so the Voice Assistant" >&2
  echo "         sidecar will not start. Install one with:" >&2
  echo "           curl -LsSf https://astral.sh/uv/install.sh | sh" >&2
  echo "           uv python install 3.12" >&2
fi

echo "==> Installing orqella into ${CARGO_HOME:-$HOME/.cargo}/bin"
cargo install --path . --locked --force

ORQELLA="${CARGO_HOME:-$HOME/.cargo}/bin/orqella"
if [[ ! -x "$ORQELLA" ]]; then
  echo "expected the installed binary at $ORQELLA but it is missing." >&2
  exit 1
fi

# Read the configured origin before touching the service so a custom port or
# canonical host still resolves to the right health check. `open --no-open`
# prints just the origin; avoid piping `doctor` here because an early pipe
# close makes it panic on EPIPE and, with `pipefail`, aborts the script.
"$ORQELLA" update configure "$PWD" >/dev/null

ORIGIN="$("$ORQELLA" open --no-open 2>/dev/null || true)"
ORIGIN="${ORIGIN:-http://orqella.localhost:9477}"
HEALTH_URL="${ORIGIN%/}/healthz"

# Safari uses the system resolver for *.localhost (unlike Chrome/Firefox), so
# add explicit loopback entries for the shell and every configured app host.
if [[ "${ORQELLA_SKIP_HOSTS:-0}" != "1" && -t 0 && -z "${ORQELLA_UPDATE_PARENT_PID:-}" ]]; then
  echo "==> Adding Orqella hostnames to /etc/hosts for Safari"
  if ! "$ORQELLA" hosts install; then
    echo "warning: could not update /etc/hosts. Run this from a terminal to fix Safari:" >&2
    echo "         $ORQELLA hosts install" >&2
  fi
else
  echo "==> Skipping /etc/hosts update (Safari). Run this later if needed:" >&2
  echo "         $ORQELLA hosts install" >&2
fi

# A daemon-launched update must stop the old process before launchd starts the
# replacement; otherwise the old process keeps the loopback port.
if [[ -n "${ORQELLA_UPDATE_PARENT_PID:-}" ]]; then
  kill "${ORQELLA_UPDATE_PARENT_PID}" 2>/dev/null || true
  sleep 0.5
fi

echo "==> Installing LaunchAgent $LABEL"
# Unload first so `service install` rewrites the plist and starts the new
# binary instead of reporting "already loaded" over the old process.
launchctl bootout "gui/$UID/$LABEL" 2>/dev/null || true
if ! service_output="$("$ORQELLA" service install 2>&1)"; then
  printf '%s\n' "$service_output" >&2
  echo "could not install the LaunchAgent." >&2
  exit 1
fi
printf '%s\n' "$service_output" | grep -v -e 'Unload failed' -e 'launchctl bootout' || true
launchctl kickstart -k "gui/$UID/$LABEL"

echo "==> Waiting for $HEALTH_URL"
for ((attempt = 0; attempt < 30; attempt++)); do
  if curl -fsS --max-time 2 "$HEALTH_URL" >/dev/null 2>&1; then
    echo
    echo "Orqella is running: $ORIGIN"
    echo "Logs: ~/Library/Logs/Orqella/"
    echo "Stop or remove the service with: orqella service uninstall"
    exit 0
  fi
  sleep 0.5
done

echo "The daemon did not become healthy at $HEALTH_URL" >&2
PORT="$(printf '%s' "$ORIGIN" | sed -n 's|.*:\([0-9][0-9]*\)/*$|\1|p')"
PORT="${PORT:-9477}"
echo "--- listeners on port $PORT" >&2
lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >&2 2>/dev/null || echo "none" >&2
echo "--- last daemon output" >&2
tail -n 20 "$HOME/Library/Logs/Orqella/orqella.err.log" >&2 2>/dev/null || true
tail -n 20 "$HOME/Library/Logs/Orqella/orqella.out.log" >&2 2>/dev/null || true
echo "--- doctor" >&2
"$ORQELLA" doctor >&2 || true
exit 1
