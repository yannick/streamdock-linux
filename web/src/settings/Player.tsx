// The built-in Lyrion player (H15-52, H15-61), for the settings page: on/off, its name in Lyrion, the
// server (discovery or a fixed one, with the servers found on the LAN), the highest sample rate and the
// audio settings (PlayerAudio.tsx). The status (track, format, buffers) is live
// (`WatchEventsResponse.player`); the settings are read again when config.toml changes.
import { useEffect, useState } from "react";

import { deviceClient } from "../api/client.js";
import { useLive, useRevisionChange } from "../api/live.js";
import { Badge, Button, DataList, Panel, Toggle } from "../design/index.js";
import type { GetPlayerResponse, LyrionServer, PlayerState } from "../gen/streamdock/v1/device_pb.js";
import { PlayerAudioFields, type AudioForm } from "./PlayerAudio.js";
import { formatText, serverRule } from "./playerFormat.js";

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

interface Form extends AudioForm {
  enabled: boolean;
  name: string;
  /** Empty: discovery. */
  server: string;
  maxSampleRate: number;
}

const DEFAULT_FORM: Form = {
  enabled: false,
  name: "",
  server: "",
  maxSampleRate: 96000,
  formats: ["flac", "mp3", "pcm"],
  outputBufferMs: 4000,
  replayGain: "",
  output: "dsp",
  transition: "",
  transitionSeconds: 5,
  resampling: "balanced",
};

/** The form as the daemon sends it (it always fills every field). */
function formOf(res: GetPlayerResponse): Form {
  const s = res.settings;
  if (!s) return DEFAULT_FORM;
  return {
    enabled: s.enabled,
    name: s.name,
    server: s.server,
    maxSampleRate: s.maxSampleRate,
    formats: [...s.formats],
    outputBufferMs: s.outputBufferMs,
    replayGain: s.replayGain,
    output: s.output,
    transition: s.transition,
    transitionSeconds: s.transitionSeconds,
    resampling: s.resampling,
  };
}

/** `GetPlayer`, again whenever the config changes; the state follows `WatchEvents`. */
function usePlayer(): { player: GetPlayerResponse | null; state: PlayerState | null; error: string | null } {
  const [player, setPlayer] = useState<GetPlayerResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reads, setReads] = useState(0);
  const live = useLive().player;
  useEffect(() => {
    deviceClient
      .getPlayer({})
      .then((res) => {
        setPlayer(res);
        setError(null);
      })
      .catch((err: unknown) => setError(message(err)));
  }, [reads]);
  useRevisionChange("config", () => setReads((n) => n + 1));
  return { player, state: live ?? player?.state ?? null, error };
}

type Result = { ok: boolean; text: string } | null;

export function PlayerSection() {
  const { player, state, error } = usePlayer();
  // Here, not in the form: a save changes the config, which remounts the form with the new values.
  const [result, setResult] = useState<Result>(null);
  if (error) {
    return (
      <Panel title="Lyrion player">
        <p className="sd-setting__error">{error}</p>
      </Panel>
    );
  }
  if (!player) return <Panel title="Lyrion player">Loading…</Panel>;
  return (
    <>
      <StatusPanel state={state} />
      <SettingsPanel key={JSON.stringify(formOf(player))} player={player} result={result} setResult={setResult} />
    </>
  );
}

function statusText(state: PlayerState | null): string {
  if (!state?.enabled) return "off";
  if (!state.connected) return "connecting";
  if (state.paused) return "paused";
  return state.playing ? "playing" : "idle";
}

function statusItems(state: PlayerState | null, status: string) {
  const s = state ?? { connected: false, name: "", server: "", playerId: "" };
  return [
    { term: "State", value: <Badge tone={s.connected ? "accent" : "muted"}>{status}</Badge> },
    { term: "Name in Lyrion", value: s.name || "—" },
    { term: "Server", value: s.server || "—" },
    { term: "Player id", value: <code>{s.playerId || "—"}</code> },
  ];
}

/** What plays: the track, its format and the buffers (only while a track plays). */
function playingItems(state: PlayerState | null) {
  if (!state?.playing) return [];
  const track = [...new Set([state.title, state.artist, state.album].filter(Boolean))].join(" · ");
  const format = formatText(state);
  return [
    { term: "Track", value: track || "—" },
    { term: "Format", value: format || "—" },
    { term: "Buffers", value: `output ${state.outputFill} % · stream ${state.decodeFill} %` },
    { term: "Underruns", value: String(state.underruns) },
  ];
}

function StatusPanel({ state }: { state: PlayerState | null }) {
  const status = statusText(state);
  const items = [...statusItems(state, status), ...playingItems(state)];
  return (
    <Panel title="Lyrion player" aside={status} className="sd-settings__module">
      <p className="sd-settings__help">
        This machine as a player of its own in Lyrion: it plays through the sound settings (Audio) and Lyrion's volume
        is the output's volume.
      </p>
      <DataList items={items} inline />
      {state?.problem && <p className="sd-setting__error">{state.problem}</p>}
      <PlayerActions on={state?.enabled === true} />
      {state?.playerId && <ServerConversion state={state} />}
    </Panel>
  );
}

interface SettingsPanelProps {
  player: GetPlayerResponse;
  result: Result;
  setResult(result: Result): void;
}

function SettingsPanel({ player, result, setResult }: SettingsPanelProps) {
  const [form, setForm] = useState<Form>(() => formOf(player));
  const [busy, setBusy] = useState(false);
  const save = () => {
    setBusy(true);
    setResult(null);
    deviceClient
      .setPlayer({ settings: form })
      .then(() => setResult({ ok: true, text: "Saved and applied." }))
      .catch((err: unknown) => setResult({ ok: false, text: message(err) }))
      .finally(() => setBusy(false));
  };
  const dirty = JSON.stringify(form) !== JSON.stringify(formOf(player));
  return (
    <Panel title="Settings" className="sd-settings__module">
      <div className="sd-setting">
        <span className="sd-setting__label">Player</span>
        <Toggle label="Built-in player" checked={form.enabled} onChange={(enabled) => setForm({ ...form, enabled })} />
      </div>
      <NameField value={form.name} onChange={(name) => setForm({ ...form, name })} />
      <ServerField value={form.server} onChange={(server) => setForm({ ...form, server })} />
      <RateField rates={player.sampleRates} value={form.maxSampleRate} onChange={(maxSampleRate) => setForm({ ...form, maxSampleRate })} />
      <PlayerAudioFields form={form} outputs={player.outputs} onChange={(audio) => setForm({ ...form, ...audio })} />
      {result && <p className={result.ok ? "sd-setting__ok" : "sd-setting__error"}>{result.text}</p>}
      <div className="sd-settings__actions">
        <Button variant="primary" disabled={!dirty || busy} onClick={save}>
          {busy ? "Saving…" : "Save"}
        </Button>
        <span className="sd-setting__help">A new name renames the player in Lyrion; other changes reconnect it.</span>
      </div>
    </Panel>
  );
}

/**
 * Converting on the server (H15-73): whether the last FLAC came resampled by Lyrion, and the rule
 * to add to Lyrion's custom-convert.conf (a change on the server, so it is shown, not applied).
 */
function ServerConversion({ state }: { state: PlayerState }) {
  const output = state.outputSampleRate || 48000;
  const converted = state.sampleRate > 0 && state.sourceSampleRate > 0 && state.sampleRate !== state.sourceSampleRate;
  return (
    <details className="sd-settings__help">
      <summary>Convert to {output / 1000} kHz on the Lyrion server{converted ? " (active)" : ""}</summary>
      <p>
        The output runs at {output / 1000} kHz. Lyrion can resample FLAC before sending it, so this machine doesn't
        have to: add this to <code>custom-convert.conf</code> next to Lyrion's <code>convert.conf</code> and restart
        Lyrion. The format above then reads "converted by Lyrion".
      </p>
      <pre>{serverRule(state.playerId, output)}</pre>
    </details>
  );
}

/** Reconnect (also: switch to the server discovery finds now) and a new player id. */
function PlayerActions({ on }: { on: boolean }) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<Result>(null);
  const run = (action: () => Promise<string>) => {
    setBusy(true);
    setNote(null);
    action()
      .then((text) => setNote({ ok: true, text }))
      .catch((err: unknown) => setNote({ ok: false, text: message(err) }))
      .finally(() => setBusy(false));
  };
  const reconnect = () => run(() => deviceClient.reconnectPlayer({}).then(() => "Reconnecting…"));
  const newId = () => {
    if (!window.confirm("Give the player a new id? Lyrion then sees a new player; remove the old one in Lyrion.")) return;
    run(() => deviceClient.resetPlayerId({}).then((res) => `New player id ${res.playerId}.`));
  };
  return (
    <>
      {note && <p className={note.ok ? "sd-setting__ok" : "sd-setting__error"}>{note.text}</p>}
      <div className="sd-settings__actions">
        <Button size="sm" disabled={!on || busy} onClick={reconnect}>
          Reconnect
        </Button>
        <Button size="sm" disabled={busy} onClick={newId}>
          New player id
        </Button>
      </div>
    </>
  );
}

function NameField({ value, onChange }: { value: string; onChange(v: string): void }) {
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor="player-name">
        Name in Lyrion
      </label>
      <input id="player-name" className="sd-input" value={value} placeholder="StreamDock" onChange={(e) => onChange(e.currentTarget.value)} />
    </div>
  );
}

function RateField({ rates, value, onChange }: { rates: readonly number[]; value: number; onChange(v: number): void }) {
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor="player-rate">
        Highest sample rate
      </label>
      <select id="player-rate" className="sd-input" value={value} onChange={(e) => onChange(Number(e.currentTarget.value))}>
        {rates.map((r) => (
          <option key={r} value={r}>
            {(r / 1000).toFixed(r % 1000 ? 1 : 0)} kHz
          </option>
        ))}
      </select>
      <span className="sd-setting__help">Lyrion resamples anything above it.</span>
    </div>
  );
}

function ServerField({ value, onChange }: { value: string; onChange(v: string): void }) {
  const [fixed, setFixed] = useState(value !== "");
  const setMode = (on: boolean) => {
    setFixed(on);
    if (!on) onChange("");
  };
  return (
    <div className="sd-setting">
      <span className="sd-setting__label">Server</span>
      <Toggle label="Lyrion server" offLabel="Automatic" onLabel="Fixed" checked={fixed} onChange={setMode} />
      {fixed && (
        <input
          aria-label="Server address"
          className="sd-input"
          value={value}
          placeholder="192.168.1.2 or 192.168.1.2:3483"
          onChange={(e) => onChange(e.currentTarget.value)}
        />
      )}
      <ServerList
        onPick={(address) => {
          setFixed(true);
          onChange(address);
        }}
      />
    </div>
  );
}

/** "Find servers": discovery on the LAN; picking one makes it the fixed server. */
function ServerList({ onPick }: { onPick(address: string): void }) {
  const [servers, setServers] = useState<LyrionServer[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const find = () => {
    setBusy(true);
    setError(null);
    deviceClient
      .findLyrionServers({})
      .then((res) => setServers(res.servers))
      .catch((err: unknown) => setError(message(err)))
      .finally(() => setBusy(false));
  };
  return (
    <>
      <div className="sd-settings__actions">
        <Button size="sm" disabled={busy} onClick={find}>
          {busy ? "Searching…" : "Find servers"}
        </Button>
      </div>
      {error && <p className="sd-setting__error">{error}</p>}
      {servers?.length === 0 && <p className="sd-settings__help">No Lyrion server answered.</p>}
      {servers && servers.length > 0 && (
        <ul className="sd-netlist">
          {servers.map((s) => (
            <li key={s.address} className="sd-netlist__item">
              <div className="sd-netlist__head">
                <span className="sd-netlist__name">{s.name || "Lyrion"}</span>
                <span className="sd-setting__help">
                  {s.address} · {s.version}
                </span>
                <Button size="sm" onClick={() => onPick(s.address)}>
                  Use
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
