# SDK: Android (Kotlin)

## Build
```sh
cargo install cargo-ndk --locked
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
scripts/build-sdk-android.sh   # dist/android/jniLibs/<abi>/libosmo_ffi.so + kotlin/uniffi/osmo_ffi/osmo_ffi.kt
scripts/test-sdk-android.sh    # instrumented tests on a running emulator/device (JDK 17-21; Android Studio's jbr works)
```
Or take `osmo-sdk-android.zip` from the release. Libraries are built with 16 KB page alignment. Sizes: 1.3-1.9 MB per ABI.

## Add to your app
- Copy `jniLibs/` into `app/src/main/jniLibs/` and `kotlin/` into a source dir (see `sdk-tests/android/sdk/build.gradle.kts`).
- Dependency: `implementation("net.java.dev.jna:jna:5.14.0@aar")`.

## Use
```kotlin
import uniffi.osmo_ffi.*

val cam = Camera("192.168.2.1", 9004u.toUShort())
val files = cam.list(ListFilter(null, false), true)
cam.download(files[0].path, cacheDir.path, object : ProgressListener {
    override fun onProgress(done: ULong, total: ULong) = !cancelled   // false cancels
})
val r = cam.trim(video.path, 5.0, 15.0, outPath, null)
val plan = cam.planDelete(listOf(p)); cam.delete(plan.map { it.path })
trimLocalFile(src, 0.5, 1.5, dst, null)        // no camera
```

## Wi-Fi and BLE (your app's job)
- BLE: implement `BleLink` (`writeCommand`, `recv`) over `BluetoothGatt`, pass to `readWifiCredentials(link)`.
- Wi-Fi: connect with `WifiNetworkSpecifier` (`ConnectivityManager.requestNetwork`) and call
  `bindProcessToNetwork(network)` so sockets use the camera's network, which has no internet.
- Run every call off the main thread. Catch `OsmoException` (`Session, NotFound, Refused, Io, Mp4, Cancelled, Ble, Internal`; each has `detail`).
