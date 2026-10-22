// The requests part of the Voice settings (H15-69, H15-68): which assist pipeline and microphone,
// whether the answer is spoken, and when a request ends (voice activity detection).
import { useState } from "react";

import { deviceClient } from "../api/client.js";
import { Button, Toggle } from "../design/index.js";
import type { AssistPipeline, Microphone } from "../gen/streamdock/v1/device_pb.js";
import { message, type VoiceForm } from "./Voice.js";

interface Props {
  form: VoiceForm;
  setForm(f: VoiceForm): void;
  microphones: Microphone[];
  testInput: string;
}

export function RequestFields({ form, setForm, microphones, testInput }: Props) {
  return (
    <>
      <PipelineField form={form} setForm={setForm} />
      <MicrophoneField form={form} setForm={setForm} microphones={microphones} testInput={testInput} />
      <div className="sd-setting">
        <span className="sd-setting__label">Spoken answer</span>
        <Toggle label="Play the answer" checked={form.speak} onChange={(speak) => setForm({ ...form, speak })} />
      </div>
      <NumberField
        id="voice-silence"
        label="Silence that ends a request"
        unit="ms"
        min={200}
        max={5000}
        value={form.silenceMs}
        onChange={(silenceMs) => setForm({ ...form, silenceMs })}
      />
      <NumberField
        id="voice-no-speech"
        label="Give up without speech after"
        unit="ms"
        min={1000}
        max={30000}
        value={form.noSpeechMs}
        onChange={(noSpeechMs) => setForm({ ...form, noSpeechMs })}
      />
      <NumberField
        id="voice-max"
        label="Longest request"
        unit="s"
        min={2}
        max={60}
        value={form.maxSeconds}
        onChange={(maxSeconds) => setForm({ ...form, maxSeconds })}
      />
      <NumberField
        id="voice-vad"
        label="Speech threshold (0.05-0.95)"
        unit=""
        min={0.05}
        max={0.95}
        step={0.05}
        value={form.vadThreshold}
        onChange={(vadThreshold) => setForm({ ...form, vadThreshold })}
        help="Lower takes quieter speech for speech, but also music and noise."
      />
    </>
  );
}

function pipelineLabel(p: AssistPipeline, preferred: string): string {
  const language = p.sttLanguage || p.language;
  return `${p.name}${language ? ` (speech: ${language})` : ""}${p.id === preferred ? " · preferred" : ""}`;
}

function PipelineField({ form, setForm }: Pick<Props, "form" | "setForm">) {
  const [list, setList] = useState<{ pipelines: AssistPipeline[]; preferred: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = () =>
    deviceClient
      .listAssistPipelines({})
      .then((res) => {
        setList({ pipelines: res.pipelines, preferred: res.preferred });
        setError(null);
      })
      .catch((err: unknown) => setError(message(err)));
  const known = list?.pipelines.some((p) => p.id === form.pipeline) ?? false;
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor="voice-pipeline">
        Assist pipeline
      </label>
      <select id="voice-pipeline" className="sd-input" value={form.pipeline} onChange={(e) => setForm({ ...form, pipeline: e.currentTarget.value })}>
        <option value="">Home Assistant&apos;s preferred</option>
        {form.pipeline && !known && <option value={form.pipeline}>{form.pipeline}</option>}
        {list?.pipelines.map((p) => (
          <option key={p.id} value={p.id}>
            {pipelineLabel(p, list.preferred)}
          </option>
        ))}
      </select>
      <Button size="sm" onClick={() => void load()}>
        {list ? "Reload list" : "Load from Home Assistant"}
      </Button>
      {error && <span className="sd-setting__error">{error}</span>}
    </div>
  );
}

function MicrophoneField({ form, setForm, microphones, testInput }: Props) {
  const known = microphones.some((m) => m.name === form.microphone);
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor="voice-microphone">
        Microphone
      </label>
      <select id="voice-microphone" className="sd-input" value={form.microphone} onChange={(e) => setForm({ ...form, microphone: e.currentTarget.value })}>
        <option value="">PipeWire&apos;s default</option>
        {form.microphone && !known && <option value={form.microphone}>{form.microphone} (not found)</option>}
        {microphones.map((m) => (
          <option key={m.name} value={m.name}>
            {m.description}
          </option>
        ))}
      </select>
      {microphones.length === 0 && <span className="sd-setting__help">No microphone found: plug in a USB microphone or sound card.</span>}
      {testInput && <span className="sd-setting__help">Test input instead of the microphone: {testInput}</span>}
    </div>
  );
}

interface NumberProps {
  id: string;
  label: string;
  unit: string;
  min: number;
  max: number;
  step?: number;
  value: number;
  onChange(v: number): void;
  help?: string;
}

export function NumberField({ id, label, unit, min, max, step = 1, value, onChange, help }: NumberProps) {
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor={id}>
        {label}
      </label>
      <input
        id={id}
        className="sd-input"
        type="number"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.currentTarget.value))}
      />
      {unit && <span className="sd-field__value">{unit}</span>}
      {help && <span className="sd-setting__help">{help}</span>}
    </div>
  );
}
