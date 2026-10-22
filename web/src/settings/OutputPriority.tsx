// The output priority (H15-79), in Settings → Audio: which output plays when several are there,
// reconnecting the first one after audio restarts, and whether other Bluetooth audio devices may
// stay connected. Choosing an output above (Outputs) puts it first here.
import { useEffect, useState } from "react";

import { deviceClient } from "../api/client.js";
import { Badge, Button, Panel, Toggle } from "../design/index.js";
import type { AudioOutput, OutputPriority, PriorityOutput } from "../gen/streamdock/v1/device_pb.js";

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

interface Entry {
  name: string;
  label: string;
  present: boolean;
}

interface Form {
  entries: Entry[];
  disconnectOthers: boolean;
}

function formOf(p: OutputPriority | undefined): Form {
  return {
    entries: (p?.outputs ?? []).map((o: PriorityOutput) => ({ name: o.name, label: o.label, present: o.present })),
    disconnectOthers: p?.disconnectOthers ?? false,
  };
}

/** `GetOutputPriority`, again whenever the outputs change (presence) or after a save. */
function usePriority(outputs: AudioOutput[]) {
  const [priority, setPriority] = useState<OutputPriority | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reads, setReads] = useState(0);
  const present = outputs.map((o) => `${o.name}:${o.selected}`).join(",");
  useEffect(() => {
    deviceClient
      .getOutputPriority({})
      .then((res) => {
        setPriority(res.priority ?? null);
        setError(null);
      })
      .catch((err: unknown) => setError(message(err)));
  }, [present, reads]);
  return { priority, error, reload: () => setReads((n) => n + 1) };
}

function moved<T>(list: T[], from: number, to: number): T[] {
  const copy = [...list];
  const [item] = copy.splice(from, 1);
  if (item !== undefined) copy.splice(to, 0, item);
  return copy;
}

export function OutputPriorityPanel({ outputs }: { outputs: AudioOutput[] }) {
  const { priority, error, reload } = usePriority(outputs);
  if (error) {
    return (
      <Panel title="Output priority" className="sd-settings__module">
        <p className="sd-setting__error">{error}</p>
      </Panel>
    );
  }
  if (!priority) return null;
  return <PriorityForm key={JSON.stringify(formOf(priority))} priority={priority} outputs={outputs} onSaved={reload} />;
}

function PriorityForm({ priority, outputs, onSaved }: { priority: OutputPriority; outputs: AudioOutput[]; onSaved(): void }) {
  const initial = formOf(priority);
  const [form, setForm] = useState<Form>(initial);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const dirty = JSON.stringify(form) !== JSON.stringify(initial);
  const addable = outputs.filter((o) => !form.entries.some((e) => e.name === o.name));
  const save = () =>
    deviceClient
      .setOutputPriority({
        priority: { outputs: form.entries.map((e) => ({ name: e.name, label: e.label })), disconnectOthers: form.disconnectOthers },
      })
      .then(() => {
        setNote({ ok: true, text: "Saved." });
        onSaved();
      })
      .catch((err: unknown) => setNote({ ok: false, text: message(err) }));
  return (
    <Panel title="Output priority" aside={`${form.entries.length}`} className="sd-settings__module">
      <p className="sd-settings__help">
        The highest output that is there plays; a lower one only while the higher ones are absent, and the higher one takes over as soon as
        it is back (also after a restart). After an audio restart the first headset is reconnected. Choosing an output above puts it first.
        The Lyrion player can be fixed to an output of its own (Settings → Lyrion player → Plays on).
      </p>
      <PriorityList form={form} setForm={setForm} />
      {addable.length > 0 && (
        <div className="sd-settings__actions">
          {addable.map((o) => (
            <Button key={o.name} size="sm" onClick={() => setForm({ ...form, entries: [...form.entries, { name: o.name, label: o.description, present: true }] })}>
              + {o.description}
            </Button>
          ))}
        </div>
      )}
      <div className="sd-setting">
        <span className="sd-setting__label">Other Bluetooth audio</span>
        <Toggle
          label="Disconnect other Bluetooth audio devices while a higher one plays"
          offLabel="Stay connected"
          onLabel="Disconnect"
          checked={form.disconnectOthers}
          onChange={(disconnectOthers) => setForm({ ...form, disconnectOthers })}
        />
        <span className="sd-setting__help">
          Off: a speaker that connects itself stays connected but never plays unless it is the highest one there.
        </span>
      </div>
      {note && <p className={note.ok ? "sd-setting__ok" : "sd-setting__error"}>{note.text}</p>}
      <div className="sd-settings__actions">
        <Button variant="primary" disabled={!dirty} onClick={() => void save()}>
          Save priority
        </Button>
      </div>
    </Panel>
  );
}

function PriorityList({ form, setForm }: { form: Form; setForm(f: Form): void }) {
  if (form.entries.length === 0) {
    return <p className="sd-settings__help">Empty: PipeWire&apos;s own choice plays. Choose an output above or add one below.</p>;
  }
  const move = (from: number, to: number) => setForm({ ...form, entries: moved(form.entries, from, to) });
  const remove = (i: number) => setForm({ ...form, entries: form.entries.filter((_, j) => j !== i) });
  return (
    <ol className="sd-priority">
      {form.entries.map((e, i) => (
        <li key={e.name}>
          <span className="sd-priority__rank">{i + 1}</span>
          <span className="sd-priority__label">{e.label || e.name}</span>
          <Badge tone={e.present ? "success" : "muted"}>{e.present ? "there" : "absent"}</Badge>
          <Button size="sm" disabled={i === 0} aria-label={`Move ${e.label} up`} onClick={() => move(i, i - 1)}>
            ▲
          </Button>
          <Button size="sm" disabled={i === form.entries.length - 1} aria-label={`Move ${e.label} down`} onClick={() => move(i, i + 1)}>
            ▼
          </Button>
          <Button size="sm" aria-label={`Remove ${e.label}`} onClick={() => remove(i)}>
            ✕
          </Button>
        </li>
      ))}
    </ol>
  );
}
