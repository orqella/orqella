#!/usr/bin/env bash
# Sign and attach an update manifest to an already-published daemon release.
#
# This is for recovering a release that was published without a signed
# ORQELLA_UPDATE.json, or for adding the manifest after an offline signing
# ceremony. The key stays outside the repository.
#
#   scripts/attach-update-manifest.sh v0.6.0 ~/.config/orqella/orqella-update.key
#
# The manifest is generated for the tag's commit, verified locally, and then
# uploaded to the GitHub release. It is not committed to the repository.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tag="${1:?usage: attach-update-manifest.sh <tag> <minisign-secret-key>}"
key="${2:-${MINISIGN_SECRET_KEY:-}}"
[ -n "$key" ] || {
  echo "pass the minisign secret key path or set MINISIGN_SECRET_KEY" >&2
  exit 1
}
[ -f "$key" ] || {
  echo "minisign secret key not found: $key" >&2
  exit 1
}
version="${tag#v}"
commit="$(git -C "$ROOT" rev-parse "${tag}^{commit}")"
public_key="${ORQELLA_UPDATE_PUBLIC_KEY:-}"
[ -n "$public_key" ] || {
  echo "set ORQELLA_UPDATE_PUBLIC_KEY to the minisign public key (or .pub path)" >&2
  exit 1
}

cd "$ROOT"
VERSION="$version" COMMIT="$commit" scripts/release-sign.sh "$key"
ORQELLA_UPDATE_PUBLIC_KEY="$public_key" scripts/release-verify.sh
gh release upload "$tag" ORQELLA_UPDATE.json ORQELLA_UPDATE.json.minisig --clobber

cat <<EOF
Uploaded signed update manifest for $tag ($commit).
Existing daemons with a configured public key can now verify the release.
EOF
