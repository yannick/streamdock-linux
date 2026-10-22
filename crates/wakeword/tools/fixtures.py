"""Reference fixtures for crates/wakeword (H15-68): frontend features and raw model outputs
from pymicro-features / pymicro-wakeword (full TFLite runtime)."""
import sys, wave, ctypes, numpy as np
from pathlib import Path
from pymicro_features import MicroFrontend
from pymicro_wakeword import MicroWakeWord, Model

wav_dir, out_dir = Path(sys.argv[1]), Path(sys.argv[2])
names = sys.argv[3:]
SCALE = 0.0390625
for name in names:
    with wave.open(str(wav_dir / f"{name}.wav")) as w:
        pcm = w.readframes(w.getnframes())
    fe = MicroFrontend()
    feats = []
    for i in range(0, len(pcm) - 320 + 1, 320):
        out = fe.process_samples(pcm[i:i + 320])
        if out.features:
            feats.append([round(v / SCALE) for v in out.features])
    (out_dir / f"{name}.features").write_text("\n".join(" ".join(map(str, f)) for f in feats) + "\n")
    for model in (Model.OKAY_NABU, Model.HEY_JARVIS):
        mww = MicroWakeWord.from_builtin(model)
        lines = []
        for g in range(0, len(feats) - 2, 3):
            x = np.array([[v * SCALE for v in f] for f in feats[g:g + 3]], dtype=np.float64)[None]
            q = np.round(x / mww.input_scale + mww.input_zero_point).astype(mww.input_dtype)
            mww.lib.TfLiteTensorCopyFromBuffer(mww.input_tensor, q.ctypes.data_as(ctypes.c_void_p), q.nbytes)
            mww.lib.TfLiteInterpreterInvoke(mww.interpreter)
            o = np.empty(1, dtype=mww.output_dtype)
            mww.lib.TfLiteTensorCopyToBuffer(mww.output_tensor, o.ctypes.data_as(ctypes.c_void_p), o.nbytes)
            lines.append(str(int(o[0])))
        (out_dir / f"{name}.{model.value}").write_text("\n".join(lines) + "\n")
        print(name, model.value, "input", mww.input_scale, mww.input_zero_point, "output", mww.output_scale, mww.output_zero_point, "max", max(map(int, lines)))
