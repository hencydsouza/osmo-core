#!/usr/bin/env bash
# Build the Rust SDK for Android, then build / test / install the app.
#   scripts/android-app.sh build      debug APK
#   scripts/android-app.sh test       unit tests + instrumented tests on the connected device/emulator
#   scripts/android-app.sh install    build + install + launch on the connected device
#   scripts/android-app.sh logs       follow app log
set -euo pipefail
cd "$(dirname "$0")/.."
export JAVA_HOME="${JAVA_HOME:-/Applications/Android Studio.app/Contents/jbr/Contents/Home}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ADB="$ANDROID_HOME/platform-tools/adb"
sdk() { [ -d dist/android/jniLibs ] || scripts/build-sdk-android.sh; }
gw() { (cd osmo-android && { [ -f local.properties ] || echo "sdk.dir=$ANDROID_HOME" > local.properties; } && ./gradlew "$@"); }
case "${1:-build}" in
  build)   sdk; gw :app:assembleDebug ;;
  test)    sdk; gw :app:testDebugUnitTest :app:connectedDebugAndroidTest ;;
  install) sdk; gw :app:assembleDebug
           "$ADB" install -r osmo-android/app/build/outputs/apk/debug/app-debug.apk
           "$ADB" shell am start -n dev.osmo.app/.MainActivity ;;
  logs)    "$ADB" logcat --pid="$("$ADB" shell pidof dev.osmo.app)" ;;
  *) echo "usage: $0 build|test|install|logs"; exit 2 ;;
esac
