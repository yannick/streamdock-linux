# Test fixtures (H15-68)

- `*.wav`: 16 kHz mono 16-bit phrases synthesised with Piper TTS, voice `en_US-ljspeech-medium`
  (trained from scratch on the LJ Speech dataset, which is public domain; H15-103), with 0.5 s of
  silence before and after (`tools/tts.py`): `okay_nabu_lights` ("Okay Nabu, turn on the kitchen
  lights."), `hey_jarvis_time` ("Hey Jarvis, what time is it?"), `neg_similar` (near misses:
  "Okay, the banana bread is ready. Hey, Jarvis Cocker is a singer. No, not now.").
- `*.features`: the frontend's output per 10 ms step from `pymicro-features` 2.0.2 (the TFLite
  Micro audio frontend), as integers (`value / 0.0390625`).
- `*.okay_nabu`, `*.hey_jarvis`: TensorFlow Lite Micro's raw uint8 output per invocation (3 steps)
  on those features, from a TFLM build (tflite-micro `64b3de8`) with the microWakeWord op set.
  The full TFLite runtime differs by a few units (its int8 kernels round differently), so TFLM,
  which ESPHome devices run, is the reference.

Piper's synthesis is random (its noise is drawn inside the model), so every run of `tts.py` gives
slightly different audio: the WAVs here are one run, and the features and outputs belong to exactly
these files. Regenerating means recomputing both and checking the detector's margin (okay_nabu
here: 0.961 against its cutoff 0.85; hey_jarvis 0.998 against 0.97; the near misses at most 0.754).
