// The now-playing band of a music tab (H15-88): which keys show it, what text, how it scrolls.
// Opened from the tab's menu; saved to `now_playing` of the tab (config.toml, applied by reload).
import { create } from "@bufbuild/protobuf";
import { useEffect, useState } from "react";

import { deviceClient } from "../api/client.js";
import { Button, Dialog, Toggle } from "../design/index.js";
import { NowPlayingSettingsSchema, type Mode, type NowPlayingSettings } from "../gen/streamdock/v1/device_pb.js";

interface Choice {
  value: string;
  label: string;
}

const KEYS: Choice[] = [
  { value: "1,2,3", label: "Top row (keys 1–3)" },
  { value: "4,5,6", label: "Bottom row (keys 4–6)" },
  { value: "1,2", label: "Keys 1–2" },
  { value: "1", label: "Key 1" },
  { value: "", label: "Off (web only)" },
];

const FIELDS: Choice[] = [
  { value: "title,artist", label: "Title · Artist" },
  { value: "title,artist,album", label: "Title · Artist · Album" },
  { value: "artist,title", label: "Artist · Title" },
  { value: "title", label: "Title" },
];

const SPEEDS: Choice[] = [
  { value: "25", label: "Slow" },
  { value: "40", label: "Normal" },
  { value: "70", label: "Fast" },
];

/** The value of a list setting as a choice key (custom values get their own entry). */
function withCurrent(choices: Choice[], value: string): Choice[] {
  return choices.some((c) => c.value === value) ? choices : [...choices, { value, label: value || "—" }];
}

function Select({ label, choices, value, onChange, help }: { label: string; choices: Choice[]; value: string; onChange(v: string): void; help?: string }) {
  return (
    <label className="sd-setting">
      <span className="sd-setting__label">{label}</span>
      <select className="sd-input" value={value} onChange={(e) => onChange(e.currentTarget.value)}>
        {withCurrent(choices, value).map((c) => (
          <option key={c.value} value={c.value}>
            {c.label}
          </option>
        ))}
      </select>
      {help && <span className="sd-setting__help">{help}</span>}
    </label>
  );
}

function Check({ label, checked, onChange }: { label: string; checked: boolean; onChange(v: boolean): void }) {
  return (
    <div className="sd-setting">
      <span className="sd-setting__label">{label}</span>
      <Toggle label={label} checked={checked} onChange={onChange} />
    </div>
  );
}

function Fields({ s, set }: { s: NowPlayingSettings; set(patch: Partial<NowPlayingSettings>): void }) {
  return (
    <>
      <Select
        label="Keys on the dock"
        choices={KEYS}
        value={s.keys.join(",")}
        onChange={(v) => set({ keys: v ? v.split(",").map(Number) : [] })}
        help="The playlists move to the other keys. Pressing the band plays or pauses."
      />
      <Select label="Text" choices={FIELDS} value={s.fields.join(",")} onChange={(v) => set({ fields: v.split(",") })} />
      <Select
        label="Long text"
        choices={[
          { value: "scroll", label: "Scroll" },
          { value: "cut", label: "Cut with …" },
        ]}
        value={s.scroll ? "scroll" : "cut"}
        onChange={(v) => set({ scroll: v === "scroll" })}
      />
      {s.scroll && <Select label="Speed" choices={SPEEDS} value={String(s.speed)} onChange={(v) => set({ speed: Number(v) })} />}
      <Check label="Cover behind the text" checked={s.cover} onChange={(cover) => set({ cover })} />
      <Check label="Progress bar" checked={s.progress} onChange={(progress) => set({ progress })} />
    </>
  );
}

export function NowPlayingDialog({ mode, onClose, run }: { mode: Mode; onClose(): void; run(p: Promise<unknown>): Promise<boolean> }) {
  const [settings, setSettings] = useState<NowPlayingSettings | null>(null);
  useEffect(() => {
    void run(
      deviceClient.getNowPlaying({ mode: mode.id }).then((res) => setSettings(res.settings ?? create(NowPlayingSettingsSchema))),
    );
  }, [mode.id, run]);
  const set = (patch: Partial<NowPlayingSettings>) => setSettings((s) => (s ? create(NowPlayingSettingsSchema, { ...s, ...patch }) : s));
  const save = async () => {
    if (settings && (await run(deviceClient.setNowPlaying({ mode: mode.id, settings })))) onClose();
  };
  return (
    <Dialog
      open
      title={`Now playing · ${mode.title}`}
      onClose={onClose}
      actions={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!settings} onClick={() => void save()}>
            Save
          </Button>
        </>
      }
    >
      {settings ? <Fields s={settings} set={set} /> : <p className="sd-setting__help">Loading…</p>}
    </Dialog>
  );
}
