#!/usr/bin/env bash
# Build the magpie-rs APK without Gradle: cargo -> aapt2 -> javac -> d8 ->
# zip -> zipalign -> apksigner. The CI job runs exactly this script.
#
#   ./scripts/build-apk.sh                       # debug, both ABIs
#   ./scripts/build-apk.sh --release             # release
#   ./scripts/build-apk.sh --release --skip-rust # reuse android/build/lib/*.so
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ABIS=("arm64-v8a" "x86_64")
PROFILE=debug
SKIP_RUST=0
VERSION_NAME=1.0.0
VERSION_CODE=1
SIGNING_KEY=""
SIGNING_PASS=android

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) PROFILE=release; shift ;;
    --skip-rust) SKIP_RUST=1; shift ;;
    --version-name) VERSION_NAME="$2"; shift 2 ;;
    --version-code) VERSION_CODE="$2"; shift 2 ;;
    --abi) ABIS=("$2"); shift 2 ;;
    --abis) IFS=',' read -ra ABIS <<< "$2"; shift 2 ;;
    --signing-key) SIGNING_KEY="$2"; shift 2 ;;
    --signing-pass) SIGNING_PASS="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

step() { printf '\n=== %s ===\n' "$1"; }
die() { printf 'ERROR: %s\n' "$1" >&2; exit 1; }

: "${ANDROID_HOME:?ANDROID_HOME must point at an Android SDK}"
SDK="$ANDROID_HOME"
API=36
BUILD_TOOLS="${BUILD_TOOLS_VERSION:-36.0.0}"
BT="$SDK/build-tools/$BUILD_TOOLS"
ANDROID_JAR="$SDK/platforms/android-$API/android.jar"
for tool in "$BT/aapt2" "$BT/d8" "$BT/zipalign" "$BT/apksigner" "$ANDROID_JAR"; do
  [[ -x "$tool" || -f "$tool" ]] || die "missing $tool"
done

BUILD="$ROOT/android/build"
OUT="$ROOT/android/app/build/outputs/apk"
rm -rf "$BUILD/gen" "$BUILD/classes" "$BUILD/dex" "$OUT"
mkdir -p "$BUILD/gen" "$BUILD/classes" "$BUILD/dex" "$OUT"

declare -A TARGET=( [arm64-v8a]=aarch64-linux-android [x86_64]=x86_64-linux-android )
declare -A JNI=( [arm64-v8a]=arm64-v8a [x86_64]=x86_64 )

# ------------------------------------------------------------------ 1/7 native
if [[ $SKIP_RUST -eq 0 ]]; then
  step "1/7 cargo build ($PROFILE: ${ABIS[*]})"
  : "${NDK_HOME:?NDK_HOME must point at an Android NDK}"
  NDKBIN="$NDK_HOME/toolchains/llvm/prebuilt/$(uname -s | tr 'A-Z' 'a-z')-x86_64/bin"
  for abi in "${ABIS[@]}"; do
    t="${TARGET[$abi]}"
    upper="$(echo "$t" | tr 'a-z-' 'A-Z_')"
    CLANG="$NDKBIN/${t}${API}-clang"
    [[ -x "$CLANG" ]] || CLANG="$NDKBIN/${t}-clang"
    export "CARGO_TARGET_${upper}_LINKER=$CLANG"
    export "CC_$(echo "$t" | tr '-' '_')=$CLANG"
    export "AR_$(echo "$t" | tr '-' '_')=$NDKBIN/llvm-ar"
    cargo build -p magpie-android --target "$t" $([[ $PROFILE == release ]] && echo --release)
  done
else
  step "1/7 cargo build (skipped)"
fi

# -------------------------------------------------------------- 2/7 stage libs
step "2/7 stage libmagpie.so"
LIBROOT="$BUILD/lib"
if [[ $SKIP_RUST -eq 0 ]]; then
  rm -rf "$LIBROOT"
  for abi in "${ABIS[@]}"; do
    src="$ROOT/target/${TARGET[$abi]}/$PROFILE/libmagpie.so"
    [[ -f "$src" ]] || die "missing $src"
    mkdir -p "$LIBROOT/${JNI[$abi]}"
    cp "$src" "$LIBROOT/${JNI[$abi]}/libmagpie.so"
    if [[ $PROFILE == release && -n "${NDK_HOME:-}" ]]; then
      strip="$NDK_HOME/toolchains/llvm/prebuilt/$(uname -s | tr 'A-Z' 'a-z')-x86_64/bin/llvm-strip"
      [[ -x "$strip" ]] && "$strip" --strip-unneeded "$LIBROOT/${JNI[$abi]}/libmagpie.so" || true
    fi
    printf '  %-10s %s MB\n' "$abi" "$(du -m "$LIBROOT/${JNI[$abi]}/libmagpie.so" | cut -f1)"
  done
fi
[[ -d "$LIBROOT" ]] || die "no staged libraries; drop --skip-rust"

# -------------------------------------------------------------------- 3/7 aapt2
step "3/7 aapt2 compile + link"
"$BT/aapt2" compile --dir android/app/src/main/res -o "$BUILD/resources.zip"
"$BT/aapt2" link \
  -o "$BUILD/base.apk" \
  -I "$ANDROID_JAR" \
  --manifest android/app/src/main/AndroidManifest.xml \
  --java "$BUILD/gen" \
  --min-sdk-version "$API" \
  --target-sdk-version "$API" \
  --version-code "$VERSION_CODE" \
  --version-name "$VERSION_NAME" \
  --auto-add-overlay \
  "$BUILD/resources.zip"

# -------------------------------------------------------------------- 4/7 javac
step "4/7 javac"
mapfile -t SOURCES < <(find android/app/src/main/java "$BUILD/gen" -name '*.java')
javac -encoding UTF-8 -source 17 -target 17 -nowarn \
  -classpath "$ANDROID_JAR" -d "$BUILD/classes" "${SOURCES[@]}" 2>&1 \
  | grep -v -E 'bootstrap class path|source value 17|target value 17|^Note:|warning' || true
[[ -d "$BUILD/classes/io" ]] || die "javac produced nothing"

# ----------------------------------------------------------------------- 5/7 d8
step "5/7 d8"
(cd "$BUILD/classes" && jar cf "$BUILD/classes.jar" .)
"$BT/d8" --release --lib "$ANDROID_JAR" --min-api "$API" --output "$BUILD/dex" "$BUILD/classes.jar"

# ---------------------------------------------------------------------- 6/7 zip
step "6/7 package dex + native libraries"
(cd "$BUILD/dex" && zip -q "$BUILD/base.apk" classes.dex)
# zip stores the paths as given, so the archive is built from a directory that
# already has the lib/<abi>/ layout. `-exec ... {} +` is used instead of
# `stdin + -@`: zip's stdin mode splits on newlines, so `-print0` silently
# truncated the list to one entry and only the first ABI made it into the APK.
(cd "$LIBROOT/.." && find lib -name '*.so' -exec zip -q -0 "$BUILD/base.apk" {} +)
(cd "$LIBROOT/.." && unzip -l "$BUILD/base.apk" | grep -c 'lib/.*\.so' | xargs -I{} echo "  {} native librar(y|ies) packaged")

# ---------------------------------------------------------- 7/7 align and sign
step "7/7 zipalign + apksigner"
ZIPALIGN_ARGS=(-f 4)
# -P 16 page-aligns the uncompressed .so entries to 16 KB (Android 15+ needs it);
# it cannot be combined with -p, and older build-tools do not know it at all.
"$BT/zipalign" -f -P 16 4 "$BUILD/base.apk" "$BUILD/aligned.apk" 2>/dev/null \
  || "$BT/zipalign" "${ZIPALIGN_ARGS[@]}" "$BUILD/base.apk" "$BUILD/aligned.apk"

KEY="${SIGNING_KEY:-$ROOT/android/debug.keystore}"
if [[ ! -f "$KEY" ]]; then
  keytool -genkeypair -v -keystore "$KEY" -storepass "$SIGNING_PASS" -keypass "$SIGNING_PASS" \
    -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
    -dname 'CN=Android Debug,O=Android,C=US' >/dev/null 2>&1
fi

NAME=app-debug.apk
[[ $PROFILE == release ]] && NAME=app-release.apk
"$BT/apksigner" sign --ks "$KEY" --ks-pass "pass:$SIGNING_PASS" --key-pass "pass:$SIGNING_PASS" \
  --out "$OUT/$NAME" "$BUILD/aligned.apk"
"$BT/apksigner" verify --print-certs "$OUT/$NAME" | head -3

printf '\nAPK: %s (%s MB)\n' "$OUT/$NAME" "$(du -m "$OUT/$NAME" | cut -f1)"
printf 'Install with: adb install -r "%s"\n' "$OUT/$NAME"
