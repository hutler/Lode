#!/usr/bin/env bash
# Runs a cargo command in desk/ against the Lode core in ../lode (this checkout) instead of the
# version pinned in Cargo.toml, without changing Cargo.toml or Cargo.lock:
#
#   scripts/with-local-core.sh cargo test
#   scripts/with-local-core.sh cargo run
#
# Use it to try Desk against core changes before they are pinned. Moving the pin for real is an
# edit of Cargo.toml (see "The core version" in README.md).
set -euo pipefail
cd "$(dirname "$0")/.."
core="$(cd ../lode/crates && pwd)"
cmd="$1"
shift
sub="$1"
shift
args=()
for c in lode-client lode-ops lode-core lode-proto lode-server lode-testkit; do
  args+=(--config "patch.'https://github.com/hutler/Lode'.$c.path='$core/$c'")
done
# A copy of the lock file, so the patched build does not rewrite the pinned one.
cp Cargo.lock Cargo.lock.pinned
trap 'mv Cargo.lock.pinned Cargo.lock' EXIT
"$cmd" "$sub" "${args[@]}" "$@"
