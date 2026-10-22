import sys, wave, numpy as np
from pathlib import Path
from collections import defaultdict
ev = Path(sys.argv[1])
def speech_end(path):
    with wave.open(str(path)) as w: a = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(float)
    idx = np.nonzero(np.abs(a) > 0.02 * 32768)[0]
    return idx[-1] / 16000 if len(idx) else 0
for model in ("okay_nabu", "hey_jarvis"):
    rows = [l.rstrip("\n").split("\t") for l in open(ev / f"res_{model}.tsv")]
    stats = defaultdict(lambda: [0, 0]); lat = []; fa = {}; fa_other = 0; other_total = 0
    for f, t in rows:
        p = Path(f); times = [float(x) for x in t.split(",") if x]
        if p.parent.name == "neg":
            with wave.open(str(p)) as w: mins = w.getnframes() / 16000 / 60
            fa[p.name] = (len(times), mins); continue
        word, voice, scale, cond = p.stem.split("__")
        if word != model:
            other_total += 1; fa_other += len(times) > 0; continue
        key = (cond,); stats[key][1] += 1
        if times:
            stats[key][0] += 1
            end = speech_end(p.with_name(p.name.replace("noise10", "clean")))
            lat.append(times[0] - end)
        else:
            print("  missed:", model, voice, scale, cond)
    print(model, {k[0]: f"{a}/{b}" for k, (a, b) in stats.items()}, "latency after speech end: median %.2f s, max %.2f s" % (np.median(lat), max(lat)) if lat else "", "| other word triggered:", f"{fa_other}/{other_total}", "| negatives:", fa)
