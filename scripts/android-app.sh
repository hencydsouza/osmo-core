#!/usr/bin/env bash
# Build the Rust SDK for Android, then build / test / install the app.
#
#   scripts/android-app.sh build        debug APK (foreground)
#   scripts/android-app.sh bg [task]    gradle task in background, log in $LOG (default :app:assembleDebug)
#   scripts/android-app.sh status       one-line state of the background build + last log lines
#   scripts/android-app.sh watch        follow progress until BUILD SUCCESSFUL/FAILED, then print errors
#   scripts/android-app.sh errors       compile errors / failure causes from the log
#   scripts/android-app.sh retry        background build with --refresh-dependencies (cached download failure)
#   scripts/android-app.sh test         unit tests + instrumented tests on the selected device
#   scripts/android-app.sh install      build + install + launch on the selected device
#   scripts/android-app.sh logs         follow app log
#   scripts/android-app.sh devices      list adb devices
#   scripts/android-app.sh pair HOST:PORT CODE   wireless-debugging pairing
#   scripts/android-app.sh connect      connect to the phone by its mDNS name
#   scripts/android-app.sh stop         kill leftover gradle clients/daemons
#   scripts/android-app.sh clean        gradle clean
#
# Device: ANDROID_SERIAL wins; else the only physical device; else the only device.
set -euo pipefail
cd "$(dirname "$0")/.."
export JAVA_HOME="${JAVA_HOME:-/Applications/Android Studio.app/Contents/jbr/Contents/Home}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
ADB="$ANDROID_HOME/platform-tools/adb"
LOG="${LOG:-/tmp/osmo-build.log}"
PIDFILE="${PIDFILE:-/tmp/osmo-build.pid}"
GRADLE_OPTS_EXTRA=(--console=plain -Dorg.gradle.internal.http.socketTimeout=300000 -Dorg.gradle.internal.http.connectionTimeout=60000)

sdk() { [ -d dist/android/jniLibs ] || scripts/build-sdk-android.sh; }
gw() { (cd osmo-android && { [ -f local.properties ] || echo "sdk.dir=$ANDROID_HOME" > local.properties; } && ./gradlew "${GRADLE_OPTS_EXTRA[@]}" "$@"); }

pick_device() {
  if [ -n "${ANDROID_SERIAL:-}" ]; then return; fi
  local all phys
  all=$("$ADB" devices | awk 'NR>1 && $2=="device"{print $1}')
  phys=$(echo "$all" | grep -v '^emulator-' || true)
  if [ "$(echo "$phys" | grep -c .)" = 1 ]; then export ANDROID_SERIAL="$phys"
  elif [ "$(echo "$all" | grep -c .)" = 1 ]; then export ANDROID_SERIAL="$all"
  else echo "pick a device: ANDROID_SERIAL=<id> $0 $*" >&2; "$ADB" devices >&2; exit 1; fi
  echo "device: $ANDROID_SERIAL" >&2
}

running() { [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; }

bg() {
  running && { echo "build already running (pid $(cat "$PIDFILE")); use status/watch"; exit 1; }
  sdk
  : > "$LOG"
  (cd osmo-android && { [ -f local.properties ] || echo "sdk.dir=$ANDROID_HOME" > local.properties; }
   nohup ./gradlew "${GRADLE_OPTS_EXTRA[@]}" "$@" > "$LOG" 2>&1 & echo $! > "$PIDFILE")
  echo "started pid $(cat "$PIDFILE"), log $LOG"
}

errors() {
  grep -E "^e: |^w: .*deprecat|What went wrong|Could not download|Caused by|FAILED|BUILD" -A3 "$LOG" | head -60 | cut -c1-240 || true
}

case "${1:-build}" in
  build)   sdk; gw :app:assembleDebug ;;
  bg)      shift; bg "${@:-:app:assembleDebug}" ;;
  retry)   bg :app:assembleDebug --refresh-dependencies ;;
  status)
    if running; then echo "RUNNING pid $(cat "$PIDFILE")"
    elif grep -q "BUILD SUCCESSFUL" "$LOG" 2>/dev/null; then echo "SUCCESS"
    elif grep -q "BUILD FAILED" "$LOG" 2>/dev/null; then echo "FAILED (scripts/android-app.sh errors)"
    else echo "IDLE"; fi
    tail -n 5 "$LOG" 2>/dev/null | cut -c1-160 ;;
  watch)
    until grep -qE "BUILD (SUCCESSFUL|FAILED)" "$LOG" 2>/dev/null || ! running; do
      printf '\r%s' "$(grep -E '^> Task ' "$LOG" | tail -1 | cut -c1-100)"; sleep 3
    done; echo; errors ;;
  errors)  errors ;;
  test)    pick_device "$@"; sdk; gw :app:testDebugUnitTest :app:connectedDebugAndroidTest ;;
  install) pick_device "$@"; sdk; gw :app:assembleDebug
           "$ADB" install -r osmo-android/app/build/outputs/apk/debug/app-debug.apk
           "$ADB" shell am start -n dev.osmo.app/.MainActivity ;;
  logs)    pick_device "$@"; "$ADB" logcat --pid="$("$ADB" shell pidof dev.osmo.app)" ;;
  devices) "$ADB" devices -l ;;
  pair)    "$ADB" pair "${2:?HOST:PORT}" "${3:?pairing code}" ;;
  connect) name=$("$ADB" mdns services | awk '/_adb-tls-connect/{print $1; exit}')
           [ -n "$name" ] || { echo "no wireless-debugging service found; enable Wireless debugging on the phone"; exit 1; }
           echo "$name"; "$ADB" -s "$name" get-state ;;
  stop)    pkill -f "gradlew" 2>/dev/null || true; (cd osmo-android && ./gradlew --stop) >/dev/null 2>&1 || true; rm -f "$PIDFILE"; echo stopped ;;
  clean)   gw clean ;;
  *) sed -n '2,20p' "$0"; exit 2 ;;
esac
