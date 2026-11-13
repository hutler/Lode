#!/bin/sh
# Smoke test of the loded image: starts it on a fresh volume, commits a file with the lode
# client inside the container, restarts it, and checks that the file and depot are still
# there. Usage: packaging/docker/smoke.sh [image]   (default loded:smoke)
set -eu
image="${1:-loded:smoke}"
vol="lode-smoke-$$"
cid=""
cleanup() {
    [ -n "$cid" ] && docker rm -f "$cid" >/dev/null 2>&1 || true
    docker volume rm "$vol" >/dev/null 2>&1 || true
}
trap cleanup EXIT

wait_listening() {
    n=0
    until docker logs "$cid" 2>/dev/null | grep -q "loded listening"; do
        n=$((n + 1))
        if [ "$n" -gt 60 ]; then
            docker logs "$cid"
            echo "smoke: the server did not start" >&2
            exit 1
        fi
        sleep 1
    done
}

docker volume create "$vol" >/dev/null
cid=$(docker run -d -v "$vol:/var/lib/lode" "$image")
wait_listening
token=$(docker logs "$cid" 2>/dev/null | grep '^lode_' | head -n 1)
[ -n "$token" ] || { echo "smoke: no admin token in the log" >&2; exit 1; }
[ "$(docker exec "$cid" id -u)" = "10001" ] || { echo "smoke: not running as uid 10001" >&2; exit 1; }

# Runs a shell command in the container as a signed-in client.
lode_sh() {
    docker exec -e LODE_SERVER=http://127.0.0.1:7480 -e LODE_TOKEN_FILE=/tmp/token \
        -e HOME=/tmp "$cid" sh -c "$1"
}
lode_sh "printf '%s\n' '$token' > /tmp/token"
lode_sh "lode admin depot create smoke"
lode_sh "mkdir -p /tmp/ws && cd /tmp/ws && lode checkout http://127.0.0.1:7480/smoke . \
    && echo 'hello from the smoke test' > hello.txt && lode add hello.txt \
    && lode commit -m 'smoke'"

docker restart "$cid" >/dev/null
# The log of the first start is kept; wait for a second "listening" line.
n=0
until [ "$(docker logs "$cid" 2>/dev/null | grep -c 'loded listening')" -ge 2 ]; do
    n=$((n + 1))
    [ "$n" -le 60 ] || { docker logs "$cid"; echo "smoke: no restart" >&2; exit 1; }
    sleep 1
done
lode_sh "printf '%s\n' '$token' > /tmp/token"
lode_sh "lode admin depot list" | grep -q smoke
got=$(lode_sh "mkdir -p /tmp/again && cd /tmp/again \
    && lode checkout http://127.0.0.1:7480/smoke . >/dev/null && cat hello.txt")
[ "$got" = "hello from the smoke test" ] || { echo "smoke: got '$got'" >&2; exit 1; }
lode_sh "loded fsck --config /var/lib/lode/loded.toml"
echo "smoke: ok"
