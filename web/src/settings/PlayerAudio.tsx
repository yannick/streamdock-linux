// The built-in player's audio settings (H15-61, H15-64): formats offered to Lyrion, replay gain, the
// transition between tracks, the output buffer and where it plays. Each is a select; the values are those of `[player]` in the config.
import type { PlayerOutput } from "../gen/streamdock/v1/device_pb.js";

export interface AudioForm {
  formats: string[];
  outputBufferMs: number;
  /** Empty: Lyrion's own setting. */
  replayGain: string;
  output: string;
  /** Empty: Lyrion's own setting. */
  transition: string;
  transitionSeconds: number;
  /** "pipewire", "fast", "balanced" or "high" (H15-80). */
  resampling: string;
}

interface Choice {
  value: string;
  label: string;
}

const FORMATS: Choice[] = [
  { value: "flac,mp3,pcm", label: "FLAC, MP3 and PCM (Lyrion sends files as they are)" },
  { value: "flac,pcm", label: "FLAC and PCM (MP3 is transcoded)" },
  { value: "pcm", label: "PCM only (Lyrion decodes everything)" },
];

const REPLAY_GAIN: Choice[] = [
  { value: "", label: "As set in Lyrion" },
  { value: "off", label: "Off" },
  { value: "track", label: "Track gain" },
  { value: "album", label: "Album gain" },
  { value: "smart", label: "Smart (album gain for whole albums)" },
];

// CPU measured on the Luckfox for 44.1 → 48 kHz stereo (H15-80); PipeWire's own conversion is ~2 %.
const RESAMPLING: Choice[] = [
  { value: "balanced", label: "Balanced: high quality (~2.9 % CPU)" },
  { value: "high", label: "High: steepest filter, +18 ms delay (~4.1 % CPU)" },
  { value: "fast", label: "Fast: a little aliasing (~1 % CPU)" },
  { value: "pipewire", label: "Let PipeWire convert (~2 % CPU)" },
];

const TRANSITIONS: Choice[] = [
  { value: "", label: "As set in Lyrion" },
  { value: "none", label: "None (gapless)" },
  { value: "crossfade", label: "Crossfade" },
  { value: "fade-in", label: "Fade in" },
  { value: "fade-out", label: "Fade out" },
  { value: "fade-in-out", label: "Fade in and out" },
];

const TRANSITION_SECONDS: Choice[] = [1, 2, 3, 4, 5, 6, 8, 10].map((n) => ({ value: String(n), label: `${n} s` }));

const BUFFERS: Choice[] = [2000, 4000, 8000, 12000, 20000].map((ms) => ({
  value: String(ms),
  label: `${ms / 1000} s${ms === 4000 ? " (default)" : ""}`,
}));

function outputChoices(outputs: readonly PlayerOutput[], current: string): Choice[] {
  const choices: Choice[] = [
    { value: "dsp", label: "Through the sound settings (Audio)" },
    { value: "default", label: "The selected output, without the sound settings" },
    ...outputs.map((o) => ({ value: o.name, label: `${o.description || o.name}, without the sound settings` })),
  ];
  // A fixed output that isn't there right now (a headset that is off) stays selectable.
  if (!choices.some((c) => c.value === current)) choices.push({ value: current, label: `${current} (not connected)` });
  return choices;
}

function ChoiceField(props: { id: string; label: string; help?: string; choices: Choice[]; value: string; onChange(v: string): void }) {
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor={props.id}>
        {props.label}
      </label>
      <select id={props.id} className="sd-input" value={props.value} onChange={(e) => props.onChange(e.currentTarget.value)}>
        {props.choices.map((c) => (
          <option key={c.value} value={c.value}>
            {c.label}
          </option>
        ))}
      </select>
      {props.help && <span className="sd-setting__help">{props.help}</span>}
    </div>
  );
}

export function PlayerAudioFields({ form, outputs, onChange }: { form: AudioForm; outputs: readonly PlayerOutput[]; onChange(f: AudioForm): void }) {
  const formats = form.formats.join(",");
  return (
    <>
      <ChoiceField
        id="player-formats"
        label="Formats"
        choices={FORMATS.some((c) => c.value === formats) ? FORMATS : [...FORMATS, { value: formats, label: formats }]}
        value={formats}
        onChange={(v) => onChange({ ...form, formats: v.split(",") })}
      />
      <ChoiceField id="player-replay-gain" label="Replay gain" choices={REPLAY_GAIN} value={form.replayGain} onChange={(replayGain) => onChange({ ...form, replayGain })} />
      <TransitionFields form={form} onChange={onChange} />
      <ChoiceField
        id="player-buffer"
        label="Output buffer"
        help="More rides out network hiccups; pause and skip react as fast either way."
        choices={BUFFERS.some((c) => c.value === String(form.outputBufferMs)) ? BUFFERS : [...BUFFERS, { value: String(form.outputBufferMs), label: `${form.outputBufferMs} ms` }]}
        value={String(form.outputBufferMs)}
        onChange={(v) => onChange({ ...form, outputBufferMs: Number(v) })}
      />
      <ChoiceField id="player-output" label="Plays on" choices={outputChoices(outputs, form.output)} value={form.output} onChange={(output) => onChange({ ...form, output })} />
      <ChoiceField
        id="player-resampling"
        label="Resampling quality"
        help="Files at another rate than the output (e.g. 44.1 kHz to 48 kHz) are converted in the player, before the sound settings."
        choices={RESAMPLING}
        value={form.resampling || "balanced"}
        onChange={(resampling) => onChange({ ...form, resampling })}
      />
    </>
  );
}

function TransitionFields({ form, onChange }: { form: AudioForm; onChange(f: AudioForm): void }) {
  const crossfade = form.transition === "crossfade";
  const needed = (form.transitionSeconds + 1) * 1000;
  return (
    <>
      <ChoiceField
        id="player-transition"
        label="Between tracks"
        choices={TRANSITIONS}
        value={form.transition}
        onChange={(transition) => onChange({ ...form, transition })}
      />
      {form.transition !== "" && form.transition !== "none" && (
        <ChoiceField
          id="player-transition-seconds"
          label="Transition length"
          help={crossfade && form.outputBufferMs < needed ? `The output buffer is raised to ${needed / 1000} s for the crossfade.` : undefined}
          choices={TRANSITION_SECONDS}
          value={String(form.transitionSeconds)}
          onChange={(v) => onChange({ ...form, transitionSeconds: Number(v) })}
        />
      )}
    </>
  );
}
