#!/usr/bin/env bash
# Copyright SimpleDevOps contributors. Licensed under Apache-2.0.
# Builds the Lode plugin for one installed engine and zips it (unreal T01, T11).
#   ./BuildPlugin.sh /path/to/UE_5.8 [Linux|Mac]
set -euo pipefail
ENGINE="${1:?usage: BuildPlugin.sh ENGINE_DIR [PLATFORM]}"
PLATFORM="${2:-$( [ "$(uname)" = Darwin ] && echo Mac || echo Linux )}"
HERE="$(cd "$(dirname "$0")" && pwd)"
PLUGIN="$HERE/../LodeSourceControl/LodeSourceControl.uplugin"
OUT="$HERE/../Build"
UAT="$ENGINE/Engine/Build/BatchFiles/RunUAT.sh"
[ -x "$UAT" ] || { echo "RunUAT.sh not found under $ENGINE" >&2; exit 1; }
VERSION=$(python3 -c "import json,sys;v=json.load(open(sys.argv[1]));print('UE%d.%d'%(v['MajorVersion'],v['MinorVersion']))" "$ENGINE/Engine/Build/Build.version")
PACKAGE="$OUT/LodeSourceControl-$VERSION"
mkdir -p "$OUT"
rm -rf "$PACKAGE"
"$UAT" BuildPlugin -Plugin="$PLUGIN" -Package="$PACKAGE" -TargetPlatforms="$PLATFORM" -Rocket
rm -rf "$PACKAGE/Intermediate"
ZIP="$PACKAGE-$PLATFORM.zip"
rm -f "$ZIP"
(cd "$OUT" && zip -qr "$(basename "$ZIP")" "$(basename "$PACKAGE")")
echo "Built $ZIP"
