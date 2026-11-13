#!/usr/bin/env bash
# Build and install Orqella for the current user on Linux.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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

"$ORQELLA" update configure "$PWD" >/dev/null

ORIGIN="$("$ORQELLA" open --no-open 2>/dev/null || true)"
ORIGIN="${ORIGIN:-http://orqella.localhost:9477}"
HEALTH_URL="${ORIGIN%/}/healthz"

# An update launched by the daemon must not leave the old process holding the
# loopback port while systemd starts the replacement.
if [[ -n "${ORQELLA_UPDATE_PARENT_PID:-}" ]]; then
  systemctl --user stop orqella.service 2>/dev/null || true
  kill "${ORQELLA_UPDATE_PARENT_PID}" 2>/dev/null || true
  sleep 0.5
fi

echo "==> Installing orqella.service"
"$ORQELLA" service install
systemctl --user restart orqella.service

echo "==> Waiting for $HEALTH_URL"
for ((attempt = 0; attempt < 30; attempt++)); do
  if curl -fsS --max-time 2 "$HEALTH_URL" >/dev/null 2>&1; then
    echo
    echo "Orqella is running: $ORIGIN"
    exit 0
  fi
  sleep 0.5
done

echo "The daemon did not become healthy at $HEALTH_URL" >&2
journalctl --user -u orqella.service -n 30 --no-pager >&2 || true
exit 1
