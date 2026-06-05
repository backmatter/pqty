#!/usr/bin/env bash
set -euo pipefail

if (( $# != 2 )); then
  echo "usage: publish-crate.sh <crate> <version>" >&2
  exit 2
fi

crate="$1"
version="$2"
endpoint="https://crates.io/api/v1/crates/$crate/$version"
status="$(
  curl --silent --show-error \
    --user-agent "pqty-release/$version" \
    --output /dev/null \
    --write-out '%{http_code}' \
    "$endpoint"
)"
case "$status" in
  200)
    echo "publish-crate: $crate $version already exists; skipping"
    ;;
  404)
    cargo publish --package "$crate" --locked
    ;;
  *)
    echo "publish-crate: crates.io returned HTTP $status for $crate $version" >&2
    exit 1
    ;;
esac
