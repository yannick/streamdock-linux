"""H15-68 evaluation set: wake word phrases from several Piper voices at three speeds, clean and
with noise (10 dB SNR), plus long negative speech with near misses."""
import sys, wave, random, numpy as np, soxr
from pathlib import Path
from piper import PiperVoice, SynthesisConfig

voices_dir, out = Path(sys.argv[1]), Path(sys.argv[2])
(out / "pos").mkdir(parents=True, exist_ok=True); (out / "neg").mkdir(parents=True, exist_ok=True)
rng = np.random.default_rng(68)
WORDS = {"okay_nabu": "Okay Nabu.", "hey_jarvis": "Hey Jarvis."}
NEG_EN = [
    "Okay, the bread is in the oven now.", "Hey, did you see the game last night?", "The navy ships came back to the harbour.",
    "Jarvis Cocker sang in Pulp for many years.", "I need a new nozzle for the garden hose.", "Could you pass me the salt and pepper please?",
    "Nabil from accounting called about the invoice.", "Okay, but not before we finish the report.", "Hey there, how was your weekend?",
    "The weather forecast says rain until Thursday.", "Turn left at the next junction and park behind the bakery.",
    "Harvest season starts in September in this valley.", "Hey Siri is not the same as Alexa or Google.",
    "My favourite podcast is about history and old maps.", "Okay now, everyone sit down, the film is starting.",
]
NEG_DE = [
    "Okay, dann fahren wir morgen früh los.", "Hey, hast du die Schlüssel gesehen?", "Der Nachbar hat einen neuen Hund, der heißt Naboo.",
    "Heute Abend kochen wir Nudeln mit Tomatensoße.", "Kannst du bitte das Fenster schließen, es zieht.", "Im Herbst fallen die Blätter von den Bäumen.",
]
def synth(voice, text, scale):
    audio = np.concatenate([c.audio_float_array for c in voice.synthesize(text, SynthesisConfig(length_scale=scale))])
    return soxr.resample(audio, voice.config.sample_rate, 16000)
def save(path, audio):
    pcm = np.clip(audio * 32767, -32768, 32767).astype(np.int16)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000); w.writeframes(pcm.tobytes())
def noisy(audio, snr_db):
    speech = audio[np.abs(audio) > 0.01]
    p = np.mean(speech ** 2) if len(speech) else 1e-6
    noise = rng.normal(0, 1, len(audio)); noise = np.cumsum(noise); noise -= np.convolve(noise, np.ones(400) / 400, mode="same")  # brownish
    noise *= np.sqrt(p / 10 ** (snr_db / 10) / np.mean(noise ** 2))
    return audio + noise
neg = []
for vf in sorted(voices_dir.glob("*.onnx")):
    voice = PiperVoice.load(str(vf)); name = vf.stem; german = name.startswith("de_")
    for word, text in WORDS.items():
        for scale in (0.85, 1.0, 1.2):
            a = synth(voice, text, scale) * 0.5
            a = np.concatenate([np.zeros(16000), a, np.zeros(24000)])
            save(out / "pos" / f"{word}__{name}__{scale}__clean.wav", a)
            save(out / "pos" / f"{word}__{name}__{scale}__noise10.wav", noisy(a, 10))
    for text in (NEG_DE if german else NEG_EN):
        neg.append(synth(voice, text, random.Random(len(neg)).choice([0.9, 1.0, 1.1])) * 0.5)
        neg.append(np.zeros(8000))
stream = np.concatenate(neg)
save(out / "neg" / "speech.wav", stream)
save(out / "neg" / "speech_noise10.wav", noisy(stream, 10))
print("positives", len(list((out / "pos").glob("*.wav"))), "negative minutes", round(len(stream) / 16000 / 60, 1))
