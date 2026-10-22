# VAD test fixtures (H15-69)

Synthetic German speech, 16 kHz mono 16-bit, leading/trailing silence trimmed:

| File | Text | Length |
|---|---|---|
| `speech-1.wav` | Welcher Tag ist heute? | 1.227 s |
| `speech-2.wav` | Mach bitte die Musik im Wohnzimmer etwas leiser. | 2.277 s |
| `speech-3.wav` | Wie wird das Wetter morgen in Zürich? | 1.679 s |

Made with Home Assistant's Piper TTS, voice `de_DE-thorsten-high` (Thorsten-Voice dataset, CC0), through
`assist_pipeline/run` with `start_stage = end_stage = "tts"` (synthesis only, no intent), then

```sh
ffmpeg -i answer.mp3 -af "silenceremove=start_periods=1:start_threshold=-50dB:stop_periods=1:stop_threshold=-50dB:stop_duration=0.05" \
  -ar 16000 -ac 1 -c:a pcm_s16le -fflags +bitexact -flags:a +bitexact -map_metadata -1 speech-N.wav
```

Silence, noise and the music-like background are mixed in by the tests (`assist/vad.rs`), so the cut points have
an exact ground truth.
