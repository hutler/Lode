#!/bin/sh
# Entrypoint of the loded container image.
#   serve (default)   initialize $LODED_DATA on first start, then run the server
#   anything else     passed to loded, e.g. `fsck --config /var/lib/lode/loded.toml`
set -eu
data="${LODED_DATA:-/var/lib/lode}"
config="$data/loded.toml"

if [ "${1:-serve}" = "serve" ]; then
    if [ ! -f "$config" ]; then
        loded init "$data"
        # Inside a container the server must listen on every interface; publish the port
        # to the host's loopback or put a TLS proxy in front (see the operator guide).
        sed -i 's/^listen = "127.0.0.1:7480"$/listen = "0.0.0.0:7480"/' "$config"
    fi
    exec loded serve --config "$config"
fi
exec loded "$@"
