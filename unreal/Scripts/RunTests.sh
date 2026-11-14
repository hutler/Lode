#!/usr/bin/env bash
# Copyright SimpleDevOps contributors. Licensed under Apache-2.0.
# Runs the plugin's automation tests in a throwaway project (unreal T02 to T10).
#   ./RunTests.sh ENGINE_DIR                      unit and fake-lode tests
#   ./RunTests.sh ENGINE_DIR --e2e LODE LODED [N] end-to-end tests against a real server, N runs
set -euo pipefail
ENGINE="${1:?usage: RunTests.sh ENGINE_DIR [--e2e LODE LODED [REPEAT]]}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
case "$(uname)" in Darwin) EDITOR="$ENGINE/Engine/Binaries/Mac/UnrealEditor.app/Contents/MacOS/UnrealEditor" ;; *) EDITOR="$ENGINE/Engine/Binaries/Linux/UnrealEditor" ;; esac
[ -x "$EDITOR" ] || { echo "editor not found: $EDITOR" >&2; exit 1; }
# A project without C++ cannot build plugins, so build the plugin first and test the built copy.
"$HERE/Scripts/BuildPlugin.sh" "$ENGINE"
VERSION=$(python3 -c "import json,sys;v=json.load(open(sys.argv[1]));print('UE%d.%d'%(v['MajorVersion'],v['MinorVersion']))" "$ENGINE/Engine/Build/Build.version")
HOSTDIR="$HERE/Build/TestHost"
rm -rf "$HOSTDIR/Plugins/LodeSourceControl"
mkdir -p "$HOSTDIR/Plugins"
cp -R "$HERE/Build/LodeSourceControl-$VERSION" "$HOSTDIR/Plugins/LodeSourceControl"
cat > "$HOSTDIR/LodeTestHost.uproject" <<'JSON'
{ "FileVersion": 3, "Category": "", "Description": "Host project for the Lode plugin tests",
  "Plugins": [ { "Name": "LodeSourceControl", "Enabled": true } ] }
JSON
FILTER="Lode"; REPEAT=1; E2E=""
if [ "${2:-}" = "--e2e" ]; then E2E=1; LODE="$3"; LODED="$4"; REPEAT="${5:-1}"; FILTER="Lode.E2E"; fi
FAILED=0
for RUN in $(seq 1 "$REPEAT"); do
  DETAILS="$HOSTDIR/e2e.json"
  if [ -n "$E2E" ]; then
    python3 "$HERE/LodeSourceControl/Tests/E2E/e2e_server.py" start --lode "$LODE" --loded "$LODED" --out "$DETAILS"
    export LODE_E2E="$DETAILS"
  fi
  if "$EDITOR" "$HOSTDIR/LodeTestHost.uproject" -ExecCmds="Automation RunTests $FILTER; Quit" -unattended -nullrhi -nosplash -nosound -stdout -FullStdOutLogOutput -ReportExportPath="$HOSTDIR/Report-$RUN" -log; then
    echo "Run $RUN passed"
  else
    echo "Run $RUN failed"; FAILED=$((FAILED + 1))
  fi
  if [ -n "$E2E" ]; then unset LODE_E2E; python3 "$HERE/LodeSourceControl/Tests/E2E/e2e_server.py" stop "$DETAILS"; fi
done
[ "$FAILED" -eq 0 ] || { echo "$FAILED of $REPEAT run(s) failed" >&2; exit 1; }
