# SDK: Swift (macOS)

## Build
```sh
scripts/build-sdk-macos.sh     # dist/macos/{libosmo_ffi.dylib, osmo_ffi.swift, osmo_ffiFFI.h, module.modulemap}
scripts/test-sdk-macos.sh      # builds and runs sdk-tests/swift (needs full Xcode for XCTest)
```
Or download `osmo-sdk-macos.zip` from the GitHub release. Add the `.swift` file, the header/modulemap and link the
dylib (set an rpath or embed it). `sdk-tests/swift/Package.swift` is a working example.

## Use
```swift
let cam = Camera(ip: "192.168.2.1", port: 9004)       // you must already be on the camera's Wi-Fi
let files = try cam.list(filter: ListFilter(kind: nil, starredOnly: false), all: true)

_ = try cam.download(path: files[0].path, destDir: dir, progress: MyProgress())   // resumes partial files
let jpg = try cam.thumbnail(path: files[0].path)       // Data
let r = try cam.trim(path: video.path, fromSecs: 5, toSecs: 15, destPath: out, progress: nil)
let still = try cam.frameClip(path: video.path, atSecs: 8, destPath: out2)         // tiny mp4; grab frame 0 with AVAssetImageGenerator

let doomed = try cam.planDelete(paths: [p])            // what WOULD go
try cam.delete(paths: doomed.map(\.path))              // only delete what the plan returned

try trimLocalFile(srcPath: a, fromSecs: 0.5, toSecs: 1.5, destPath: b, progress: nil)  // no camera needed
```
`ProgressListener.onProgress(done:total:) -> Bool`: return `false` to cancel. `cam.cancel()` also cancels from another thread.

## BLE (your app's job)
```swift
final class Link: BleLink {
    func writeCommand(bytes: Data) throws { /* write to the camera's GATT characteristic */ }
    func recv(timeoutMs: UInt32) throws -> Data? { /* next notification or nil on timeout */ }
}
let creds = try readWifiCredentials(link: Link())     // ssid + password
```
Then join that Wi-Fi (NEHotspotConfiguration / system APIs) before using `Camera`.

## Errors
`OsmoError` cases: `Session, NotFound, Refused, Io, Mp4, Cancelled, Ble, Internal`, each with `detail`. Bad
arguments (negative/NaN times) are `Refused`, never a crash; a Rust panic is `Internal`.

Calls block: use a background queue/Task. Live-camera test: `OSMO_LIVE=1 swift test`.
