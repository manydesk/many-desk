#!/usr/bin/env bash
# Builds ManyDesk.app for iOS from the Rust library and ios/ (no Xcode project): the
# simulator by default, a phone with DEVICE=1 (signed with your Apple development
# identity: SIGN_ID="Apple Development: …", PROFILE=path/to/embedded.mobileprovision).
# Needs full Xcode (xcode-select -s /Applications/Xcode.app).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/app"
OUT="$ROOT/dist-native"
VERSION="$(grep -m1 '^version' "$APP/Cargo.toml" | cut -d'"' -f2)"
BUILD="${MANYDESK_BUILD:-$(date +%Y%m%d.%H%M)}"
export MANYDESK_BUILD="$BUILD"

if [ "${DEVICE:-0}" = 1 ]; then
  TARGET=aarch64-apple-ios SDK=iphoneos CLANG_TARGET=arm64-apple-ios15.0
else
  TARGET=aarch64-apple-ios-sim SDK=iphonesimulator CLANG_TARGET=arm64-apple-ios15.0-simulator
fi
rustup target add "$TARGET" >/dev/null
SYSROOT="$(xcrun --sdk "$SDK" --show-sdk-path)"

# 1. Rust → libmanydesk.a
(cd "$APP" && IPHONEOS_DEPLOYMENT_TARGET=15.0 cargo rustc --lib --release --target "$TARGET" --crate-type staticlib)
LIB="$APP/target/$TARGET/release/libmanydesk.a"

# 2. the app bundle
B="$APP/target/ios/ManyDesk.app"
rm -rf "$B" && mkdir -p "$B"
xcrun --sdk "$SDK" clang -target "$CLANG_TARGET" -isysroot "$SYSROOT" -O2 -o "$B/ManyDesk" "$ROOT/ios/main.m" "$LIB" \
  -framework UIKit -framework Foundation -framework Metal -framework QuartzCore -framework CoreGraphics \
  -framework CoreFoundation -framework CoreMedia -framework CoreVideo -framework VideoToolbox \
  -framework AudioToolbox -framework CoreAudio -framework AVFoundation -framework Security \
  -framework SystemConfiguration -lresolv -lc++
sed -e "s/VERSION/$VERSION/" -e "s/BUILD/$BUILD/" "$ROOT/ios/Info.plist" > "$B/Info.plist"
sips -Z 180 "$APP/assets/icon.png" --out "$B/AppIcon.png" >/dev/null

# 3. signing: ad hoc for the simulator, your identity for a phone
if [ "${DEVICE:-0}" = 1 ]; then
  : "${SIGN_ID:?set SIGN_ID to your Apple Development identity}"
  : "${PROFILE:?set PROFILE to a provisioning profile for az.manydesk.ios}"
  cp "$PROFILE" "$B/embedded.mobileprovision"
  codesign -f -s "$SIGN_ID" "$B"
else
  codesign -f -s - "$B"
fi

mkdir -p "$OUT"
rm -rf "$OUT/ManyDesk-ios.app"
cp -R "$B" "$OUT/ManyDesk-ios.app"
echo "Built $OUT/ManyDesk-ios.app ($TARGET)"
[ "${DEVICE:-0}" = 1 ] || echo "Run: xcrun simctl install booted '$OUT/ManyDesk-ios.app' && xcrun simctl launch booted az.manydesk.ios"
