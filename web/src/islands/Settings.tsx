// The settings screen (H15-22): every module's settings, validated and saved by the daemon.
// Secrets (the Home Assistant token) are write-only: the page can store one, never read it.
import { useEffect, useRef, useState } from "react";

import { deviceClient } from "../api/client.js";
import { useRevisionChange } from "../api/live.js";
import { AudioSection, useAudio } from "../settings/Audio.js";
import { ModuleField } from "../settings/ModuleField.js";
import { PlayerSection } from "../settings/Player.js";
import { VoiceSection } from "../settings/Voice.js";
import { EthernetPanel } from "../settings/Ethernet.js";
import { BluetoothPanel, type BluetoothList, useNetwork, WifiModePanel, WifiPanel } from "../settings/Network.js";
import { Button, Panel, Toggle } from "../design/index.js";
import {
  SettingType,
  type AudioState,
  type GetNetworkResponse,
  type GetSettingsResponse,
  type ImportBackupResponse,
  type KeyAsset,
  type ModuleSettings,
  type SettingField,
} from "../gen/streamdock/v1/device_pb.js";

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

function SecretField({ module, field, onSaved }: { module: string; field: SettingField; onSaved(): void }) {
  const [value, setValue] = useState("");
  const [status, setStatus] = useState<string | null>(null);
  const save = () => {
    deviceClient
      .setSecret({ module, key: field.key, value })
      .then(() => {
        setValue("");
        setStatus("stored");
        onSaved();
      })
      .catch((err: unknown) => setStatus(message(err)));
  };
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor={`${module}-${field.key}`}>
        {field.label}
        {field.required && <span className="sd-setting__required"> *</span>}
      </label>
      <div className="sd-setting__row">
        <input
          id={`${module}-${field.key}`}
          className="sd-input"
          type="password"
          autoComplete="off"
          placeholder={field.secretSet ? "•••••••• (stored, enter a new one to replace)" : "not set"}
          value={value}
          onChange={(e) => setValue(e.currentTarget.value)}
        />
        <Button size="sm" disabled={!value.trim()} onClick={save}>
          Store
        </Button>
      </div>
      <span className="sd-setting__help">
        {field.secretSet ? "A token is stored. " : "No token stored. "}
        {field.help}
      </span>
      {status && <span className={status === "stored" ? "sd-setting__ok" : "sd-setting__error"}>{status}</span>}
    </div>
  );
}

function ModuleForm({ module, onSaved }: { module: ModuleSettings; onSaved(): void }) {
  const [enabled, setEnabled] = useState(module.enabled);
  const [values, setValues] = useState<Record<string, string>>({ ...module.values });
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  // Show what the daemon has after every reload (e.g. after a save, or a change elsewhere, H15-49),
  // keeping the result message. Only when this module changed, so editing one form survives a
  // change of another.
  const saved = JSON.stringify([module.enabled, module.values]);
  useEffect(() => {
    setEnabled(module.enabled);
    setValues({ ...module.values });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `saved` is the module's content
  }, [saved]);

  const save = () => {
    setBusy(true);
    deviceClient
      .updateModuleSettings({ module: module.id, enabled, values })
      .then((res) => {
        setResult({
          ok: true,
          text: res.restartRequired ? "Saved. Restart streamdockd to apply." : "Saved and applied.",
        });
        onSaved();
      })
      .catch((err: unknown) => setResult({ ok: false, text: message(err) }))
      .finally(() => setBusy(false));
  };

  return (
    <Panel
      title={module.title}
      aside={module.error ? "problem" : module.enabled ? "enabled" : module.configured ? "disabled" : "not set up"}
      accent={module.enabled}
      className="sd-settings__module"
    >
      <p className="sd-settings__help">{module.help}</p>
      {module.error && <p className="sd-setting__error">{module.error}</p>}
      <div className="sd-setting">
        <Toggle label={`Enable ${module.title}`} checked={enabled} onChange={setEnabled} />
      </div>
      {module.fields.map((field) =>
        field.type === SettingType.SECRET ? (
          <SecretField key={field.key} module={module.id} field={field} onSaved={onSaved} />
        ) : (
          <ModuleField
            key={field.key}
            moduleId={module.id}
            field={field}
            values={values}
            onChange={(v) => setValues((old) => ({ ...old, [field.key]: v }))}
          />
        ),
      )}
      <div className="sd-settings__actions">
        <Button variant="primary" disabled={busy} onClick={save}>
          Save
        </Button>
        {result && <span className={result.ok ? "sd-setting__ok" : "sd-setting__error"}>{result.text}</span>}
      </div>
    </Panel>
  );
}

/** Export and import of the whole configuration (H15-26). */
function BackupPanel({ onImported }: { onImported(): void }) {
  const file = useRef<HTMLInputElement>(null);
  const [error, setError] = useState<string | null>(null);
  const [imported, setImported] = useState<ImportBackupResponse | null>(null);

  const exportBackup = () => {
    setError(null);
    deviceClient
      .exportBackup({})
      .then((res) => {
        const url = URL.createObjectURL(new Blob([new Uint8Array(res.data)], { type: "application/toml" }));
        const a = document.createElement("a");
        a.href = url;
        a.download = res.fileName;
        a.click();
        URL.revokeObjectURL(url);
      })
      .catch((err: unknown) => setError(message(err)));
  };

  const importBackup = (chosen: File) => {
    setError(null);
    setImported(null);
    chosen
      .arrayBuffer()
      .then((buf) => deviceClient.importBackup({ data: new Uint8Array(buf) }))
      .then((res) => {
        setImported(res);
        onImported();
      })
      .catch((err: unknown) => setError(message(err)));
  };

  return (
    <Panel title="Backup" aside="config file" className="sd-settings__module">
      <p className="sd-settings__help">
        Everything on this page as one file, to keep or to move to another machine. Tokens are never included. An
        import keeps this machine's address and dock settings and saves the old config as <code>.bak</code>.
      </p>
      <div className="sd-settings__actions">
        <Button variant="primary" onClick={exportBackup}>
          Export
        </Button>
        <Button onClick={() => file.current?.click()}>Import…</Button>
        <input
          ref={file}
          type="file"
          accept=".toml,application/toml,text/plain"
          hidden
          onChange={(e) => {
            const chosen = e.currentTarget.files?.[0];
            e.currentTarget.value = "";
            if (chosen) importBackup(chosen);
          }}
        />
      </div>
      {error && <p className="sd-setting__error">{error}</p>}
      {imported && (
        <div className="sd-setting">
          <span className="sd-setting__ok">
            {imported.restartRequired
              ? "Imported. Modules are applied; restart streamdockd for the daemon settings."
              : "Imported and applied."}
          </span>
          {imported.notes.map((n) => (
            <span key={n} className="sd-setting__help">
              {n}
            </span>
          ))}
          {imported.missingSecrets.map((m) => (
            <span key={`${m.module}-${m.key}`} className="sd-setting__error">
              Missing on this machine: {m.label}. Store it in its card above.
            </span>
          ))}
        </div>
      )}
    </Panel>
  );
}

/** A valid image name from a file name: `Desk Lamp.PNG` → `desk-lamp`. */
function imageName(fileName: string): string {
  return fileName
    .replace(/\.[^.]*$/, "")
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^[-_]+|-+$/g, "")
    .slice(0, 64);
}

function previewUrl(asset: KeyAsset): string {
  return URL.createObjectURL(new Blob([new Uint8Array(asset.preview)], { type: "image/png" }));
}

/** Uploaded key images (H15-28): keys show one with `image = "name"` in `[[modules.keys.page.key]]`. */
function KeyImagesPanel() {
  const [assets, setAssets] = useState<KeyAsset[] | null>(null);
  const [previews, setPreviews] = useState<Record<string, string>>({});
  const [chosen, setChosen] = useState<File | null>(null);
  const [name, setName] = useState("");
  const [status, setStatus] = useState<{ ok: boolean; text: string } | null>(null);
  const file = useRef<HTMLInputElement>(null);

  const load = () => {
    deviceClient
      .listKeyAssets({})
      .then((res) => setAssets(res.assets))
      .catch((err: unknown) => setStatus({ ok: false, text: message(err) }));
  };
  useEffect(load, []);
  // Uploads and deletes from another browser show up at once (H15-49).
  useRevisionChange("assets", load);
  useEffect(() => {
    const urls = Object.fromEntries((assets ?? []).map((a) => [a.name, previewUrl(a)]));
    setPreviews(urls);
    return () => Object.values(urls).forEach((u) => URL.revokeObjectURL(u));
  }, [assets]);

  const upload = () => {
    if (!chosen) return;
    setStatus(null);
    chosen
      .arrayBuffer()
      .then((buf) => deviceClient.setKeyAsset({ name, data: new Uint8Array(buf) }))
      .then((res) => {
        setStatus({ ok: true, text: `Stored "${res.asset?.name ?? name}". Use image = "${res.asset?.name ?? name}" on a key.` });
        setChosen(null);
        setName("");
        load();
      })
      .catch((err: unknown) => setStatus({ ok: false, text: message(err) }));
  };

  const remove = (asset: KeyAsset) => {
    deviceClient
      .deleteKeyAsset({ name: asset.name })
      .then(() => {
        setStatus({ ok: true, text: `Deleted "${asset.name}".` });
        load();
      })
      .catch((err: unknown) => setStatus({ ok: false, text: message(err) }));
  };

  const validName = /^[a-z0-9][a-z0-9_-]{0,63}$/.test(name);
  return (
    <Panel title="Key images" aside={assets ? `${assets.filter((a) => !a.builtin).length} uploaded` : undefined} className="sd-settings__module">
      <p className="sd-settings__help">
        Pictures for custom keys (PNG or JPEG, up to 4 MB). A key shows one with <code>image = "name"</code>, a toggle
        with <code>on_image</code> / <code>off_image</code>. The frame keeps the key's state colour; the label sits on
        a dark band.
      </p>
      <div className="sd-setting">
        <label className="sd-setting__label" htmlFor="key-image-name">
          New image
        </label>
        <div className="sd-setting__row">
          <Button onClick={() => file.current?.click()}>{chosen ? "Other file…" : "Choose file…"}</Button>
          <input
            id="key-image-name"
            className="sd-input"
            placeholder="name, e.g. desk-lamp"
            value={name}
            onChange={(e) => setName(e.currentTarget.value.toLowerCase())}
          />
          <Button variant="primary" size="sm" disabled={!chosen || !validName} onClick={upload}>
            Upload
          </Button>
        </div>
        <input
          ref={file}
          type="file"
          accept="image/png,image/jpeg"
          hidden
          onChange={(e) => {
            const picked = e.currentTarget.files?.[0];
            e.currentTarget.value = "";
            if (picked) {
              setChosen(picked);
              if (!name) setName(imageName(picked.name));
            }
          }}
        />
        <span className="sd-setting__help">
          {chosen ? `${chosen.name} (${Math.ceil(chosen.size / 1024)} KB)` : "No file chosen."} Names: lowercase
          letters, digits, - and _. An existing name is replaced, and keys using it update at once.
        </span>
      </div>
      {status && <p className={status.ok ? "sd-setting__ok" : "sd-setting__error"}>{status.text}</p>}
      {assets && <UploadedList assets={assets.filter((a) => !a.builtin)} previews={previews} onRemove={remove} />}
      {assets && <IconGallery icons={assets.filter((a) => a.builtin)} previews={previews} />}
    </Panel>
  );
}

function UploadedList({
  assets,
  previews,
  onRemove,
}: {
  assets: KeyAsset[];
  previews: Record<string, string>;
  onRemove(a: KeyAsset): void;
}) {
  if (assets.length === 0) return null;
  return (
    <ul className="sd-images">
      {assets.map((a) => (
        <li key={a.name} className="sd-images__item">
          {previews[a.name] && <img className="sd-images__preview" src={previews[a.name]} alt="" />}
          <span className="sd-images__name">{a.name}</span>
          <span className="sd-setting__help">
            {a.width}×{a.height}
          </span>
          <Button size="sm" onClick={() => onRemove(a)}>
            Delete
          </Button>
        </li>
      ))}
    </ul>
  );
}

/** The built-in icons (H15-48), by group: use one with image = "name". */
function IconGallery({ icons, previews }: { icons: KeyAsset[]; previews: Record<string, string> }) {
  if (icons.length === 0) return null;
  const groups = [...new Set(icons.map((i) => i.group))];
  return (
    <div className="sd-icons">
      <h3 className="sd-slot__title">Built-in icons</h3>
      <p className="sd-settings__help">
        Shipped with streamdockd and tinted by the key's state. Use one with <code>image = "name"</code> or pick it in the
        keys editor; uploading an image with the same name replaces it.
      </p>
      {groups.map((group) => (
        <section key={group}>
          <span className="sd-setting__label">{group}</span>
          <ul className="sd-icons__grid">
            {icons
              .filter((i) => i.group === group)
              .map((i) => (
                <li key={i.name} className="sd-icons__item" title={i.name}>
                  {previews[i.name] && <img className="sd-images__preview" src={previews[i.name]} alt="" />}
                  <span className="sd-icons__name">{i.name}</span>
                </li>
              ))}
          </ul>
        </section>
      ))}
    </div>
  );
}

/** The sections of the settings page, in nav order; `network`/`bluetooth`/`audio` only where managed. */
const SECTIONS = [
  { id: "integrations", label: "Integrations" },
  { id: "network", label: "Network" },
  { id: "bluetooth", label: "Bluetooth" },
  { id: "audio", label: "Audio" },
  { id: "player", label: "Lyrion player" },
  { id: "voice", label: "Voice" },
  { id: "dock", label: "Dock & daemon" },
  { id: "images", label: "Key images" },
  { id: "backup", label: "Backup" },
] as const;

type SectionId = (typeof SECTIONS)[number]["id"];

/** The section in the URL hash (`/settings#network`), following Back and Forward. */
function useSection(available: readonly SectionId[]): SectionId {
  const read = (): SectionId => {
    const hash = window.location.hash.slice(1) as SectionId;
    return available.includes(hash) ? hash : available[0]!;
  };
  const [section, setSection] = useState<SectionId>(read);
  useEffect(() => {
    const onHash = () => setSection(read());
    onHash();
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- re-read when the available sections change
  }, [available.join()]);
  return section;
}

export default function Settings() {
  const [settings, setSettings] = useState<GetSettingsResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reloads, setReloads] = useState(0);
  const { network, bluetooth, reload: reloadNetwork } = useNetwork();
  const { managed: audioManaged, audio } = useAudio();

  useEffect(() => {
    deviceClient
      .getSettings({})
      .then((res) => {
        setSettings(res);
        setError(null);
      })
      .catch((err: unknown) => setError(message(err)));
  }, [reloads]);
  // config.toml changed (another browser, the keys editor, an edit on disk): read it again (H15-49).
  useRevisionChange("config", () => setReloads((n) => n + 1));

  const managed = network?.managed === true;
  const available = SECTIONS.map((s) => s.id).filter(
    (id) => (managed || (id !== "network" && id !== "bluetooth")) && (audioManaged === true || id !== "audio"),
  );
  const section = useSection(available);

  if (error) {
    return (
      <Panel title="Settings">
        <p className="sd-setting__error">{error}</p>
      </Panel>
    );
  }
  if (!settings) {
    return <Panel title="Settings">Loading…</Panel>;
  }
  const reload = () => setReloads((n) => n + 1);
  return (
    <div className="sd-settings-layout">
      <nav className="sd-settings-nav" aria-label="Settings sections">
        {SECTIONS.filter((s) => available.includes(s.id)).map((s) => (
          <a key={s.id} href={`#${s.id}`} aria-current={s.id === section ? "page" : undefined}>
            {s.label}
          </a>
        ))}
      </nav>
      <div className="sd-settings">
        <SectionBody
          section={section}
          settings={settings}
          network={network}
          audio={audio}
          bluetooth={bluetooth}
          reload={reload}
          reloadNetwork={reloadNetwork}
        />
      </div>
    </div>
  );
}

interface SectionBodyProps {
  section: SectionId;
  settings: GetSettingsResponse;
  network: GetNetworkResponse | null;
  audio: AudioState | null;
  bluetooth: BluetoothList;
  reload(): void;
  reloadNetwork(): void;
}

function SectionBody({ section, settings, network, audio, bluetooth, reload, reloadNetwork }: SectionBodyProps) {
  switch (section) {
    case "integrations":
      return settings.modules.map((m) => <ModuleForm key={m.id} module={m} onSaved={reload} />);
    case "network":
    case "bluetooth":
      return <MachineSection section={section} network={network} bluetooth={bluetooth} reloadNetwork={reloadNetwork} />;
    case "audio":
      return <AudioSection audio={audio} />;
    case "player":
      return <PlayerSection />;
    case "voice":
      return <VoiceSection />;
    case "dock":
      return <DaemonPanel settings={settings} />;
    case "images":
      return <KeyImagesPanel />;
    case "backup":
      return <BackupPanel onImported={reload} />;
  }
}

function MachineSection(props: { section: "network" | "bluetooth"; network: GetNetworkResponse | null; bluetooth: BluetoothList; reloadNetwork(): void }) {
  const { network, reloadNetwork } = props;
  if (props.section === "network") return <NetworkSection network={network} reload={reloadNetwork} />;
  return network?.bluetooth ? <BluetoothPanel status={network.bluetooth} list={props.bluetooth} reload={reloadNetwork} /> : null;
}

function NetworkSection({ network, reload }: { network: GetNetworkResponse | null; reload(): void }) {
  return (
    <>
      {network?.wifi?.available && <WifiModePanel mode={network.wifiMode} radioOn={network.wifi.radioOn} reload={reload} />}
      {network?.ethernet && <EthernetPanel status={network.ethernet} reload={reload} />}
      {network?.wifi && <WifiPanel status={network.wifi} reload={reload} />}
    </>
  );
}

/** The daemon's own settings (read-only here; edited in config.toml). */
function DaemonPanel({ settings }: { settings: GetSettingsResponse }) {
  return (
    <Panel title="Dock & daemon" aside="read-only">
      <p className="sd-settings__help">
        These come from <code>config.toml</code> and apply after a restart: address, docks, default brightness, sleep and
        the night window.
      </p>
      <dl className="sd-settings__daemon">
        {settings.daemon.map((d) => (
          <div key={d.key}>
            <dt>{d.label}</dt>
            <dd>{d.value}</dd>
          </div>
        ))}
        <div>
          <dt>Config file</dt>
          <dd>{settings.configPath}</dd>
        </div>
      </dl>
    </Panel>
  );
}
