// Voice (H15-68, H15-69), for the settings page: the wake word (on/off, word, threshold, a live
// panel with the microphone level and the detector's probability), the requests to Home Assistant
// (pipeline, microphone, spoken answer, end of speech) and uploading wake words trained elsewhere.
// The settings live in `[modules.homeassistant.assist]` and are read again when config.toml changes;
// the listener's state follows `WatchEvents` (`wake`).
import { useEffect, useState } from "react";

import { deviceClient } from "../api/client.js";
import { useLive, useRevisionChange } from "../api/live.js";
import { Badge, Button, DataList, Meter, Panel, Slider, Toggle } from "../design/index.js";
import type {
  GetVoiceResponse,
  VoiceSettings,
  WakeWordModel,
  WakeWordState,
} from "../gen/streamdock/v1/device_pb.js";
import { RequestFields } from "./VoiceRequests.js";

export function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

/** The form: the settings as plain values. */
export interface VoiceForm {
  pipeline: string;
  microphone: string;
  speak: boolean;
  silenceMs: number;
  vadThreshold: number;
  noSpeechMs: number;
  maxSeconds: number;
  wakeWord: string;
  /** `null`: the model's own threshold. */
  wakeThreshold: number | null;
}

const DEFAULT_FORM: VoiceForm = {
  pipeline: "",
  microphone: "",
  speak: true,
  silenceMs: 700,
  vadThreshold: 0.6,
  noSpeechMs: 5000,
  maxSeconds: 15,
  wakeWord: "",
  wakeThreshold: null,
};

/** The form as the daemon sends it (it always fills every field). */
function formOf(s: VoiceSettings | undefined): VoiceForm {
  if (!s) return DEFAULT_FORM;
  const { pipeline, microphone, speak, silenceMs, vadThreshold, noSpeechMs, maxSeconds, wakeWord } = s;
  // The thresholds are f32 on the wire: round to what the UI shows (0.6000000238 → 0.6).
  const round = (v: number) => Math.round(v * 100) / 100;
  const wakeThreshold = s.wakeThreshold === undefined ? null : round(s.wakeThreshold);
  return { pipeline, microphone, speak, silenceMs, vadThreshold: round(vadThreshold), noSpeechMs, maxSeconds, wakeWord, wakeThreshold };
}

/** `GetVoice`, again whenever the config changes. */
function useVoice(): { voice: GetVoiceResponse | null; error: string | null; reload(): void } {
  const [voice, setVoice] = useState<GetVoiceResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reads, setReads] = useState(0);
  useEffect(() => {
    deviceClient
      .getVoice({})
      .then((res) => {
        setVoice(res);
        setError(null);
      })
      .catch((err: unknown) => setError(message(err)));
  }, [reads]);
  const reload = () => setReads((n) => n + 1);
  useRevisionChange("config", reload);
  return { voice, error, reload };
}

export function VoiceSection() {
  const { voice, error, reload } = useVoice();
  const live = useLive().wake;
  if (error) {
    return (
      <Panel title="Voice">
        <p className="sd-setting__error">{error}</p>
      </Panel>
    );
  }
  if (!voice) return <Panel title="Voice">Loading…</Panel>;
  if (!voice.available) {
    return (
      <Panel title="Voice">
        <p className="sd-settings__help">
          Push-to-talk and the wake word talk to Home Assistant&apos;s assist pipeline. Set up Home Assistant first (Integrations).
        </p>
      </Panel>
    );
  }
  return (
    <>
      <WakeStatus state={live} models={voice.models} />
      <SettingsForm key={JSON.stringify(formOf(voice.settings))} voice={voice} />
      <UploadPanel models={voice.models} inUse={voice.settings?.wakeWord ?? ""} onChanged={reload} />
    </>
  );
}

// ---------- Live status ----------

const PHASE_TEXT: Record<string, string> = {
  listening: "listening",
  request: "heard: asking",
  starting: "starting",
  off: "off",
  "test input played": "test input played",
};

/** -60 dBFS (quiet room) … 0 dBFS as 0 … 1. */
function levelOf(db: number): number {
  return Math.min(1, Math.max(0, (db + 60) / 60));
}

function heardText(s: WakeWordState): string {
  if (s.heardCount === 0) return "not yet";
  const at = new Date(Number(s.heardAtUnixMs)).toLocaleTimeString();
  return `${s.heardCount}× · last at ${at}`;
}

function WakeStatus({ state, models }: { state: WakeWordState | null; models: WakeWordModel[] }) {
  if (!state?.enabled) {
    return (
      <Panel title="Wake word" aside={<Badge tone="muted">off</Badge>} className="sd-settings__module">
        <p className="sd-settings__help">Off. Choose a wake word below to listen hands-free.</p>
      </Panel>
    );
  }
  const listening = state.phase !== "off";
  const word = models.find((m) => m.id === state.model)?.wakeWord ?? state.wakeWord;
  return (
    <Panel title="Wake word" aside={<Badge tone={listening ? "accent" : "muted"}>{PHASE_TEXT[state.phase] ?? state.phase}</Badge>} className="sd-settings__module">
      {state.problem && <p className="sd-setting__error">{state.problem}</p>}
      <LiveMeters state={state} word={word} />
    </Panel>
  );
}

function LiveMeters({ state, word }: { state: WakeWordState; word: string }) {
  const above = state.mean > state.threshold;
  return (
    <div className="sd-voice__live">
      <div className="sd-voice__meter">
        <span className="sd-setting__label">Microphone</span>
        <Meter value={levelOf(state.levelDb)} label="Microphone level" />
        <span className="sd-setting__help">{state.levelDb <= -99 ? "silence" : `${state.levelDb.toFixed(0)} dBFS`}</span>
      </div>
      <div className="sd-voice__meter">
        <span className="sd-setting__label">&ldquo;{word}&rdquo;</span>
        <Meter value={state.probability} label="Wake word probability" tone={above ? "success" : "accent"} />
        <span className="sd-setting__help">
          {Math.round(state.probability * 100)} % · threshold {Math.round(state.threshold * 100)} %
        </span>
      </div>
      <DataList
        inline
        items={[
          { term: "Heard", value: heardText(state) },
          { term: "Last request", value: state.lastResult || "—" },
        ]}
      />
    </div>
  );
}

// ---------- Settings ----------

function toSettings(f: VoiceForm): Omit<Partial<VoiceSettings>, "$typeName"> {
  return {
    pipeline: f.pipeline,
    microphone: f.microphone,
    speak: f.speak,
    silenceMs: f.silenceMs,
    vadThreshold: f.vadThreshold,
    noSpeechMs: f.noSpeechMs,
    maxSeconds: f.maxSeconds,
    wakeWord: f.wakeWord,
    wakeThreshold: f.wakeThreshold ?? undefined,
  };
}

function SettingsForm({ voice }: { voice: GetVoiceResponse }) {
  const initial = formOf(voice.settings);
  const [form, setForm] = useState<VoiceForm>(initial);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const dirty = JSON.stringify(form) !== JSON.stringify(initial);
  const save = () => {
    setBusy(true);
    deviceClient
      .saveVoice({ settings: toSettings(form) })
      .then(() => setResult({ ok: true, text: "Saved; applied without a restart." }))
      .catch((err: unknown) => setResult({ ok: false, text: message(err) }))
      .finally(() => setBusy(false));
  };
  return (
    <>
      <WakeFields form={form} setForm={setForm} models={voice.models} />
      <Panel title="Requests" className="sd-settings__module">
        <RequestFields form={form} setForm={setForm} microphones={voice.microphones} testInput={voice.testInput} />
      </Panel>
      {result && <p className={result.ok ? "sd-setting__ok" : "sd-setting__error"}>{result.text}</p>}
      <div className="sd-settings__actions">
        <Button variant="primary" disabled={!dirty || busy} onClick={save}>
          Save voice settings
        </Button>
      </div>
    </>
  );
}

function modelLabel(m: WakeWordModel): string {
  const languages = m.languages.length > 0 ? ` (${m.languages.join(", ")})` : "";
  return `${m.wakeWord}${languages}${m.builtin ? "" : " · uploaded"}`;
}

interface FieldsProps {
  form: VoiceForm;
  setForm(f: VoiceForm): void;
}

function WakeFields({ form, setForm, models }: FieldsProps & { models: WakeWordModel[] }) {
  const on = form.wakeWord !== "";
  const model = models.find((m) => m.id === form.wakeWord);
  const turn = (enabled: boolean) => setForm({ ...form, wakeWord: enabled ? (models[0]?.id ?? "") : "", wakeThreshold: null });
  return (
    <Panel title="Listen for a wake word" className="sd-settings__module">
      <div className="sd-setting">
        <span className="sd-setting__label">Wake word</span>
        <Toggle label="Wake word" checked={on} onChange={turn} />
      </div>
      {on && (
        <div className="sd-setting">
          <label className="sd-setting__label" htmlFor="voice-wake-word">
            Say
          </label>
          <select id="voice-wake-word" className="sd-input" value={form.wakeWord} onChange={(e) => setForm({ ...form, wakeWord: e.currentTarget.value, wakeThreshold: null })}>
            {models.map((m) => (
              <option key={m.id} value={m.id}>
                {modelLabel(m)}
              </option>
            ))}
          </select>
          <span className="sd-setting__help">
            Trained for {model?.languages.join(", ") || "unknown languages"}; it may not recognise other accents as well.
          </span>
        </div>
      )}
      {on && model && <ThresholdField form={form} setForm={setForm} model={model} />}
    </Panel>
  );
}

function ThresholdField({ form, setForm, model }: FieldsProps & { model: WakeWordModel }) {
  const value = Math.round((form.wakeThreshold ?? model.threshold) * 100);
  return (
    <div className="sd-setting">
      <span className="sd-setting__label">Threshold</span>
      <Slider label="Wake word threshold" min={50} max={99} value={value} onChange={(v) => setForm({ ...form, wakeThreshold: v / 100 })} />
      <span className="sd-field__value">{value} %</span>
      <span className="sd-setting__help">
        Lower hears you more easily but also triggers on other speech; higher needs a clearer word. The model&apos;s own value is{" "}
        {Math.round(model.threshold * 100)} %.{" "}
        {form.wakeThreshold !== null && (
          <Button size="sm" onClick={() => setForm({ ...form, wakeThreshold: null })}>
            Use the model&apos;s
          </Button>
        )}
      </span>
    </div>
  );
}

// ---------- Uploads ----------

function UploadPanel({ models, inUse, onChanged }: { models: WakeWordModel[]; inUse: string; onChanged(): void }) {
  const uploaded = models.filter((m) => !m.builtin);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const remove = (id: string) =>
    deviceClient
      .deleteWakeWord({ id })
      .then(() => {
        setNote({ ok: true, text: `Deleted ${id}.` });
        onChanged();
      })
      .catch((err: unknown) => setNote({ ok: false, text: message(err) }));
  return (
    <Panel title="Your own wake word" className="sd-settings__module">
      <TrainingHelp />
      <UploadForm onDone={(text, ok) => (setNote({ ok, text }), ok && onChanged())} />
      {note && <p className={note.ok ? "sd-setting__ok" : "sd-setting__error"}>{note.text}</p>}
      {uploaded.length > 0 && (
        <ul className="sd-voice__uploads">
          {uploaded.map((m) => (
            <li key={m.id}>
              <span>
                {modelLabel(m)} <code className="sd-setting__help">{m.id}</code>
              </span>
              <Button size="sm" disabled={m.id === inUse} onClick={() => void remove(m.id)}>
                Delete
              </Button>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}

function TrainingHelp() {
  return (
    <details className="sd-settings__help">
      <summary>How to make one</summary>
      <p>
        Wake words are microWakeWord models. Training doesn&apos;t run on the dock: it needs a PC with a GPU or Google Colab and
        takes an hour or more. Use the <code>basic_training_notebook</code> of{" "}
        <a href="https://github.com/kahrendt/microWakeWord" target="_blank" rel="noreferrer">
          microWakeWord
        </a>
        : enter your phrase, it makes thousands of spoken samples with Piper (any Piper language), trains and exports a{" "}
        <code>.tflite</code> model with its <code>.json</code> manifest. Upload both here; the dock checks that it can run the
        model (up to 1 MiB, the microWakeWord operators, 40 features in, a probability out) before it keeps it.
      </p>
    </details>
  );
}

function UploadForm({ onDone }: { onDone(text: string, ok: boolean): void }) {
  const [model, setModel] = useState<File | null>(null);
  const [manifest, setManifest] = useState<File | null>(null);
  const [busy, setBusy] = useState(false);
  const upload = async () => {
    if (!model || !manifest) return;
    setBusy(true);
    try {
      const res = await deviceClient.uploadWakeWord({
        model: new Uint8Array(await model.arrayBuffer()),
        manifest: new Uint8Array(await manifest.arrayBuffer()),
      });
      onDone(`Added “${res.model?.wakeWord ?? "?"}”. Choose it above to use it.`, true);
    } catch (err) {
      onDone(message(err), false);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="sd-voice__upload">
      <label className="sd-setting__label">
        Model (.tflite)
        <input type="file" accept=".tflite" className="sd-input" onChange={(e) => setModel(e.currentTarget.files?.[0] ?? null)} />
      </label>
      <label className="sd-setting__label">
        Manifest (.json)
        <input type="file" accept=".json,application/json" className="sd-input" onChange={(e) => setManifest(e.currentTarget.files?.[0] ?? null)} />
      </label>
      <Button disabled={!model || !manifest || busy} onClick={() => void upload()}>
        Upload
      </Button>
    </div>
  );
}

