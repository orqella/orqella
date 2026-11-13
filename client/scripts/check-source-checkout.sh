#!/usr/bin/env bash
# Validate the supported source-build layout inside the Orqella repository.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CORE="${ORQELLA_CORE_DIR:-$ROOT/..}"

if [[ ! -f "$CORE/Cargo.toml" ]] || [[ ! -d "$CORE/crates/orqella-tunnel-core" ]]; then
  cat >&2 <<EOF
Could not find the Orqella core tree at:
  $CORE

The desktop client uses path dependencies on the shared Link and tunnel crates.
Keep it inside the Orqella repository:

  orqella/
    client/   (this client)
    crates/   (shared crates)

Or set ORQELLA_CORE_DIR to the Orqella repository root.
EOF
  exit 1
fi

if ! grep -q '^name = "orqella"' "$CORE/Cargo.toml"; then
  echo "$CORE does not look like the Orqella repository." >&2
  exit 1
fi

echo "source layout ok: $ROOT + $CORE"
