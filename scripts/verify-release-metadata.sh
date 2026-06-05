#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"

tag="${1:-${GITHUB_REF_NAME:-}}"
if [[ -z "$tag" ]]; then
  echo "verify-release-metadata: pass the release tag or set GITHUB_REF_NAME" >&2
  exit 1
fi

package_version() {
  cargo pkgid --package "$1" |
    sed -E 's/.*[@#]([^@#]+)$/\1/'
}

pqty_version="$(package_version pqty)"
adapter_version="$(package_version pqty-fls)"
if [[ "$pqty_version" != "$adapter_version" ]]; then
  echo "verify-release-metadata: crate versions differ: pqty=$pqty_version pqty-fls=$adapter_version" >&2
  exit 1
fi
if [[ "$tag" != "v$pqty_version" ]]; then
  echo "verify-release-metadata: tag $tag does not match crate version v$pqty_version" >&2
  exit 1
fi

echo "verify-release-metadata: $tag matches pqty and pqty-fls"
