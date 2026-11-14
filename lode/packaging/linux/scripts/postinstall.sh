#!/bin/sh
# lode-server: the data directory, and the next steps. An upgrade restarts a running server.
set -e
if [ ! -d /var/lib/lode ]; then
    install -d -o lode -g lode -m 0750 /var/lib/lode
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
    systemctl try-restart loded.service || true
fi
if [ ! -f /var/lib/lode/loded.toml ]; then
    cat <<'TEXT'

Lode server installed. To finish:
  sudo -u lode loded setup
  sudo systemctl enable --now loded

TEXT
fi
