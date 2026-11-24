#!/usr/bin/env bash
# Verify the signed update manifest for the revision being released.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/ORQELLA_UPDATE.json"
SIGNATURE="$MANIFEST.minisig"
PUBLIC_KEY="${ORQELLA_UPDATE_PUBLIC_KEY:-${MINISIGN_PUBLIC_KEY:-}}"

if [[ ! -f "$MANIFEST" || ! -f "$SIGNATURE" ]]; then
  echo "missing $MANIFEST or $SIGNATURE; run scripts/release-sign.sh first" >&2
  exit 1
fi
if [[ -z "$PUBLIC_KEY" ]]; then
  echo "set ORQELLA_UPDATE_PUBLIC_KEY to the minisign public key (or a .pub path)" >&2
  exit 1
fi
if ! command -v minisign >/dev/null 2>&1; then
  echo "minisign was not found; install it before verifying a release" >&2
  exit 1
fi

key_file=""
cleanup() {
  if [[ -n "$key_file" ]]; then rm -f "$key_file"; fi
}
trap cleanup EXIT

if [[ -f "$PUBLIC_KEY" ]]; then
  key_file="$(mktemp)"
  cp "$PUBLIC_KEY" "$key_file"
elif [[ "$PUBLIC_KEY" == untrusted\ comment:* ]]; then
  key_file="$(mktemp)"
  printf '%s\n' "$PUBLIC_KEY" >"$key_file"
else
  key_file="$(mktemp)"
  printf 'untrusted comment: Orqella update key\n%s\n' "$PUBLIC_KEY" >"$key_file"
fi

minisign -Vm "$MANIFEST" -p "$key_file"

python3 - "$ROOT" "$MANIFEST" <<'PY'
import json
import subprocess
import sys

root, manifest_path = sys.argv[1:3]
with open(manifest_path, encoding="utf-8") as handle:
    manifest = json.load(handle)

if manifest.get("schema") != 1:
    raise SystemExit("unsupported update manifest schema")
release_commit = str(manifest.get("commit", ""))
if len(release_commit) < 7 or any(c not in "0123456789abcdefABCDEF" for c in release_commit):
    raise SystemExit("update manifest has an invalid commit id")


def git(*args: str) -> str:
    return subprocess.check_output(["git", "-C", root, *args], text=True).strip()


head = git("rev-parse", "HEAD")
if head == release_commit:
    raise SystemExit("update manifest describes the commit that contains it")
subprocess.run(
    ["git", "-C", root, "merge-base", "--is-ancestor", release_commit, head],
    check=True,
)
changed = [line for line in git("diff", "--name-only", release_commit, head, "--").splitlines() if line]
unexpected = [path for path in changed if path not in {"ORQELLA_UPDATE.json", "ORQELLA_UPDATE.json.minisig"}]
if unexpected:
    raise SystemExit(
        "changes after the signed release commit are not allowed: " + ", ".join(unexpected)
    )

version = git("show", f"{release_commit}:Cargo.toml")
declared = next(
    (line.split('"')[1] for line in version.splitlines() if line.startswith("version = \"")),
    "",
)
if declared and declared != manifest.get("version"):
    raise SystemExit(
        f"manifest version {manifest.get('version')} does not match Cargo.toml {declared}"
    )
print(
    f"verified release {manifest.get('version')} for {release_commit} "
    f"(HEAD {head[:8]}, signature + commit binding ok)"
)
PY
