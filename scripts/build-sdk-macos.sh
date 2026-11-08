#!/usr/bin/env bash
# Build the macOS SDK into dist/macos: libosmo_ffi.dylib plus Swift bindings.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=dist/macos
rm -rf "$OUT" && mkdir -p "$OUT"
LIBS=()
for t in aarch64-apple-darwin x86_64-apple-darwin; do
  if rustup target list --installed | grep -qx "$t"; then
    cargo build --release -p osmo-ffi --target "$t"
    LIBS+=("target/$t/release/libosmo_ffi.dylib")
  else
    echo "skipping $t (rustup target add $t to include it)"
  fi
done
[ ${#LIBS[@]} -gt 0 ] || { echo "no macOS target installed"; exit 1; }
if [ ${#LIBS[@]} -gt 1 ]; then
  lipo -create "${LIBS[@]}" -output "$OUT/libosmo_ffi.dylib"
else
  cp "${LIBS[0]}" "$OUT/libosmo_ffi.dylib"
fi
install_name_tool -id @rpath/libosmo_ffi.dylib "$OUT/libosmo_ffi.dylib"
cargo run -q -p osmo-ffi --bin uniffi-bindgen -- generate \
  --library "${LIBS[0]}" --language swift --out-dir "$OUT"
# SwiftPM wants the module map named module.modulemap.
cp "$OUT/osmo_ffiFFI.modulemap" "$OUT/module.modulemap"
echo "built $OUT:"; ls -la "$OUT"
