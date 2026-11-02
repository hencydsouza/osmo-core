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
| Resume after Ctrl-C | Not yet tested live. |
| Large video download | Not yet tested live. |
| Empty card, wrong WiFi | Not yet tested live (offline tests only). |

Findings from real data (the captured fixtures never showed these):
1. Four videos listed size `?`: their size was not decoded from the manifest record.
2. One entry (`..._0011_D`) listed with no file extension, so its download URL is likely wrong.
3. The final progress line was stale (`7.8 KiB / 6.1 MiB` before `Saved`). Fixed.

Next: `osmo ls --dump <file>` captures the raw manifest so findings 1 and 2 can be reproduced offline and
fixed test-first.
