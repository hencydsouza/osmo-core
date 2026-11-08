# Getting started (CLI)

`osmo` talks to a DJI Osmo Action 4. Bluetooth wakes it and reads the Wi-Fi password; media moves over the
camera's own Wi-Fi (it has no internet, so your computer is offline while joined).

## Install
```sh
cargo build --release          # binary: target/release/osmo
```
macOS: give your terminal Bluetooth permission (System Settings > Privacy & Security > Bluetooth).
`ffmpeg` is only needed for `osmo frame`; everything else is self-contained.

## First run
1. Camera on. `osmo scan` finds it.
2. `osmo pair`, approve on the camera screen. The camera is remembered.
3. `osmo wifi` prints SSID and password. Join that Wi-Fi.
4. `osmo ls` lists the newest media.

## macOS: skip the manual Wi-Fi dance
Copy `.env.example` to `.env`, set `OSMO_RETURN_TO` (your home network) and its password, then run from that directory:
```sh
osmo with-wifi -- ls --all
osmo with-wifi -- get DCIM/100MEDIA/DJI_..._0001_D.MP4 -o ~/Movies/osmo
```
It wakes the camera, joins its Wi-Fi, runs the command, then rejoins home (even on failure/Ctrl-C).
See [cli.md](cli.md) for every command.

## Troubleshooting
| Symptom | Fix |
|---|---|
| `no matching camera found` | Camera off or asleep; press its button, retry `osmo scan` |
| `no response from <ip>` | Not on the camera's Wi-Fi; run `osmo wifi`, join, retry |
| Empty/short `ls` | Camera not in playback mode yet; rerun, add `-v` |
| Download dropped | Rejoin Wi-Fi, rerun the same `get`; it resumes |
| `rm` refuses | Path typo, glob, or no verified handle; nothing was sent |
