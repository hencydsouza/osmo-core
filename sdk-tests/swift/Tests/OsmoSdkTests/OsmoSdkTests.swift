import AVFoundation
import XCTest
@testable import OsmoSdk

private let fixtures = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .deletingLastPathComponent().appendingPathComponent("fixtures")

private func tmp(_ name: String) -> String {
    let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir.appendingPathComponent(name).path
}

private final class MockLink: BleLink {
    var written: [Data] = []
    func writeCommand(bytes: Data) throws { written.append(bytes); throw OsmoError.Ble(detail: "device disconnected") }
    func recv(timeoutMs: UInt32) throws -> Data? { nil }
}

private final class Stopper: ProgressListener {
    var calls = 0
    func onProgress(done: UInt64, total: UInt64) -> Bool { calls += 1; return false }
}

final class OsmoSdkTests: XCTestCase {
    func fixture(_ name: String) -> String { fixtures.appendingPathComponent(name).path }

    func testVersion() { XCTAssertFalse(sdkVersion().isEmpty) }

    func testTrimFixtureProducesPlayableClip() async throws {
        for name in ["h264_aac.mp4", "hevc_aac.mp4", "faststart.mp4"] {
            let out = tmp("clip.mp4")
            let r = try trimLocalFile(srcPath: fixture(name), fromSecs: 0.5, toSecs: 1.5, destPath: out, progress: nil)
            XCTAssertGreaterThan(r.bytes, 0)
            let asset = AVURLAsset(url: URL(fileURLWithPath: out))
            let playable = try await asset.load(.isPlayable)
            XCTAssertTrue(playable, name)
            let secs = try await asset.load(.duration).seconds
            XCTAssertEqual(secs, r.durationSecs, accuracy: 0.2, name)
        }
    }

    func testFrameClipThenAVAssetImageGeneratorReturnsAnImage() async throws {
        let out = tmp("frame.mp4")
        _ = try trimLocalFile(srcPath: fixture("h264_aac.mp4"), fromSecs: 1.0, toSecs: 1.5, destPath: out, progress: nil)
        let gen = AVAssetImageGenerator(asset: AVURLAsset(url: URL(fileURLWithPath: out)))
        gen.appliesPreferredTrackTransform = true
        let (image, _) = try await gen.image(at: .zero)
        XCTAssertGreaterThan(image.width, 0)
    }

    func testBadRangeThrowsRefusedNotCrash() {
        for (from, to) in [(-1.0, 1.0), (Double.nan, 1.0), (0.0, Double.infinity)] {
            XCTAssertThrowsError(try trimLocalFile(srcPath: fixture("h264_aac.mp4"), fromSecs: from, toSecs: to, destPath: tmp("x.mp4"), progress: nil)) {
                guard case OsmoError.Refused = $0 else { return XCTFail("\($0)") }
            }
        }
        XCTAssertThrowsError(try trimLocalFile(srcPath: fixture("h264_aac.mp4"), fromSecs: 5, toSecs: 1, destPath: tmp("x.mp4"), progress: nil)) {
            guard case OsmoError.Mp4 = $0 else { return XCTFail("\($0)") }
        }
        let cam = Camera(ip: "127.0.0.1", port: 9)
        XCTAssertThrowsError(try cam.trim(path: "DCIM/x.MP4", fromSecs: -1, toSecs: 1, destPath: tmp("y.mp4"), progress: nil))
    }

    func testMockBleLinkRoundTrip() {
        let link = MockLink()
        XCTAssertThrowsError(try readWifiCredentials(link: link)) {
            guard case OsmoError.Ble = $0 else { return XCTFail("\($0)") }
        }
        XCTAssertFalse(link.written.isEmpty, "the SDK wrote its first frame through the app's link")
    }

    func testProgressListenerCanCancel() {
        let stop = Stopper()
        XCTAssertThrowsError(try trimLocalFile(srcPath: fixture("h264_aac.mp4"), fromSecs: 0, toSecs: 1.5, destPath: tmp("c.mp4"), progress: stop)) {
            guard case OsmoError.Cancelled = $0 else { return XCTFail("\($0)") }
        }
        XCTAssertGreaterThan(stop.calls, 0)
    }

    func testErrorsAreTypedEnums() {
        XCTAssertThrowsError(try trimLocalFile(srcPath: "/nonexistent.mp4", fromSecs: 0, toSecs: 1, destPath: tmp("z.mp4"), progress: nil)) {
            guard case OsmoError.Io = $0 else { return XCTFail("\($0)") }
        }
        let cam = Camera(ip: "127.0.0.1", port: 9)
        XCTAssertThrowsError(try cam.download(path: "../../etc/passwd", destDir: tmp("d"), progress: nil)) {
            guard case OsmoError.NotFound = $0 else { return XCTFail("\($0)") }
        }
    }

    /// Needs the Action 4 and its Wi-Fi: OSMO_LIVE=1 swift test.
    func testLiveListing() throws {
        try XCTSkipUnless(ProcessInfo.processInfo.environment["OSMO_LIVE"] == "1")
        let cam = Camera(ip: "192.168.2.1", port: 9004)
        let files = try cam.list(filter: ListFilter(kind: nil, starredOnly: false), all: false)
        XCTAssertFalse(files.isEmpty)
    }
}
