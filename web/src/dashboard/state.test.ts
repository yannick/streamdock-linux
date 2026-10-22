import { create } from "@bufbuild/protobuf";
import { describe as suite, expect, it } from "vitest";

import {
  DetachedSchema,
  DeviceEventSchema,
  DeviceSchema,
  FaceState,
  KeyEventSchema,
  KeyImageSchema,
  KnobEventSchema,
  ActivityLevel,
  ActivitySchema,
  ScreenSchema,
  type DeviceEvent,
} from "../gen/streamdock/v1/device_pb.js";
import { initialState, LOG_LIMIT, reducer, type DashboardState } from "./state.js";

const device = create(DeviceSchema, { id: "D1", model: "StreamDock N3", firmwareVersion: "V25", brightness: 80 });

const ev = (kind: DeviceEvent["kind"]): DeviceEvent => create(DeviceEventSchema, { deviceId: "D1", timestampUnixMs: 1000n, kind });
const attached = ev({ case: "attached", value: device });
const key = (k: number, pressed: boolean) => ev({ case: "key", value: create(KeyEventSchema, { key: k, pressed }) });
const turn = (knob: number, by: number) =>
  ev({ case: "knob", value: create(KnobEventSchema, { knob, action: { case: "turned", value: by } }) });
const knobPress = (knob: number, pressed: boolean) =>
  ev({ case: "knob", value: create(KnobEventSchema, { knob, action: { case: "pressed", value: pressed } }) });

function run(events: DeviceEvent[], from: DashboardState = reducer(initialState, { type: "connection", connected: true })) {
  return events.reduce((s, event, i) => reducer(s, { type: "event", event, skipped: 0n, now: 5000 + i }), from);
}

suite("dashboard reducer", () => {
  it("tracks attach, key presses, knob turns and presses", () => {
    const s = run([attached, key(4, true), key(8, true), turn(2, 1), turn(2, 1), turn(2, -1), knobPress(3, true)]);
    const view = s.devices.get("D1")!;
    expect(view.device.model).toBe("StreamDock N3");
    expect(view.keys[3].pressed).toBe(true);
    expect(view.keys[7].pressed).toBe(true);
    expect(view.knobs[1]).toMatchObject({ position: 1, lastTurn: -1 });
    expect(view.knobs[2].pressed).toBe(true);
    expect(s.log[0].text).toBe("KNOB 3 DOWN");
    expect(s.log.map((l) => l.text)).toContain("BUTTON 8 DOWN");
    expect(s.received).toBe(7);
  });

  it("moves lastTurnAt only on turns, so presses don't replay the turn flash", () => {
    const s = run([attached, turn(1, 1), knobPress(1, true)]);
    const knob = s.devices.get("D1")!.knobs[0];
    expect(knob.lastTurnAt).toBe(5001);
    expect(knob.lastActive).toBe(5002);
  });

  it("releases keys and removes detached devices", () => {
    let s = run([attached, key(1, true), key(1, false)]);
    expect(s.devices.get("D1")!.keys[0].pressed).toBe(false);
    s = run([ev({ case: "detached", value: create(DetachedSchema) })], s);
    expect(s.devices.size).toBe(0);
    expect(s.log[0]).toMatchObject({ text: "DETACHED", tone: "warn" });
  });

  it("applies brightness updates", () => {
    const s = run([attached, ev({ case: "updated", value: create(DeviceSchema, { ...device, brightness: 30 }) })]);
    expect(s.devices.get("D1")!.device.brightness).toBe(30);
  });

  it("rebuilds devices from the snapshot after a reconnect", () => {
    let s = run([attached, key(2, true)]);
    s = reducer(s, { type: "connection", connected: false });
    expect(s.devices.size).toBe(1); // keep showing the last state while offline
    s = reducer(s, { type: "connection", connected: true });
    expect(s.devices.size).toBe(0);
    s = run([attached], s);
    expect(s.devices.get("D1")!.keys[1].pressed).toBe(false);
  });

  it("logs skipped events and caps the log", () => {
    let s = run([attached]);
    s = reducer(s, { type: "event", event: key(1, true), skipped: 5n, now: 1 });
    expect(s.skipped).toBe(5);
    expect(s.log[1].text).toMatch(/SKIPPED 5/);
    s = run(Array.from({ length: LOG_LIMIT + 20 }, (_, i) => key((i % 9) + 1, i % 2 === 0)), s);
    expect(s.log.length).toBe(LOG_LIMIT);
  });

  it("keeps the latest screen and drops it on reconnect", () => {
    let s = run([attached]);
    const screen = create(ScreenSchema, {
      modes: [{ id: "demo-1", title: "Demo 1" }],
      activeMode: "demo-1",
      faces: [{ key: 1, label: "Lamp", detail: "on", state: FaceState.ON }],
    });
    s = reducer(s, { type: "screen", screen });
    expect(s.screen?.activeMode).toBe("demo-1");
    expect(s.screen?.faces[0].label).toBe("Lamp");
    s = reducer(s, { type: "connection", connected: false });
    expect(s.screen).not.toBeNull(); // keep showing it while offline
    s = reducer(s, { type: "connection", connected: true });
    expect(s.screen).toBeNull();
  });

  it("tracks the image version of every LCD key", () => {
    const img = (key: number, version: bigint, blank = false) =>
      ev({ case: "keyImage", value: create(KeyImageSchema, { key, version, blank }) });
    const s = run([attached, img(2, 5n), img(2, 7n), img(6, 8n, true), img(9, 9n)]);
    const view = s.devices.get("D1")!;
    expect(view.images[1]).toEqual({ version: 7n, blank: false });
    expect(view.images[5]).toEqual({ version: 8n, blank: true });
    expect(view.images[0]).toBeUndefined();
    expect(s.log.some((l) => l.text.includes("IMAGE"))).toBe(false); // not logged
  });

  it("keeps the activity newest first, capped, and replays it after a reconnect", () => {
    const act = (id: number, text: string, level = ActivityLevel.OK) =>
      create(ActivitySchema, { id: BigInt(id), source: "homeassistant", level, text });
    let s = reducer(initialState, { type: "connection", connected: true });
    s = reducer(s, { type: "activity", activity: act(1, "connected") });
    s = reducer(s, { type: "activity", activity: act(2, "toggle Deckenspots") });
    expect(s.activity.map((a) => a.text)).toEqual(["toggle Deckenspots", "connected"]);
    for (let i = 3; i < 250; i++) s = reducer(s, { type: "activity", activity: act(i, `x${i}`) });
    expect(s.activity).toHaveLength(200);
    s = reducer(s, { type: "connection", connected: false });
    expect(s.activity).toHaveLength(200);
    s = reducer(s, { type: "connection", connected: true });
    expect(s.activity).toHaveLength(0);
  });

  it("ignores events for unknown devices and out-of-range controls", () => {
    const s = run([attached, key(12, true), turn(7, 1)]);
    const view = s.devices.get("D1")!;
    expect(view.keys.every((k) => !k.pressed)).toBe(true);
    expect(view.knobs.every((k) => k.position === 0)).toBe(true);
  });
});
