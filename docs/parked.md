# Parked work

Everything on the Osmosis parity list is built. These items are parked: built but not proven, or blocked on
something only the owner or a capture can supply. State as of 2026-11-08, tested on one DJI Osmo Action 4.
Evidence lives in `docs/hardware-log.md`; the decision trail in `.superpowers/sdd/2026-11-03-osmosis-parity/progress.md`.

## Not working on the Action 4

| Item | What happens | Why parked | What would restart it |
|---|---|---|---|
| **Un-favourite** (`star --off`) | The camera answers OK and keeps the star. Confirmed on the camera screen. | Our frame matches Osmosis's documented one byte for byte (`on=00`). Star-on works. Six payload variants on 2026-11-08 were all acknowledged and ignored, so the camera acks any content and blind guessing cannot find the right frame. Osmosis never verified un-favourite on an Action 4. | A capture of what DJI Mimo sends when it un-stars a photo on an Action 4 (Wi-Fi capture on the Mac, decrypted with the camera's SSID and password). |
| **Time sync** (`sync-time`) | The camera refuses with `0x00e0` (not supported). | Our registration, subscriptions, receiver (type `0x08`, id 1) and payload match Osmosis's sender line for line. Osmosis verified it on a Nano only and ignores the reply. The Action 4 gives the same `e0` for the old playback entry, so it likely has a different command. | The same Mimo capture, setting the camera clock from the app. |

## Built, not proven

| Item | State | Why parked | What would restart it |
|---|---|---|---|
| **Highlight marks** (`marks`, `ls --highlights`) | The camera answers `0xe3` ("supported, bad or missing parameter"). `ls --highlights` returns a valid empty list, so no clip on the card has marks. | Cannot tell a bad request from a clip with no marks. The decoder is checked only against the documented layout with synthetic replies. | A clip with real marks: record one and tag moments during it (if the Action 4 allows tagging). |
| **Burst or interval expand** (`burst`) | Code matches Osmosis (same query bytes, 120-frame limit, filters). Correctly refuses an ordinary photo. | The card has no burst or interval group, so expansion has only run against simulated replies. | Shoot a burst or interval photo. |
| **GPS** (`gps`) | The camera approves the R-SDK connection and takes 60 s of fixes from stdin. | There is no location source: fixes must be piped in as JSON lines. Whether the camera applied them is not visible from here. | A Mac or phone location source, and a way to read the position the camera recorded. |
| **Other camera models** | The full Osmosis model table is ported: ports, Xtra rebadges (UDP 10004, no TCP poke), Pocket 3 storage. | Only an Action 4 is available. These are "ported, unverified". | Hardware for each model. |

## Parked by decision

| Item | State | Why parked | What would restart it |
|---|---|---|---|
| **RTMP livestream** (`stream`) | On a plain phone hotspot the camera joins, accepts start, and connects to the receiver. Only about 358 KiB (16 video frames) arrived, then nothing for 2.5 minutes. On the home network `Mohammad Huzair-4G/5G` the start command answers `0xD6` every time (likely the combined-band name with a `/`). | The only receiver tried was `ffmpeg -listen`, a bare-bones RTMP server that may stall publishers, so sustained video was never shown. After any stream attempt the camera stays off its own hotspot and needs a restart. | Phone hotspot with a plain 2.4 GHz name, a camera restart, and `brew install mediamtx` as a proper receiver. Then record and measure the bytes. |
| **Live shooting mode and recording status** | Skipped. | A session that holds playback shows constant values (32 of 32 captured frames identical), and a live check would interrupt a recording. | A status read that works without holding playback. |
| **Drones** (Mavic, Neo, DCF HTTP API) | Not built. | Excluded by the owner. | A decision to include them, and a drone to test with. |

## Known limits in what is built

- `rm` now searches older listing pages (fixed 2026-11-08), but a delete that needs more than about 40 s of walking is refused, not sent. The multi-page walk is tested with simulated pages only: the card has 14 files, so everything fits on one page.
- The final review of the GPS wire code, camera-memory code and Bluetooth session was done 2026-11-08; see `docs/hardware-log.md`.
- `trim` and `frame` need `ffmpeg` installed.
- The Wi-Fi helper only works on macOS.
