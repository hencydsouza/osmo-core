# DJI Mimo 1.7.20 static analysis (2026-11-08)

Target: `1659945441726-DJI-Mimo-official-release-225631_sec(1.7.20…).apk`, SHA-256
`0a4c89db59a0f8bc8047c3ee2eb00999e66e30b7f3f6e6005311d57a8a0d8f7a`, package `dji.mimo`, version 1.7.20 (code 225631),
minSdk 24, targetSdk 30. Zip entries are dated 2022-06-21. Method: static only (unzip, aapt, dexdump, llvm-nm, strings).
Nothing was executed. Purpose: interoperability with the owner's own camera. No Mimo code or binary is stored in this repo.

## Observed

- **The Java code is hidden by a packer.** `classes.dex` is 77.5 MB but holds only 11 classes and 206 methods, all in
  `com.secneo.apkwrapper.*`. `libDexHelper.so`, `libDexHelper-x86.so`, `assets/libDexHelper32` and `libdexjni.so` ship with it.
  This is the SecNeo/Bangcle hardening product. The real app logic (activities, pairing flow, command wiring) is not readable
  statically. Getting it means dumping decrypted code from a running instrumented app; not done, owner's decision.
- **The native layer is readable.** 160 arm64 libraries. Key ones: `libdjisdk_jni.so` (31 MB, no dynamic symbols, 270 `JNI_*`
  entry points and about 1,086 `Action*` class-name strings in its strings), `libdjibase.so` (21 MB, 54,142 exported C++ symbols,
  626 `dji::sdk::*Msg` JSON message classes), `libilink_live.so` (RTMP publisher), `libdji_cross_playback.so`,
  `libilink_network.so`, `libDJIFileSystem.so`, `libDJIVideoJNI.so`.
- **Livestream = the phone publishes RTMP.** `libilink_live.so` contains `IlinkRtmpPublisher` built on an SRS RTMP client, with the
  string "only support rtmp plain text" (so `rtmp://` only, no `rtmps://`). JNI entry points: `ControlLiveStream`,
  `SetLiveStreamParam`, `SwitchLiveStreamMode`, `GetLiveStreamSettingsInfo`, `GetLiveStreamDeviceState`,
  `GetLiveStreamRunningState` and listeners for both states. There are no hard-coded `rtsp://` or camera-address strings.
- **Wi-Fi/BLE helpers.** `GetWifiSsid/Pwd/MacThroughBle`, `SetCountryCodeThroughBle`, `SetWiFiFrequencyBand`/`GetWiFiFrequencyBand`,
  `ConnectWiFi`, `GetWiFiPortFdSet`, `BleVerifyDevice`, `StartAwakeBleDevice`, `StartBleScan`.
- **Playback and files.** `StartPlayback{State,Video,Audio}Observer`, `StartFilelistStateObserver`, `GetFileListState`, a file task
  queue (`FileTask*`, `FileTransferList`, `FileBatchAction`, `FileQueryLocation`, `FileCache*`), `BatchZipFileManager`,
  `ActionEnterPlayback`/`ActionExitPlayback`/`ActionDeletePhoto`-style class names.
- **Activation:** `Activate*`/`DeActivate*` state, observer and data entry points; licence enable.
- **Time:** `JNI_SetTimeSyncRequestCallback` exists.

## Inferred (not proven)

- Mimo's livestream is phone-side: the camera's live view reaches the phone, which re-publishes RTMP. Osmosis (and osmo-core) instead
  tell the camera to push RTMP itself over BLE-configured Wi-Fi. Two different designs; the camera-direct one is what the Action 4 rejected
  with `0xD6` on the home network.
- Time sync may be camera-initiated: a callback named "time sync request" suggests the camera asks the app for the time and the app
  answers, rather than the app pushing a set-time. This could explain the Action 4's `0x00/0x6A` refusal (`0xe0`). Unverified.
- Playback observers imply Mimo can stream a clip's video/audio from the camera for in-app playback (a preview without a full download).
- The Wi-Fi frequency-band call suggests the camera's hotspot band can be switched from the app.

## Unknown

- Command set/ID bytes and payload layouts for any of the above: the native class names do not carry them. Getting them needs disassembly
  of specific functions (arm64) or a runtime capture.
- Anything in the Java layer (hidden by the packer).
- Whether Mimo 1.7.20 knows the Action 4 at all. The build is from June 2022; the Action 4 was released in August 2023, so its
  model-specific handling cannot be in this build. Treat every finding here as "family behaviour", to be verified on the Action 4.

## Candidate work, ranked by what the evidence supports

1. **Answer a camera time request** (test on the Action 4: watch the datalink for a camera-originated `0x00/0x6A`-like push after register).
2. **Wi-Fi band switch and country code over BLE** (names known; ids unknown).
3. **Playback streaming preview** (large; needs ids and a decoder on the app side).
4. **Phone-published RTMP** (needs the camera's live-view feed first; large).
