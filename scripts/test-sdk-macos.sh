#!/usr/bin/env bash
# Build the macOS SDK, install the generated bindings into the Swift test package, run `swift test`.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/build-sdk-macos.sh
P=sdk-tests/swift/Sources
rm -rf "$P" && mkdir -p "$P/osmo_ffiFFI/include" "$P/OsmoSdk"
cp dist/macos/osmo_ffiFFI.h "$P/osmo_ffiFFI/include/"
cp dist/macos/module.modulemap "$P/osmo_ffiFFI/include/module.modulemap"
echo '// header-only target' > "$P/osmo_ffiFFI/dummy.c"
cp dist/macos/osmo_ffi.swift "$P/OsmoSdk/"
cd sdk-tests/swift
# XCTest ships with Xcode, not the Command Line Tools.
[ -d /Applications/Xcode.app ] && export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
swift test
