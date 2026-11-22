#!/usr/bin/env bash
# Build and install Orqella for the current user on Linux.
#
#   ./install-linux.sh [--firewall|--no-firewall]
#
# The default asks once whether to add the LAN-scoped Link UDP firewall rule.
# Use --firewall for unattended installs, or --no-firewall to skip it.
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

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "install-linux.sh supports Linux only." >&2
  exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo was not found. Install Rust from https://rustup.rs and try again." >&2
  exit 1
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

ORIGIN="$("$ORQELLA" open --no-open 2>/dev/null || true)"
ORIGIN="${ORIGIN:-http://orqella.localhost:9477}"
HEALTH_URL="${ORIGIN%/}/healthz"

echo "==> Installing orqella.service"
"$ORQELLA" service install
systemctl --user restart orqella.service

echo "==> Waiting for $HEALTH_URL"
for ((attempt = 0; attempt < 30; attempt++)); do
  if curl -fsS --max-time 2 "$HEALTH_URL" >/dev/null 2>&1; then
    echo
    echo "Orqella is running: $ORIGIN"
    install_link_firewall
    exit 0
  fi
  sleep 0.5
done

echo "The daemon did not become healthy at $HEALTH_URL" >&2
journalctl --user -u orqella.service -n 30 --no-pager >&2 || true
exit 1
