#!/bin/sh
# Checks the lode-server and lode packages inside a container of the target distribution
# (run by the release workflow, as root):
#   docker run --rm -v "$PWD:/src" -w /src <image> packaging/linux/test-package.sh dist
# Installs them with install.sh from the folder <dist>, sets the server up, starts it the
# way the systemd unit does, signs in, then removes the packages and checks that the data
# directory stays.
set -eu
dist="$(cd "${1:?usage: test-package.sh <dist folder>}" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"
fail() { echo "test-package: $*" >&2; exit 1; }
# Run a command as the lode user (runuser where util-linux has it, else sudo).
as_lode() {
    if command -v runuser > /dev/null; then runuser -u lode -- "$@"; else sudo -u lode -- "$@"; fi
}

LODE_DOWNLOAD_URL="$dist" sh "$here/../install.sh"

command -v loded > /dev/null || fail "loded is not installed"
command -v lode > /dev/null || fail "lode is not installed"
getent passwd lode > /dev/null || fail "no lode user"
[ "$(stat -c %U /var/lib/lode)" = lode ] || fail "/var/lib/lode is not owned by lode"
unit=/usr/lib/systemd/system/loded.service
[ -f "$unit" ] || fail "no systemd unit"
[ -f /usr/share/bash-completion/completions/lode ] || fail "no bash completion for lode"
[ -f /usr/share/bash-completion/completions/loded ] || fail "no bash completion for loded"
ls /usr/share/zsh/*/_lode > /dev/null 2>&1 || fail "no zsh completion for lode"

printf 'test-password-123\n' > /tmp/pw
chmod 0644 /tmp/pw
as_lode loded setup --local --yes --admin-password-file /tmp/pw > /tmp/setup.log
grep -q "systemctl enable --now loded" /tmp/setup.log || fail "setup does not point at the unit: $(cat /tmp/setup.log)"
[ -f /var/lib/lode/loded.toml ] || fail "setup wrote no configuration"

# No systemd in a container: run the unit's command as its user.
exec_start="$(sed -n 's/^ExecStart=//p' "$unit")"
as_lode $exec_start > /tmp/loded.log 2>&1 &
server=$!
ok=""
for _ in $(seq 1 50); do
    if lode login http://127.0.0.1:7480 --name admin --password-stdin < /tmp/pw > /dev/null 2>&1; then
        ok=1
        break
    fi
    sleep 0.2
done
[ -n "$ok" ] || fail "cannot sign in: $(cat /tmp/loded.log)"
lode --json whoami | grep -q '"admin"' || fail "whoami is not admin"
lode --json self-update | grep -q '"package_managed"' || fail "self-update does not leave a packaged lode to the package manager"
kill "$server"
wait "$server" || true

if command -v apt-get > /dev/null; then
    apt-get remove -y lode-server lode > /dev/null
else
    if command -v dnf > /dev/null; then dnf remove -y lode-server lode > /dev/null; else yum remove -y lode-server lode > /dev/null; fi
fi
[ ! -e /usr/bin/loded ] && [ ! -e /usr/bin/lode ] || fail "the binaries are still installed"
[ ! -f "$unit" ] || fail "the unit is still there"
[ -f /var/lib/lode/loded.toml ] || fail "the data directory was removed"
echo "test-package: ok"
