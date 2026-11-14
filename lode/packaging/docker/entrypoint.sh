#!/bin/sh
# Entrypoint of the loded container image.
#   serve (default)   set up $LODED_DATA on first start, then run the server
#   anything else     passed to loded, e.g. `fsck --config /var/lib/lode/loded.toml`
#
# First start:
#   LODED_DOMAIN set            `loded setup` for built-in HTTPS on port 443 with a Let's
#                               Encrypt certificate (LODED_EMAIL optional); needs
#                               LODED_ADMIN_PASSWORD_FILE
#   LODED_ADMIN_PASSWORD_FILE   without a domain: `loded setup` for plain HTTP on port 7480,
#                               with that admin password
#   neither                     `loded init`, which prints the admin token once to the log
# Both setups keep daily backups in the data volume (backups/).
#   LODED_DATABASE_URL_FILE     a file holding a postgres:// URL: on first start the new
#                               server's metadata moves into that (empty) Postgres database
#   LODED_WAIT_FOR_LOCK=1       with Postgres, a second container waits as a standby and
#                               takes over when the active one stops
set -eu
data="${LODED_DATA:-/var/lib/lode}"
config="$data/loded.toml"

if [ "${1:-serve}" = "serve" ]; then
    if [ ! -f "$config" ]; then
        if [ -n "${LODED_DOMAIN:-}" ]; then
            if [ -z "${LODED_ADMIN_PASSWORD_FILE:-}" ]; then
                echo "loded-entrypoint: LODED_DOMAIN needs LODED_ADMIN_PASSWORD_FILE (the admin password)" >&2
                exit 1
            fi
            set -- --domain "$LODED_DOMAIN" --listen 0.0.0.0:443
            if [ -n "${LODED_EMAIL:-}" ]; then
                set -- "$@" --email "$LODED_EMAIL"
            fi
            loded setup --yes --data-dir "$data" --admin-password-file "$LODED_ADMIN_PASSWORD_FILE" "$@"
        elif [ -n "${LODED_ADMIN_PASSWORD_FILE:-}" ]; then
            loded setup --yes --data-dir "$data" --admin-password-file "$LODED_ADMIN_PASSWORD_FILE" \
                --local --listen 0.0.0.0:7480
        else
            loded init "$data"
            # Inside a container the server must listen on every interface; publish the port
            # to the host's loopback or put a TLS proxy in front (see the operator guide).
            sed -i 's/^listen = "127.0.0.1:7480"$/listen = "0.0.0.0:7480"/' "$config"
        fi
        if [ -n "${LODED_DATABASE_URL_FILE:-}" ]; then
            loded migrate-db --config "$config" --to postgres --url-file "$LODED_DATABASE_URL_FILE"
        fi
    fi
    if [ "${LODED_WAIT_FOR_LOCK:-}" = "1" ]; then
        exec loded serve --config "$config" --wait-for-lock
    fi
    exec loded serve --config "$config"
fi
exec loded "$@"
