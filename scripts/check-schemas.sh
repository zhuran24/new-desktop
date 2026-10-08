#!/usr/bin/env bash
# Compare complete generated file sets, including additions and stale artifacts.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
schema_tmp=$(mktemp -d -t nd-schema.XXXXXXXXXX)
trap 'find "$schema_tmp" -depth -delete' EXIT
cargo run --quiet --locked -p nd-wire --bin nd-wire-schema -- "$schema_tmp/protocol"
cargo run --quiet --locked -p nd-mod-proto --bin nd-mod-schema -- "$schema_tmp"
diff -ru -- "$schema_tmp/protocol" protocol
for generated in "$schema_tmp"/mods/*/hooks/proto.ts; do
  relative=${generated#"$schema_tmp/"}
  diff -u -- "$generated" "$relative"
done
for checked_in in mods/*/hooks/proto.ts; do
  test -f "$schema_tmp/$checked_in"
done
