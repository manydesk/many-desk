#!/usr/bin/env bash
# Tauri Linux build (.deb + AppImage) in Docker. ARCH=amd64 (default) or arm64.
# The image (docker/linux/Dockerfile) has WebKitGTK, PipeWire/X11 headers and Rust.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCH="${ARCH:-amd64}"
IMAGE="md-linux-dev-$ARCH"
docker image inspect "$IMAGE" >/dev/null 2>&1 || docker build --platform "linux/$ARCH" -t "$IMAGE" "$ROOT/docker/linux"
mkdir -p "$ROOT/dist-tauri"
docker run --rm --platform "linux/$ARCH" \
  -v "$ROOT:/src" -v "md-cargo-$ARCH:/root/.cargo/registry" -v "md-target-$ARCH:/src/src-tauri/target" \
  -e APPIMAGE_EXTRACT_AND_RUN=1 -e NO_STRIP=1 \
  "$IMAGE" bash -c '
    set -e
    # The repo'"'"'s node_modules hold the host (macOS) CLI binary; use a Linux one.
    (cd /tmp && npm i --silent --no-save @tauri-apps/cli@2.12.3 >/dev/null)
    cd /src
    /tmp/node_modules/.bin/tauri build --bundles deb,appimage
    v=$(node -p "require(\"./src-tauri/tauri.conf.json\").version")
    b=src-tauri/target/release/bundle
    cp "$b"/deb/*.deb "dist-tauri/ManyDesk-$v-linux-'"$ARCH"'.deb"
    cp "$b"/appimage/*.AppImage "dist-tauri/ManyDesk-$v-linux-'"$ARCH"'.AppImage"
  '
ls -lh "$ROOT"/dist-tauri/*linux*
