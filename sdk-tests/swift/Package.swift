// swift-tools-version:5.9
import PackageDescription

// Sources/ is filled by scripts/test-sdk-macos.sh from dist/macos (generated bindings + header).
let dist = Context.packageDirectory + "/../../dist/macos"

let package = Package(
    name: "OsmoSdk",
    platforms: [.macOS(.v12)],
    products: [.library(name: "OsmoSdk", targets: ["OsmoSdk"])],
    targets: [
        .target(name: "osmo_ffiFFI", path: "Sources/osmo_ffiFFI"),
        .target(
            name: "OsmoSdk",
            dependencies: ["osmo_ffiFFI"],
            path: "Sources/OsmoSdk",
            linkerSettings: [
                .unsafeFlags(["-L\(dist)", "-losmo_ffi", "-Xlinker", "-rpath", "-Xlinker", dist])
            ]
        ),
        .testTarget(name: "OsmoSdkTests", dependencies: ["OsmoSdk"], path: "Tests/OsmoSdkTests"),
    ]
)
