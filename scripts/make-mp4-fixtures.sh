#!/usr/bin/env bash
# Regenerates osmo-core/tests/fixtures/*.mp4: small, synthetic, one video + one audio track each.
# Needs ffmpeg. Safe to re-run; overwrites existing fixtures.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=osmo-core/tests/fixtures
mkdir -p "$OUT"

# h264_aac.mp4: baseline case, moov at the end (ffmpeg's default), H.264 video + AAC audio,
# a keyframe every 10 frames (-g 10) so trim/keyframe-snap tests have more than one sync sample.
ffmpeg -y -f lavfi -i testsrc=size=160x120:rate=25 -f lavfi -i "sine=frequency=440" \
  -t 2 -g 10 -bf 2 -c:v libx264 -pix_fmt yuv420p -c:a aac \
  "$OUT/h264_aac.mp4"

# hevc_aac.mp4: same shape, HEVC video tagged hvc1 (as DJI cameras tag it) instead of H.264.
ffmpeg -y -f lavfi -i testsrc=size=160x120:rate=25 -f lavfi -i "sine=frequency=440" \
  -t 2 -g 10 -bf 2 -c:v libx265 -tag:v hvc1 -pix_fmt yuv420p -c:a aac \
  "$OUT/hevc_aac.mp4"

# faststart.mp4: moov moved to the front of the file (the opposite layout from the other two).
ffmpeg -y -f lavfi -i testsrc=size=160x120:rate=25 -f lavfi -i "sine=frequency=440" \
  -t 2 -g 10 -bf 2 -c:v libx264 -pix_fmt yuv420p -c:a aac -movflags +faststart \
  "$OUT/faststart.mp4"

ls -la "$OUT"
