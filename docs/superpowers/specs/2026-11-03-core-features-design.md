# osmo-core: core feature pack (browse everything, thumbnails, favourite, delete)

Status: draft for review. Date: 2026-11-03. Builds on `2026-11-03-osmo-core-design.md` (v0.1.0-proto).

## Intent and why

The prototype can pair, list the newest 45 files, and download one. A usable media client also has to browse the
*whole* library, show thumbnails, and let you favourite or delete files. The owner asked for the **core** of the
Osmosis feature set only, not all of it, so this pack stops at those. Everything below is ported from Osmosis
(`MEDIA_PROTOCOL.md`, `camera/CameraSession.kt`, `core/CameraFile.kt`, `core/EmbeddedJpeg.kt`) and verified against
the same hardware as before: one Osmo Action 4 (SD card only, 14 files on it today).

## Goals

1. **Full handle decoding.** Today the manifest decoder reads name, extension, size and store. Paging, favourite and
   delete address a file by a numeric *handle*; videos carry theirs in the record, stills do not and get theirs from
   a per-list fit (`handle = base + seq * step`) that is cross-checked against the bytes in the record. Port that
   (including promotion, revocation and shared-handle flagging) until the golden snapshots' `handle`, `cmdHandle`
   and `deletable` columns match for all 14 fixtures plus the live Action 4 capture.
2. **Paging.** List past the newest 45: one cursor per store (SD / internal), each the oldest handle of that store's
   last page, with Osmosis's end-of-list rules (short page, the `0c 01` end marker, truncation by declared count).
   CLI: `osmo ls --all`, plus `--page-size N` so paging can be exercised live on a 14-file card.
3. **Live status.** Battery %, storage totals/free per store, from the status pushes the session already receives
   (`0x0d/0x02`, `0x02/0xdc`, `0x02/0x80`). Shown by `osmo ls`.
4. **Thumbnails.** `osmo thumb <path> -o file.jpg`: fetch the thumbnail over HTTP (`MISC/THM/...scr`); for stills
   fall back to the JPEG embedded in the original's EXIF (first 64 KiB, ranged).
5. **Sidecars.** `osmo get --sidecars`: also fetch the unlisted `.DNG` / `.WAV` beside a file (same path, extension
   swapped, same store) when the camera has them.
6. **Favourite.** `osmo star <path> [--off]` (`0x02/0xBF`).
7. **Delete.** `osmo rm <path>... --yes` (`0x00/0x28`) with the safety rules below.

## Non-goals

Burst/interval groups, highlight marks, trimmed download, frame grab, time sync, RTMP livestream, GPS sync, other
camera models, drones, multi-camera memory, bindings, any GUI. Internal-storage paging is ported and fixture-tested
but cannot be live-verified (the Action 4 has no internal store).

## Anti-goals

- A delete that can hit the wrong file. Handles are irreversible; "probably right" is a failure.
- Reporting a write as done when the camera did not answer.
- Silently listing a truncated library as complete.
- Committing raw captures: fixtures are reassembled manifests only (names, sizes, handles), never raw datagrams.

## Constraints and facts the builder needs

Reference: `/Users/hency/Desktop/projects/dji-osmo/osmosis` (read-only). The oracle for goal 1 is
`app/src/test/resources/manifests/golden/*.golden.txt` (columns:
`name|ext|handle|cmdHandle|size|resolution|resLabel|duration|starred|mediaType|group|deletable`) and the live
capture `osmo-core/tests/fixtures/manifests/oa4_live_14.bin`; the Kotlin logic is `CameraSession.kt`
`decodeComposite`/`resolveRecord`/`withCmdHandles`/`flagHandleCollisions`/`stepPagination`/`storeHasOlderPage`/
`collectStores`/`SliceInfo`, and `CameraFile.kt` (`seq`, `opHandle`, `deletable`).

Wire facts (MEDIA_PROTOCOL.md §1-3, §19-20):
- **Delete** `0x00/0x28`, app to camera, datalink: payload `[count:u8][handle:u32-LE x count][counter:u32-LE]00[count:u32-LE]01 01 00 00`;
  reply `0000` = ok, `00d6` = no such handle. One command may delete a whole selection.
- **Favourite** `0x02/0xBF`: payload `01 01 [handle:u32-LE][counter:u32-LE]00[on:u8]00 00 00`; reply `00` = ok. Playback mode must be active.
- **Handles:** a delete handle must come from the bytes at the record's marker AND agree with the fitted
  `base + seq * step` from the other records of the same list; a handle held by more than one record is
  non-deletable for all of them. A favourite-only fitted handle (`cmdHandle`) never makes a file deletable.
- **Write window:** inline writes stop being answered roughly 70 s after the session registered (Osmosis refreshes at
  40 s). A write must therefore ride a session registered recently; do not attempt one on an old session.
- **Pagination:** list query cursor is `u32-LE` at bytes 10-13 of the `0x00/0x26` payload; top bit selects the store
  (`0x00000001` / `0x40000001` = newest page); a page request is a pair of queries (counter 1 = SD, counter 2 =
  internal) plus the trigger frame already used by `ls`; counters echo in each reply chunk. Page size is byte 14 (45).
  The Action 4 SD handles look like `0x0004xxxx` with step `0x10`.
- **Thumbnails:** `GET /v2?storage=N&path=MISC/THM/<dir>/<base>.scr`; stills may instead use the EXIF-embedded JPEG
  from the first 64 KiB of the original (`EmbeddedJpeg.fromHeader`).
- **Sidecars:** same path as the parent with `.DNG` or `.WAV`, same store; a HEAD/GET that 404s means none.
- Everything from the original design still holds: sans-IO core behind `BleLink`/`Datagram`, no OS types in the
  public API, MIT attribution, secrets never logged or committed, "fixture-tested" vs "live-verified" kept distinct.

## Decisions made (owner and me)

- Owner: core features only, not the whole Osmosis feature set.
- Mine: CLI verbs `ls --all`, `thumb`, `star`, `rm`; `rm` refuses without `--yes` and prints what it would delete;
  `rm` deletes only files whose handle is verified (`deletable`), and says so otherwise; a write on a session older
  than 40 s returns an error rather than being attempted; delete is confirmed by the camera's `0000` reply and then by
  the file being absent from a fresh listing.
- Mine: handle/fit logic lives in the pure manifest module so it is testable offline against the goldens.

## Resolved after design review

- **Oracle for goal 1.** The 14 Osmosis goldens are already copied into `osmo-core/tests/fixtures/manifests/golden/`
  and `tests/golden.rs` already iterates them (today comparing 4 of 12 columns; this work extends it to the
  `handle`, `cmdHandle` and `deletable` columns). The live capture `oa4_live_14.bin` has no golden: its expected
  handles are derived and hand-checked. The Action 4 convention, confirmed by the `oa4_45` golden, is
  `handle = 0x40000 + seq * 0x10`; the live capture's videos carry `0x400e0` (0014), `0x400d0` (0013), ... `0x40020`
  (0002) at their marker, so every one of its 14 files is expected to get `0x40000 + seq * 0x10` and be deletable.
  The owner's live check (deleting a known throwaway photo) is the final proof.
- **`rm` safety (my call).** Dry-run by default: without `--yes` it prints exactly what it would delete and exits.
  Only explicit paths, never globs or "all"; at most 20 per call; every path must resolve to a verified, unshared
  handle in a listing taken in the same session, else that path is refused by name and nothing is sent for it.
  The agent building this never runs `rm` against the camera; the owner does.
- **Write window vs `rm` control flow (my call).** `rm` and `star` use one fresh session: list (reads, any age) then
  immediately write, so the write is seconds old; a write is refused if the session is older than 40 s. Confirmation is
  the camera's `0000` reply, then absence from a second listing taken on the same session (reads are fine at any age).
- **`star` and playback (my call).** The list step already leaves the camera in playback; `star` sends the write on
  that session and adds no mode switch of its own. `close()` (leave playback) runs on drop as today.
- **Status layouts.** Authoritative sources, in order: `net/DumlSession.kt` `applyStatusFrame`, then
  `MEDIA_PROTOCOL.md` sections 18-20, then `net/StorageStatusTest.kt` for known-good payload bytes. Decode only those
  fields; ignore the rest.

## Left to the builder

Module layout, how a page walk reuses or reopens a session (Osmosis tries the live session first and falls back to a
fresh registered one), type names, CLI flag spellings beyond those above, test organisation.

## Live verification plan (owner's Action 4)

Read-only first: `ls --all --page-size 5` (14 files in three pages must equal the plain listing), `ls` status line,
`thumb` on a photo and a video. Then reversible: `star` on a photo and back. Last, destructive: the owner shoots a
throwaway photo and `rm` deletes only that one; nothing else is ever deleted during testing.

## Risks

- The handle fit is the subtle part; a bug there is how a wrong file gets deleted. Golden parity plus an explicit
  "never delete without agreement" test is the mitigation.
- Paging was hard-won in Osmosis (truncated pages, empty answers, two-store pages); expect timing quirks.
- Status payload layouts differ by body; only decode what Osmosis verified, ignore the rest.
- Internal-store behaviour is fixture-only.

## Success

On the owner's Action 4: `ls --all --page-size 5` equals the plain listing; the status line shows battery and card
space matching the camera's screen; thumbnails open as images; a star toggles and survives a re-list; `rm --yes` removes
exactly one throwaway photo and refuses everything it cannot verify. `cargo test` stays green offline, and every claim
is labelled fixture-tested or live-verified.
