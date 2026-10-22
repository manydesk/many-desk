#!/usr/bin/env bash
# Tauri Windows build from macOS/Linux: cross-compiles with mingw-w64 (no MSVC licence
# needed), then makes the installer and the portable exe with NSIS in Docker (Homebrew's
# makensis crashes on recent macOS).
# Needs: rustup target x86_64-pc-windows-gnu, mingw-w64, Docker.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REL="$ROOT/src-tauri/target/x86_64-pc-windows-gnu/release"
VERSION="$(node -p "require('$ROOT/src-tauri/tauri.conf.json').version")"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
# Static C/C++ runtimes (libstdc++ for OpenH264, see src-tauri/.cargo/config.toml).
LIBDIR="$(dirname "$(x86_64-w64-mingw32-gcc -print-file-name=libstdc++.a)")"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-L native=$LIBDIR -C link-arg=-static-libgcc -C link-arg=-static-libstdc++"

cd "$ROOT"
[ -f build/bin-win/manydesk-input.exe ] || npm run build:helper:win
# Compiles the exe and writes the installer script; its own makensis step may fail here.
STAMP=$(mktemp)
npx tauri build --target x86_64-pc-windows-gnu --bundles nsis || true
[ "$REL/manydesk.exe" -nt "$STAMP" ] || { echo "build failed"; exit 1; }

mkdir -p dist-tauri
docker run --rm -v "$ROOT:$ROOT" -v "$HOME/Library/Caches/tauri:$HOME/Library/Caches/tauri" debian:13 bash -c "
  set -e
  apt-get update -qq >/dev/null && apt-get install -y -qq nsis >/dev/null 2>&1
  makensis -V2 -INPUTCHARSET UTF8 '$REL/nsis/x64/installer.nsi'
  makensis -V2 -DVERSION=$VERSION -DREL='$REL' -DROOT='$ROOT' '$ROOT/scripts/portable-win.nsi'
"
cp "$REL/nsis/x64/nsis-output.exe" "dist-tauri/ManyDesk-$VERSION-win-x64-setup.exe"
ls -lh dist-tauri/*win*
