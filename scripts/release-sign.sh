#!/usr/bin/env bash
# Sign the daemon update manifest for the current release commit.
#
# Run this after committing the code for a release and BEFORE committing the
# manifest itself. The manifest points at the current HEAD; the following
# commit may add only ORQELLA_UPDATE.json and its .minisig file.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)}"
COMMIT="${COMMIT:-$(git -C "$ROOT" rev-parse HEAD)}"
CHANNEL="${CHANNEL:-stable}"
OUT="${OUT:-$ROOT/ORQELLA_UPDATE.json}"
KEY="${MINISIGN_SECRET_KEY:-${1:-}}"

if [[ -z "$VERSION" ]]; then
  echo "could not determine the release version" >&2
  exit 1
fi
if [[ -z "$KEY" ]]; then
  cat >&2 <<'EOF'
Set MINISIGN_SECRET_KEY (or pass the secret-key path as the first argument).

The key must stay offline. Generate it once with:

  minisign -G -p orqella-update.pub -s orqella-update.key

Commit only the public key or publish it with the release notes. Back up the
secret key offline; losing it means users cannot verify future updates.
EOF
  exit 1
fi
if ! command -v minisign >/dev/null 2>&1; then
  echo "minisign was not found; install it before signing a release" >&2
  exit 1
fi
if [[ -e "$OUT" ]] && ! git -C "$ROOT" ls-files --error-unmatch "$(basename "$OUT")" >/dev/null 2>&1; then
  echo "refusing to overwrite untracked $OUT; remove it first" >&2
  exit 1
fi

python3 - "$OUT" "$VERSION" "$COMMIT" "$CHANNEL" <<'PY'
import json
import sys
from datetime import datetime, timezone

out, version, commit, channel = sys.argv[1:5]
value = {
    "schema": 1,
    "version": version,
    "commit": commit,
    "channel": channel,
    "published_at": datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
}
with open(out, "w", encoding="utf-8") as handle:
    json.dump(value, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY

minisign -S -s "$KEY" -m "$OUT" -t "version:$VERSION commit:$COMMIT channel:$CHANNEL"

cat <<EOF
Signed release $VERSION for commit $COMMIT.

Review and commit only the manifest and signature:

  git -C "$ROOT" add ORQELLA_UPDATE.json ORQELLA_UPDATE.json.minisig
  git -C "$ROOT" commit -m "release: v$VERSION"
  git -C "$ROOT" tag v$VERSION

The daemon refuses to update unless the manifest is signed and the commit it
names is the release commit (only manifest files may follow it).
EOF
