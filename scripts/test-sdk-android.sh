#!/usr/bin/env bash
# Build the SDK for Android and run the instrumented tests on the connected device/emulator.
# Needs a JDK 17-21 (Android Studio's jbr works) and a running emulator.
set -euo pipefail
cd "$(dirname "$0")/.."
export JAVA_HOME="${JAVA_HOME:-/Applications/Android Studio.app/Contents/jbr/Contents/Home}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
scripts/build-sdk-android.sh
cd sdk-tests/android
./gradlew -q :sdk:connectedDebugAndroidTest
