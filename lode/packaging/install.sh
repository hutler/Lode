#!/bin/sh
# Installs Lode on Linux from the newest GitHub release, as a .deb or .rpm package:
#
#   curl -fsSL https://raw.githubusercontent.com/hutler/simpledevops/main/lode/packaging/install.sh | sh
#
# It installs the server (lode-server) and the client (lode). To choose, pass arguments
# after `sh -s --`:
#   sh -s -- --client           only the client
#   sh -s -- --version 0.1.0    a given release instead of the newest
#
# Environment: LODE_DOWNLOAD_URL, a folder (URL or local path) holding the release files
# instead of GitHub, for mirrors and tests.
set -eu

repo="hutler/simpledevops"
version=""
packages="lode-server lode"
while [ $# -gt 0 ]; do
    case "$1" in
        --client) packages="lode" ;;
        --server) packages="lode-server lode" ;;
        --version) version="${2:?--version needs a value}"; shift ;;
        -h|--help) echo "usage: install.sh [--client | --server] [--version V]"; exit 0 ;;
        *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
    esac
    shift
done

fail() { echo "install.sh: $*" >&2; exit 1; }

case "$(uname -s)" in
    Linux) ;;
    *) fail "this script is for Linux; see the user guide for Windows and macOS" ;;
esac
case "$(uname -m)" in
    x86_64|amd64) deb_arch=amd64; rpm_arch=x86_64 ;;
    aarch64|arm64) deb_arch=arm64; rpm_arch=aarch64 ;;
    *) fail "no package for $(uname -m); the release archives list what is available" ;;
esac
if command -v apt-get > /dev/null 2>&1 && command -v dpkg > /dev/null 2>&1; then
    kind=deb
elif command -v dnf > /dev/null 2>&1; then
    kind=rpm; installer="dnf install -y"
elif command -v yum > /dev/null 2>&1; then
    kind=rpm; installer="yum install -y"
elif command -v zypper > /dev/null 2>&1; then
    kind=rpm; installer="zypper --non-interactive install --allow-unsigned-rpm"
else
    fail "no apt, dnf, yum or zypper here; use the release archive instead"
fi

sudo=""
if [ "$(id -u)" -ne 0 ]; then
    command -v sudo > /dev/null 2>&1 || fail "run as root, or install sudo"
    sudo="sudo"
fi

# fetch <url-or-path> <destination>
fetch() {
    case "$1" in
        http://*|https://*|file://*)
            if command -v curl > /dev/null 2>&1; then
                curl -fsSL -o "$2" "$1"
            elif command -v wget > /dev/null 2>&1; then
                wget -q -O "$2" "$1"
            else
                fail "curl or wget is needed"
            fi
            ;;
        *) cp "$1" "$2" ;;
    esac
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if [ -n "${LODE_DOWNLOAD_URL:-}" ]; then
    base="${LODE_DOWNLOAD_URL%/}"
else
    if [ -z "$version" ]; then
        # The newest release whose tag is lode-v<version> (the repository has other products).
        fetch "https://api.github.com/repos/$repo/releases?per_page=100" "$work/releases.json"
        version="$(sed -n 's/.*"tag_name": *"lode-v\([^"]*\)".*/\1/p' "$work/releases.json" | head -n 1)"
        [ -n "$version" ] || fail "no Lode release found"
    fi
    base="https://github.com/$repo/releases/download/lode-v$version"
fi

fetch "$base/SHA256SUMS" "$work/SHA256SUMS"
if [ -z "$version" ]; then
    # From a folder: the version of the newest server or client package in it.
    version="$(sed -n 's/.*  lode_\(.*\)_'"$deb_arch"'\.deb$/\1/p' "$work/SHA256SUMS" | tail -n 1)"
    [ -n "$version" ] || fail "no packages listed in $base/SHA256SUMS"
fi

files=""
for p in $packages; do
    if [ "$kind" = deb ]; then
        f="${p}_${version}_${deb_arch}.deb"
    else
        f="${p}-${version}.${rpm_arch}.rpm"
    fi
    echo "Downloading $f"
    fetch "$base/$f" "$work/$f"
    line="$(grep "  $f\$" "$work/SHA256SUMS" || true)"
    [ -n "$line" ] || fail "$f is not in SHA256SUMS"
    (cd "$work" && echo "$line" | sha256sum -c - > /dev/null) || fail "$f does not match its checksum"
    files="$files $work/$f"
done

chmod 0755 "$work"
chmod 0644 $files
if [ "$kind" = deb ]; then
    $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y $files
else
    # shellcheck disable=SC2086
    $sudo $installer $files
fi
echo "Lode $version installed."
