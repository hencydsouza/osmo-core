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
osmo ls --all                 # every file, paged (--page-size N to force small pages)
osmo thumb <path> -o t.jpg    # the camera's thumbnail
osmo star <path>              # favourite a file (--off does NOT work on an Action 4)
osmo rm <path>                # dry run: shows what would be deleted
osmo rm <path> --yes          # really delete (cannot be undone)
osmo sync-time                # set the camera's clock and time zone from this computer's
```

`rm` deletes only exact paths from the newest listing, all or nothing: a typo, glob, or a file without a verified
handle refuses the whole command and nothing is sent. It confirms on a fresh session that the files are gone
and reports an error if the camera said OK but they are still listed.

`get` takes the path exactly as `ls` prints it and resumes a partial file (Ctrl-C leaves a resumable partial
file; the camera leaves playback mode on its own a few seconds later). It holds the datalink open while
downloading: the camera puts its WiFi to sleep when idle, and on the Action 4 the WiFi dropped during long,
interrupted downloads that held no datalink. With the datalink held, a 5.2 GiB download finished in one attempt
with no drop (`--no-keepalive` turns it off, and `--storage` skips the datalink entirely). If the WiFi drops, run `osmo wifi`, rejoin, and re-run the same `get`: it resumes.
`-v` logs every download attempt.

macOS: your terminal needs Bluetooth permission (System Settings > Privacy & Security > Bluetooth).

## Develop

```
cargo test --workspace     # offline: fixtures and fakes, no camera needed
```

Protocol layers are tested against Osmosis's captured manifests and documented frames. Passing tests prove our
side of the protocol, not that a given camera accepts it; hardware runs are logged in `docs/hardware-log.md`.
