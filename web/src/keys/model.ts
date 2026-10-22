// The custom keys editor's data (H15-29): `[modules.keys]` as `KeyPages`, loaded with `GetKeyPages` and
// saved whole with `SaveKeyPages`. The daemon validates exactly like at startup; its message is shown as is.
import { clone, create, equals } from "@bufbuild/protobuf";
import { useCallback, useEffect, useRef, useState } from "react";

import { deviceClient } from "../api/client.js";
import { useRevisionChange } from "../api/live.js";
import {
  GetKeyPagesResponseSchema,
  KeyActionSchema,
  KeyPagesSchema,
  KeySlotSchema,
  KnobSlotSchema,
  type GetKeyPagesResponse,
  type KeyAction,
  type KeyPages,
  type KeySlot,
  type KnobSlot,
} from "../gen/streamdock/v1/device_pb.js";

/** A key (1-9) or knob (1-2) of a page. */
export type SlotRef = { kind: "key"; number: number } | { kind: "knob"; number: number };

export type ActionType = NonNullable<KeyAction["kind"]["case"]>;

export const ACTION_TYPES: ReadonlyArray<{ type: ActionType; label: string }> = [
  { type: "http", label: "HTTP / webhook" },
  { type: "homeassistant", label: "Home Assistant" },
  { type: "lyrion", label: "Lyrion" },
  { type: "toggle", label: "Toggle (on / off)" },
  { type: "multi", label: "Multi (steps)" },
  { type: "brightness", label: "Dock brightness" },
  { type: "audio", label: "Audio volume / mute" },
  { type: "mode", label: "Switch mode" },
  { type: "profile", label: "Switch profile" },
  { type: "command", label: "Run a command" },
  { type: "assist", label: "Push-to-talk (assist)" },
];

/** A new action of `type`, with sensible defaults. */
export function newAction(type: ActionType): KeyAction {
  switch (type) {
    case "http":
      return create(KeyActionSchema, { kind: { case: "http", value: { method: "POST", url: "https://" } } });
    case "homeassistant":
      return create(KeyActionSchema, {
        kind: { case: "homeassistant", value: { service: "light.toggle", dataJson: "" } },
      });
    case "lyrion":
      return create(KeyActionSchema, { kind: { case: "lyrion", value: { command: ["pause"] } } });
    case "toggle":
      return create(KeyActionSchema, {
        kind: { case: "toggle", value: { on: newAction("homeassistant"), off: newAction("homeassistant") } },
      });
    case "multi":
      return create(KeyActionSchema, {
        kind: { case: "multi", value: { steps: [{ step: { case: "action", value: newAction("http") } }] } },
      });
    default:
      return newControlAction(type);
  }
}

/** A new command, mode, profile, brightness or audio action. */
function newControlAction(type: ActionType): KeyAction {
  switch (type) {
    case "brightness":
      return create(KeyActionSchema, { kind: { case: "brightness", value: { change: { case: "set", value: 80 } } } });
    case "audio":
      return create(KeyActionSchema, { kind: { case: "audio", value: { change: { case: "step", value: 2 } } } });
    case "mode":
      return create(KeyActionSchema, { kind: { case: "mode", value: { id: "" } } });
    case "profile":
      return create(KeyActionSchema, { kind: { case: "profile", value: { name: "" } } });
    case "command":
      return create(KeyActionSchema, { kind: { case: "command", value: { argv: [] } } });
    case "assist":
      return create(KeyActionSchema, { kind: { case: "assist", value: {} } });
    default:
      return create(KeyActionSchema, {});
  }
}

/** The mode id a page gets: its own, else "keys", "keys-2", … like the daemon. */
export function pageModeId(pages: KeyPages, index: number): string {
  const own = pages.pages[index]?.id.trim();
  if (own) return own;
  return index === 0 ? "keys" : `keys-${index + 1}`;
}

export function findSlot(pages: KeyPages, page: number, slot: SlotRef): KeySlot | KnobSlot | undefined {
  const p = pages.pages[page];
  if (!p) return undefined;
  return slot.kind === "key" ? p.keys.find((k) => k.key === slot.number) : p.knobs.find((k) => k.knob === slot.number);
}

/** A copy of `pages` with `value` as the slot (added if new), or the slot removed when `value` is null. */
export function withSlot(pages: KeyPages, page: number, slot: SlotRef, value: KeySlot | KnobSlot | null): KeyPages {
  const next = clone(KeyPagesSchema, pages);
  const p = next.pages[page];
  if (!p) return next;
  if (slot.kind === "key") {
    p.keys = p.keys.filter((k) => k.key !== slot.number);
    if (value) p.keys.push(clone(KeySlotSchema, value as KeySlot));
    p.keys.sort((a, b) => a.key - b.key);
  } else {
    p.knobs = p.knobs.filter((k) => k.knob !== slot.number);
    if (value) p.knobs.push(clone(KnobSlotSchema, value as KnobSlot));
    p.knobs.sort((a, b) => a.knob - b.knob);
  }
  return next;
}

/** Saves all pages; resolves to whether the daemon needs a restart to use them. */
export async function savePages(pages: KeyPages): Promise<boolean> {
  const res = await deviceClient.saveKeyPages({ pages });
  return res.restartRequired;
}

export interface KeyPagesState {
  data: GetKeyPagesResponse | null;
  error: string | null;
  reload(): void;
  /** Newer pages from a change elsewhere (H15-49), waiting while the user edits; `accept` shows them. */
  incoming: GetKeyPagesResponse | null;
  accept(): void;
}

/**
 * `GetKeyPages`, loaded on mount and again with `reload()`. When the config or the images change
 * elsewhere (another browser, an edit on disk), the pages are read again and offered as `incoming`
 * if they differ, so the editor can take them without losing unsaved edits.
 */
export function useKeyPages(): KeyPagesState {
  const [data, setData] = useState<GetKeyPagesResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [incoming, setIncoming] = useState<GetKeyPagesResponse | null>(null);
  const [tick, setTick] = useState(0);
  const current = useRef<GetKeyPagesResponse | null>(null);
  current.current = data;
  useEffect(() => {
    deviceClient
      .getKeyPages({})
      .then((res) => {
        setData(res);
        setIncoming(null);
        setError(null);
      })
      .catch((err: unknown) => setError(errorText(err)));
  }, [tick]);
  useRevisionChange("both", () => {
    deviceClient
      .getKeyPages({})
      .then((res) => {
        const now = current.current;
        if (now === null || !equals(GetKeyPagesResponseSchema, res, now)) setIncoming(res);
      })
      .catch(() => {});
  });
  const reload = useCallback(() => setTick((n) => n + 1), []);
  const accept = useCallback(() => {
    if (incoming) setData(incoming);
    setIncoming(null);
  }, [incoming]);
  return { data, error, reload, incoming, accept };
}

/** A daemon error without the Connect code prefix. */
export function errorText(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

/** A built-in icon: its name (used like an image name) and group. */
export interface IconRef {
  name: string;
  group: string;
}

/** What the pickers offer, from `GetKeyPages`. */
export interface Choices {
  modes: string[];
  profiles: string[];
  images: string[];
  /** Built-in icons (H15-48). */
  icons: IconRef[];
  allowCommands: boolean;
  homeassistant: boolean;
  lyrion: boolean;
}

export function choicesOf(data: GetKeyPagesResponse): Choices {
  return {
    modes: data.modes,
    profiles: data.profiles,
    images: data.images,
    icons: data.icons.map((i) => ({ name: i.name, group: i.group })),
    allowCommands: data.pages?.allowCommands ?? false,
    homeassistant: data.homeassistant,
    lyrion: data.lyrion,
  };
}
