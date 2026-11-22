#!/usr/bin/env bash
# Build and install Orqella for the current user on macOS.
#
#   ./install-macos.sh
#
# This runs `cargo install --path .`, refreshes the per-user LaunchAgent
# (com.orqella.daemon) so the daemon starts at login, restarts it, and waits
# for the loopback listener to become healthy. Re-run it after `git pull`.
#
#   ./install-macos.sh [--firewall|--no-firewall]
#
# The default asks once whether to add the LAN-scoped Link UDP firewall rule.
#
# It does not edit /etc/hosts. Safari users who want the canonical
# `orqella.localhost` origin and full app surfaces can run
# `orqella hosts install` separately; Chrome, Firefox, and the desktop client
# need no extra setup.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

firewall_mode="${ORQELLA_FIREWALL:-ask}"
for arg in "$@"; do
  case "$arg" in
    --firewall) firewall_mode=install ;;
    --no-firewall) firewall_mode=skip ;;
    *)
      echo "usage: $0 [--firewall|--no-firewall]" >&2
      exit 2
      ;;
  esac
done
case "$firewall_mode" in
  ask|install|skip) ;;
  *)
    echo "ORQELLA_FIREWALL must be ask, install, or skip" >&2
    exit 2
    ;;
esac

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

install_link_firewall() {
  if "$ORQELLA" firewall status --check >/dev/null 2>&1; then
    return 0
  fi
  case "$firewall_mode" in
    install)
      if ! "$ORQELLA" firewall install; then
        echo "warning: could not add the Link firewall rule; run '$ORQELLA firewall install' later." >&2
      fi
      ;;
    skip)
      echo "Link firewall: skipped. Run '$ORQELLA firewall install' to allow direct LAN connections."
      ;;
    ask)
      if [[ -t 0 ]]; then
        read -r -p "Allow Orqella Link UDP traffic from the LAN? [Y/n] " answer
        case "${answer:-y}" in
          y|Y|yes|YES)
            if ! "$ORQELLA" firewall install; then
              echo "warning: could not add the Link firewall rule; run '$ORQELLA firewall install' later." >&2
            fi
            ;;
          *)
            echo "Link firewall: skipped. Run '$ORQELLA firewall install' to allow direct LAN connections."
            ;;
        esac
      else
        echo "Link firewall: run '$ORQELLA firewall install' to allow direct LAN connections."
      fi
      ;;
  esac
}

# Read the configured origin before touching the service. `open --no-open`
# prints just the origin; avoid piping `doctor` here because an early pipe
# close makes it panic on EPIPE and, with `pipefail`, aborts the script.
ORIGIN="$("$ORQELLA" open --no-open 2>/dev/null || true)"
ORIGIN="${ORIGIN:-http://orqella.localhost:9477}"
PORT="$(printf '%s' "$ORIGIN" | sed -n 's|.*:\([0-9][0-9]*\)/*$|\1|p')"
PORT="${PORT:-9477}"

# Safari does not resolve *.localhost through the system resolver, but the
# loopback origin always works. Safari host entries are optional and installed
# explicitly with `orqella hosts install`; this installer does not edit
# /etc/hosts.
HEALTH_URL="http://127.0.0.1:${PORT}/healthz"

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
    echo "Orqella is running: http://127.0.0.1:${PORT}/"
    echo "Canonical: $ORIGIN"
    echo "Safari: run '$ORQELLA hosts install' once for the canonical origin and full app surfaces."
    echo "Logs: ~/Library/Logs/Orqella/"
    echo "Stop or remove the service with: orqella service uninstall"
    install_link_firewall
    exit 0
  fi
  sleep 0.5
done

echo "The daemon did not become healthy at $HEALTH_URL" >&2
echo "--- listeners on port $PORT" >&2
lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >&2 2>/dev/null || echo "none" >&2
echo "--- last daemon output" >&2
tail -n 20 "$HOME/Library/Logs/Orqella/orqella.err.log" >&2 2>/dev/null || true
tail -n 20 "$HOME/Library/Logs/Orqella/orqella.out.log" >&2 2>/dev/null || true
echo "--- doctor" >&2
"$ORQELLA" doctor >&2 || true
exit 1
