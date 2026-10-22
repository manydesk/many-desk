#!/usr/bin/env bash
# Cross-compiles the Windows input helper (SendInput) with Go. Works from macOS, Linux or Windows (Git Bash).
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p build/bin-win
SRC=native/win/inputhelper_windows.go
OUT=build/bin-win/manydesk-input.exe
if [[ -f "$OUT" && "$OUT" -nt "$SRC" ]]; then
  exit 0
fi
GOOS=windows GOARCH=amd64 CGO_ENABLED=0 go build -ldflags "-s -w" -o "$OUT" "$SRC"
echo "built $OUT"
