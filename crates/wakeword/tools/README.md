# Reference tooling (H15-68)

Not part of the build: how the fixtures and the measurements in `docs/wake-word.md` were made.
Run the Python scripts with `python -I` in a venv with `pymicro-wakeword pymicro-features
piper-tts soxr numpy` and Piper voices from <https://huggingface.co/rhasspy/piper-voices>.

- `tts.py <voices> <out>`: the fixture phrases (Piper TTS, 16 kHz, 0.5 s silence around). The
  committed ones use `en_US-ljspeech-medium` (public-domain data); Piper's output is random per
  run, see `tests/fixtures/README.md`.
- `fixtures.py <wav dir> <out dir> <names…>`: frontend features from `pymicro-features`
  (`*.features`); its model outputs are the full TFLite runtime's, which is **not** the
  reference: overwrite them with `drive`'s (below).
- `ww_shim.cc`, `drive.c`: TensorFlow Lite Micro (tflite-micro `64b3de8`, built with
  `create_tflm_tree.py` and `zig c++`) with the microWakeWord op set; `drive <model> <scale>
  <zero point> <file.features>` prints TFLM's raw output per invocation, the reference for
  `tests/model.rs`.
- `eval_tts.py <voices> <out>`: the evaluation set (wake words from several voices at three
  speeds, clean and with brown noise at 10 dB SNR; negative speech with near misses), then
  `cargo run --release -p wakeword --example eval -- <model> <cutoff> 5 <out>/pos/*.wav
  <out>/neg/*.wav > <out>/res_<model>.tsv` and `eval_metrics.py <out>`.
- `examples/bench.rs`: CPU per second of audio (run on the board, process CPU time).
