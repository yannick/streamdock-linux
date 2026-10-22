"""Synthesise 16 kHz mono test phrases with Piper (H15-68 test audio)."""
import sys, wave, numpy as np, soxr
from pathlib import Path
from piper import PiperVoice, SynthesisConfig

voices_dir, out_dir = Path(sys.argv[1]), Path(sys.argv[2])
# The committed fixtures (`tests/fixtures`) come from `en_US-ljspeech-medium`: trained from scratch
# on LJ Speech, which is public domain (H15-103; the lessac voice's data is research-only).
PHRASES = {
    "en_US-ljspeech-medium": {
        "okay_nabu_lights": "Okay Nabu, turn on the kitchen lights.",
        "hey_jarvis_time": "Hey Jarvis, what time is it?",
        "alexa_weather": "Alexa, what is the weather like today?",
        "hey_mycroft": "Hey Mycroft, play some music.",
        "neg_news": "The quick brown fox jumps over the lazy dog, and then the news came on the radio about the weather in the mountains.",
        "neg_similar": "Okay, the banana bread is ready. Hey, Jarvis Cocker is a singer. No, not now.",
    },
    "en_GB-alan-medium": {
        "okay_nabu_gb": "Okay Nabu, what's on my calendar?",
        "hey_jarvis_gb": "Hey Jarvis, set a timer for ten minutes.",
        "neg_gb": "I went to the shop this morning and bought some bread, milk and a newspaper.",
    },
    "de_DE-thorsten-medium": {
        "okay_nabu_de": "Okay Nabu, mach das Licht im Wohnzimmer an.",
        "neg_de": "Heute Abend gibt es Nudeln mit Tomatensoße, und morgen fahren wir in die Berge.",
    },
}
for voice_name, phrases in PHRASES.items():
    voice = PiperVoice.load(str(voices_dir / f"{voice_name}.onnx"))
    for name, text in phrases.items():
        audio = np.concatenate([c.audio_float_array for c in voice.synthesize(text, SynthesisConfig())])
        audio = soxr.resample(audio, voice.config.sample_rate, 16000)
        # 0.5 s silence before and after, at a moderate level.
        pad = np.zeros(8000, dtype=np.float32)
        audio = np.concatenate([pad, audio * 0.5, pad])
        pcm = np.clip(audio * 32767, -32768, 32767).astype(np.int16)
        with wave.open(str(out_dir / f"{name}.wav"), "wb") as w:
            w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000); w.writeframes(pcm.tobytes())
        print(name, len(pcm) / 16000)
