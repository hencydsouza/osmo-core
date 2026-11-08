# CLI reference

Global: `-v` logs protocol events to stderr. Paths are exactly as `osmo ls` prints them. Times are seconds, `m:ss` or `h:mm:ss`.

## Camera / pairing
| Command | What |
|---|---|
| `scan` | Find cameras over Bluetooth |
| `cameras [--scan]` | List remembered cameras |
| `pair [--device ID]` | Pair; approve on camera |
| `wifi [--device ID]` | Print Wi-Fi SSID/password |
| `with-wifi [--return-to NET] -- <cmd>` | macOS: join camera Wi-Fi, run `osmo <cmd>`, rejoin |

`--device` takes an id or a name substring that matches exactly one camera.

## Media (need the camera's Wi-Fi)
| Command | What |
|---|---|
| `ls [--all] [--page-size N] [--photos\|--videos] [--starred]` | List media. `--all` pages through everything |
| `get <path>... [-o DIR] [--proxy] [--storage N] [--no-keepalive]` | Download, resuming partial files; several paths in one go, a failure does not stop the rest |
| `thumb <path> -o FILE` | Camera thumbnail |
| `trim <video> --from T --to T -o DIR` | Lossless slice, no re-encode, no ffmpeg. Cut starts at the keyframe at or before `--from` |
| `frame <video> --at T -o DIR` | One full-res JPEG (needs ffmpeg) |
| `star <path>` | Favourite (`--off` does not work on Action 4) |
| `rm <path>... [--yes]` | Delete. Dry run without `--yes`. Irreversible |

Notes:
- `get --proxy` fetches the small low-res copy (about 9x smaller).
- `get` holds the datalink open so the camera's Wi-Fi does not sleep on long transfers; `--storage 0|1` (SD / internal) skips the datalink entirely and `--no-keepalive` turns the hold off.
- `rm` is all-or-nothing: any typo/glob/unverified handle refuses everything and sends nothing; it re-lists on a fresh session to confirm the files are gone.
- Burst/interval leads show `[burst]` in `ls`; fetch expanded frames with `get --storage 0 <path>`.

## Other
| Command | What |
|---|---|
| `stream` | RTMP livestream (`OSMO_RTMP_URL` in `.env`), Ctrl-C stops. Run directly, not via `with-wifi` |
| `gps` | Send GPS fixes from JSON lines on stdin. Not hardware-tested |
| `marks <video>` | Highlight marks. Unverified on Action 4 |
| `burst <lead>` | List burst frames. Unverified |
| `sync-time` | Does not work on Action 4 (camera answers 0xe0) |

Config: `.env` in the working directory (gitignored; contains plaintext Wi-Fi passwords). See `.env.example`.
Status of every feature: [hardware-log.md](hardware-log.md), [parked.md](parked.md).
