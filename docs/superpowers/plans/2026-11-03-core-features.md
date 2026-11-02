# Core Feature Pack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Browse the whole library (handles, paging, status), fetch thumbnails and sidecars, and favourite and delete files, on an Osmo Action 4.

**Architecture:** Extend the existing pure manifest decoder with full handle logic first, because paging, favourite and delete all address files by handle. New behaviour is pure functions plus thin session methods on the existing `CameraSession<D: Datagram>`; writes go through one guarded path (fresh-session check, playback check, explicit reply handling). CLI verbs are thin wrappers over tested pure helpers.

**Tech Stack:** Existing Rust workspace (`osmo-core`, `osmo-cli`), `ureq`, `clap`, `thiserror`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-03-core-features-design.md` (read it and `2026-11-03-osmo-core-design.md`). Reference source, read-only: `/Users/hency/Desktop/projects/dji-osmo/osmosis` (call it `$OSMOSIS`).

## Global Constraints

- Test device: Osmo Action 4, UDP 9004, SD card only, 14 files on it. Hardware is run **by the owner**; pause at each HW checkpoint and say what you need.
- Handle namespace on the Action 4: `handle = 0x40000 + seq * 0x10`; `seq` is the 4-digit number in `_NNNN_D` of the file name.
- **Never send a write for a file that is not `deletable()`** (`handle != 0 && !handle_shared`); favourite uses `cmd_handle`, which must be non-zero.
- Write window: a favourite or delete is refused if the session registered more than **40 s** ago (`Timing.write_window`, default 40 s). Reads are fine at any age.
- `rm`: dry-run unless `--yes`; explicit paths only, no globs or "all"; at most **20** per call; refused paths are named and nothing is sent for them.
- Delete wire payload: `[count:u8][handle:u32-LE x count][counter:u32-LE]00[count:u32-LE]01 01 00 00`; reply `0000` ok, `00d6` no such handle. Favourite payload: `01 01 [handle:u32-LE][counter:u32-LE]00[on:u8]00 00 00`; reply `00` ok.
- Page size default 45 (byte 14 of the `0x00/0x26` payload); cursor `u32-LE` at bytes 10-13, top bit = store; counters 1 (SD) and 2 (internal).
- Fixtures are reassembled manifests only (names, sizes, handles); never raw datagrams. Secrets never logged or committed.
- Report results as **fixture-tested** or **live-verified**; log live runs in `docs/hardware-log.md`.
- Commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Branches: `feat/handles` (T1-2), `feat/paging` (T3-4, merge after HW-A), `feat/thumbs-sidecars` (T5-6, merge after HW-B), `feat/writes` (T7-8, merge after HW-C and HW-D). Merge to `main` with `--no-ff`.

## Review Focus

1. A delete whose handle is shared or unverified is refused **before any byte is sent**, and the user is told which file and why (Task 8).
2. `rm` with a mistyped or unknown path is refused by name and nothing is sent for it; a valid path next to it is not silently deleted either (Task 8).
3. A write the camera never answers is reported as "no reply", never as done; the CLI says to verify by re-listing (Tasks 7, 8).
4. A page the camera cuts short is not silently treated as the end of the library: `ls --all` warns and `more_available` stays true (Tasks 3, 4).
5. A write attempted on an old session is refused with an actionable message instead of being sent and ignored (Task 7).

---

### Task 1: Full handle decoding and the starred flag

**Files:**
- Modify: `osmo-core/src/media.rs` (new fields, `deletable`), `osmo-core/src/datalink/manifest.rs` (candidate, fit, collisions, star)
- Test: `osmo-core/tests/golden.rs` (extend), `osmo-core/tests/live_oa4.rs` (extend), unit tests in `manifest.rs`
- Reference: `$OSMOSIS/.../camera/CameraSession.kt` `resolveRecord` (handleCandidate, starFlagBySignature, starFlag), `withCmdHandles`, `flagHandleCollisions`; `core/CameraFile.kt` (`seq`, `deletable`)

**Interfaces:**
- Produces: `CameraFile` gains `seq: u32`, `cmd_handle: u32`, `handle_candidate: u32`, `handle_shared: bool`, `starred: bool` (existing fields unchanged, `handle: u32`); `impl CameraFile { pub fn deletable(&self) -> bool }`. `decode_manifest` and `collect_stores` keep their signatures but now return fully resolved records. In `manifest.rs`: `pub(crate) fn with_cmd_handles(files: Vec<CameraFile>) -> Vec<CameraFile>` and `pub(crate) fn flag_handle_collisions(files: Vec<CameraFile>) -> Vec<CameraFile>`.

- [ ] **Step 1: Failing tests.** In `golden.rs` extend the per-fixture comparison to golden columns 0,1,2,3,4,8,10,11 (`name|ext|handle|cmdHandle|size|...|starred|...|group|deletable`; handle columns are `{:08x}`, booleans `true/false`). In `live_oa4.rs` add `live_listing_handles_follow_the_action4_fit`: for each of the 14 files `handle == 0x40000 + seq*0x10`, `cmd_handle == handle`, `deletable()`, and `seq` equals the `_NNNN_D` number. In `manifest.rs` unit tests (synthetic `CameraFile` lists, helper `file(seq, handle, candidate)`): `shared_handle_disables_delete_for_every_holder` (two records with handle `0x40010`, a third unique stays deletable); `a_marker_handle_the_fit_contradicts_is_revoked` (seq 1..3 at `0x40010,0x40020,0x40030`, seq 4 at `0x99999` -> handle 0, `cmd_handle == 0x40040`, not deletable); `candidate_is_promoted_only_when_the_fit_agrees` (a still with handle 0 and candidate `0x40050` at seq 5 is promoted; one at seq 6 with candidate `0x40061` is not and keeps `cmd_handle == 0x40060`); `fewer_than_two_handles_gets_no_fit` (`cmd_handle` stays 0).
- [ ] **Step 2:** Run `cargo test -p osmo-core --test golden --test live_oa4` and `cargo test -p osmo-core manifest`. Expected: compile failure (new fields), then handle/star mismatches.
- [ ] **Step 3: Implement.** `seq` from the name's `_NNNN_D` (0 if absent). `handle_candidate`: `u32` at `self_pos-17` when `bytes[self_pos-7]==0x19 && bytes[self_pos-6]==0x06`, else 0. `with_cmd_handles`: per `group`, points = records with `handle != 0 && seq > 0`, deduped by seq and sorted; needs >= 2; step = most common positive integer delta `(h2-h1)/(s2-s1)` over adjacent pairs, base = most common `handle - seq*step`; ties resolve to the first value seen in iteration order; then per record apply exactly the three cases of Osmosis (promote when `handle==0 && candidate!=0 && candidate==fitted`; revoke when `handle!=0 && handle!=fitted`; else just set `cmd_handle=fitted`); records with no fit or `seq == 0` are untouched. `flag_handle_collisions`: any non-zero handle held by >1 record sets `handle_shared` on all holders. Star: port `starFlagBySignature` first, `starFlag` as fallback (strictly `== 1`). Call order matches Osmosis: `decode_composite` ends with `with_cmd_handles`, `decode_manifest` and each `collect_stores` slice then apply `flag_handle_collisions`.
- [ ] **Step 4:** Run `cargo test --workspace`. Expected: all green, including all 14 goldens on the new columns.
- [ ] **Step 5:** `git checkout -b feat/handles`; commit `feat(media): full handle fit, collisions, starred flag (golden parity)`.

---

### Task 2: Live status decoding

**Files:**
- Create: `osmo-core/src/status.rs`; modify `osmo-core/src/lib.rs` (`pub mod status;`), `osmo-core/src/datalink/session.rs`, `osmo-cli/src/listing.rs`, `osmo-cli/src/main.rs`
- Test: unit tests in `status.rs`, `osmo-core/tests/session_fake.rs`, `osmo-cli/src/listing.rs`
- Reference: `$OSMOSIS/.../net/DumlSession.kt` `applyStatusFrame`, `app/src/test/.../net/StorageStatusTest.kt` (known-good payloads), `MEDIA_PROTOCOL.md` sections 18-20

**Interfaces:**
- Produces: `pub struct Status { pub battery_percent: Option<u8>, pub battery_millivolts: Option<u32>, pub charging: Option<bool>, pub docked: Option<bool>, pub sd_total_mb: Option<u32>, pub sd_free_mb: Option<u32>, pub internal_total_mb: Option<u32>, pub internal_free_mb: Option<u32> }` with `Default` and `pub fn apply(&mut self, set: u8, cmd: u8, payload: &[u8]) -> bool`; `CameraSession::status(&self) -> &Status`; `pub fn format_status(s: &Status) -> String` in `osmo-cli/src/listing.rs`.

- [ ] **Step 1: Failing tests.** `status.rs`: port the payload vectors of `StorageStatusTest.kt`; plus `battery_push_decodes_percent_and_millivolts` (34-byte `0x0D/0x02`, percent at byte 20, mV u16-LE at byte 1, accept mV only in 2000..5000), `single_store_storage_frame_confirms_no_internal` (22-byte `0x02/0xDC`: `internal_total_mb == Some(0)`), `a_zeroed_first_block_after_a_real_capacity_is_ignored`. `session_fake.rs`: `status_pushes_during_a_listing_are_decoded` (fake pushes a `0x0D/0x02` frame with percent 87 during the list; `s.status().battery_percent == Some(87)`). `listing.rs`: `format_status_shows_battery_and_free_space` (e.g. `battery 87%  SD 105.2 GiB free of 119.1 GiB`; absent fields are omitted, empty status gives `""`).
- [ ] **Step 2:** Run; Expected: compile failures, then FAIL.
- [ ] **Step 3: Implement** `Status::apply` per Osmosis (`0x02/0x80` storage total/free at bytes 5/9 is not needed; decode only `0x0D/0x02`, `0x02/0xDC` and keep the playback bit where it already lives), have `CameraSession::parse_status` feed every scanned frame into it, and print `format_status` as the first line of `osmo ls` output when non-empty.
- [ ] **Step 4:** `cargo test --workspace`. Expected: green.
- [ ] **Step 5:** Commit `feat: live battery and storage status`; merge `feat/handles` to `main` (`--no-ff`).

---

### Task 3: Paging (pure rules and session walk)

**Files:**
- Create: `osmo-core/src/datalink/paging.rs`; modify `datalink/mod.rs`, `datalink/session.rs`, `datalink/stream.rs`
- Test: unit tests in `paging.rs`, `osmo-core/tests/session_fake.rs`
- Reference: `$OSMOSIS/.../camera/CameraSession.kt` `SliceInfo`, `storeHasOlderPage`, `stepPagination`, `oldestHandle`, `fetchNextPage`, `listCmd`; `MEDIA_PROTOCOL.md` "Paginate the full library"

**Interfaces:**
- Produces: `pub struct SliceInfo { pub declared: i64, pub records: usize, pub end_marker: bool, pub ended: bool }` with `pub fn incomplete(&self) -> bool` (`declared in 1..=512 && records < declared`); `pub fn slice_info(blob: &[u8], ctr: u8) -> SliceInfo`; `pub fn store_has_older_page(slice_len: usize, cursor_moved: bool, info: Option<&SliceInfo>, page_size: usize) -> bool` (rules: not moved -> false; no info -> `len >= page_size`; incomplete -> true; end marker -> false; else `len >= page_size`); `pub struct PageStep { pub fresh: Vec<CameraFile>, pub sd_cursor: u32, pub internal_cursor: u32, pub more_available: bool }`; `pub fn step_pagination(sd_cursor: u32, internal_cursor: u32, page: &[CameraFile], seen: &mut HashSet<(u8, String)>, info: &HashMap<u8, SliceInfo>, page_size: usize) -> PageStep` (each store's next cursor = oldest non-zero handle in that store's slice strictly below its current cursor, else unchanged; dedupe key `(storage, path)`; `more_available` needs fresh files and at least one store with an older page).
- Session: `CameraSession::set_page_size(&mut self, n: u8)` (1..=255, default 45), `more_available(&self) -> bool`, `list_next_page(&mut self) -> Result<Vec<CameraFile>, SessionError>` (returns only unseen files; empty once exhausted); `Diagnostics` gains `truncated: bool`.

- [ ] **Step 1: Failing tests.** `paging.rs`: `a_short_page_ends_the_library` (`store_has_older_page(13, true, None, 45) == false`, `(45, true, None, 45) == true`); `the_end_marker_is_final_even_on_a_full_page`; `truncation_outranks_the_end_marker` (declared 21, records 17, marker present -> true); `a_store_that_ran_out_stops_advancing_while_the_other_pages` (SD files only: internal cursor unchanged); `files_are_deduped_by_store_and_path` (same path on both stores is two files); `handle_zero_never_becomes_a_cursor`; `slice_info_reads_declared_count_and_marker` on `nano_45.bin`/`xtra_13.bin`/`oa4_6.bin` fixtures (full page: not marked ended-by-marker; final pages: marker true). `session_fake.rs`: `second_page_query_carries_the_oldest_sd_handle_as_cursor` (page 1 manifest from `oa4_45.bin` chunked as in the existing tests, page 2 from `oa4_6.bin`; check at least one path of page 2 is new, else choose another fixture; assert page 2's counter-1 query bytes 10-13 equal the minimum non-zero SD handle of page 1, `list_next_page` returns only unseen files, and `more_available()` is false after the short final page); `page_size_reaches_byte_14_of_the_query` (`set_page_size(5)`); `a_truncated_page_keeps_more_available_and_sets_the_flag` (fake serves a slice whose header count exceeds its records; `diagnostics().truncated` and `more_available()`).
- [ ] **Step 2:** Run `cargo test -p osmo-core paging` and `--test session_fake`. Expected: compile failure then FAIL.
- [ ] **Step 3: Implement.** Pure functions as above in `paging.rs`. In the session: `list_newest` seeds cursors (SD and internal = oldest non-zero handle per store of the first page, else the newest-page selector `0x00000001` / `0x40000001`), records `SliceInfo` per store from `collect`, and `more_available` via `store_has_older_page`; `list_next_page` sends the pair of queries (counter 1 with `sd_cursor`, trigger frame, counter 2 with `internal_cursor`) on the **same** session with the same batch/ack/presence loop as `query_newest` (extract the shared collect loop rather than duplicating it); on a truncated slice retry the page query once with doubled batch waits, and if still truncated return the files, set `truncated`, and leave `more_available` true. `list_cmd` takes the page size. Do **not** build a fresh-session fallback yet; if live paging (HW-A) fails inline, add it as a follow-up.
- [ ] **Step 4:** `cargo test --workspace`. Expected: green.
- [ ] **Step 5:** `git checkout -b feat/paging`; commit `feat(datalink): paging with per-store cursors and truncation detection`.

---

### Task 4: CLI paging and HW-A

**Files:** modify `osmo-cli/src/main.rs`, `osmo-cli/src/listing.rs`; docs `docs/hardware-log.md`
**Interfaces:** Consumes `set_page_size`, `list_next_page`, `more_available`, `diagnostics().truncated`. Produces CLI `osmo ls [--all] [--page-size N]`, and `pub fn truncation_warning(truncated: bool) -> Option<String>` in `listing.rs`.

- [ ] **Step 1: Failing tests** in `listing.rs`: `truncation_warning_names_the_risk` (`Some` containing "incomplete" when true, `None` when false); `star_marker_appears_in_the_listing` (a starred file's row ends with a star marker, others unchanged).
- [ ] **Step 2:** Run `cargo test -p osmo-cli`. Expected: FAIL.
- [ ] **Step 3: Implement** the flags (`--all` loops `list_next_page` until `!more_available()` or an empty page, printing the combined listing sorted as the camera returned it; `--page-size` validated 1..=255) and the star marker. Print the warning to stderr after the listing when truncated.
- [ ] **Step 4:** `cargo test --workspace` green; `cargo clippy --workspace --all-targets` clean.
- [ ] **Step 5 (HW-A, needs the camera):** Ask the owner to join the camera's WiFi and run `osmo ls` then `osmo -v ls --all --page-size 5`. Expected: the second output lists the same 14 files over three pages, no truncation warning, and the status line shows battery and card space matching the camera screen. If an inline page fails, stop and add the fresh-session fallback before merging. Record results in `docs/hardware-log.md`; commit; merge `feat/paging` to `main`.

---

### Task 5: Thumbnails

**Files:**
- Create: `osmo-core/src/embedded_jpeg.rs`; modify `osmo-core/src/lib.rs`, `osmo-core/src/http.rs` (`get_bytes`, `get_range`), `osmo-cli/src/main.rs`, `osmo-cli/src/listing.rs`
- Test: unit tests in `embedded_jpeg.rs`, `osmo-core/tests/http.rs`, `listing.rs`
- Reference: `$OSMOSIS/.../core/EmbeddedJpeg.kt`, `net/ImageLoader.kt`

**Interfaces:**
- Produces: `pub fn embedded_jpeg(head: &[u8]) -> Option<Vec<u8>>` and `pub const HEAD_BYTES: usize = 65_536`; `HttpClient::get_bytes(&self, path: &str) -> Option<Vec<u8>>` and `get_range(&self, path: &str, start: u64, end: u64) -> Option<Vec<u8>>` (inclusive end, `None` on non-2xx or error); `pub fn is_jpeg(b: &[u8]) -> bool` (starts `FF D8`) in `embedded_jpeg.rs`; CLI `osmo thumb <path> -o <file.jpg> [--storage N]`.

- [ ] **Step 1: Failing tests.** `embedded_jpeg.rs`: `extracts_the_jpeg_inside_an_app1_segment` (hand-built: SOI, APP1 whose body contains an inner SOI...EOI), `returns_none_when_image_data_starts_first` (SOS before any APP1), `returns_none_on_truncated_or_non_jpeg_input`, `never_panics_on_garbage` (2000 pseudo-random bytes and every prefix of the synthetic JPEG). `http.rs` tests (scripted server): `get_bytes_returns_the_body_or_none_on_404`, `get_range_sends_an_inclusive_range_header` (`Range: bytes=0-65535`). `listing.rs`: `thumb_sources_prefer_the_scr_then_the_embedded_jpeg_for_stills` (pure `thumb_sources(&CameraFile) -> Vec<ThumbSource>`: stills `[Scr, Embedded]`, videos `[Scr]`).
- [ ] **Step 2:** Run; Expected: FAIL.
- [ ] **Step 3: Implement** the port, the two HTTP getters (new agent per call, same timeouts as `head`), and `osmo thumb`: resolve the file via a listing (or derive `MISC/THM/<dir>/<base>.scr` from `--storage` and the path), try sources in order, accept only bytes that `is_jpeg`, write the file, print its size.
- [ ] **Step 4:** `cargo test --workspace` green.
- [ ] **Step 5:** `git checkout -b feat/thumbs-sidecars`; commit `feat: thumbnails (scr, embedded EXIF jpeg)`.

---

### Task 6: Sidecars and HW-B

**Files:** modify `osmo-core/src/media.rs`, `osmo-cli/src/main.rs`, `osmo-cli/src/listing.rs`; docs
**Interfaces:** Produces `CameraFile::is_video(&self) -> bool` (ext in `MP4 MOV OSV INSV`) and `CameraFile::sidecar_paths(&self) -> Vec<String>` (stills: the same path with `.DNG`; videos: with `.WAV`); CLI `osmo get <path> --sidecars` (HEAD each candidate on the same store, download the ones that exist with the same resumable logic, print which were found).

- [ ] **Step 1: Failing tests** in `media.rs`: `still_sidecar_is_the_dng_beside_it`, `video_sidecar_is_the_wav_beside_it`, `is_video_is_case_insensitive_on_the_stored_upper_case_extension`; in `osmo-core/tests/http.rs`: `a_404_sidecar_is_skipped_without_error` using `head` returning `None`.
- [ ] **Step 2:** Run; Expected: FAIL.
- [ ] **Step 3: Implement** `sidecar_paths`, `is_video`, and the `--sidecars` flag in `get` (a missing sidecar prints `no sidecar` and is not an error).
- [ ] **Step 4:** `cargo test --workspace` green.
- [ ] **Step 5 (HW-B, needs the camera):** Ask the owner to run `osmo thumb <a photo path> -o /tmp/t1.jpg`, the same for a video, and `osmo get <photo> --sidecars -o /tmp/osmo-test`. Check the thumbnails open as images (read them with the image viewer); sidecars exist only if the card has JPEG+DNG or WAV backups, otherwise log "no sidecars on card, fixture-tested only". Delete the test files afterwards. Log results; commit; merge `feat/thumbs-sidecars` to `main`.

---

### Task 7: Guarded writes and favourite

**Files:**
- Create: `osmo-core/src/datalink/writes.rs`; modify `datalink/mod.rs`, `datalink/session.rs` (`Timing.write_window`, session start time), `osmo-cli/src/main.rs`
- Test: unit tests in `writes.rs`, `osmo-core/tests/session_fake.rs`
- Reference: `$OSMOSIS/.../camera/CameraSession.kt` `favoritePayload`, `setFavorite`, `refreshSessionForWrite`; `MEDIA_PROTOCOL.md` section 3

**Interfaces:**
- Produces: `pub fn favourite_payload(handle: u32, counter: u32, on: bool) -> Vec<u8>`; `pub enum WriteError { SessionTooOld { age_secs: u64 }, NotInPlayback, NoHandle, NotDeletable(String), TooMany { max: usize }, Rejected(u16), NoReply }` (`thiserror`, each message actionable); `CameraSession::set_favourite(&mut self, file: &CameraFile, on: bool) -> Result<(), WriteError>` (requires playback held, `cmd_handle != 0`, session age <= `Timing.write_window`, waits up to 2 s for `0x02/0xBF`, status 0 = ok); `Timing.write_window: Duration` (default 40 s, `fast()` 20 ms); `CameraSession::session_age(&self) -> Duration`.

- [ ] **Step 1: Failing tests.** `writes.rs`: `favourite_payload_matches_the_documented_frame` (`favourite_payload(0x40104040, 1, true)` is `01 01 40 40 10 40 01 00 00 00 00 01 00 00 00`, and `Frame{target:0x0102,id:0xFDBE,ty:0x40|0x02<<8|0xBF<<16,payload}.encode()` equals `551c041b0201befd4002bf0101404010400100000000010000008c88`). `session_fake.rs`: `favourite_sends_0x02_0xbf_and_accepts_status_0`; `favourite_rejection_is_reported` (reply status `0xE0` -> `Rejected(0xE0)`); `favourite_with_no_reply_is_no_reply_not_success` (bounded time); `a_write_on_an_old_session_is_refused_without_sending` (fast `write_window` 20 ms, sleep past it, error `SessionTooOld`, no `0x02/0xBF` frame in the log); `a_file_without_a_command_handle_is_refused` (`NoHandle`); `a_write_outside_playback_is_refused` (fake that never confirms playback -> `NotInPlayback`).
- [ ] **Step 2:** Run; Expected: FAIL.
- [ ] **Step 3: Implement** the guarded path as one private `fn write_guard(&self) -> Result<(), WriteError>` (age, playback) reused by Task 8, the favourite counter starting at 1 per session, and CLI `osmo star <path> [--off]` (one session: list, find the file, `set_favourite`, then print the camera's answer; on `NoReply` tell the user to check the camera screen or re-list).
- [ ] **Step 4:** `cargo test --workspace` green.
- [ ] **Step 5 (HW-C, needs the camera):** `git checkout -b feat/writes`; commit `feat: guarded writes and favourite`. Ask the owner to run `osmo star <a photo path>`, confirm the heart on the camera screen, run `osmo ls` (star marker present), then `osmo star <path> --off` and re-list (marker gone). Log results.

---

### Task 8: Delete and HW-D

**Files:** modify `osmo-core/src/datalink/writes.rs`, `datalink/session.rs`, `osmo-cli/src/main.rs`, `osmo-cli/src/listing.rs`; docs
**Interfaces:**
- Consumes: `write_guard`, `WriteError`, `CameraFile::deletable`.
- Produces: `pub fn delete_payload(handles: &[u32], counter: u32) -> Vec<u8>`; `CameraSession::delete_files(&mut self, files: &[&CameraFile]) -> Result<(), WriteError>` (validates **every** file first: `NotDeletable(path)` for any non-deletable, `TooMany{max:20}`; only then sends `0x00/0x28`, waits up to 10 s; `0x0000` ok, `0x00d6` -> `Rejected(0xD6)`, silence -> `NoReply`); `pub fn plan_deletions<'a>(files: &'a [CameraFile], requested: &[String]) -> Result<Vec<&'a CameraFile>, Vec<String>>` in `listing.rs` (the `Err` carries one refusal sentence per bad path); CLI `osmo rm <path>... [--yes]`.

- [ ] **Step 1: Failing tests.** `writes.rs`: `delete_payload_matches_the_documented_frame` (`delete_payload(&[0x40104480], 1)` is `01 80 44 10 40 01 00 00 00 00 01 00 00 00 01 01 00 00`; full frame `Frame{target:0x0102,id:0xA000,ty:0x40|0x28<<16,..}` equals `551f044e020100a0400028018044104001000000000100000001010000a0d1`), and a two-file payload per the doc (`0b`-style layout: count byte, both handles, counter, `00`, count u32, `01 01 00 00`). `session_fake.rs`: `delete_refuses_a_shared_handle_before_sending_anything` (two files with the same handle; error `NotDeletable`; the log has **no** `0x00/0x28` frame), `delete_refuses_more_than_20_files`, `delete_sends_the_documented_payload_and_accepts_0000`, `delete_no_reply_is_not_success`, `delete_status_00d6_is_rejected`. `listing.rs`: `plan_deletions_refuses_unknown_paths_by_name_and_deletes_nothing` (one valid + one typo path: `Err` naming the typo; the valid one is not returned), `plan_deletions_refuses_a_non_deletable_file`, `plan_deletions_dedupes_repeated_paths`, `plan_deletions_refuses_globs_and_directory_paths`.
- [ ] **Step 2:** Run; Expected: FAIL.
- [ ] **Step 3: Implement** the pure builder, the session method (one `delete_counter` per session), `plan_deletions`, and `osmo rm`: open a session, list, `plan_deletions`; without `--yes` print `would delete:` plus each path and size and exit 0 without sending anything; with `--yes` call `delete_files`, then re-list on the same session and require every path to be absent (print `deleted N file(s)` or exit 1 naming any survivor). Never print or act on anything outside the explicit paths.
- [ ] **Step 4:** `cargo test --workspace` green; `cargo clippy --workspace --all-targets` clean.
- [ ] **Step 5 (HW-D, needs the camera, the owner runs `rm`):** Ask the owner to shoot one throwaway photo, run `osmo ls` to see it, then `osmo rm <that path>` (dry-run, expect "would delete"), then `osmo rm <that path> --yes`, then `osmo ls` (gone) and confirm on the camera. The agent never runs `rm` against the camera. Log results; update README usage and `docs/protocol.md` (new verbs, what is live-verified); commit; merge `feat/writes` to `main`; tag `v0.2.0`.
