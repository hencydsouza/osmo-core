# osmo SDK

Native library (Rust, via UniFFI) for apps that talk to a DJI Osmo Action 4 over its own Wi-Fi: list, thumbnail,
download, favourite, delete and lossless trim/frame. Same code as the `osmo` CLI.

## Artifacts (`scripts/build-sdk-*.sh`, or the `sdk` workflow)
| Platform | Output | Size |
|---|---|---|
| macOS | `dist/macos/libosmo_ffi.dylib` + `osmo_ffi.swift` + header/modulemap | ~1-2 MB |
| Android | `dist/android/jniLibs/{arm64-v8a,armeabi-v7a,x86_64,x86}/libosmo_ffi.so` + `kotlin/uniffi/osmo_ffi/osmo_ffi.kt` (needs JNA aar) | ~1.3-1.9 MB per ABI |
| Windows | `osmo_ffi.dll` + C# bindings (uniffi-bindgen-cs), built by CI only | - |

## What the app must do
- **BLE**: implement `BleLink` (`writeCommand`, `recv`) over the platform's GATT stack and pass it to
  `readWifiCredentials`; the SDK does the pairing protocol. Connect, enable notifications on fff4/fff5 first.
- **Wi-Fi**: join the camera hotspot with the returned SSID/password and bind traffic to that network
  (Android: `ConnectivityManager.bindProcessToNetwork`; macOS: join via system APIs). The camera has no internet.
- **Threading**: all calls block; run them off the UI thread. `Camera.cancel()` and a `ProgressListener` returning
  `false` stop a transfer/trim. A `Camera` is safe to share, calls run one at a time per camera.
- **Errors**: `OsmoError`/`OsmoException` variants carry a `detail` string. Panics become `Internal`.

## API tour
Swift: `let cam = Camera(ip: "192.168.2.1", port: 9004); let files = try cam.list(filter: ListFilter(kind: nil, starredOnly: false), all: false)`
Kotlin: `val cam = Camera("192.168.2.1", 9004u); cam.trim(path, 1.0, 3.0, dest, null)`
Also: `trimLocalFile` (no camera), `frameClip`, `planDelete` then `delete` (delete only what the plan returned).

## Not included
Livestream, GPS sync, burst, marks, time sync, other camera models (only Action 4 verified live), drones.
Windows is built by CI and not verified locally.
