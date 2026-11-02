# Protocol notes

What `osmo-core` does on the wire, and which Osmosis source each piece was ported from. The full
reference is Osmosis's `MEDIA_PROTOCOL.md` and `docs/01-protocol-map.md`
(<https://github.com/KonradIT/osmosis>); this file only records what this port relies on.

## Pipeline

BLE pair -> read WiFi credentials -> (user joins the camera's WiFi) -> TCP-7001 poke -> UDP datalink
(media list) -> HTTP download.

| Stage | Transport | Module | Ported from |
|---|---|---|---|
| Frame codec, CRCs, scanning | any | `duml` | `DjiMessage.kt`, `DjiCrc.kt`, `DumlTransport.scanFrames` |
| Command builders | BLE `fff5` | `duml::commands` | `OsmoCommands.kt`, `Payloads.kt` |
| Advert decode, model profile | BLE adverts | `ble::advert`, `ble::model` | `BleAdvert.kt`, `CameraModel.kt` |
| Pairing, WiFi creds | BLE GATT (`BleLink`) | `ble::session` | `RtmpStreamer.pair`, `GattClient.kt` |
| UDP transport, ack window | UDP 9004 (`Datagram`) | `datalink::transport` | `DumlTransport.kt` |
| Manifest reassembly + decode | n/a (pure) | `datalink::stream`, `datalink::manifest` | `CameraSession.kt` (`manifestBytes`, `decodeComposite`, `resolveRecord`) |
| Session, playback, list | UDP | `datalink::session` | `DumlSession.kt`, `CameraSession.fetchFileList` |
| File download | HTTP `/v2` | `http` | `HttpClient.kt`, `MediaDownloader.downloadOne` |

## Things that bite (all ported, all covered by tests)

- **BLE:** write DUML to `fff5` (write-without-response), notify on `fff4`/`fff5`, arm pairing with `01 00` on
  `fff4`. Space writes by at least ~100 ms. The camera drops an idle link in ~6 s: send `0x00/0x2b 01 01` about once
  a second. Approval arrives as a `0x07/0x46` *request* that must be answered.
- **Pairing identity:** 32 chars, minted once and persisted. A new one per run re-prompts on the camera.
- **Datalink sequencing:** every packet carries `[ack = my previous seq][my seq]` in our own sequence space, and
  the 34-byte window ack echoes the peer's own cursors. Wrong values here silently drop writes while reads keep
  working (two historical outages in Osmosis).
- **Manifest:** select reply chunks by DUML command `0x00/0x27`, never by the `4A 01` payload prefix (parameter
  pushes share it). A store that is not mounted yet answers empty: wait ~1.7 s after playback and re-ask the SD once.
- **HTTP:** the camera cuts long transfers about every 10 s and answers 404/500 when merely busy. Resume with a
  ranged request, back off between attempts, restart from zero if a ranged request is answered 200.

## Not implemented (by design, for now)

Time sync, delete, favourite, burst/highlights, thumbnails, trimmed download, RTMP livestream, GPS sync, drones,
the `0x01/0x01` playback route, paging beyond the newest 45 files per store, Xtra rebadges, auto WiFi join.
