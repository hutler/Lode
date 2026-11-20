#!/bin/sh
# Writes the shell completion scripts of lode and loded into <out> for the Linux packages:
#   packaging/linux/completions.sh <bin folder> <out>
# The scripts do not depend on the processor, so when the packaged executables cannot run
# here (an aarch64 build on an x86_64 runner) the ones of this machine's build write them.
set -eu
bin="${1:?usage: completions.sh <bin folder> <out>}"
out="${2:?usage: completions.sh <bin folder> <out>}"
mkdir -p "$out"
for exe in lode loded; do
    run="$bin/$exe"
    if ! "$run" completions bash > /dev/null 2>&1; then
        cargo build --quiet --release -p lode-cli -p loded
        run="target/release/$exe"
    fi
    "$run" completions bash > "$out/$exe.bash"
    "$run" completions zsh > "$out/_$exe"
    "$run" completions fish > "$out/$exe.fish"
done
