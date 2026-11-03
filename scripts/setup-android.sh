#!/usr/bin/env bash
# Installs what the Android build needs: command-line tools (Homebrew), the SDK platform,
# build tools, NDK and the Rust targets; with WITH_EMULATOR=1 also an emulator
# with an arm64 system image.
set -euo pipefail

export JAVA_HOME="${JAVA_HOME:-$(/usr/libexec/java_home -v 17)}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
mkdir -p "$ANDROID_HOME"

command -v sdkmanager >/dev/null || brew install --cask android-commandlinetools

yes | sdkmanager --sdk_root="$ANDROID_HOME" --licenses >/dev/null || true
# What building the APK needs (~0.9 GB).
sdkmanager --sdk_root="$ANDROID_HOME" "platform-tools" "platforms;android-35" "build-tools;35.0.0" "ndk;27.2.12479018"

rustup target add aarch64-linux-android x86_64-linux-android

echo "Android build tools ready in $ANDROID_HOME"

# An emulator to test on (~2 GB more): WITH_EMULATOR=1.
[ "${WITH_EMULATOR:-0}" = 1 ] || exit 0
sdkmanager --sdk_root="$ANDROID_HOME" "emulator" "system-images;android-35;google_apis;arm64-v8a"
echo no | "$ANDROID_HOME/cmdline-tools/latest/bin/avdmanager" create avd -n manydesk -k "system-images;android-35;google_apis;arm64-v8a" -d pixel_7 --force 2>/dev/null \
  || echo no | avdmanager create avd -n manydesk -k "system-images;android-35;google_apis;arm64-v8a" -d pixel_7 --force

echo "Emulator \"manydesk\" ready"
