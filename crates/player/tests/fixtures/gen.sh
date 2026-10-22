#!/bin/sh
# Regenerates the decoder test fixtures (H15-56): half a second of synthetic tones, 44.1 kHz stereo
# (440 Hz left, 660 Hz right), so no third-party audio. Needs ffmpeg, flac and lame.
set -eu
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
ffmpeg -loglevel error -f lavfi -i "sine=frequency=440:sample_rate=44100:duration=0.5" \
  -f lavfi -i "sine=frequency=660:sample_rate=44100:duration=0.5" \
  -filter_complex "[0:a][1:a]join=inputs=2:channel_layout=stereo" -fflags +bitexact -flags:a +bitexact -c:a pcm_s16le "$tmp/tone.wav"
flac --silent --force --best --no-padding -o tone.flac "$tmp/tone.wav"
lame --silent -b 128 "$tmp/tone.wav" tone.mp3
