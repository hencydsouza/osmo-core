# osmo-core

Platform-agnostic Rust core for talking to DJI Osmo cameras without the DJI SDK: BLE pairing, the camera's
WiFi AP datalink (media list), and resumable HTTP download. Plus an `osmo` CLI that proves it on real hardware.

Status: prototype. Verified on an **Osmo Action 4** only. See `docs/hardware-log.md` for what is live-verified
versus fixture-tested.

Protocol knowledge and test fixtures are ported from [Osmosis](https://github.com/KonradIT/osmosis) (MIT);
see `NOTICE` and `docs/protocol.md`.

## Layout

- `osmo-core/` - the library. No async runtime and no OS types in its public API; BLE and UDP sit behind the
  small `BleLink` and `Datagram` traits, so Android/Swift/desktop shells can plug in their own.
- `osmo-cli/` - the `osmo` binary (btleplug for desktop Bluetooth).

Seams today: BLE is behind `BleLink` and the UDP datalink behind `Datagram`. HTTP (`ureq`) and the TCP poke use
the standard library directly, and joining the camera's WiFi is manual, so a non-desktop shell would need to
wrap or replace `http.rs`. Swappable HTTP and network-join traits are planned for the first such consumer.

## Use

```
cargo build --release
osmo scan                     # find cameras over Bluetooth
osmo pair                     # pair; approve on the camera screen
osmo wifi                     # print the camera's WiFi name and password
# join that WiFi on this machine (the camera has no internet)
osmo ls                       # list the newest media (needs the camera's WiFi)
osmo get DCIM/100MEDIA/DJI_20260101_0001_D.MP4 -o ~/Movies/osmo
```

`get` takes the path exactly as `ls` prints it and resumes a partial file (Ctrl-C leaves a resumable partial
file; the camera leaves playback mode on its own a few seconds later). It holds the datalink open while
downloading as a precaution: Osmosis documents that the camera puts its WiFi to sleep when idle, and on the
Action 4 the WiFi was seen dropping during long, interrupted downloads that held no datalink. Whether holding it
fully prevents that on a multi-GB file is not yet confirmed (`--no-keepalive` turns it off, and `--storage` skips
the datalink entirely). If the WiFi drops, run `osmo wifi`, rejoin, and re-run the same `get`: it resumes.
`-v` logs every download attempt.

macOS: your terminal needs Bluetooth permission (System Settings > Privacy & Security > Bluetooth).

## Develop

```
cargo test --workspace     # offline: fixtures and fakes, no camera needed
```

Protocol layers are tested against Osmosis's captured manifests and documented frames. Passing tests prove our
side of the protocol, not that a given camera accepts it; hardware runs are logged in `docs/hardware-log.md`.
