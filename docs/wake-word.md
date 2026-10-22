# Wake word (H15-68)

Say a wake word ("Okay Nabu", "Hey Jarvis", …) and the dock asks Home Assistant, as if the
push-to-talk key (H15-43) had been pressed. Settings → **Voice**.

## How it works

1. With `wake_word` set in `[modules.homeassistant.assist]`, the daemon keeps **one** microphone
   stream open (`pw-record`, 16 kHz mono; or `test_input`, a WAV played once, for development).
   Turning the wake word off stops it.
2. Every 10 ms the **frontend** (`crates/wakeword/src/frontend.rs`) turns the audio into 40
   log-mel features: TensorFlow Lite Micro's audio frontend (window, kissfft int16, filterbank,
   noise reduction, PCAN, log), bit for bit.
3. Every 30 ms the **model**, a microWakeWord streaming CNN (int8, 50–80 KB), gives a probability.
   `crates/wakeword/src/model.rs` reads the `.tflite` file and runs its 13 operators itself
   (pure Rust, results equal to TensorFlow Lite Micro's).
4. The **detector** averages the last 5 probabilities; above the threshold the word was heard
   (then 1 s without another detection).
5. The speech that follows goes to an assist request (`Assist::request_with`), ended by voice
   activity detection (`assist::vad`, H15-69). The microphone keeps draining; detection resumes
   once the answer has played.

The live state (phase, microphone level, probability, heard count, last result) is
`WatchEventsResponse.wake`; the activity feed gets "heard" and how the request ended.

## Configuration

```toml
[modules.homeassistant.assist]
wake_word = "okay_nabu"   # model id; absent = off
wake_threshold = 0.85     # 0.5-0.99; absent = the model's own
microphone = "alsa_input.usb-…"   # PipeWire source; absent = the default
```

The Voice settings also edit the push-to-talk keys of the same table (`pipeline`, `speak`,
`silence_ms`, `vad_threshold`, `no_speech_ms`, `max_seconds`).

## Models

Built in (Apache-2.0, `crates/wakeword/models/`): **okay_nabu** (trained for en, nl, fr, de, it,
es, sv), **hey_jarvis**, **alexa**, **hey_mycroft** (en).

Your own: a microWakeWord `.tflite` with its `.json` manifest, uploaded in Settings → Voice and
stored in `wakewords/` next to the config. The upload is refused unless the dock can run it:
at most 1 MiB, only the microWakeWord operators (CONV_2D, DEPTHWISE_CONV_2D, FULLY_CONNECTED,
CONCATENATION, STRIDED_SLICE, SPLIT_V, RESHAPE, LOGISTIC, QUANTIZE and the variable ops), int8
input `[1, stride, 40]`, uint8 output, and a manifest with `wake_word`, `trained_languages`,
`micro.probability_cutoff`, `micro.sliding_window_size` and `feature_step_size` 10. At most 20
uploads.

## Training a wake word

Not on the dock: it needs TensorFlow, ideally a GPU (a PC or Google Colab), and an hour or more.

1. Open microWakeWord's `notebooks/basic_training_notebook.ipynb`
   (<https://github.com/kahrendt/microWakeWord>) in Colab or Jupyter.
2. Enter the phrase. It generates thousands of spoken samples with
   [Piper](https://github.com/rhasspy/piper) (pick Piper voices of the languages you speak; other
   languages work if Piper has a voice for them), mixes them with background noise and the
   negative data it downloads, trains and quantises the model.
3. It writes `<name>.tflite` and `<name>.json`. Upload both in Settings → Voice, choose the word,
   and try it with the live panel: lower the threshold if it misses you, raise it if it triggers
   on other speech.

The notebook's own README calls it a starting point that needs experimenting; the built-in
words are the reliable ones.

## Measurements

- **Correctness:** frontend features equal `pymicro-features` 2.0.2, model outputs equal
  TensorFlow Lite Micro (`64b3de8`) bit for bit, on x86_64 and armv7 (qemu-arm), for three Piper
  TTS phrases (`crates/wakeword/tests/fixtures`, voice `en_US-ljspeech-medium`, public-domain data). The full TensorFlow Lite runtime
  (`pymicro-wakeword`) is deliberately not the reference: its int8 kernels round differently
  (up to 6/255). musl's `log1pf` would change one filterbank bin, so those tables are constants.
- **CPU on the Luckfox (Cortex-A7, one core):** 2.5 % for okay_nabu (frontend 0.8 + model 1.5),
  1.8 % for hey_jarvis, process CPU time of `crates/wakeword/examples/bench.rs`.
- **Detection on the fixtures:** each word once in its phrase, no false accept on the other
  phrases or the near-miss sentence ("Okay, the banana bread … Hey, Jarvis Cocker …").
- **Synthetic evaluation** (`crates/wakeword/tools`): each word from 7 Piper voices (5 English,
  2 German) at 3 speeds, clean and with brown noise at 10 dB SNR; 9.3 minutes of negative
  speech (English and German, near misses like "Okay, …", "Hey, …", "Jarvis Cocker", "navy").

  | | clean | 10 dB SNR | trigger after the word ends | false accepts |
  |---|---|---|---|---|
  | okay_nabu, threshold 0.85 (default) | 21/21 | 14/21 | median 0.16 s, max 0.31 s | 0 (negatives, other word) |
  | okay_nabu, threshold 0.80 | 21/21 | 18/21 | | 0 |
  | hey_jarvis, threshold 0.97 (default) | 15/15 English, 0/6 German voices | same | median 0.06 s | 0 |

  hey_jarvis is trained for English only; okay_nabu also for German. 9 minutes are far too few
  to estimate false accepts per hour (the model authors target under 0.5 per hour at the default
  threshold); real rooms, real voices and a real microphone will differ from TTS, so the
  threshold is a setting with a live probability meter.

## The NPU (RV1106)

Not used. At about 2 % of the core, the CPU path leaves nothing worth offloading, while the NPU
would need the `rknpu` kernel driver built for our kernel (not on the board's image), an RKNN
model conversion of a streaming model with resource variables, and its Mini Runtime's native
tensor-memory API, with per-call overhead in the same range as the whole CPU cost. Decided on
H15-68; revisit only for a much larger model.
