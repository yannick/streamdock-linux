#!/bin/sh
# Builds the decode benchmark input (H15-56): 3 minutes of 44.1 kHz stereo, pink noise under two
# tones, so the FLAC has a bitrate like real music (synthetic, no third-party audio). Needs ffmpeg.
# Usage: bench-input.sh out.flac   (or out.mp3, out.wav)
set -eu
out=${1:?usage: bench-input.sh out.flac}
ffmpeg -loglevel error -y \
  -f lavfi -i "anoisesrc=color=pink:amplitude=0.25:sample_rate=44100:duration=180:seed=56" \
  -f lavfi -i "sine=frequency=220:sample_rate=44100:duration=180" \
  -f lavfi -i "sine=frequency=330:sample_rate=44100:duration=180" \
  -filter_complex "[0:a]asplit[n1][n2];[n1][1:a]amix=inputs=2[l];[n2][2:a]amix=inputs=2[r];[l][r]join=inputs=2:channel_layout=stereo,aformat=sample_fmts=s16|s16p" \
  -fflags +bitexact "$out"
ls -l "$out"
