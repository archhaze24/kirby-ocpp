#!/usr/bin/env bash
# Renders the Homebrew formula and the Scoop manifest for a release tag.
#
#   render-packaging.sh vX.Y.Z [ASSET_DIR [OUT_DIR]]
#
# Checksums come from the release's `kirby-ocpp-<tag>-<target>.<ext>.sha256` files: read from
# ASSET_DIR when given, otherwise downloaded from the public GitHub release URL (no token).
# Writes kirby-ocpp.rb and kirby-ocpp.json into OUT_DIR (default: ./packaging-out).
# Pass "-" as ASSET_DIR to download while still choosing OUT_DIR.
set -euo pipefail

tag="${1:?usage: render-packaging.sh <tag> [asset-dir|-] [out-dir]}"
assets="${2:--}"
out="${3:-packaging-out}"
version="${tag#v}"
repo="archhaze24/kirby-ocpp"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if ! [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$ ]]; then
  echo "error: '$tag' is not a vX.Y.Z tag" >&2
  exit 2
fi

# target:extension of every archive that a template uses
archives=(
  "aarch64-apple-darwin:tar.gz"
  "x86_64-apple-darwin:tar.gz"
  "aarch64-unknown-linux-musl:tar.gz"
  "x86_64-unknown-linux-musl:tar.gz"
  "x86_64-pc-windows-msvc:zip"
)

mkdir -p "$out"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

sed_args=(-e "s|@VERSION@|$version|g" -e "s|@TAG@|$tag|g")
for entry in "${archives[@]}"; do
  target="${entry%%:*}"
  ext="${entry#*:}"
  name="kirby-ocpp-${tag}-${target}.${ext}.sha256"
  if [ "$assets" = "-" ]; then
    curl --fail --silent --show-error --location --proto '=https' \
      -o "$work/$name" "https://github.com/$repo/releases/download/$tag/$name"
    file="$work/$name"
  else
    file="$assets/$name"
  fi
  [ -f "$file" ] || { echo "error: missing $file" >&2; exit 1; }
  hash="$(awk 'NR == 1 { print $1 }' "$file")"
  if ! [[ "$hash" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: $file does not start with a sha256 hex digest" >&2
    exit 1
  fi
  sed_args+=(-e "s|@SHA_${target//-/_}@|$hash|g")
done

sed "${sed_args[@]}" "$root/packaging/homebrew/kirby-ocpp.rb.in" > "$out/kirby-ocpp.rb"
sed "${sed_args[@]}" "$root/packaging/scoop/kirby-ocpp.json.in" > "$out/kirby-ocpp.json"

if grep -n '@[A-Z_]*@\|@SHA_' "$out/kirby-ocpp.rb" "$out/kirby-ocpp.json"; then
  echo "error: unreplaced placeholder left in the output" >&2
  exit 1
fi
echo "rendered $out/kirby-ocpp.rb and $out/kirby-ocpp.json for $tag"
