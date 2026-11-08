# osmo SDK design (2026-11-08)

Owner decisions (from the conversation): one package that Android, macOS and Windows apps can use; **no ffmpeg inside it**;
first build targets macOS + Android; Windows is built by CI but cannot be run here; test rigorously.

## What the SDK is

A native library, `osmo_ffi` (`libosmo_ffi.dylib` / `.so` / `.dll`), plus generated bindings (Swift, Kotlin, C#).
Everything is blocking Rust: the app calls from a background thread. The SDK owns the camera protocol, the datalink
over UDP, HTTP downloads and MP4 trimming. The **app owns**: Bluetooth (each OS has its own stack), joining the camera's
Wi-Fi (Android: `bindProcessToNetwork`), saving settings, and UI.

## Layers

| Crate | Role | Depends on |
|---|---|---|
| `osmo-core` | protocol + MP4 trim (pure Rust, no I/O policy) | thiserror, rand, ureq (no TLS) |
| `osmo-sdk` | high-level `Camera` API: list, thumbnail, download, star, delete, trim; listing/planning logic | osmo-core |
| `osmo-ffi` | UniFFI wrapper: exported objects, errors, callback interfaces (`BleLink`, progress) | osmo-sdk |
| `osmo-cli` | the command-line tool; uses `osmo-sdk`; keeps btleplug, clap, tokio | osmo-sdk |

`osmo-sdk` and `osmo-ffi` must not depend on btleplug, tokio, clap or any process spawn.

## ffmpeg replacement

- **Trim:** pure-Rust MP4 stream copy. Read `moov` (at the end of DJI files) by range, pick the video keyframe at or before
  the start, cut every track at the matching time, rewrite the sample tables, copy the sample bytes. Output is a normal MP4.
- **Frame grab:** the SDK produces a tiny clip that starts on a keyframe (`frame_clip`); the app extracts the still with
  its own platform decoder (`MediaMetadataRetriever`, `AVAssetImageGenerator`, Media Foundation). The result reports the
  clip's real start time so the app seeks to `at - actual_start`.
- The CLI keeps `osmo frame` on ffmpeg (desktop tool); `osmo trim` moves to the pure-Rust path.

## Platform notes

- Android native sockets are not subject to the Java cleartext-HTTP policy, so HTTP to `192.168.2.1` works from the `.so`.
- Callback interface `BleLink { write_command(bytes), recv(timeout_ms) }` is implemented by the app over its own BLE.
- A panic must never unwind across the FFI boundary: every exported call returns an error instead.
