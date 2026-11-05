#!/usr/bin/env bash
# Prints the CHANGELOG.md section of a release tag (vX.Y.Z or vX.Y.Z-pre) to stdout.
# A prerelease tag without a section of its own falls back to "## [Unreleased]".
# Exits 1 when no non-empty section is found, so a release cannot go out without notes.
set -euo pipefail

tag="${1:?usage: release-notes.sh <tag> [changelog-file]}"
file="${2:-CHANGELOG.md}"
version="${tag#v}"

extract() {
  awk -v want="$1" '
    /^## \[/ {
      if (on) exit
      if (index($0, "## [" want "]") == 1) { on = 1; next }
    }
    on && /^\[[^]]+\]: / { next }  # link reference definitions at the end of the file
    on { print }
  ' "$file"
}

is_blank() { [ -z "$(printf '%s' "$1" | tr -d '[:space:]')" ]; }

notes="$(extract "$version")"
if is_blank "$notes" && [[ "$version" == *-* ]]; then
  notes="$(extract "Unreleased")"
fi
if is_blank "$notes"; then
  echo "::error::$file has no section '## [$version]' with content" >&2
  exit 1
fi
printf '%s\n' "$notes"
