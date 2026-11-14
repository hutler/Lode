#!/bin/sh
# lode-server: the service account, before the files arrive.
set -e
if ! getent group lode > /dev/null 2>&1; then
    groupadd --system lode
fi
if ! getent passwd lode > /dev/null 2>&1; then
    useradd --system --gid lode --home-dir /var/lib/lode --no-create-home \
        --shell /usr/sbin/nologin --comment "Lode server" lode
fi
