# osmo-core: design

Status: draft for approval. Date: 2026-11-03.

## Intent and why

Osmosis (github.com/KonradIT/osmosis, MIT) is an Android app that talks to DJI Osmo cameras with no DJI SDK:
BLE pairing, the camera's WiFi AP, a DUML-over-UDP datalink for the media list, and plain HTTP for file
download. All of that knowledge lives in Kotlin, welded to Android APIs.

The owner wants Android, Windows and Mac apps, and later features nobody has scoped yet (video editing,
Gyroflow-style stabilization, "much more"). So the camera-talking part must be a standalone, platform-agnostic
library, not code inside any one app. `osmo-core` is that library, plus a small CLI that proves it against real
hardware before any GUI exists.

## Goals

1. A Rust library (`osmo-core`) that implements the camera protocol, with all OS-specific I/O behind interfaces.
2. A CLI (`osmo`) that, against a real **DJI Osmo Action 4** (model byte `0x0014`, datalink UDP 9004, WPA2, TCP-7001
   poke, all verified in Osmosis), can:
   - `scan`: find the camera over BLE
   - `pair`: pair (PIN `"osmo"`, user approves on the camera screen) and persist the app identifier
   - `wifi`: read the camera's WiFi SSID and password over BLE
   - `ls`: with the machine already joined to the camera's AP, open the datalink and list media files
   - `get <file>`: download one file over HTTP, resumable, with progress
3. The protocol layer is testable offline: `cargo test` passes with no camera, using Osmosis's golden manifest
   fixtures and known-good frame test vectors.
4. Git history and docs a stranger can follow: feature branches, a protocol doc ported from Osmosis, an
   attribution/licence note.

## Non-goals (this prototype)

- Any GUI, including the Mac app.
- Auto-joining the camera's WiFi. The user joins it by hand. This is the one truly OS-specific step and gets
  an interface only.
- RTMP livestream control, GPS sync, delete, favourite, highlights, burst expand, thumbnails, trimmed download,
  time sync, drones, or any model other than the Action 4.
- Bindings (UniFFI/JNI/Swift). The API should not make them hard later; none are built now.
- Editing or Gyroflow features. They are downstream consumers of downloaded files, not part of this crate.

## Anti-goals (a failure even if it technically works)

- Android or Mac types leaking into the core API.
- Protocol knowledge re-derived from guesswork instead of ported from Osmosis's documented, hardware-verified behaviour.
- Committing WiFi credentials, BLE/pcap captures, or device identifiers. They stay out of git.
- A "works on my camera once" demo with no tests behind it.

## Decisions made by the owner

- Language: Rust, for Android + Windows + Mac reuse and later Gyroflow proximity.
- First prototype scope: scan, pair, WiFi creds, list files, download one file (above).
- Manual WiFi join for now.
- Test camera: Osmo Action 4, in hand.
- Separate repo, `/Users/hency/Desktop/projects/dji-osmo/osmo-core`, with git branches and documentation.

## Constraints and facts the builder needs

Reference source of truth is the Osmosis checkout at `/Users/hency/Desktop/projects/dji-osmo/osmosis` (read-only):
`MEDIA_PROTOCOL.md`, `docs/01-protocol-map.md`, `app/src/main/java/dev/konraditurbe/osmosis/{duml,ble,net,camera}/`,
and golden manifests in `app/src/test/resources/manifests/` (including `oa4_45.bin`, `oa4_6.bin` for the Action 4).

Pipeline, from the Osmosis docs:
- BLE: service `fff0`; write DUML to `fff5` (write-without-response); notify on `fff4` and `fff5`; request MTU 517;
  arm pairing by writing `01 00` to `fff4`. Scan with no hardware filter; match DJI manufacturer ids
  (`0x08AA`, `0xF7AA`, `0xE5C0`) or the name. Model id is in the advert payload.
- DUML frame: `0x55`, 10-bit length + 6-bit version, CRC8 header, CRC16 trailer. BLE and UDP differ in message-id
  byte order. Osmosis's `OsmoCommands.kt` and `DjiMessage.kt` encode this.
- BLE sequence: session wake `0x00/0x2b` (`04 00`), then SetPairingPIN `0x07/0x45` (identifier is a 32-char
  per-install id, persist it), approval arrives as a `0x07/0x46` *request* that must be answered, then keepalive
  `0x00/0x2b 01 01` at about 1 Hz (camera drops an idle link in about 6 s), then GetWifiSsid `0x07/0x07` and
  GetWifiPassword `0x07/0x0e`, leaving a gap between writes.
- WiFi AP at `192.168.2.1`. Datalink: optional TCP-7001 poke, UDP handshake, sliding-window acks, routing headers,
  registration, then enter playback mode and query `0x00/0x26`; the reply is a chunked `0x00/0x27` stream that
  must be selected by DUML command, not payload prefix. Window and ack rules are documented in Osmosis's
  `DumlTransport.kt` comments and `MEDIA_PROTOCOL.md` "Datalink transport / sequencing".
- Manifest decode ("CompositePack") is the largest piece to port. The golden `.bin` fixtures are its test oracle.
- HTTP `/v2` file API: Range-resumable GET, `Connection: close`, treat 404/500 as transient ("busy") not fatal,
  handle a server answering 200 to a ranged request.

Layering (the builder's structure, but these seams are required):
1. Pure protocol: framing, CRCs, command builders, manifest decoder, state machines. No I/O, no async.
2. Transport interfaces: a BLE link (connect, write to `fff5`, notification stream) and a UDP/HTTP/network
   binder (join-AP is an interface; the CLI implements it as "assume already joined").
3. Desktop implementations and the CLI on top. Desktop BLE via the `btleplug` crate is the expected choice.

## Calls I made (owner can overrule)

- Repo layout: a Cargo workspace with `osmo-core` (library) and `osmo-cli` (binary `osmo`).
- Licence MIT with attribution to Konrad Iturbe for ported protocol knowledge and fixtures.
- Fixtures are copied into this repo (with attribution) so tests do not depend on the Osmosis checkout path.
- The app identifier is persisted in the user config dir, not the repo.
- Branch flow: `main` for docs, one feature branch per milestone, merged by PR/merge commit.

## Resolved after design review

- **Is the Osmosis material enough to port the datalink without captures?** Yes. The Kotlin source is a complete,
  hardware-verified implementation and `MEDIA_PROTOCOL.md` explains it. No capture step is planned. Port from the source.
- **TCP-7001 poke on the Action 4:** required. The Action 4 profile uses Osmosis's defaults (`tcpPoke = true`, port 9004).
  Port it: connect TCP to 7001 (1.2 s timeout), write the SetPairingPIN frame with PIN `"osmo"`, wait about 400 ms.
- **Test vectors:** they exist. Port `DjiCrcTest`, `DjiMessageTest`, `SessionCommandsTest`, `DumlTransportTest`
  and the `OsmoCommands` frame expectations, plus the DUML examples in `MEDIA_PROTOCOL.md`.
- **Multi-homed Mac (my call):** the CLI trusts OS routing to `192.168.2.1`. It warns if that address is not
  on the default interface's subnet. The core takes an optional bind-interface parameter but does not require one.
- **`get <file>` (my call):** `<file>` is the camera path exactly as `ls` prints it. `get` goes straight to HTTP,
  not through the datalink. If the Action 4 drops its AP when the datalink is idle, add a background keepalive
  to `get`; verify on hardware rather than assume.
- **`wifi` output (my call):** prints SSID and password to the terminal on request only. Never logged, never
  written to disk, never in test output.

## Left to the builder

Async runtime vs threads, HTTP and error-handling crates, module names, CLI argument library, exact trait shapes,
test organisation, CI.

## Risks

- Manifest decode and datalink windowing are intricate and were hard-won in Osmosis; expect the list step to take
  most of the effort.
- Playback-mode and store-mount timing quirks (about 1.7 s wait, re-ask empty store) exist and must be ported, not rediscovered.
- macOS requires a Bluetooth privacy grant for the terminal app running the CLI.
- Pairing needs a human approval tap on the camera. It cannot be automated in tests.
- Hardware validation is manual. Offline tests cannot prove the datalink works on a real Action 4.

## Success

On the owner's Action 4: `osmo scan`, `osmo pair`, `osmo wifi`, `osmo ls`, `osmo get` each work in turn, and
`cargo test` is green offline. Anything that only passed against fixtures is reported as fixture-tested, not live-verified.
