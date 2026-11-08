#!/usr/bin/env bash
# Build the Android SDK into dist/android: jniLibs/<abi>/libosmo_ffi.so plus Kotlin bindings.
# Needs: rustup android targets, cargo-ndk, ANDROID_NDK_HOME (or ANDROID_HOME/ndk/<version>).
set -euo pipefail
cd "$(dirname "$0")/.."
if [ -z "${ANDROID_NDK_HOME:-}" ]; then
  SDK="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
  ANDROID_NDK_HOME="$(ls -d "$SDK"/ndk/* 2>/dev/null | sort -V | tail -1)"
  export ANDROID_NDK_HOME
fi
[ -d "$ANDROID_NDK_HOME" ] || { echo "set ANDROID_NDK_HOME"; exit 1; }
OUT=dist/android
rm -rf "$OUT" && mkdir -p "$OUT/jniLibs" "$OUT/kotlin"
RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384" cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -t x86 -o "$OUT/jniLibs" build -p osmo-ffi --release
# Bindings come from a host build of the same crate (the interface is identical on every target).
cargo build --release -p osmo-ffi
case "$(uname)" in Darwin) HOSTLIB=target/release/libosmo_ffi.dylib ;; *) HOSTLIB=target/release/libosmo_ffi.so ;; esac
cargo run -q -p osmo-ffi --bin uniffi-bindgen -- generate --library "$HOSTLIB" --language kotlin --out-dir "$OUT/kotlin"
echo "built $OUT:"; find "$OUT" -type f | sort | xargs ls -la
