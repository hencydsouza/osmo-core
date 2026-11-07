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

## HW-2: sizes, a 4.1 GiB download, and the hotspot dropping (2026-11-03)

- **HTTP size fallback: live-verified.** The four videos with a manifest size of 0 now list 10.7, 5.2, 4.1 and
  6.9 GiB.
- **4.1 GiB video: live-verified** (completed over several resumes: interrupted by Ctrl-C at 23%, then
  stalled at 58% and 90%).
- **Finding:** each stall was followed by the camera's WiFi disappearing; `osmo wifi` and a rejoin were needed
  each time. This run used the old default (datalink closed after the lookup). Likely cause: the camera cuts a
  long transfer periodically and then answers "busy" for a few retries, and those quiet gaps exceed the AP's
  ~10 s idle limit. Osmosis holds the AP up with a datalink heartbeat for the whole transfer.
- **Change:** `get` now holds the datalink open by default (`--no-keepalive` to disable), logs each download
  attempt with `-v`, and a stalled download now says how to wake the camera and resume.
  Not yet re-verified live.

## Keepalive and AP wake re-verified (2026-11-03) - live-verified

With the datalink held open (the new default), a 5.2 GiB video downloaded in a single attempt
(`attempt 1 from 0: Done`) with no stall and no hotspot drop; the result is a valid MP4 of exactly the
manifest size. The `0x53/0x10` wake also brought the AP up on every `osmo wifi` run since it was added
(about five, plus the earlier 2.0 GiB run). Not yet tested: a card with no files, Bluetooth off, and a
multi-attempt download *with* keepalive (no cuts happened in either keepalive run).

## HW-A: paging and live status (2026-11-03) - live-verified

- `osmo ls` printed the same 14 files plus a status line (`battery 33%  SD 87.0 GiB free of 119.1 GiB`). The
  owner has not yet compared the numbers against the camera's own screen.
- `osmo -v ls --all --page-size 5` returned the same 14 files in the same order, with no truncation warning, over
  four page queries returning 5, 4, 4 and 1 new files. The cursor is inclusive, so each later page repeats the
  boundary file; the dedupe by (store, path) handled it. Paging worked inline on the live session, so no
  fresh-session fallback was needed.
- Finding: on the last page only one record was a video, so the list could not be fitted and the cursor did not move.
  A page of only photos would have stopped `ls --all` silently at the first page. Fixed test-first: paging cursors
  fall back to a still's own handle candidate (paging only reads; delete still requires the verified handle).
- Not covered live: internal-store paging (the Action 4 has none), a genuinely cut-short page (fixture-tested
  only), and a card with more than one default-size page (this card has 14 files).

## HW-B: thumbnails and sidecars (2026-11-03) - live-verified

- `osmo thumb` on a photo and on a video: both came from the camera's `.scr` file (`MISC/THM/...scr`), as JPEG,
  about 530 to 560 KiB each. Opened as real 960x720 images (a frame of the scene, not a placeholder). The `.thm`
  and embedded-EXIF fallbacks were not needed on this camera, so they are fixture-tested only.
- `osmo get --sidecars` on a photo: `no sidecar`, the expected answer for a card that does not shoot JPEG+DNG; the
  HEAD for the missing `.DNG` came back as a clean 404. A real DNG/WAV sidecar download is not yet verified live.
- Not covered live: locating a file older than the newest page (this card has 14 files, so every file is on page 1).

## HW-C: favourite on and off (2026-11-03) - partly live-verified, one bug found

- Star on: the camera accepted it, a fresh listing confirmed it, and `ls` showed the star. Live-verified.
- A run before the owner had joined the camera's WiFi failed cleanly with the route warning and the "join the
  camera's WiFi" message. Live-verified error path.
- Star off: acknowledged (status 0) twice, in separate runs, yet the star never cleared in any later listing.
- Cause: each `osmo` run started its write counter at 1, and the camera treats a repeated counter as a repeat
  (acknowledge, do nothing); Osmosis's own tests note the counter "has to advance". The same bug would have made
  every delete after the first silently do nothing. Fixed test-first: one shared write counter, persisted by the
  CLI and seeded from the clock so it cannot collide with counters used earlier. Not yet re-verified live.

## HW-C follow-up (2026-11-03): un-favourite ignored, counter theory not confirmed

- Star ON works on a fresh file (0009) with a large clock-seeded counter; a fresh listing and `ls` show the star.
- Star OFF is acknowledged (status 0) but the camera keeps the heart, confirmed on the camera's own screen.
  It failed with counter 1 (separate process) and with the clock-seeded counter, so the counter is not the cause.
- Osmosis restarts counters at 1 per session and only unit-tests the off payload; un-favourite was never hardware-verified.
- Open: the correct un-favourite encoding for the Action 4. Delete not started.

## HW-D: delete (2026-11-03) - delete live-verified, confirmation step fixed after

- Dry run on a throwaway photo printed `would delete:` and `Nothing was deleted`; a following `ls` still listed it.
- A typo path was refused by name (`not in the camera's newest listing`) and nothing was sent.
- `rm --yes`: the camera answered OK (status 0) and the photo was gone from the next `osmo ls` (14 files; owner's
  photo count back to 14). Live-verified.
- The in-command check failed: it re-listed on the SAME session, the camera answered on a different channel and
  returned 0 files, so `rm` reported "cannot be confirmed" although the delete worked. Fixed: the check now runs on a
  fresh session, like `star`. Not yet re-verified live.

## Time sync on the Action 4 (2026-11-03): camera refuses it

- `osmo -v sync-time` registered fine, then the camera answered `0x00/0x6A` with status `0x00e0` (refused), the same code it gives `0x02/0x0C`.
- Osmosis verified this command on a Nano only, and ignores the reply, so it would never have noticed a refusal.
- Our registration steps and payload bytes match Osmosis's. Whether the camera clock changed anyway is not yet confirmed.

## Highlight marks on the Action 4 (2026-11-04): camera answers e3

- `osmo -v marks` on `DJI_20261101084646_0008_D.MP4` (handle 0x00040080): the camera replied status `0xe3`.
- Osmosis's status table describes `e3` as "supported, bad/missing parameter" (not `e0`, "not supported"). The request was rejected; the cause, including whether this command works on Action 4, remains unknown.
- `osmo ls --highlights` sends list query byte 37 = 2, intended to filter for marked clips.

- `osmo -v ls --highlights` returned an empty list with an end frame. The filter has not been checked with a known marked clip, so this does not prove the card has no marks or explain `e3`. A known tagged clip is needed to test both filter and decoder.

- macOS Wi-Fi helper: initial attempts without passwords returned "Could not find network" or `-3900` for the Action 4 hotspot and the home network, despite both SSIDs being preferred. The cause is unconfirmed.

## macOS Wi-Fi helper on the Action 4 (2026-11-04)

- `osmo with-wifi -- -v ls` woke the camera over BLE, joined its hotspot, listed 14 files, and requested reconnection to the home network. It retrieved the camera password via BLE and used the locally configured home password.
- macOS hid the current SSID, so the helper could not verify reconnection by name. Afterward, the Wi-Fi interface's IP address and default gateway matched those observed before switching, consistent with returning to the home network but not conclusive proof of its SSID.
- No listed file had a burst suffix. Burst expansion still requires a live test with a newly recorded burst.

## Live checks of the 4 Nov features (2026-11-07), Action 4, camera battery 20%

- `with-wifi -- -v ls`: woke, paired, joined the camera Wi-Fi, listed 14 files, rejoined home Wi-Fi (one join attempt failed, the retry worked; internet confirmed by ping). Live-verified. The helper's "reconnect OK" is NOT a real check on this Mac: macOS hides the SSID, so `verify_restore` returns OK unverified.
- `with-wifi -- burst <normal photo>`: refused cleanly ("requires a numbered group lead"), nothing sent. A real burst is untested: the card has none.
- `cameras` / `cameras --scan`: remembered camera listed as `last`; scan marks it `nearby`. Selection among several cameras untested (only one exists).
- `gps` with fake fixes: the camera rejected the R-SDK connection ("camera rejected the R-SDK connection"). Unknown whether the approval prompt was missed or the camera refuses. Untested beyond that.
- `stream` to a local ffmpeg RTMP receiver: first run failed before touching the camera because `ctrlc::try_set_handler` refuses when SIGINT is ignored (background job); fixed with `set_handler`. Second run: pair, prepare, Wi-Fi join, stabilization, configure were accepted, then "no camera reply to stream start"; the receiver saw no connection. Not working yet; cause unknown.

## Follow-up (2026-11-07): GPS and stream

- Owner reports no approval prompt appeared on the camera during `gps`. The R-SDK rejection message now names the path (`response code N` or `verify data N`) so the next run shows which one fired.
- Owner reports the camera stays on "preparing to livestream" after the failed `stream` run, and it no longer advertises over Bluetooth (`no matching camera found`). It needs a power cycle before retesting.
- `stream -v` now prints each frame the camera sends (`rx set/cmd flags payload`, battery pushes excluded) so a stalled start shows where the camera stops answering. Credentials never appear in these lines.

## Stream and GPS on the restarted camera (2026-11-07)

- `gps` with 60 s of fake fixes: connected (the camera approved R-SDK), fixes sent, clean disconnect. The camera's side (whether it used the fixes) was not observable. Earlier rejection was while the camera was stuck after the failed stream run.
- `stream` (home network "Mohammad Huzair-4G/5G", local ffmpeg RTMP receiver on the Mac): pair, prepare (`02/e1`=00), Wi-Fi join (`07/47`=0000), stabilization (`02/8e`=00) and configure (`08/78`=00) are all accepted, then every start (`02/8e`) is answered `0xD6`, over 8 tries about 4 s apart. The receiver never saw a connection. Reproduced on a freshly restarted camera.
- Undocumented in Osmosis: `0xD6` on start, and the `ee/03` push whose bytes change around the Wi-Fi step (`03 71...` -> `03 79...` -> `02 79...` -> `02 29...` -> `02 79...`).
- Found while testing: the start status was never checked (any reply counted as started) - fixed; `ctrlc::try_set_handler` failed when SIGINT was ignored - fixed; the 6 s scan missed the camera often - now retried up to 3 times; the first rx trace filled its 200-line cap with repeated pushes - now logs changes only.
- Open: whether the camera really joins the home network (the combined-band name contains `/`), or `D6` means something else. Next test: a plain 2.4 GHz phone-hotspot name via OSMO_RTMP_SSID / OSMO_RTMP_WIFI_PASSWORD.
