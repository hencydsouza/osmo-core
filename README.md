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
osmo get <path> <path> ...      # several files in one go: completed ones are skipped, a failure doesn't stop the rest
osmo ls --all                 # every file, paged (--page-size N to force small pages)
osmo ls --photos|--videos      # only photos or only videos; --starred for favourites only (combine with --all)
osmo thumb <path> -o t.jpg    # the camera's thumbnail
osmo marks <video-path>        # highlight marks (moments tagged while recording), as m:ss.mmm
osmo burst <group-lead-path>   # list burst/interval frames; not yet verified on an Action 4
osmo stream                   # send RTMP from the camera; configured in .env, Ctrl-C requests stop
osmo star <path>              # favourite a file (--off does NOT work on an Action 4)
osmo rm <path>                # dry run: shows what would be deleted
osmo rm <path> --yes          # really delete (cannot be undone)
osmo sync-time                # set the camera's clock and time zone from this computer's
```

`ls` marks numbered burst/interval leads with `[burst]`. `burst` requires a fitted command handle and
reports an error when the reply is incomplete or reaches the 120-frame request limit. Frame handles are
not exposed for deletion or favourite operations. To download an individual expanded frame, use its printed
path with `osmo get --storage 0 <path>` for SD (or `--storage 1` for internal storage).

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

### macOS Wi-Fi helper

Copy `.env.example` to `.env` and fill in the home network name and password locally. The helper wakes the camera
and reads its Wi-Fi password over Bluetooth, joins its Wi-Fi, runs an `osmo` command, and attempts to reconnect to
the home network, including on failure or interruption:

```sh
./target/debug/osmo with-wifi -- -v ls
./target/debug/osmo with-wifi -- -v burst 'DCIM/DJI_001/your-burst-lead.JPG'
```

macOS may hide the current Wi-Fi name from terminal programs. `OSMO_RETURN_TO` in `.env` provides the network to
return to; it can also be supplied explicitly:

```sh
./target/debug/osmo with-wifi --return-to 'Your home Wi-Fi' -- -v ls
# Or set once per shell (add this to ~/.zshrc to keep it):
export OSMO_RETURN_TO='Your home Wi-Fi'
./target/debug/osmo with-wifi -- -v ls
```

The helper reads `.env` from the current working directory; existing environment variables take precedence. `.env`
is gitignored and stores the home password in plaintext, so restrict file access. Passwords are also passed as arguments to
macOS `networksetup`, where other local processes can briefly see them. The helper currently expects the Action 4's
`192.168.2.x` network. If macOS hides network names, it cannot verify the final reconnection; check the Wi-Fi menu
if needed.

### RTMP livestream

Set `OSMO_RTMP_URL` in `.env` to your RTMP server's destination. The camera joins the network named by
`OSMO_RETURN_TO` using `OSMO_HOME_WIFI_PASSWORD`; alternatively set `OSMO_RTMP_SSID` and
`OSMO_RTMP_WIFI_PASSWORD` for a different network. The URL may contain a private stream key; keep `.env` private.
The camera must be able to reach the server on that network.

```sh
./target/debug/osmo stream --resolution 1080p --bitrate 6000
```

Run this command directly, rather than through `with-wifi`. Press Ctrl-C to send the camera's stop command;
check the camera or server to confirm it stopped. Streaming takes the camera off its own hotspot, so media browsing
may require waking the hotspot again. Osmosis has tested the protocol on an Action 4; this Rust implementation has
only been tested with simulated BLE replies. The visual effect of stabilization is unverified.

## Develop

```
cargo test --workspace     # offline: fixtures and fakes, no camera needed
```

Protocol layers are tested against Osmosis's captured manifests and documented frames. Passing tests prove our
side of the protocol, not that a given camera accepts it; hardware runs are logged in `docs/hardware-log.md`.
