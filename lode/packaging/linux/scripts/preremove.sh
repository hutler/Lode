#!/bin/sh
# lode-server: stop the server when the package is removed (not on upgrade). Debian passes
# "remove", RPM passes 0.
set -e
case "${1:-}" in
    remove|0)
        if [ -d /run/systemd/system ]; then
            systemctl disable --now loded.service > /dev/null 2>&1 || true
        fi
        ;;
esac
