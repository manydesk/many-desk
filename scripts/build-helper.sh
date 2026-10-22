#!/usr/bin/env bash
# Builds the native CoreGraphics input helper as a universal binary (Apple Silicon + Intel).
# Deployment target matches Electron's own minimum (macOS 13).
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p build/bin build/tmp
SRC=native/InputHelper.swift
OUT=build/bin/manydesk-input
if [[ -f "$OUT" && "$OUT" -nt "$SRC" && "$OUT" -nt "$0" ]]; then
  exit 0
fi
swiftc -O -target arm64-apple-macos13 "$SRC" -o build/tmp/input-arm64
swiftc -O -target x86_64-apple-macos13 "$SRC" -o build/tmp/input-x86_64
lipo -create build/tmp/input-arm64 build/tmp/input-x86_64 -output "$OUT"
rm -rf build/tmp
echo "built $OUT ($(lipo -archs "$OUT"))"
