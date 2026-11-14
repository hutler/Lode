#!/bin/sh
# lode-server: the data directory /var/lib/lode and the `lode` user are kept. To remove them
# as well:  sudo rm -rf /var/lib/lode && sudo userdel lode
set -e
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
