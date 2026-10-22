// The latest-state messages of `WatchEvents` (H15-49), shared by every component of a page over one
// stream: this machine's network and Bluetooth, and the config and image revisions that tell
// screens to read their data again when something changed elsewhere, and the built-in player.
import { useEffect, useRef, useSyncExternalStore } from "react";

import { watchDeviceEvents, type ConfigState, type NetworkState, type PlayerState, type WakeWordState } from "./client.js";

export interface Live {
  network: NetworkState | null;
  config: ConfigState | null;
  player: PlayerState | null;
  /** The wake word listener (H15-68). */
  wake: WakeWordState | null;
}

let live: Live = { network: null, config: null, player: null, wake: null };
const listeners = new Set<() => void>();
let started = false;

function update(part: Partial<Live>): void {
  live = { ...live, ...part };
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  if (!started) {
    started = true;
    void watchDeviceEvents(
      {
        onEvent: () => {},
        onNetwork: (network) => update({ network }),
        onConfig: (config) => update({ config }),
        onPlayer: (player) => update({ player }),
        onWake: (wake) => update({ wake }),
      },
      new AbortController().signal,
    );
  }
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The latest network state and config revisions (`null` until the stream sent them). */
export function useLive(): Live {
  return useSyncExternalStore(subscribe, () => live);
}

/**
 * Calls `onChange` when the config revision (`"config"`), the image revision (`"assets"`) or
 * either (`"both"`) changes after the first one seen; the first one is what the page loaded with.
 */
export function useRevisionChange(kind: "config" | "assets" | "both", onChange: () => void): void {
  const { config } = useLive();
  const key = config === null ? null : revisionKey(config, kind);
  const seen = useRef<string | null>(null);
  const callback = useRef(onChange);
  callback.current = onChange;
  useEffect(() => {
    if (key === null) return;
    if (seen.current !== null && seen.current !== key) callback.current();
    seen.current = key;
  }, [key]);
}

function revisionKey(config: ConfigState, kind: "config" | "assets" | "both"): string {
  if (kind === "config") return String(config.revision);
  if (kind === "assets") return String(config.assetsRevision);
  return `${config.revision}/${config.assetsRevision}`;
}
