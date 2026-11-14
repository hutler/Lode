#!/bin/sh
# Fills in the winget manifests and the Homebrew formula for a release:
#   packaging/render-manifests.sh <version> <SHA256SUMS> <output folder>
# {{VERSION}} becomes the version and {{SHA256_<file>}} the file's hash from SHA256SUMS.
# Fails if a needed file is not listed.
set -eu
version="${1:?usage: render-manifests.sh <version> <SHA256SUMS> <output folder>}"
sums="${2:?}"
out="${3:?}"
here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$out/winget" "$out/homebrew"
render() {
    text="$(sed "s/{{VERSION}}/$version/g" "$1")"
    for name in $(printf '%s\n' "$text" | sed -n 's/.*{{SHA256_\([^}]*\)}}.*/\1/p'); do
        hash="$(awk -v f="$name" '{ n = $2; sub(/^\*/, "", n); if (n == f) print $1 }' "$sums")"
        [ -n "$hash" ] || { echo "render-manifests: $name is not in $sums" >&2; exit 1; }
        text="$(printf '%s\n' "$text" | sed "s/{{SHA256_$name}}/$hash/g")"
    done
    printf '%s\n' "$text" > "$2"
}
for f in "$here"/winget/*.yaml; do
    render "$f" "$out/winget/$(basename "$f")"
done
render "$here/homebrew/lode.rb" "$out/homebrew/lode.rb"
echo "Rendered manifests for $version into $out"
