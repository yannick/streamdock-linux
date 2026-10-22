// This machine's audio (H15-50), for the settings page: outputs (PipeWire sinks) with volume and
// mute, which one plays, and the sound (H15-54: character pad, loudness, tone, balance, correction
// EQ, crossfeed, preamp and limiter). Live: the state arrives with
// every change through WatchEvents, so another browser, a dock knob or a headset connecting shows
// up at once. Shown only where the daemon manages the audio (`[audio] manage = true`).
import { useEffect, useRef, useState } from "react";

import { deviceClient, watchDeviceEvents } from "../api/client.js";
import { Badge, Button, Panel, Slider, Toggle, XYPad } from "../design/index.js";
import type { AudioBand, AudioOutput, AudioPreset, AudioState, AudioTone } from "../gen/streamdock/v1/device_pb.js";
import { OutputPriorityPanel } from "./OutputPriority.js";

const MAX_VOLUME = 150;
const MAX_GAIN = 12;

const KIND_LABEL: Record<string, string> = { bluetooth: "Bluetooth", usb: "USB", builtin: "Built-in" };

/** Tone presets: bass, mid, treble in dB. */
const PRESETS: ReadonlyArray<{ label: string; bass: number; mid: number; treble: number }> = [
  { label: "Flat", bass: 0, mid: 0, treble: 0 },
  { label: "Bass boost", bass: 6, mid: 0, treble: 0 },
  { label: "Loudness", bass: 5, mid: -1, treble: 4 },
  { label: "Speech", bass: -4, mid: 3, treble: 1 },
  { label: "Treble boost", bass: 0, mid: 0, treble: 5 },
];

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

/** `GetAudio` once (managed or not), then every change from `WatchEvents`. */
export function useAudio(): { managed: boolean | null; audio: AudioState | null } {
  const [managed, setManaged] = useState<boolean | null>(null);
  const [audio, setAudio] = useState<AudioState | null>(null);
  useEffect(() => {
    deviceClient
      .getAudio({})
      .then((res) => {
        setManaged(res.managed);
        if (res.state) setAudio(res.state);
      })
      .catch(() => setManaged(false));
    const abort = new AbortController();
    void watchDeviceEvents({ onEvent: () => {}, onAudio: setAudio }, abort.signal);
    return () => abort.abort();
  }, []);
  return { managed, audio };
}

export function AudioSection({ audio }: { audio: AudioState | null }) {
  const [error, setError] = useState<string | null>(null);
  const run = (action: Promise<unknown>) => {
    setError(null);
    action.catch((err: unknown) => setError(message(err)));
  };
  if (!audio) return <Panel title="Audio">Loading…</Panel>;
  return (
    <>
      <Panel title="Outputs" aside={audio.problem ? "problem" : `${audio.outputs.length}`} className="sd-settings__module">
        {audio.problem && <p className="sd-setting__error">{audio.problem}</p>}
        {error && <p className="sd-setting__error">{error}</p>}
        <ul className="sd-netlist">
          {audio.outputs.map((o) => (
            <OutputRow key={o.name} output={o} run={run} />
          ))}
        </ul>
        {audio.outputs.length === 0 && !audio.problem && <p className="sd-settings__help">No audio output.</p>}
      </Panel>
      <OutputPriorityPanel outputs={audio.outputs} />
      {audio.tone && <SoundPanels tone={audio.tone} presets={audio.presets} run={run} />}
    </>
  );
}

/** "LDAC · 48 kHz · 990 kbit/s": the codec, PipeWire's rate and the measured Bluetooth bitrate. */
function quality(o: AudioOutput): string {
  const parts = [
    o.codec && o.codec.toUpperCase().replace("_", "-"),
    o.sampleRate > 0 && `${(o.sampleRate / 1000).toFixed(o.sampleRate % 1000 ? 1 : 0)} kHz`,
    o.bitrateKbps > 0 && `${o.bitrateKbps} kbit/s`,
  ];
  return parts.filter(Boolean).join(" · ");
}

function OutputRow({ output: o, run }: { output: AudioOutput; run(action: Promise<unknown>): void }) {
  // While dragging, the slider shows the local value; live updates take over again on release.
  const [dragging, setDragging] = useState<number | null>(null);
  const volume = dragging ?? o.volumePercent;
  const last = useRef(0);
  // While dragging at most every 200 ms; always on release (the daemon applies the latest).
  const setVolume = (percent: number, muted = o.muted, force = true) => {
    const now = Date.now();
    if (!force && now - last.current < 200) return;
    last.current = now;
    run(deviceClient.setAudioVolume({ name: o.name, volumePercent: percent, muted }));
  };
  return (
    <li className="sd-netlist__item" data-active={o.selected || undefined}>
      <div className="sd-netlist__head">
        <span className="sd-netlist__name">{o.description}</span>
        {o.selected && <Badge tone="success">playing here</Badge>}
        <span className="sd-setting__help">{KIND_LABEL[o.kind] ?? o.kind}</span>
        {o.codec && <Badge>{o.codec.toUpperCase().replace("_", "-")}</Badge>}
        {o.running && <span className="sd-setting__help">{quality(o) || "active"}</span>}
      </div>
      <div className="sd-settings__actions sd-netlist__form">
        <Slider
          label={`Volume of ${o.description}`}
          value={volume}
          max={MAX_VOLUME}
          onChange={(v) => {
            setDragging(v);
            setVolume(v, o.muted, false);
          }}
          onCommit={(v) => {
            setDragging(null);
            setVolume(v);
          }}
        />
        <span className="sd-setting__help">{volume} %</span>
        <Toggle label="Mute" checked={o.muted} onLabel="Muted" offLabel="On" onChange={(muted) => setVolume(o.volumePercent, muted)} />
        {!o.selected && (
          <Button size="sm" variant="primary" onClick={() => run(deviceClient.setAudioOutput({ name: o.name }))}>
            Play here
          </Button>
        )}
      </div>
    </li>
  );
}

/** The editable sound settings (everything of `AudioTone` but the read-only values). */
type Sound = Omit<AudioTone, "$typeName" | "$unknown" | "available" | "loudnessBassDb" | "loudnessTrebleDb">;

type Change = (patch: Partial<Sound>, done?: boolean) => void;

const BAND_KIND = ["Low shelf", "Peak", "Peak", "Peak", "Peak", "Peak", "Peak", "Peak", "Peak", "High shelf"];

function editable(tone: AudioTone): Sound {
  const { $typeName: _t, $unknown: _u, available: _a, loudnessBassDb: _lb, loudnessTrebleDb: _lt, ...sound } = tone;
  return sound;
}

/** Local edits while dragging, sent at most every 150 ms and once on release (the latest wins). */
function useSound(tone: AudioTone, run: (action: Promise<unknown>) => void): { shown: Sound; change: Change } {
  const [local, setLocal] = useState<Sound | null>(null);
  const shown = local ?? editable(tone);
  const last = useRef(0);
  const change: Change = (patch, done = true) => {
    const next = { ...shown, ...patch };
    setLocal(done ? null : next);
    const now = Date.now();
    if (!done && now - last.current < 150) return;
    last.current = now;
    run(deviceClient.setAudioTone({ tone: next }));
  };
  return { shown, change };
}

function SoundPanels({ tone, presets, run }: { tone: AudioTone; presets: AudioPreset[]; run(action: Promise<unknown>): void }) {
  const { shown, change } = useSound(tone, run);
  if (!tone.available) {
    return (
      <Panel title="Sound" aside="unavailable">
        <p className="sd-settings__help">The sound processing (Streamdock DSP) isn&apos;t running on this machine.</p>
      </Panel>
    );
  }
  return (
    <>
      <CharacterPanel sound={shown} tone={tone} change={change} />
      <TonePanel sound={shown} change={change} />
      <CorrectionPanel sound={shown} presets={presets} change={change} />
      <OutputStagePanel sound={shown} change={change} />
    </>
  );
}

/** B&O-style: the sound character pad and loudness. */
function CharacterPanel({ sound, tone, change }: { sound: Sound; tone: AudioTone; change: Change }) {
  const loud = tone.loudnessBassDb > 0.05 ? `now +${tone.loudnessBassDb.toFixed(1)} dB bass, +${tone.loudnessTrebleDb.toFixed(1)} dB treble` : "none at this volume";
  return (
    <Panel title="Sound character" aside="all outputs">
      <p className="sd-settings__help">Drag the point: warm ↔ bright, relaxed ↔ energetic. The centre is neutral.</p>
      <XYPad
        label="Sound character"
        x={sound.warmth}
        y={sound.energy}
        left="Warm"
        right="Bright"
        top="Energetic"
        bottom="Relaxed"
        onChange={(warmth, energy) => change({ warmth, energy }, false)}
        onCommit={(warmth, energy) => change({ warmth, energy })}
      />
      <div className="sd-settings__actions">
        <Button size="sm" onClick={() => change({ warmth: 0, energy: 0 })}>
          Neutral
        </Button>
        <Toggle label="Loudness" checked={sound.loudness} onChange={(loudness) => change({ loudness })} />
        {sound.loudness && <span className="sd-setting__help">More bass the lower the volume: {loud}.</span>}
      </div>
    </Panel>
  );
}

function TonePanel({ sound, change }: { sound: Sound; change: Change }) {
  return (
    <Panel title="Tone" aside="all outputs">
      <div className="sd-settings__actions">
        {PRESETS.map((p) => (
          <Button key={p.label} size="sm" onClick={() => change({ bassDb: p.bass, midDb: p.mid, trebleDb: p.treble })}>
            {p.label}
          </Button>
        ))}
      </div>
      <DbSlider label="Bass (120 Hz)" value={sound.bassDb} min={-MAX_GAIN} max={MAX_GAIN} onChange={(v, done) => change({ bassDb: v }, done)} />
      <DbSlider label="Mid (1 kHz)" value={sound.midDb} min={-MAX_GAIN} max={MAX_GAIN} onChange={(v, done) => change({ midDb: v }, done)} />
      <DbSlider label="Treble (8 kHz)" value={sound.trebleDb} min={-MAX_GAIN} max={MAX_GAIN} onChange={(v, done) => change({ trebleDb: v }, done)} />
      <BalanceSlider value={sound.balance} onChange={(v, done) => change({ balance: v }, done)} />
    </Panel>
  );
}

/** Headphone / room correction: a preset (e.g. AutoEq), custom bands, or off. */
function CorrectionPanel({ sound, presets, change }: { sound: Sound; presets: AudioPreset[]; change: Change }) {
  return (
    <Panel title="Correction EQ" aside={sound.correction ? "on" : "off"}>
      <p className="sd-settings__help">
        Evens out a headphone&apos;s or speaker&apos;s own frequency response. Presets come from AutoEq measurements.
      </p>
      <select
        className="sd-input"
        aria-label="Correction"
        value={sound.correction}
        onChange={(e) => change({ correction: e.currentTarget.value })}
      >
        <option value="">Off</option>
        {presets.map((p) => (
          <option key={p.id} value={p.id}>
            {p.label}
          </option>
        ))}
        <option value="custom">Custom (10 bands)</option>
      </select>
      {sound.correction === "custom" && <BandsEditor bands={sound.bands} change={change} />}
    </Panel>
  );
}

function BandsEditor({ bands, change }: { bands: AudioBand[]; change: Change }) {
  const set = (i: number, patch: Partial<Pick<AudioBand, "freqHz" | "gainDb" | "q">>) =>
    change({ bands: bands.map((b, n) => (n === i ? { ...b, ...patch } : b)) });
  return (
    <table className="sd-bands">
      <thead>
        <tr>
          <th>Band</th>
          <th>Hz</th>
          <th>dB</th>
          <th>Q</th>
        </tr>
      </thead>
      <tbody>
        {bands.map((b, i) => (
          <tr key={i}>
            <td>{BAND_KIND[i]}</td>
            <td>
              <NumberInput label={`Band ${i + 1} frequency`} value={b.freqHz} min={20} max={20000} step={1} onChange={(freqHz) => set(i, { freqHz })} />
            </td>
            <td>
              <NumberInput label={`Band ${i + 1} gain`} value={b.gainDb} min={-24} max={24} step={0.1} onChange={(gainDb) => set(i, { gainDb })} />
            </td>
            <td>
              <NumberInput label={`Band ${i + 1} Q`} value={b.q} min={0.1} max={10} step={0.01} onChange={(q) => set(i, { q })} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function NumberInput(props: { label: string; value: number; min: number; max: number; step: number; onChange(v: number): void }) {
  return (
    <input
      className="sd-input"
      type="number"
      aria-label={props.label}
      value={Number(props.value.toFixed(2))}
      min={props.min}
      max={props.max}
      step={props.step}
      onChange={(e) => {
        const v = Number(e.currentTarget.value);
        if (Number.isFinite(v)) props.onChange(v);
      }}
    />
  );
}

/** Crossfeed for headphones, preamp and the limiter. */
function OutputStagePanel({ sound, change }: { sound: Sound; change: Change }) {
  // bs2b feeding level: 15 dB is subtle, 1 dB strong; shown as strength 1..15.
  const strength = Math.round(16 - sound.crossfeedDb);
  return (
    <Panel title="Headphones & level">
      <div className="sd-settings__actions">
        <Toggle label="Crossfeed" checked={sound.crossfeed} onChange={(crossfeed) => change({ crossfeed })} />
        <span className="sd-setting__help">Blends a little of each channel into the other, like speakers, so headphones sound less &quot;inside the head&quot;.</span>
      </div>
      {sound.crossfeed && (
        <div className="sd-setting">
          <span className="sd-setting__label">Crossfeed strength: {strength}</span>
          <Slider label="Crossfeed strength" value={strength} min={1} max={15} onChange={(v) => change({ crossfeedDb: 16 - v }, false)} onCommit={(v) => change({ crossfeedDb: 16 - v })} />
        </div>
      )}
      <DbSlider label="Preamp" value={sound.preampDb} min={-20} max={20} onChange={(v, done) => change({ preampDb: v }, done)} />
      <DbSlider label="Limiter ceiling" value={sound.limitDb} min={-20} max={0} onChange={(v, done) => change({ limitDb: v }, done)} />
      <p className="sd-settings__help">The limiter keeps boosts (preamp, bass, loudness) from clipping.</p>
    </Panel>
  );
}

function DbSlider(props: { label: string; value: number; min: number; max: number; onChange(v: number, done: boolean): void }) {
  const { label, value, min, max, onChange } = props;
  return (
    <div className="sd-setting">
      <span className="sd-setting__label">
        {label}: {value > 0 ? "+" : ""}
        {value.toFixed(0)} dB
      </span>
      <Slider label={label} value={value} min={min} max={max} onChange={(v) => onChange(v, false)} onCommit={(v) => onChange(v, true)} />
    </div>
  );
}

function BalanceSlider({ value, onChange }: { value: number; onChange(v: number, done: boolean): void }) {
  const pct = Math.round(value * 100);
  const text = pct === 0 ? "centre" : pct < 0 ? `${-pct} % left` : `${pct} % right`;
  return (
    <div className="sd-setting">
      <span className="sd-setting__label">Balance: {text}</span>
      <Slider label="Balance" value={pct} min={-100} max={100} onChange={(v) => onChange(v / 100, false)} onCommit={(v) => onChange(v / 100, true)} />
    </div>
  );
}
