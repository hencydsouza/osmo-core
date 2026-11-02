# osmo-core Prototype Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust workspace (`osmo-core` library + `osmo` CLI) that scans, pairs, reads WiFi credentials, lists media, and downloads one file from a DJI Osmo Action 4, with the protocol layer fully testable offline.

**Architecture:** Pure protocol code (framing, command builders, manifest decoder, state machines) sits behind two small blocking I/O traits: `BleLink` (GATT notify/write) and `Datagram` (UDP). The core has no async runtime and no OS types, so Android/Swift bindings stay possible. Only the CLI uses tokio and `btleplug`. Protocol knowledge is ported from the Osmosis Kotlin source, read-only at `/Users/hency/Desktop/projects/dji-osmo/osmosis` (call it `$OSMOSIS`).

**Tech Stack:** Rust 2021 (stable, cargo 1.97 installed), `thiserror`, `rand`, `ureq` 2.x (blocking HTTP), `btleplug` + `tokio` + `clap` + `directories` (CLI only), `tiny_http` (dev-dep, test server).

**Spec:** `docs/superpowers/specs/2026-11-03-osmo-core-design.md`

## Global Constraints

- Test device: Osmo Action 4, model id `0x0014`, datalink UDP **9004**, WPA2, **TCP-7001 poke required** (connect 1.2 s timeout, write the SetPairingPIN frame with PIN `"osmo"`, wait 400 ms), camera at **192.168.2.1**.
- BLE: service `0000fff0-0000-1000-8000-00805f9b34fb`; write DUML to `fff5` write-without-response; notify on `fff4` and `fff5`; request MTU 517; arm pairing by writing `01 00` to `fff4`. DJI manufacturer ids `0x08AA`, `0xF7AA`, `0xE5C0`.
- CRC8 init `0x77` poly `0x8C`; CRC16 init `0x3692` poly `0x8408` (reflected forms, exactly as `$OSMOSIS/.../duml/DjiCrc.kt`).
- The pairing identifier is 32 chars, minted once and persisted in the user config dir (never the repo). Default token for cameras is `"osmo"`.
- WiFi password: printed by `osmo wifi` on request only; never logged, never written to disk, never in test output. No captures or credentials are ever committed.
- Core crate: no async runtime, no Android/Mac/Windows types in its public API.
- Licence MIT with attribution to Konrad Iturbe in `NOTICE` for ported protocol knowledge and fixtures. Fixtures are copied into the repo.
- HTTP: Range-resumable GET with `Connection: close`; 404/500 are transient "busy", not fatal; a 200 answer to a ranged request means "restart from byte 0".
- Anything verified only against fixtures or a fake is reported as **fixture-tested**; only hardware runs are **live-verified**.
- Commit messages end with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Branch flow: one branch per milestone (below), merged to `main` with `--no-ff` once its tests pass.

**Milestone branches:** `feat/protocol-basics` (Tasks 1-4), `feat/ble-pairing` (5-6, merge after HW-1), `feat/datalink-ls` (7-10), `feat/http-get-cli` (11-12, merge after HW-2).

## Review Focus

Failure modes the spec implies but no happy-path test covers, most likely first. Each has a pinned test in the owning task.

1. Camera has no SD card or the store answers empty: `ls` prints "no files" and exits 0, never hangs or errors (Task 10).
2. Mac not on the camera's WiFi / camera asleep: `ls` fails in bounded time (about 20 handshake attempts) with a message naming `192.168.2.1:9004` and "join the camera's WiFi" (Task 10, Task 12).
3. User never approves pairing on the camera: `pair` stops after 60 s with a clear message, not a hang (Task 5).
4. Partial or complete destination file for `get`: resume from the partial length; a complete file reports "already complete" and does not re-download; Ctrl-C leaves a resumable file (Task 11).
5. No Bluetooth adapter / Bluetooth off / terminal lacks Bluetooth permission: `scan` fails with an actionable message, not a panic (Task 6).

---

### Task 1: Scaffold workspace, licence, fixtures

**Files:**
- Create: `Cargo.toml` (workspace, members `osmo-core`, `osmo-cli`), `osmo-core/Cargo.toml`, `osmo-core/src/lib.rs`, `osmo-cli/Cargo.toml`, `osmo-cli/src/main.rs`, `.gitignore`, `LICENSE`, `NOTICE`, `README.md`
- Create: `osmo-core/tests/fixtures/manifests/*.bin` and `osmo-core/tests/fixtures/manifests/golden/*.golden.txt` copied from `$OSMOSIS/app/src/test/resources/manifests/`
- Branch: `git checkout -b feat/protocol-basics`

**Interfaces:**
- Produces: workspace that builds; `osmo-core` exports empty modules `duml`, `ble`, `datalink`, `http`, `media`; CLI binary named `osmo`.

- [ ] **Step 1:** Create the workspace and crates. `osmo-core` deps: `thiserror`, `rand`, `ureq = "2"`; dev-deps: `tiny_http`. `osmo-cli` deps: `osmo-core` (path), `btleplug`, `tokio` (rt-multi-thread, macros, time), `clap` (derive), `directories`. `.gitignore`: `/target`, `*.pcap`, `*.btsnoop`, `*.log`, `.env`.
- [ ] **Step 2:** Copy the fixtures and goldens verbatim. Write `LICENSE` (MIT, owner Hency Dsouza) and `NOTICE` crediting Konrad Iturbe / Osmosis (MIT), dimadesu/dji-remote (MIT, vendored DUML framing) and xaionaro-go/djictl (CC0, streaming frames, not used yet). README: one paragraph + "work in progress".
- [ ] **Step 3:** Add `osmo-core/tests/smoke.rs` with `fn fixtures_present()` asserting `tests/fixtures/manifests/oa4_45.bin` is 11433 bytes and its golden file has 45 lines.
- [ ] **Step 4:** Run `cargo test --workspace`. Expected: builds, 1 test passes.
- [ ] **Step 5:** Commit `chore: scaffold workspace, licence, fixtures`.

---

### Task 2: DUML framing, CRCs, frame scanning

**Files:**
- Create: `osmo-core/src/duml/mod.rs`, `crc.rs`, `frame.rs`, `scan.rs`
- Test: unit tests in each file
- Reference: `$OSMOSIS/.../duml/{DjiCrc,DjiMessage,ByteReader,ByteWriter}.kt`, `net/DumlTransport.kt` (`scanFrames`, `findReply`, `findRespStatus`), tests `app/src/test/.../duml/{DjiCrcTest,DjiMessageTest}.kt`

**Interfaces:**
- Produces:
  - `pub fn crc8(data: &[u8]) -> u8`, `pub fn crc16(data: &[u8]) -> u16`
  - `pub struct Frame { pub target: u16, pub id: u16, pub ty: u32, pub payload: Vec<u8> }` (`ty` is the 24-bit LE field: `flags | set<<8 | cmd<<16`)
  - `impl Frame { pub fn encode(&self) -> Vec<u8>; pub fn decode(data: &[u8]) -> Result<Frame, FrameError>; pub fn flags(&self) -> u8; pub fn cmd_set(&self) -> u8; pub fn cmd_id(&self) -> u8 }`. `decode` enforces SOF `0x55`, `data.len() == length byte`, version byte `0x04`, both CRCs (the BLE variant).
  - `pub fn pack_string(s: &str) -> Vec<u8>` (`[len:u8][utf8]`)
  - `pub struct Scanned { pub set: u8, pub cmd: u8, pub payload: Vec<u8> }`, `pub fn scan_frames(raw: &[u8]) -> Vec<Scanned>` (10-bit length + 6-bit version, version must be 1, advance one byte at a time, both CRCs verified), `pub fn find_reply(datagrams: &[Vec<u8>], set: u8, cmd: u8) -> Option<Vec<u8>>` (first non-empty-payload match), `pub fn find_resp_status(d: &[u8], set: u8, cmd: u8) -> Option<u16>`
  - `pub enum FrameError` via `thiserror`

- [ ] **Step 1: Failing tests.** Port every vector in `DjiCrcTest.kt` and `DjiMessageTest.kt`. Add: `session_wake_frame_matches_doc`: `Frame{target:0xF002,id:0xCB1B,ty:0x2B0040,payload:vec![4,0]}.encode()` equals hex `550f04a202f01bcb40002b04009ab9`; `session_keepalive_frame_matches_doc` with payload `[1,1]` equals `550f04a202f01bcb40002b0101abd6`; `decode_rejects_bad_crc`, `decode_rejects_wrong_length`; `scan_frames_finds_frame_with_length_over_255` (build a 300-byte-payload frame, wrap in junk bytes, assert found); `scan_frames_skips_false_sof` (a stray `0x55` inside a payload does not yield a phantom frame).
- [ ] **Step 2:** `cargo test -p osmo-core duml` fails (modules missing).
- [ ] **Step 3:** Implement the signatures above. Frame layout: `55, len_lo, (ver<<2)|len_hi, crc8(first 3), target LE, id LE, ty 24-bit LE, payload, crc16 LE over everything before it`; encode writes `13 + payload.len()` in the length field with version 4 for BLE.
- [ ] **Step 4:** `cargo test -p osmo-core duml` passes.
- [ ] **Step 5:** Commit `feat(duml): frame codec, CRCs, scanner`.

---

### Task 3: Command builders (pairing, wifi getters, session, device info)

**Files:**
- Create: `osmo-core/src/duml/commands.rs`
- Reference: `$OSMOSIS/.../duml/{OsmoCommands,Payloads}.kt`, test `SessionCommandsTest.kt`; `MEDIA_PROTOCOL.md` §21-26

**Interfaces:**
- Consumes: `Frame`, `pack_string` (Task 2).
- Produces:
  - `pub const DEFAULT_IDENTIFIER: &str = "284ae5b8d76b3375a04a6417ad71bea3"`; `pub fn new_install_identity() -> String` (random UUID text truncated to 32 chars)
  - Target constants `TARGET_APP_TO_WIFI = 0x0702`, `TARGET_APP_TO_CAMERA = 0x0102`, `TARGET_APP_TO_SESSION = 0xF002`, `TARGET_APP_TO_1C = 0x1C02`
  - `pub fn set_pairing_pin(pin: &str, identifier: &str, id: u16) -> Vec<u8>`
  - `pub fn wifi_query(cmd_id: u8, payload: &[u8], id: u16) -> Vec<u8>` (type `0x40 | 0x07<<8 | cmd<<16`)
  - `pub fn command(set: u8, cmd: u8, payload: &[u8], target: u16, id: u16) -> Vec<u8>`
  - `pub fn session_ping(payload: &[u8], id: u16) -> Vec<u8>`; `pub const SESSION_WAKE: [u8;2] = [4,0]`, `SESSION_KEEPALIVE: [u8;2] = [1,1]`; `pub fn session_5310(id: u16) -> Vec<u8>`; `pub const APP_DEVICE_INFO: [u8; 62]` (`00 41 50 50` + 37 zeros + `02` + 8 zeros + `02 08` + 10 zeros; assert length in a test)
  - `pub const CMD_WIFI_SSID: u8 = 0x07`, `CMD_WIFI_PASSWORD: u8 = 0x0e`, `CMD_WIFI_MAC: u8 = 0x0c`

- [ ] **Step 1: Failing tests** with exact doc vectors (all ids `0xA000`): `set_pairing_pin("osmo", DEFAULT_IDENTIFIER, 0xA000)` = `553304c2020700a0400745203238346165356238643736623333373561303461363431376164373162656133046f736d6f8c02`; `wifi_query(CMD_WIFI_SSID, &[], 0xA000)` = `550d0433020700a04007077472`; password = `550d0433020700a040070eb5ef`; mac = `550d0433020700a040070ca7cc`; `new_install_identity().len() == 32` and two calls differ; plus the tests in `SessionCommandsTest.kt` ported.
- [ ] **Step 2:** Run, see FAIL.
- [ ] **Step 3:** Implement per signatures. `command` type is `0x40 | set<<8 | cmd<<16`.
- [ ] **Step 4:** Run, PASS.
- [ ] **Step 5:** Commit `feat(duml): command builders`.

---

### Task 4: BLE advert decode and the Action 4 model profile

**Files:**
- Create: `osmo-core/src/ble/mod.rs`, `advert.rs`, `model.rs`
- Reference: `$OSMOSIS/.../ble/{BleAdvert,BleConstants,Brand,CameraModel}.kt`

**Interfaces:**
- Produces:
  - `pub const SERVICE_FFF0: &str`, `CHAR_FFF4`, `CHAR_FFF5` (full 128-bit UUID strings), `DJI_COMPANY_IDS: [u16; 3] = [0x08AA, 0xF7AA, 0xE5C0]`, `pub fn is_dji_company_id(id: u16) -> bool`
  - `pub struct Advert { pub model_id: Option<u16>, pub new_format: bool, pub raw_product_type: Option<u16> }`, `pub fn decode_advert(payload: &[u8]) -> Advert` (payload has the company id already stripped; new-format flag is bit 2 of byte 5, product type is u16 LE at bytes 10..12, product type 203 maps to model `0x0014`)
  - `pub fn model_name(id: u16) -> Option<&'static str>` for the full `MODEL_NAMES` table
  - `pub struct CameraModel { pub name: &'static str, pub datalink_port: u16, pub tcp_poke: bool, pub wpa3: bool, pub verified: bool }`, `pub fn resolve_model(id: Option<u16>) -> CameraModel` (Action 4 = verified, 9004, poke, WPA2; unknown ids fall back to 9004, poke, WPA2, unverified)

- [ ] **Step 1: Failing tests:** classic payload `[0x14,0x00,...]` decodes to `0x0014`; the Pocket 4 Pro payload from the `BleAdvert.kt` doc comment (`00 00 00 ee 00 04 bd 6e 56 20 da 00 00 10`) decodes to model `0x0022`, `new_format == true`, `raw_product_type == Some(218)`; a new-format payload with product type 203 gives `0x0014`; payload shorter than 2 bytes gives `model_id == None`; all-zero classic id gives `None`; `resolve_model(Some(0x0014))` has port 9004, `tcp_poke`, `verified`; `resolve_model(None)` is unverified with the defaults.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implement; copy the `MODEL_NAMES` and `PRODUCT_TYPE_TO_MODEL_ID` tables exactly.
- [ ] **Step 4:** PASS; `cargo test --workspace` green.
- [ ] **Step 5:** Commit `feat(ble): advert decoding, Action 4 profile`; merge `feat/protocol-basics` into `main` with `--no-ff`.

---

### Task 5: BLE pairing and WiFi-credential session (transport-agnostic)

**Files:**
- Create: `osmo-core/src/ble/link.rs`, `reassembler.rs`, `session.rs`; branch `feat/ble-pairing`
- Reference: `$OSMOSIS/.../stream/DumlReassembler.kt`, `stream/RtmpStreamer.kt` (`pair`, `request`, `onNotification` auto-reply), `ble/GattClient.kt`, `MEDIA_PROTOCOL.md` "Waking a sleeping camera" and §22-26

**Interfaces:**
- Consumes: `Frame`, `commands::*` (Tasks 2-3).
- Produces:
  - `pub trait BleLink { fn write_command(&mut self, bytes: &[u8]) -> Result<(), LinkError>; fn recv(&mut self, timeout: Duration) -> Result<Option<Vec<u8>>, LinkError>; }` (raw notification fragments from `fff4`/`fff5`; the impl has already armed pairing and enabled notifications)
  - `pub struct Reassembler`, `pub fn feed(&mut self, fragment: &[u8]) -> Vec<Frame>` (port `DumlReassembler`)
  - `pub struct WifiCreds { pub ssid: String, pub password: Option<String>, pub mac: Option<[u8; 6]> }`
  - `pub struct PairOptions { pub identifier: String, pub pin: String, pub approve_timeout: Duration }` with `Default` (pin `"osmo"`, `approve_timeout` 60 s, identifier `DEFAULT_IDENTIFIER`)
  - `pub fn pair_and_read_wifi(link: &mut dyn BleLink, opts: &PairOptions, on_step: &mut dyn FnMut(&str)) -> Result<WifiCreds, PairError>`
  - `pub enum PairError { NotApproved, NoReply(&'static str), Link(LinkError) }`

- [ ] **Step 1: Failing tests** using a scripted `FakeLink` (queue of replies keyed on the command written):
  - `happy_path_returns_creds`: replies `0x07/0x45 [00 01]` (already paired), then SSID/password/MAC replies; assert `WifiCreds` fields and that the write order is wake `0x00/0x2b 04 00`, pair, keepalive, `0x07/0x07`, `0x07/0x0e`, `0x07/0x0c`, with no two writes closer than 100 ms in a fake clock.
  - `approval_flow`: `0x07/0x45 [00 02]` then an inbound `0x07/0x46` **request** (flags `0x40`); assert the session writes a response frame (flags `0xC0`, target bytes swapped, same id) and proceeds.
  - `inbound_00_81_gets_app_device_info_reply`.
  - `keepalive_sent_about_once_per_second` while waiting for approval.
  - **Review Focus 3:** `never_approved_times_out`: no `0x07/0x46`, `approve_timeout` = 200 ms in the test, returns `PairError::NotApproved` and does not loop forever.
  - `password_missing_is_none_not_error` (Nano-style empty password reply).
  - `reassembler_joins_fragmented_frame`, `reassembler_handles_two_frames_in_one_fragment`.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implement as a single-threaded loop: keepalive is emitted from the receive loop by elapsed time, no threads. Requests are matched on `(cmd_set, cmd_id)` with the response flag `0x80` set, retrying each step twice (8 s step timeout, injectable for tests). Mirror `RtmpStreamer.pair`'s status handling (`0x01` already paired, `0x02` wait for `0x07/0x46`).
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(ble): pairing and wifi-credential session`.

---

### Task 6: btleplug adapter and CLI `scan`, `pair`, `wifi`; HW-1

**Files:**
- Create: `osmo-cli/src/{main.rs,ble_btleplug.rs,config.rs}`

**Interfaces:**
- Consumes: `BleLink`, `pair_and_read_wifi`, `decode_advert`, `resolve_model`.
- Produces: CLI `osmo scan [--seconds 8]`, `osmo pair [--device <id|name-substring>]`, `osmo wifi [--device ...]`. `config.rs`: `pub fn load_or_create_identity() -> io::Result<String>` (file in `directories::ProjectDirs` config dir, created once), `pub fn save_last_device(id: &str)`, `pub fn last_device() -> Option<String>`.
- `ble_btleplug.rs`: `pub struct BtleplugLink` implementing `BleLink` by bridging blocking calls to a tokio runtime; `connect(peripheral_id) -> Result<BtleplugLink>` requests MTU where the platform allows, discovers `fff0`, subscribes to `fff4` and `fff5`, writes `01 00` to `fff4`.

- [ ] **Step 1: Failing tests** for the pure parts only: `identity_is_stable_across_calls` (temp config dir via env override), `identity_is_32_chars`, `scan_filter_accepts_dji_company_id_and_osmo_name_rejects_others`. Live BLE is hardware-only.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implement. **Review Focus 5:** when no adapter exists or the scan call errors, print "Bluetooth unavailable: check Bluetooth is on and this terminal has Bluetooth permission (System Settings, Privacy and Security, Bluetooth)" and exit 1. `scan` prints name, id, RSSI, model name per camera and counts (not names) other devices. `wifi` prints SSID and password to stdout only; nothing else prints credentials.
- [ ] **Step 4:** `cargo test --workspace` and `cargo build --release` pass.
- [ ] **Step 5:** Commit `feat(cli): scan, pair, wifi`.
- [ ] **Step 6 (HW-1, needs the camera):** Tell the user: "Power on the Action 4, keep it next to the Mac, and allow Bluetooth for your terminal." Run `osmo scan`, expect a camera line with model `OsmoAction4`. Run `osmo pair` and have the user accept the prompt on the camera screen. Run `osmo wifi` and confirm the SSID matches what the camera shows. Append results (live-verified vs not, any surprises, redacted) to `docs/hardware-log.md`. Fix and re-run until it works, then commit `docs: HW-1 results` and merge `feat/ble-pairing` into `main` with `--no-ff`.

---

### Task 7: Datalink transport (pure parts: headers, sequencing, ack)

**Files:**
- Create: `osmo-core/src/datalink/mod.rs`, `transport.rs`; branch `feat/datalink-ls`
- Reference: `$OSMOSIS/.../net/DumlTransport.kt` (read the doc comments: they explain the window rules), test `app/src/test/.../net/DumlTransportTest.kt`, `MEDIA_PROTOCOL.md` "Datalink transport / sequencing"

**Interfaces:**
- Produces:
  - `pub trait Datagram { fn send(&mut self, pkt: &[u8]) -> io::Result<()>; fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>>; }`
  - `pub struct UdpPeer` implementing `Datagram` over `std::net::UdpSocket` (200 ms read timeout, send to `ip:port`)
  - `pub fn udp_header(pkt_type: u8, payload_len: usize, session: u16, seq: u16) -> [u8; 8]` (`w0 = 1<<15 | (8+len)&0x3FFF` LE, session LE, seq LE, pkt_type, then XOR of the first seven bytes)
  - `pub fn routing_header(seq: u16, cmd_counter: u8, drone: bool) -> [u8; 12]` (`ack = seq-8`)
  - `pub struct Transport<D: Datagram>` with `new(d: D, base_seq: u16, session: u16)`, `handshake(&mut self, payload: &[u8], attempts: u32) -> Option<Vec<u8>>`, `send_duml(&mut self, set: u8, cmd: u8, payload: &[u8], recv_type: u8, recv_id: u8, cmd_type: u8)`, `send_ack(&mut self)` (34-byte pktType 4 with video, download, control groups), `recv_all(&mut self, dur: Duration) -> Vec<Vec<u8>>` (latches the peer channel from bytes 8-9 and cursors from 34-byte pktType-1 frames at `[10:12]` and `[18:20]`), `sync_seq_to_peer_channel(&mut self)`

- [ ] **Step 1: Failing tests:** port every assertion in `DumlTransportTest.kt` (header layout, XOR byte, routing header ack = seq-8, `scan_frames` cases are already covered in Task 2); add `ack_echoes_download_cursor` (feed a 34-byte pktType-1 frame with `[18:20] = 0x1234`, then `send_ack` payload group 2 start/end equal `0x1234`) and `seq_advances_by_8_per_sent_packet`; use a `FakeDatagram` recording sends.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implement exactly as the Kotlin comments describe: `send_ack` carries seq 0; the control group stays at `base_seq`; session id random in `0x1000..0xFFFE`; base seq random in `0x1000..0xF000` and 8-aligned.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(datalink): transport headers, sequencing, ack`.

---

### Task 8: Manifest stream reassembly

**Files:**
- Create: `osmo-core/src/datalink/stream.rs`
- Reference: `$OSMOSIS/.../camera/CameraSession.kt` lines 1343-1545 (`countMediaPaths`, `manifestBytes`, `chunkTally`, `streamsEnded`, `distinctPaths`) and 1480-1484 (`hasEndMarker`)

**Interfaces:**
- Produces:
  - `pub fn manifest_bytes(raw: &[u8], request_ctr: Option<u8>) -> Vec<u8>` (select `0x00/0x27` frames by DUML command, require sub-header `4A 01`, drop the 10-byte sub-header, concatenate in arrival order; with `request_ctr` filter on sub-header byte 4; without it, fall back to `raw` if reassembly finds fewer media paths)
  - `pub fn count_media_paths(bytes: &[u8]) -> usize`
  - `pub fn chunk_tally(raw: &[u8]) -> BTreeMap<u8, BTreeMap<u8, u32>>`, `pub fn streams_ended(raw: &[u8], required: &[u8]) -> bool`
  - `pub fn has_end_marker(bytes: &[u8]) -> bool` (`0c 01 0d`)

- [ ] **Step 1: Failing tests** on fixtures: for `oa4_45.bin`, `count_media_paths(&manifest_bytes(raw, None)) == 45`; `has_end_marker` is false for `oa4_45.bin` and true for `oa4_6.bin`; `streams_ended` is true for `oa4_6.bin` with required `[1, 2]` when both counters closed (check by `chunk_tally` first and assert what you observe, matching the Kotlin test expectations in `ManifestRobustnessTest.kt` lines 150-170); **Review Focus 1:** `empty_store_stream_is_closed` using a synthetic blob containing only a `4A 04` start frame under counter 1 and a closed data stream under counter 2, expecting `streams_ended(.., &[1,2]) == true` and `manifest_bytes(raw, Some(1))` empty; `parameter_push_chunks_are_ignored` (a `4A 01` payload under cmd `0x00/0x99` must not enter the manifest).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implement per the signatures, walking frames one byte at a time with the same length/CRC rules as `scan_frames`.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(datalink): manifest stream reassembly`.

---

### Task 9: CompositePack decoder and CameraFile

**Files:**
- Create: `osmo-core/src/media.rs`, `osmo-core/src/datalink/manifest.rs`
- Test: `osmo-core/tests/golden.rs`
- Reference: `CameraSession.kt` lines 1610-1780 (`decodeManifest`, `readPathField`, `decodeComposite`, `listBoundary`) and `resolveRecord` (same file, search for it), `core/CameraFile.kt`, `camera/PathAddressing.kt`, `core/StorageRules.kt`

**Interfaces:**
- Consumes: `manifest_bytes`, `count_media_paths` (Task 8).
- Produces:
  - `pub struct CameraFile { pub path: String, pub thumb_path: String, pub name: String, pub ext: String, pub storage: u8, pub size_bytes: u64, pub handle: u32, pub group: u8 }`
  - `impl CameraFile { pub fn url_path(&self) -> String }` returning `/v2?storage={storage}&path={path}` exactly (no percent-encoding)
  - `pub fn decode_manifest(raw_blob: &[u8]) -> Vec<CameraFile>` (reassemble then decode records; media-path field `1a [total] 00 00 00 01 "DCIM/..."`, thumbnail `... 02 "MISC/..."`, filename `0d [len]`, size and handle from the `03 ff 19 06` marker per the Kotlin comments; two concatenated lists split by `listBoundary`; dedupe by path)
  - `pub fn collect_stores(raw_blob: &[u8], sd_ctr: u8, internal_ctr: u8) -> Vec<CameraFile>` (decode each counter's slice, stamp `storage` 0 = SD and 1 = internal, fall back to the merged decode when neither slice has records or both are identical)

- [ ] **Step 1: Failing tests** in `tests/golden.rs`: for every `*.bin` that has a `golden/<name>.golden.txt`, decode with `decode_manifest` and compare the `name|ext|size` columns (golden columns 0, 1, 4: `name|ext|handle|cmdHandle|size|resolution|resLabel|duration|starred|mediaType|group|deletable`), sorted, to the golden lines; assert record counts equal. Columns beyond name, ext and size are out of scope for the prototype; note that in the test's doc comment. Additional tests: `url_path_for_sd_file` (`/v2?storage=0&path=DCIM/DJI_001/DJI_20260903165023_0012_D.JPG` for the first `oa4_45` file), `empty_blob_decodes_to_empty`, `truncated_blob_does_not_panic` (cut `oa4_45.bin` at 5000 bytes: returns fewer files, no panic).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Port the decoder; keep it panic-free on arbitrary bytes (bounds-check every read).
- [ ] **Step 4:** `cargo test -p osmo-core --test golden` passes for all 14 fixtures.
- [ ] **Step 5:** Commit `feat(media): CompositePack decoder with golden tests`.

---

### Task 10: Datalink session (open, register, playback, list)

**Files:**
- Create: `osmo-core/src/datalink/session.rs`
- Test: `osmo-core/tests/session_fake.rs`
- Reference: `net/DumlSession.kt` (`openDatalink`, `handshakeFrame`), `camera/CameraSession.kt` lines 200-300 (`openAndRegister`, `fetchFileList`, `queryNewestPage`, `retrySdIfNotReady`, `listCmd`), 814-860 (`enterPlaybackConfirmed`, `parseStatus` at 1108 for the playback bit), 2220-2321 (`appDeviceInfo`, `syncTime`, `subscription`), `MEDIA_PROTOCOL.md` §1, §4-8 and "Holding playback mode"

**Interfaces:**
- Consumes: `Transport`, `Datagram`, `decode_manifest`/`collect_stores`, `streams_ended`, `scan_frames`.
- Produces:
  - `pub struct SessionConfig { pub ip: String, pub port: u16, pub tcp_poke: bool }` with `SessionConfig::action4(ip)`
  - `pub struct CameraSession<D: Datagram>`; `pub fn open(cfg: &SessionConfig) -> Result<CameraSession<UdpPeer>, SessionError>` and `pub fn with_datagram(d: D, tcp_poke: false, ..) -> CameraSession<D>` for tests
  - `impl<D: Datagram> CameraSession<D> { pub fn list_newest(&mut self) -> Result<Vec<CameraFile>, SessionError>; pub fn keepalive_tick(&mut self); pub fn close(self) }`
  - `pub enum SessionError { NoHandshake { addr: String }, Io(io::Error) }`, whose `Display` for `NoHandshake` reads: `no response from {addr}: is this machine joined to the camera's WiFi?`

- [ ] **Step 1: Failing tests** with a `FakeDatagram` that replays fixture bytes when it sees the `0x00/0x26` query:
  - `lists_oa4_newest_page_from_fixture`: handshake reply is a pktType-0 datagram; after the list queries, replay `oa4_45.bin` split into the datagrams it was captured from (split raw blob at frame boundaries, one frame per datagram); assert 45 files, first name `DJI_20260903165023_0012_D.JPG`; assert the writes include handshake with the randomised base in the first two payload bytes, registration (`0x00/0x81`, `0x00/0x88`, `0x03/0xDA`), eight `0x00/0x99` subscriptions, playback `0x02/0x0c` `01 01 00 01`, the two `0x00/0x26` queries (counters 1 and 2) and the trigger frame `4a040e1001000000000001000000`.
  - **Review Focus 1:** `empty_store_returns_empty_list_without_error` (only a `4A 04` start under each counter): returns `Ok(vec![])` within a bounded number of receive batches.
  - **Review Focus 2:** `no_handshake_error_names_address` (fake never answers; 20 attempts, `NoHandshake`, message contains `192.168.2.1:9004`).
  - `empty_sd_is_requeried_once` (first SD answer empty, second has records).
  - `tcp_poke_bytes_are_set_pairing_pin_osmo` (assert the exact bytes via an injected poke closure; do not open a real socket).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implement the flow: poke (if configured) then handshake, drain 5 batches with `send_ack`, `sync_seq_to_peer_channel`, register, enter playback (confirm via the `0x02/0x80` bit 30, then try `0x01/0x01` as the Pocket 3 route only if refused; the Action 4 only needs the first), wait about 1.7 s before the first store query (injectable to 0 in tests), query SD ctr 1 cursor `0x00000001` and internal ctr 2 cursor `0x40000001`, collect up to 15 batches of 800 ms with `send_ack` and the `0x00/0x88` presence beat each batch, stop on `streams_ended`, or after 2 stable batches once past batch 4. Skip `syncTime`.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(datalink): session open, register, list`; merge `feat/datalink-ls` into `main` with `--no-ff`.

---

### Task 11: HTTP client and resumable download

**Files:**
- Create: `osmo-core/src/http.rs`; branch `feat/http-get-cli`
- Test: `osmo-core/tests/http.rs`
- Reference: `$OSMOSIS/.../net/HttpClient.kt`, `net/MediaDownloader.kt` (`downloadOne` loop and constants)

**Interfaces:**
- Produces:
  - `pub struct HttpClient { pub ip: String }`, `pub fn new(ip: &str) -> Self`, `pub fn url(&self, path: &str) -> String` (`http://{ip}{path}`), `pub fn head(&self, path: &str) -> Option<u64>` (Content-Length)
  - `pub enum Fetch { Done, Interrupted, RangeIgnored, Failed }`
  - `pub fn download(&self, path: &str, out: &mut dyn Write, range_start: u64, on_progress: &mut dyn FnMut(u64)) -> Fetch` (new connection per request with `Connection: close`; `Range: bytes={n}-` when `n > 0`; 200 to a ranged request returns `RangeIgnored` without writing)
  - `pub fn download_resumable(&self, path: &str, dest: &Path, remote_size: Option<u64>, on_progress: &mut dyn FnMut(u64)) -> Result<Outcome, DownloadError>`; `Outcome { Saved, AlreadyComplete }`; constants `MAX_RESUMES = 20`, `MAX_BARREN = 5`, `RESUME_BACKOFF = 750 ms * barren` (injectable to 0 in tests)

- [ ] **Step 1: Failing tests** against a `tiny_http` server on localhost (bind `127.0.0.1:0`; `HttpClient` takes `ip:port` for tests via a `with_base` constructor): `downloads_whole_file`; `resumes_after_cut` (server drops the connection after 1 KB, second request gets `Range: bytes=1024-` and 206); `range_ignored_restarts_from_zero` (server answers 200; file ends byte-identical, not spliced); `busy_404_then_500_then_ok` (transient statuses retried, finishes); `gives_up_after_five_barren_attempts`; **Review Focus 4:** `existing_partial_file_is_resumed`, `existing_complete_file_reports_already_complete` (dest length equals `remote_size`; no GET issued, assert via server request count).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implement with `ureq` agents built per request; open the destination for append/seek at the resume offset; flush on error so partial data survives; truncate only on `RangeIgnored`. Constants and loop semantics as in `MediaDownloader.downloadOne`.
- [ ] **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(http): resumable download`.

---

### Task 12: CLI `ls` and `get`, docs; HW-2

**Files:**
- Modify: `osmo-cli/src/main.rs`; Create: `docs/protocol.md`, `docs/hardware-log.md` (if absent), update `README.md`

**Interfaces:**
- Consumes: `CameraSession`, `SessionConfig::action4`, `HttpClient`, `CameraFile::url_path`.
- Produces: `osmo ls [--ip 192.168.2.1] [--port 9004]` printing `storage  size  path` per file; `osmo get <path> [-o <dir>] [--ip ...]`.

- [ ] **Step 1: Failing tests** (CLI logic as pure functions in `osmo-cli/src/lib.rs` or module with tests): `format_listing_aligns_columns`; `get_rejects_path_not_starting_with_dcim` (error before any network use); `get_derives_filename_from_path`; `route_warning_when_ip_not_on_local_subnet` (given interface address list, `192.168.2.1` absent gives the warning text; present gives none).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implement. **Review Focus 2:** on `NoHandshake`, print its message and a hint to join the camera's WiFi (SSID from `osmo wifi`), exit 1. `ls` with zero files prints `no files` and exits 0. `get` infers `storage` by looking the path up in a fresh `ls` unless `--storage N` is given (default 0); it goes straight to HTTP. If hardware shows the AP dropping while idle, add a `keepalive_tick` loop during the download (decide from HW-2, record in the log).
- [ ] **Step 4:** `cargo test --workspace` passes; `cargo clippy --workspace` has no warnings; `cargo build --release` works.
- [ ] **Step 5 (HW-2, needs the camera):** Tell the user: "Join your Mac to the Action 4's WiFi using the SSID and password from `osmo wifi`. Camera on, SD card in with some clips." Run `osmo ls`: expect your files with sizes. Run `osmo get <a small photo path>` and compare size with `ls`. Interrupt a large video download with Ctrl-C and re-run `osmo get` to confirm resume. Record each result as live-verified or not in `docs/hardware-log.md`; note the empty-card and wrong-WiFi cases only if the user can try them.
- [ ] **Step 6:** Write `docs/protocol.md` (the pipeline, the sequence numbers and quirks this port relies on, with pointers back to Osmosis `MEDIA_PROTOCOL.md`), update README usage, commit `docs: protocol notes, usage, hardware log`; merge `feat/http-get-cli` into `main` with `--no-ff` and tag `v0.1.0-proto`.
