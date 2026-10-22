// The live dashboard (H15-7): every attached dock as built, animating on every press and turn,
// plus the event log and device status. All state comes from the WatchEvents stream.
import { useEffect, useReducer, useState } from "react";

import { deviceClient, watchDeviceEvents } from "../api/client.js";
import {
  initialState,
  LCD_KEY_COUNT,
  reducer,
  type DashboardState,
  type DeviceView,
  type LogEntry,
} from "../dashboard/state.js";
import { TabBar, useDockMenus, useSlotTrigger, type Slot } from "../dashboard/edit.js";
import { useDockInput } from "../dashboard/input.js";
import { KeyTile, Knob, NowPlaying, Panel, Slider, Tabs, type KeyTileProps, type KnobProps, type NowPlayingProps } from "../design/index.js";
import {
  ActivityLevel,
  FaceState,
  type Activity,
  type KeyFace,
  type NowPlaying as NowPlayingMessage,
  type Screen,
} from "../gen/streamdock/v1/device_pb.js";

export interface DashboardProps {
  /** Shown before the stream connects. */
  title: string;
}

function Brightness({ view }: { view: DeviceView }) {
  const [value, setValue] = useState(view.device.brightness);
  const [dragging, setDragging] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Follow updates from other clients unless this one is being dragged.
  useEffect(() => {
    if (!dragging) setValue(view.device.brightness);
  }, [view.device.brightness, dragging]);

  const commit = (percent: number) => {
    setDragging(false);
    deviceClient
      .setBrightness({ deviceId: view.device.id, percent })
      .then(() => setError(null))
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)));
  };

  return (
    <div className="sd-field">
      <span className="sd-field__label">Brightness</span>
      <Slider
        label="Brightness"
        value={value}
        onChange={(v) => {
          setDragging(true);
          setValue(v);
        }}
        onCommit={commit}
      />
      <span className="sd-field__value">{value}%</span>
      {error && <span className="sd-field__error">{error}</span>}
    </div>
  );
}

const FACE_STATE_CLASS: Record<FaceState, string> = {
  [FaceState.UNSPECIFIED]: "idle",
  [FaceState.IDLE]: "idle",
  [FaceState.ON]: "on",
  [FaceState.ACTIVE]: "active",
  [FaceState.UNAVAILABLE]: "unavailable",
};

/** A key's face as set by the active mode: label, detail line, and its state as a color. */
function Face({ face }: { face: KeyFace }) {
  return (
    <span className={`sd-face sd-face--${FACE_STATE_CLASS[face.state] ?? "idle"}`}>
      <span className="sd-face__label">{face.label}</span>
      {face.detail && <span className="sd-face__detail">{face.detail}</span>}
    </span>
  );
}

/**
 * URL of the image an LCD key shows on the dock right now (served by the daemon, versioned so the
 * browser never shows a stale one), or undefined when the key is blank or not reported yet.
 */
function imageUrl(view: DeviceView, key: number): string | undefined {
  const img = view.images[key - 1];
  if (!img || img.blank) return undefined;
  return `/dock/${encodeURIComponent(view.device.id)}/keys/${key}?v=${img.version}`;
}

/** Renders a face, or nothing (the tile's default) when the key has none. */
function face(f: KeyFace | undefined) {
  return f ? <Face face={f} /> : undefined;
}

/** The now-playing message (H15-88) as the design system component's props. */
export function nowPlayingProps(np: NowPlayingMessage): NowPlayingProps {
  return {
    text: np.text,
    coverUrl: np.coverUrl || undefined,
    playing: np.playing,
    paused: np.paused,
    idle: np.idle,
    durationMs: np.durationMs,
    elapsedMs: np.elapsedMs,
    elapsedAtMs: Number(np.elapsedAtUnixMs),
    scroll: np.scroll,
    speed: np.scrollPxPerSecond,
    pauseMs: np.scrollPauseMs,
    showCover: np.showCover,
    showProgress: np.showProgress,
  };
}

/** The label of an LCD key: its part of the now-playing band, else its face. */
function keyLabel(view: DeviceView, screen: Screen | null, key: number) {
  const np = screen?.nowPlaying;
  const index = np?.keys.indexOf(key) ?? -1;
  if (np && index >= 0) {
    return <NowPlaying {...nowPlayingProps(np)} slice={{ index, count: np.keys.length }} />;
  }
  if (view.images[key - 1]) return <span className="sd-face sd-face--blank" />;
  return face(screen?.faces.find((f) => f.key === key));
}

/** The band's keys are drawn here, not fetched as images every frame (H15-88). */
function keyImage(view: DeviceView, screen: Screen | null, key: number): string | undefined {
  return screen?.nowPlaying?.keys.includes(key) ? undefined : imageUrl(view, key);
}

type OpenMenu = (slot: Slot, x: number, y: number) => void;

/** A key tile with the right-click / long-press menu of its slot. */
function MenuKeyTile({ at, open, ...props }: KeyTileProps & { at: Slot; open: OpenMenu }) {
  return <KeyTile {...props} {...useSlotTrigger(at, open)} />;
}

function MenuKnob({ at, open, ...props }: KnobProps & { at: Slot; open: OpenMenu }) {
  return <Knob {...props} {...useSlotTrigger(at, open)} />;
}

function Dock({ view, screen, open }: { view: DeviceView; screen: Screen | null; open: OpenMenu }) {
  const knobRole = (knob: number) =>
    knob === 3 && screen && screen.modes.length > 0 ? "Mode" : screen?.knobs.find((k) => k.knob === knob)?.label;
  const lcd = view.keys.slice(0, LCD_KEY_COUNT);
  const buttons = view.keys.slice(LCD_KEY_COUNT);
  // Release flash: a new pulse on every release; none while pressed.
  const pulse = (k: { pressed: boolean; lastActive: number }) => (k.pressed ? undefined : k.lastActive);
  // Clicks and turns here are real input (H15-51): they reach the active mode and its integration.
  const [input, inputError] = useDockInput(view.device.id);
  return (
    <Panel
      title={view.device.model || "StreamDock"}
      aside={`${view.device.id} · FW ${view.device.firmwareVersion || "?"}`}
      aria-label={`Dock ${view.device.id}`}
      accent
    >
      <div className="sd-dock">
        <div className="sd-dock__lcd">
          {lcd.map((k, i) => (
            <MenuKeyTile
              key={i}
              at={{ type: "key", n: i + 1 }}
              open={open}
              number={i + 1}
              pressed={k.pressed}
              pulse={pulse(k)}
              onClick={() => void input.click(i + 1)}
              image={keyImage(view, screen, i + 1)}
              label={keyLabel(view, screen, i + 1)}
            />
          ))}
        </div>
        <div className="sd-dock__buttons">
          {buttons.map((k, i) => (
            <MenuKeyTile
              key={i}
              at={{ type: "button", n: i + 1 + LCD_KEY_COUNT }}
              open={open}
              number={i + 1 + LCD_KEY_COUNT}
              pressed={k.pressed}
              pulse={pulse(k)}
              onClick={() => void input.click(i + 1 + LCD_KEY_COUNT)}
            />
          ))}
        </div>
        <div className="sd-dock__knobs">
          {view.knobs.map((k, i) => (
            <MenuKnob
              key={i}
              at={{ type: "knob", n: i + 1 }}
              open={open}
              number={i + 1}
              position={k.position}
              pressed={k.pressed}
              turn={k.lastTurn}
              pulse={k.lastTurnAt}
              label={knobRole(i + 1)}
              onTurn={(detents) => input.turn(i + 1, detents)}
              onPress={() => void input.press(i + 1)}
            />
          ))}
        </div>
      </div>
      {inputError && <p className="sd-field__error">{inputError}</p>}
      <Brightness view={view} />
    </Panel>
  );
}

const ACTIVITY_LEVEL_CLASS: Record<ActivityLevel, string> = {
  [ActivityLevel.UNSPECIFIED]: "info",
  [ActivityLevel.INFO]: "info",
  [ActivityLevel.OK]: "ok",
  [ActivityLevel.ERROR]: "error",
};

/** What the daemon and the modules did: actions with their results, status changes (H15-21). */
function ActivityPanel({ activity, screen }: { activity: Activity[]; screen: Screen | null }) {
  const [filter, setFilter] = useState("all");
  const title = (source: string) =>
    source === "daemon" ? "Daemon" : (screen?.modes.find((m) => m.id === source)?.title ?? source);
  const sources = [...new Set(activity.map((a) => a.source))].sort();
  const shown = filter === "all" ? activity : activity.filter((a) => a.source === filter);
  const errors = activity.filter((a) => a.level === ActivityLevel.ERROR).length;
  return (
    <Panel title="Activity" aside={errors > 0 ? `${errors} failed` : `${activity.length}`} className="sd-activity" aria-label="Activity">
      {sources.length > 1 && (
        <Tabs
          label="Show activity of"
          tabs={[{ id: "all", label: "All" }, ...sources.map((s) => ({ id: s, label: title(s) }))]}
          value={sources.includes(filter) ? filter : "all"}
          onChange={setFilter}
        />
      )}
      <ol className="sd-activity__list" aria-live="polite">
        {shown.length === 0 && <li className="sd-activity__empty">Nothing yet. Press a key on the dock.</li>}
        {shown.map((a) => (
          <li key={a.id.toString()} className={`sd-activity__entry sd-activity__entry--${ACTIVITY_LEVEL_CLASS[a.level] ?? "info"}`}>
            <time>{new Date(Number(a.timestampUnixMs)).toLocaleTimeString()}</time>
            <span className="sd-activity__source">{title(a.source)}</span>
            <span className="sd-activity__text">{a.text}</span>
          </li>
        ))}
      </ol>
    </Panel>
  );
}

/** The daemon's latest problem with finding docks (e.g. a kernel without hidraw, H15-40). */
function dockProblem(activity: Activity[]): string | undefined {
  const problems = activity.filter((a) => a.source === "daemon" && a.level === ActivityLevel.ERROR && /dock/i.test(a.text));
  return problems.at(-1)?.text;
}

/** Shown while no dock is attached: why, if the daemon knows, else how to get one. */
function NoDock({ connected, title, activity }: { connected: boolean; title: string; activity: Activity[] }) {
  if (!connected) {
    return (
      <Panel title={title}>
        <p>Connecting to streamdockd…</p>
      </Panel>
    );
  }
  const problem = dockProblem(activity);
  return (
    <Panel title="No dock connected">
      {problem && <p className="sd-field__error">{problem}</p>}
      <p>Plug in the StreamDock; it appears here immediately.</p>
    </Panel>
  );
}

function EventLog({ log }: { log: LogEntry[] }) {
  return (
    <Panel title="Event log" aside={`${log.length}`} className="sd-log" aria-label="Event log">
      <ol className="sd-log__list" aria-live="polite">
        {log.map((e) => (
          <li key={e.seq} className={`sd-log__entry sd-log__entry--${e.tone}`}>
            <time>{new Date(e.at).toLocaleTimeString()}</time>
            <span className="sd-log__dev">{e.deviceId}</span>
            <span className="sd-log__text">{e.text}</span>
          </li>
        ))}
      </ol>
    </Panel>
  );
}

function StatusBar({ state }: { state: DashboardState }) {
  return (
    <div className="sd-status" role="status">
      <span className={`sd-status__link${state.connected ? " is-online" : ""}`}>
        {state.connected ? "LINK ONLINE" : "LINK LOST · RETRYING"}
      </span>
      <span>DOCKS {state.devices.size}</span>
      <span>EVENTS {state.received}</span>
      {state.skipped > 0 && <span className="sd-status__warn">SKIPPED {state.skipped}</span>}
    </div>
  );
}

export default function Dashboard({ title }: DashboardProps) {
  const [state, dispatch] = useReducer(reducer, initialState);

  useEffect(() => {
    const abort = new AbortController();
    void watchDeviceEvents(
      {
        onEvent: (event, skipped) => dispatch({ type: "event", event, skipped, now: Date.now() }),
        onScreen: (screen) => dispatch({ type: "screen", screen }),
        onActivity: (activity) => dispatch({ type: "activity", activity }),
        onConnection: (connected) => dispatch({ type: "connection", connected }),
      },
      abort.signal,
    );
    return () => abort.abort();
  }, []);

  const menus = useDockMenus(state.screen);
  const devices = [...state.devices.values()];
  return (
    <div className="sd-dashboard">
      <StatusBar state={state} />
      {state.screen && <TabBar screen={state.screen} />}
      {menus.ui}
      <div className="sd-dashboard__grid">
        <div className="sd-dashboard__devices">
          {state.screen?.nowPlaying && (
            <Panel title="Now playing" aside={state.screen.modes.find((m) => m.id === state.screen?.activeMode)?.title}>
              <NowPlaying {...nowPlayingProps(state.screen.nowPlaying)} />
            </Panel>
          )}
          {devices.length === 0 ? (
            <NoDock connected={state.connected} title={title} activity={state.activity} />
          ) : (
            devices.map((view) => <Dock key={view.device.id} view={view} screen={state.screen} open={(slot, x, y) => void menus.open(slot, x, y)} />)
          )}
        </div>
        <div className="sd-dashboard__side">
          <ActivityPanel activity={state.activity} screen={state.screen} />
          <EventLog log={state.log} />
        </div>
      </div>
    </div>
  );
}
