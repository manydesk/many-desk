#!/usr/bin/env bash
# Packages the native app (app/) for distribution into dist-native/:
#   mac      ManyDesk.app (universal: Apple Silicon + Intel) in a DMG
#   win      one-file ManyDesk.exe (input helper embedded) — cross-compiled with mingw-w64 —
#            and its installer (NSIS, scripts/windows/manydesk.nsi)
#   linux    .deb + .tar.gz for amd64 and arm64, built in Docker (ARCHS="amd64 arm64")
# Usage: scripts/package-native.sh [--bump major|minor|patch] [mac] [win] [linux]   (default: all)
#
# Versions: app/Cargo.toml holds major.minor.patch. --bump raises one part first (major
# resets minor and patch, minor resets patch) and writes it back to Cargo.toml/Cargo.lock.
# Every run also stamps a build number (yyyymmdd.hhmm, MANYDESK_BUILD) into all its
# packages: the app shows "ManyDesk 0.14.0 · build 20261020.1405" in the sidebar, and
# `manydesk --version` prints it (macOS/Linux), so you can tell which build is installed.
#
# macOS signing: Screen Recording and Accessibility grants are keyed to the app's code
# signature. An ad-hoc signature changes with every build, so each rebuild would silently
# lose both (the host captures nothing; viewers see a blank session). The mac target
# therefore signs with a stable identity:
#   MANYDESK_SIGN_ID=<name or SHA-1>   use this keychain identity (e.g. "Developer ID Application: …")
#   unset                              use "ManyDesk Local Signing"; created once as a self-signed
#                                      code-signing certificate in the login keychain if missing
#   MANYDESK_SIGN_ID=-                 ad hoc (grants are lost on every rebuild)
# One-time setup: nothing beyond the first mac run (it creates the certificate). After the
# first build signed this way, grant Screen Recording / Accessibility once more; later
# rebuilds keep them. Check with: codesign -d -r- dist-native/ManyDesk.app
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/app"
OUT="$ROOT/dist-native"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
mkdir -p "$OUT"

BUMP=""
ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --bump) BUMP="${2:-}"; shift 2 ;;
    --bump=*) BUMP="${1#--bump=}"; shift ;;
    *) ARGS+=("$1"); shift ;;
  esac
done

# Raises major, minor or patch in app/Cargo.toml (and the package's entry in Cargo.lock).
bump_version() {
  local cur new major minor patch
  cur="$(grep -m1 '^version' "$APP/Cargo.toml" | cut -d'"' -f2)"
  IFS=. read -r major minor patch <<<"$cur"
  case "$1" in
    major) new="$((major + 1)).0.0" ;;
    minor) new="$major.$((minor + 1)).0" ;;
    patch) new="$major.$minor.$((patch + 1))" ;;
    *) echo "--bump takes major, minor or patch (got \"$1\")" >&2; exit 2 ;;
  esac
  perl -0pi -e "s/^version = \"\Q$cur\E\"/version = \"$new\"/m" "$APP/Cargo.toml"
  perl -0pi -e "s/(name = \"manydesk\"\nversion = )\"\Q$cur\E\"/\\1\"$new\"/" "$APP/Cargo.lock"
  echo "version $cur -> $new" >&2
}
[ -n "$BUMP" ] && bump_version "$BUMP"

VERSION="$(grep -m1 '^version' "$APP/Cargo.toml" | cut -d'"' -f2)"
# One build number for everything this run produces.
export MANYDESK_BUILD="${MANYDESK_BUILD:-$(date +%Y%m%d.%H%M)}"
TARGETS="${ARGS[*]:-mac win linux}"
echo "ManyDesk $VERSION · build $MANYDESK_BUILD ($TARGETS)" >&2

LOCAL_SIGN_ID="ManyDesk Local Signing"

# Creates the self-signed code-signing identity in the login keychain if it is missing.
ensure_local_identity() {
  security find-identity -p codesigning | grep -qF "\"$LOCAL_SIGN_ID\"" && return 0
  echo "Creating self-signed code-signing certificate \"$LOCAL_SIGN_ID\" in the login keychain" >&2
  local T PASS=manydesk
  T="$(mktemp -d)"
  cat > "$T/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $LOCAL_SIGN_ID
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF
  # /usr/bin/openssl (LibreSSL) writes a PKCS#12 that `security import` can read.
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$T/cert.cnf" \
    -keyout "$T/key.pem" -out "$T/cert.pem" >/dev/null 2>&1
  /usr/bin/openssl pkcs12 -export -inkey "$T/key.pem" -in "$T/cert.pem" -name "$LOCAL_SIGN_ID" \
    -passout "pass:$PASS" -out "$T/id.p12"
  security import "$T/id.p12" -k "$HOME/Library/Keychains/login.keychain-db" -P "$PASS" -T /usr/bin/codesign >/dev/null
  rm -rf "$T"
}

# Signs ManyDesk.app (helper first, then the bundle) with a stable identity when possible.
sign_mac() {
  local app="$1" id="${MANYDESK_SIGN_ID:-}"
  if [ -z "$id" ]; then
    if ensure_local_identity; then id="$LOCAL_SIGN_ID"; else id="-"; fi
  fi
  if [ "$id" = "-" ]; then
    echo "WARNING: signing ad hoc; Screen Recording/Accessibility grants will not survive the next rebuild" >&2
  fi
  codesign --force -s "$id" -i az.manydesk.native.input "$app/Contents/MacOS/manydesk-input"
  codesign --force -s "$id" "$app"
}

mac() {
  bash "$ROOT/scripts/build-helper.sh"
  rustup target add x86_64-apple-darwin >/dev/null
  (cd "$APP" && cargo build --release --target aarch64-apple-darwin && cargo build --release --target x86_64-apple-darwin)
  local B="$OUT/ManyDesk.app/Contents"
  rm -rf "$OUT/ManyDesk.app"
  mkdir -p "$B/MacOS" "$B/Resources"
  lipo -create "$APP/target/aarch64-apple-darwin/release/manydesk" "$APP/target/x86_64-apple-darwin/release/manydesk" -output "$B/MacOS/manydesk"
  cp "$ROOT/build/bin/manydesk-input" "$B/MacOS/"
  cp "$APP/assets/icon.icns" "$B/Resources/icon.icns"
  cat > "$B/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>ManyDesk</string>
  <key>CFBundleDisplayName</key><string>ManyDesk</string>
  <key>CFBundleIdentifier</key><string>az.manydesk.native</string>
  <key>CFBundleExecutable</key><string>manydesk</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$MANYDESK_BUILD</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Voice chat with the other side of a session.</string>
  <key>NSCameraUsageDescription</key><string>Show your camera to the other side of a session when you turn it on.</string>
  <key>NSLocalNetworkUsageDescription</key><string>Find and reach ManyDesk on your local network, also without internet, and wake computers there.</string>
  <key>NSScreenCaptureUsageDescription</key><string>Show this screen to the people you let connect.</string>
</dict></plist>
EOF
  sign_mac "$OUT/ManyDesk.app"
  local DMG="$OUT/ManyDesk-$VERSION-mac-universal.dmg" STAGE
  STAGE="$(mktemp -d)"
  cp -R "$OUT/ManyDesk.app" "$STAGE/" && ln -s /Applications "$STAGE/Applications"
  rm -f "$DMG"
  hdiutil create -volname ManyDesk -srcfolder "$STAGE" -format UDZO "$DMG" >/dev/null 2>&1
  rm -rf "$STAGE"
}

win() {
  bash "$ROOT/scripts/build-helper-win.sh"
  local LIBDIR
  LIBDIR="$(dirname "$(x86_64-w64-mingw32-gcc -print-file-name=libstdc++.a)")"
  export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
  export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="-L native=$LIBDIR -C link-arg=-static-libgcc -C link-arg=-static-libstdc++"
  export CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_gnu="$ROOT/scripts/cmake/mingw-x86_64.cmake"
  (cd "$APP" && cargo build --release --target x86_64-pc-windows-gnu)
  cp "$APP/target/x86_64-pc-windows-gnu/release/manydesk.exe" "$OUT/ManyDesk-$VERSION-win-x64.exe"
  # the installer (scripts/windows/manydesk.nsi): NSIS here, or in Docker when not installed
  local SETUP="ManyDesk-$VERSION-win-x64-setup.exe"
  if command -v makensis >/dev/null; then
    # makensis reads UTF-8 (the Russian and Turkish texts) only under a UTF-8 locale
    local UTF8=C.UTF-8
    locale -a 2>/dev/null | grep -qix 'c.utf-\?8' || UTF8=en_US.UTF-8
    LC_ALL=$UTF8 makensis -V2 -DVERSION="$VERSION" -DEXE="$OUT/ManyDesk-$VERSION-win-x64.exe" -DICON="$APP/assets/icon.ico" \
      -DLICENSE="$ROOT/LICENSE" -DOUTFILE="$OUT/$SETUP" "$ROOT/scripts/windows/manydesk.nsi"
  elif command -v docker >/dev/null; then
    docker run --rm --platform linux/amd64 -v "$ROOT:/src" ubuntu:24.04 bash -c "apt-get update -qq >/dev/null && apt-get install -y -qq nsis >/dev/null && \
      makensis -V2 -DVERSION=$VERSION -DEXE=/src/dist-native/ManyDesk-$VERSION-win-x64.exe -DICON=/src/app/assets/icon.ico \
      -DLICENSE=/src/LICENSE -DOUTFILE=/src/dist-native/$SETUP /src/scripts/windows/manydesk.nsi"
  else
    echo "no makensis or docker: $SETUP not built" >&2
  fi
}

# The .deb and .tar.gz, built where this runs (SRC: the repository there).
LINUX_BUILD='
        set -e
        cd "$SRC/app" && cargo build --release
        P=/tmp/manydesk-pkg; rm -rf $P
        mkdir -p $P/DEBIAN $P/usr/bin $P/usr/share/applications $P/usr/share/icons/hicolor/256x256/apps
        install -m755 target/release/manydesk $P/usr/bin/manydesk
        install -m644 assets/icon.png $P/usr/share/icons/hicolor/256x256/apps/manydesk.png
        cat > $P/usr/share/applications/manydesk.desktop <<EOF
[Desktop Entry]
Name=ManyDesk
Comment=Remote desktop
Exec=manydesk
Icon=manydesk
Terminal=false
Type=Application
Categories=Network;RemoteAccess;
EOF
        cat > $P/DEBIAN/control <<EOF
Package: manydesk
Version: $VERSION
Architecture: $ARCH
Maintainer: ManyDesk
Depends: libgl1, libxkbcommon0, libxkbcommon-x11-0, libgtk-3-0, libpipewire-0.3-0, libasound2 | libasound2t64, libgstreamer1.0-0, libgstreamer-plugins-base1.0-0, gstreamer1.0-plugins-base
Recommends: gstreamer1.0-plugins-bad
Description: ManyDesk remote desktop (native, peer-to-peer)
EOF
        dpkg-deb --root-owner-group -b $P "$SRC/dist-native/ManyDesk-$VERSION-linux-$ARCH.deb" >/dev/null
        tar -C target/release -czf "$SRC/dist-native/ManyDesk-$VERSION-linux-$ARCH.tar.gz" manydesk
'

# MANYDESK_LINUX_NATIVE=1: on this Linux machine, for its own architecture (CI runners; the
# packages docker/linux/Dockerfile lists must be installed). Otherwise in Docker, per ARCHS.
linux() {
  if [ -n "${MANYDESK_LINUX_NATIVE:-}" ]; then
    SRC="$ROOT" VERSION="$VERSION" ARCH="$(dpkg --print-architecture)" bash -c "$LINUX_BUILD"
    return
  fi
  for arch in ${ARCHS:-amd64 arm64}; do
    local IMAGE="md-linux-dev-$arch"
    docker build -q --platform "linux/$arch" -t "$IMAGE" "$ROOT/docker/linux" >/dev/null
    docker run --rm --platform "linux/$arch" -v "$ROOT:/src" -v "md-cargo-$arch:/root/.cargo/registry" -v "md-target-native-$arch:/src/app/target" \
      -e SRC=/src -e VERSION="$VERSION" -e ARCH="$arch" -e MANYDESK_BUILD="$MANYDESK_BUILD" "$IMAGE" bash -c "$LINUX_BUILD"
  done
}

for t in $TARGETS; do "$t"; done
echo "Built ManyDesk $VERSION · build $MANYDESK_BUILD:" >&2
ls -1 "$OUT" | grep -F -- "-$VERSION-" | sed 's/^/  /' >&2
ls -lh "$OUT"
