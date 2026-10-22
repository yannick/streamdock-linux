// Dashboard state, derived purely from the WatchEvents stream (no other source of truth).
import type { Activity, Device, DeviceEvent, KeyEvent, KeyImage, KnobEvent, Screen } from "../gen/streamdock/v1/device_pb.js";

/** Keys 1-6 are LCD keys, 7-9 plain buttons. */
export const KEY_COUNT = 9;
export const LCD_KEY_COUNT = 6;
export const KNOB_COUNT = 3;
/** Event log entries kept, newest first. */
export const LOG_LIMIT = 200;
/** Activity entries kept, newest first (the daemon keeps the same number). */
export const ACTIVITY_LIMIT = 200;

export interface KeyState {
  pressed: boolean;
  /** Time of the last press or release (ms), for activity animations. */
  lastActive: number;
}

export interface KnobState {
  pressed: boolean;
  /** Detents turned since the device appeared: positive clockwise. */
  position: number;
  /** Direction of the last turn: +1 clockwise, -1 counter-clockwise, 0 none yet. */
  lastTurn: -1 | 0 | 1;
  /** Time of the last turn (ms); changes only on turns, to replay the turn flash. */
  lastTurnAt: number;
  /** Time of the last turn, press or release (ms). */
  lastActive: number;
}

/** What an LCD key shows, as announced by the daemon (`KeyImage` events). */
export interface KeyImageState {
  version: bigint;
  blank: boolean;
}

export interface DeviceView {
  device: Device;
  /** Index 0 = LCD key 1; undefined until the daemon reported the key. */
  images: (KeyImageState | undefined)[];
  /** Index 0 = key 1. */
  keys: KeyState[];
  /** Index 0 = knob 1 (bottom left), 1 = knob 2 (top), 2 = knob 3 (bottom right). */
  knobs: KnobState[];
}

export type LogTone = "key" | "knob" | "device" | "warn";

export interface LogEntry {
  seq: number;
  /** Event time (ms since the epoch), from the daemon. */
  at: number;
  deviceId: string;
  text: string;
  tone: LogTone;
}

export interface DashboardState {
  connected: boolean;
  /** Attached devices by id, in attach order. */
  devices: Map<string, DeviceView>;
  /** Newest first, at most LOG_LIMIT. */
  log: LogEntry[];
  /** Events received since the page loaded. */
  received: number;
  /** Events the daemon reported as skipped because this client fell behind. */
  skipped: number;
  /** What the dock shows (modes, key faces, knob roles); null until the daemon sent it. */
  screen: Screen | null;
  /** What the daemon and modules did, newest first. */
  activity: Activity[];
  seq: number;
}

export type DashboardAction =
  | { type: "connection"; connected: boolean }
  | { type: "event"; event: DeviceEvent; skipped: bigint; now: number }
  | { type: "screen"; screen: Screen }
  | { type: "activity"; activity: Activity };

export const initialState: DashboardState = {
  connected: false,
  devices: new Map(),
  log: [],
  received: 0,
  skipped: 0,
  screen: null,
  activity: [],
  seq: 0,
};

function freshDevice(device: Device): DeviceView {
  return {
    device,
    images: Array.from({ length: LCD_KEY_COUNT }, () => undefined),
    keys: Array.from({ length: KEY_COUNT }, () => ({ pressed: false, lastActive: 0 })),
    knobs: Array.from({ length: KNOB_COUNT }, () => ({ pressed: false, position: 0, lastTurn: 0 as const, lastTurnAt: 0, lastActive: 0 })),
  };
}

type LogLine = { text: string; tone: LogTone };

/** Human-readable text for the event log, or null for events that aren't logged. */
export function describe(event: DeviceEvent): LogLine | null {
  const kind = event.kind;
  switch (kind.case) {
    case "attached":
      return { text: `ATTACHED ${kind.value.model || kind.value.id} · fw ${kind.value.firmwareVersion || "?"}`, tone: "device" };
    case "detached":
      return { text: "DETACHED", tone: "warn" };
    case "updated":
      return { text: `BRIGHTNESS ${kind.value.brightness}%`, tone: "device" };
    case "key":
      return describeKey(kind.value);
    case "knob":
      return describeKnob(kind.value);
    default:
      return null;
  }
}

function describeKey({ key, pressed }: KeyEvent): LogLine {
  const name = key <= LCD_KEY_COUNT ? `KEY ${key}` : `BUTTON ${key}`;
  return { text: `${name} ${pressed ? "DOWN" : "UP"}`, tone: "key" };
}

function describeKnob({ knob, action }: KnobEvent): LogLine | null {
  if (action.case === "turned") {
    const dir = action.value > 0 ? "CW" : "CCW";
    return { text: `KNOB ${knob} ${dir} ${action.value > 0 ? "+" : ""}${action.value}`, tone: "knob" };
  }
  if (action.case === "pressed") return { text: `KNOB ${knob} ${action.value ? "DOWN" : "UP"}`, tone: "knob" };
  return null;
}

function withLog(state: DashboardState, entry: Omit<LogEntry, "seq">): Pick<DashboardState, "log" | "seq"> {
  const seq = state.seq + 1;
  return { seq, log: [{ ...entry, seq }, ...state.log].slice(0, LOG_LIMIT) };
}

function applyToDevice(view: DeviceView, event: DeviceEvent, now: number): DeviceView {
  const kind = event.kind;
  switch (kind.case) {
    case "updated":
      return { ...view, device: kind.value };
    case "keyImage":
      return withKeyImage(view, kind.value);
    case "key":
      return withKey(view, kind.value, now);
    case "knob":
      return withKnob(view, kind.value, now);
    default:
      return view;
  }
}

function withKeyImage(view: DeviceView, image: KeyImage): DeviceView {
  const i = image.key - 1;
  if (i < 0 || i >= LCD_KEY_COUNT) return view;
  const images = view.images.slice();
  images[i] = { version: image.version, blank: image.blank };
  return { ...view, images };
}

function withKey(view: DeviceView, event: KeyEvent, now: number): DeviceView {
  const i = event.key - 1;
  if (i < 0 || i >= KEY_COUNT) return view;
  const keys = view.keys.slice();
  keys[i] = { pressed: event.pressed, lastActive: now };
  return { ...view, keys };
}

function withKnob(view: DeviceView, event: KnobEvent, now: number): DeviceView {
  const i = event.knob - 1;
  if (i < 0 || i >= KNOB_COUNT) return view;
  const knob = view.knobs[i];
  const action = event.action;
  let next: KnobState;
  if (action.case === "turned") {
    next = { ...knob, position: knob.position + action.value, lastTurn: action.value > 0 ? 1 : -1, lastTurnAt: now, lastActive: now };
  } else if (action.case === "pressed") {
    next = { ...knob, pressed: action.value, lastActive: now };
  } else {
    return view;
  }
  const knobs = view.knobs.slice();
  knobs[i] = next;
  return { ...view, knobs };
}

export function reducer(state: DashboardState, action: DashboardAction): DashboardState {
  switch (action.type) {
    case "connection":
      return onConnection(state, action.connected);
    case "screen":
      return { ...state, screen: action.screen };
    case "activity":
      return { ...state, activity: [action.activity, ...state.activity].slice(0, ACTIVITY_LIMIT) };
    case "event":
      return onEvent(state, action);
  }
}

function onConnection(state: DashboardState, connected: boolean): DashboardState {
  if (connected === state.connected) return state;
  // A new stream starts over with `attached` events for every device: rebuild from them.
  return {
    ...state,
    connected,
    devices: connected ? new Map() : state.devices,
    screen: connected ? null : state.screen,
    // The new stream replays the history; keep showing the old one while offline.
    activity: connected ? [] : state.activity,
  };
}

function onEvent(state: DashboardState, action: Extract<DashboardAction, { type: "event" }>): DashboardState {
  const { event, now } = action;
  const skipped = state.skipped + Number(action.skipped);
  const received = state.received + 1;
  const at = event.timestampUnixMs ? Number(event.timestampUnixMs) : now;
  const devices = withDevices(state.devices, event, now);

  let next: DashboardState = { ...state, devices, received, skipped };
  if (action.skipped > 0n) {
    next = { ...next, ...withLog(next, { at, deviceId: event.deviceId, text: `SKIPPED ${action.skipped} EVENTS (CLIENT TOO SLOW)`, tone: "warn" }) };
  }
  const line = describe(event);
  if (line) next = { ...next, ...withLog(next, { at, deviceId: event.deviceId, ...line }) };
  return next;
}

/** The attached devices after `event`: added, removed or updated. */
function withDevices(devices: Map<string, DeviceView>, event: DeviceEvent, now: number): Map<string, DeviceView> {
  const next = new Map(devices);
  const kind = event.kind;
  if (kind.case === "attached") {
    next.set(event.deviceId, freshDevice(kind.value));
  } else if (kind.case === "detached") {
    next.delete(event.deviceId);
  } else {
    const view = next.get(event.deviceId);
    if (view) next.set(event.deviceId, applyToDevice(view, event, now));
  }
  return next;
}
