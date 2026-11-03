#!/usr/bin/env bash
# Builds dist-native/ManyDesk-<version>-android.apk: the Rust library (NDK clang) plus the
# small Java shell in android/, packed with the SDK's own tools (no Gradle).
#   ABIS="arm64-v8a x86_64"  which phones / emulators (default: arm64-v8a armeabi-v7a x86_64 —
#                            older and low-end phones run 32-bit ARM)
#   PROFILE=debug            a debug Rust build (faster to build)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/app"
SRC="$ROOT/android"
OUT="$ROOT/dist-native"
export JAVA_HOME="${JAVA_HOME:-$(/usr/libexec/java_home -v 17)}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
NDK="$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)"
export ANDROID_NDK_HOME="$NDK"
BT="$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)"
PLATFORM="$(ls -d "$ANDROID_HOME"/platforms/android-* | sort -V | tail -1)/android.jar"
VERSION="$(grep -m1 '^version' "$APP/Cargo.toml" | cut -d'"' -f2)"
ABIS="${ABIS:-arm64-v8a armeabi-v7a x86_64}"
PROFILE="${PROFILE:-release}"
B="$APP/target/android-apk"
rm -rf "$B" && mkdir -p "$B/classes" "$B/lib"

# 1. Rust → lib/<abi>/libmanydesk.so, with the NDK's clang for the C parts (ring, openh264,
# opus through CMake) and as the linker
TC="$NDK/toolchains/llvm/prebuilt/darwin-x86_64"
PFLAG=()
[ "$PROFILE" = release ] && PFLAG=(--release)
for abi in $ABIS; do
  case $abi in
    arm64-v8a) tri=aarch64-linux-android clang=aarch64-linux-android sys=aarch64-linux-android ;;
    armeabi-v7a) tri=armv7-linux-androideabi clang=armv7a-linux-androideabi sys=arm-linux-androideabi ;;
    x86_64) tri=x86_64-linux-android clang=x86_64-linux-android sys=x86_64-linux-android ;;
    *) echo "unsupported ABI $abi" >&2; exit 1 ;;
  esac
  t=${tri//-/_}
  T=$(echo "$t" | tr a-z A-Z)
  export "CC_$t=$TC/bin/${clang}26-clang" "CXX_$t=$TC/bin/${clang}26-clang++" "AR_$t=$TC/bin/llvm-ar"
  export "CARGO_TARGET_${T}_LINKER=$TC/bin/${clang}26-clang"
  # the NDK's CMake toolchain takes the ABI as a CMake variable (not from the environment)
  printf 'set(ANDROID_ABI %s)\nset(ANDROID_PLATFORM android-26)\ninclude("%s/build/cmake/android.toolchain.cmake")\n' "$abi" "$NDK" > "$B/toolchain-$abi.cmake"
  export "CMAKE_TOOLCHAIN_FILE_$t=$B/toolchain-$abi.cmake"
  export ANDROID_NDK="$NDK"
  rustup target add "$tri" >/dev/null
  (cd "$APP" && cargo rustc --lib "${PFLAG[@]}" --target "$tri" --crate-type cdylib)
  mkdir -p "$B/lib/$abi"
  cp "$APP/target/$tri/$PROFILE/libmanydesk.so" "$B/lib/$abi/"
  # the C++ runtime openh264 links against
  cp "$TC/sysroot/usr/lib/$sys/libc++_shared.so" "$B/lib/$abi/"
done

# 2. Java → classes.dex
javac -source 11 -target 11 -classpath "$PLATFORM" -d "$B/classes" $(find "$SRC/src" -name '*.java')
"$BT/d8" --min-api 26 --output "$B" $(find "$B/classes" -name '*.class') --lib "$PLATFORM"

# 3. resources + manifest → base apk, then the code and the libraries
"$BT/aapt2" compile --dir "$SRC/res" -o "$B/res.zip"
"$BT/aapt2" link -o "$B/base.apk" -I "$PLATFORM" --manifest "$SRC/AndroidManifest.xml" \
  --version-name "$VERSION" --min-sdk-version 26 --target-sdk-version 34 "$B/res.zip"
(cd "$B" && zip -q base.apk classes.dex && zip -qr base.apk lib)

# 4. align and sign (a local key; a store release needs its own)
KEY="$ROOT/android/debug.keystore"
[ -f "$KEY" ] || keytool -genkeypair -keystore "$KEY" -storepass android -keypass android -alias manydesk \
  -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=ManyDesk Local" >/dev/null 2>&1
"$BT/zipalign" -f -p 4 "$B/base.apk" "$B/aligned.apk"
mkdir -p "$OUT"
APK="$OUT/ManyDesk-$VERSION-android.apk"
"$BT/apksigner" sign --ks "$KEY" --ks-pass pass:android --ks-key-alias manydesk --out "$APK" "$B/aligned.apk"
echo "Built $APK"
