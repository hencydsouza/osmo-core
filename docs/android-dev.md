# Android app: dev environment

App: `osmo-android/` (Kotlin, Compose, M3 Expressive, Navigation 3). Native SDK via `:sdk`.

## One-time on the Mac
- Android Studio (its bundled JBR is the JDK), SDK platform 37 (Gradle auto-installs it), `adb` in `~/Library/Android/sdk/platform-tools`.
- `rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android` and `cargo install cargo-ndk --locked`.

## One-time on the phone
1. Settings > About phone > tap **Build number** 7 times (Developer options appear).
2. Settings > System > Developer options: turn on **USB debugging**.
3. Plug in via USB, tap **Allow** on the "Allow USB debugging?" prompt. `adb devices` must show it as `device`.

### Wireless (optional, Android 11+)
Developer options > **Wireless debugging** on (same Wi-Fi as the Mac) > **Pair device with pairing code**:
```sh
adb pair <ip>:<pairing-port>     # enter the 6-digit code
adb connect <ip>:<port>          # the port on the main Wireless debugging screen
```
Caveat: the app later joins the *camera's* Wi-Fi, which drops wireless debugging. Use USB for camera work.

## Daily loop
```sh
scripts/android-app.sh install   # builds Rust .so for 4 ABIs (first time), builds APK, installs, launches
scripts/android-app.sh logs
scripts/android-app.sh test      # JVM unit tests + instrumented tests on the connected device/emulator
```
With a phone and the emulator both attached, pass `ANDROID_SERIAL=<serial>` (see `adb devices`).
Emulator AVD for CI-like runs: `osmo_test` (arm64, API 37).

## Notes
- The release build is minified; keep rules for `uniffi.**` and `com.sun.jna.**` (already in `proguard-rules.pro`).
- Libraries use 16 KB page alignment (required on newer devices).
- Rebuild the SDK after Rust changes: `scripts/build-sdk-android.sh`.
