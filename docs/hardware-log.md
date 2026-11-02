# Hardware verification log

Device: DJI Osmo Action 4 (BLE model id 0x0014). Host: macOS, CLI run from the agent's shell.
Identifiers, credentials and addresses are deliberately not recorded here.

## HW-1: BLE scan, pair, WiFi credentials (2026-11-03) - live-verified

| Step | Result |
|---|---|
| `osmo scan` | Found the camera by name and DJI manufacturer id; model decoded as `OsmoAction4`; 13 unrelated devices counted, not listed. |
| `osmo pair` (first run) | Wake, SetPairingPIN, on-camera approval, then credentials. Completed. |
| `osmo wifi` (second run) | No approval prompt (persisted identity recognised). SSID returned; password returned (12 chars). |

Surprises: none. The Action 4 returns its WiFi password over BLE.

## HW-2: media list and download (2026-11-03) - partially live-verified

Run by the owner in their own terminal on the camera's WiFi (no keepalive flag).

| Step | Result |
|---|---|
| `osmo ls` | **Live-verified.** Listed 10 files on the SD card (photos and videos, sizes from 6 MiB to 2 GiB) in one run. |
| `osmo get <photo>` | **Live-verified.** Saved a 6,373,376-byte JPEG that opens as the full 3648x2736 frame (no truncation). The byte count was not compared against the camera's own Content-Length. |
| Resume after Ctrl-C | **Live-verified.** Interrupted a 127.8 MiB clip at 34%, re-ran, and it completed. The result is a structurally valid MP4 (`ftyp`/`mdat`/`moov` boxes end exactly at the end of the file). |
| 128 MiB video download | **Live-verified** (via the resume run above), no `--keepalive` needed. A multi-GB file is untested. |
| Wrong WiFi | **Live-verified.** Off the camera's WiFi, `ls` printed the route warning, gave up after the handshake attempts with a clear message, and exited non-zero. |
| Empty card | Not tested live. |

Findings from real data (the captured fixtures never showed these):
1. Four videos listed size `?`: their size was not decoded from the manifest record.
2. One entry (`..._0011_D`) listed with no file extension, so its download URL is likely wrong.
3. The final progress line was stale (`7.8 KiB / 6.1 MiB` before `Saved`). Fixed.

Next: `osmo ls --dump <file>` captures the raw manifest so findings 1 and 2 can be reproduced offline and
fixed test-first.

## Finding after HW-2: the camera's WiFi was missing (2026-11-03)

After the first run, the camera's WiFi no longer appeared although the camera was on. The camera's AP sleeps
when idle and is brought up by the BLE session sequence including `0x53/0x10` (to receiver type 0x1C), which
Osmosis sends after pairing. The Rust pairing session omitted that step (the earlier success most likely
rode on an AP that was already up). Fixed test-first: the pairing order test now expects
wake, pair, keepalive, `0x53/0x10`, then the credential getters. Not yet re-verified live.

## Finding: the listing was non-deterministic (2026-11-03) - fixed, not yet re-verified live

Three `ls` runs in a row returned 10 files, then 6 different ones, then none; `get` lookups sometimes failed
with "not in the newest listing". The dump of the empty run held 249 frames but zero `0x00/0x27` list replies,
and the camera reported "not in playback mode" in all 162 of its status frames.

Cause: Osmosis's `MEDIA_PROTOCOL.md` says the Action 4 answers `e0` (refused) to `0x02/0x0c` and enters
playback only via `0x01/0x01`; while in capture it answers the list with `d8`, nothing, or a partial page.
This port only implemented `0x02/0x0c`. That was a wrong call of mine ("the Action 4 only needs the first
route"), made without checking the Action 4 notes.

Fix (test-first, against a fake that behaves like an Action 4): a refusal now falls through to the
`0x01/0x01` route; `ls` warns when the camera never confirms playback; `-v` logs protocol events; `--dump`
no longer overwrites a good capture with a run that captured no media records.

## HW-2 re-run after the playback fix (2026-11-03) - live-verified

Three `ls -v` runs in a row: identical results every time, 14 files on the SD card, no warning. Log of each:
`playback: 0x02/0x0C refused (0xe0), trying 0x01/0x01` then `held via 0x01/0x01` on a cold camera, and
`already held (the camera says so)` on the following runs; handshake base and channel matched every time.
This confirms the Action 4 needs the `0x01/0x01` route and that the earlier 10/6/0-file runs were the camera
being listed while still in capture. The earlier extension-less entry was a partial record from the same cause.

Ground truth: the manifest sizes of the two downloaded files equal the bytes on disk exactly (a 128 MiB video
and a 6 MiB photo). The captured manifest is saved as a regression fixture (`oa4_live_14.bin`, file names and
sizes only; the raw dump also contains device status frames and is not committed).

Remaining oddity, understood: four videos (316 to 817 s long, the longest on the card) carry a size of 0 in
the manifest, so `ls` and `get` now ask the camera for the size over HTTP (the same fallback Osmosis uses).
That HEAD request is not yet verified live.
