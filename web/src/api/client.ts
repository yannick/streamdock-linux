// Connect client for the streamdockd API (streamdock.v1, binary protobuf only; ADR-001).
import { createClient, type Client } from "@connectrpc/connect";
import { createConnectTransport } from "@connectrpc/connect-web";

import {
  DeviceService,
  type Activity,
  type AudioState,
  type ConfigState,
  type DeviceEvent,
  type NetworkState,
  type PlayerState,
  type WakeWordState,
  type Screen,
  type WatchEventsResponse,
} from "../gen/streamdock/v1/device_pb.js";

export type { Activity, AudioState, ConfigState, DeviceEvent, NetworkState, PlayerState, Screen, WakeWordState };

/** Client for the daemon that served this page. */
export const deviceClient: Client<typeof DeviceService> = createClient(
  DeviceService,
  createConnectTransport({ baseUrl: window.location.origin, useBinaryFormat: true }),
);

export interface WatchHandlers {
  /** Called for every event; the stream starts with an `attached` event per attached device. */
  onEvent(event: DeviceEvent, skipped: bigint): void;
  /** Called with what the dock shows (mode, key faces): at the start and on every change. */
  onScreen?(screen: Screen): void;
  /** Called for every activity entry: the recent history first, then live. */
  onActivity?(activity: Activity): void;
  /** Called with this machine's audio (H15-50): at the start and on every change, where managed. */
  onAudio?(audio: AudioState): void;
  /** Called with this machine's network and Bluetooth: at the start and on every change (H15-49). */
  onNetwork?(network: NetworkState): void;
  /** Called with the config and image revisions: at the start and on every change (H15-49). */
  onConfig?(config: ConfigState): void;
  /** Called with the built-in Lyrion player's state: at the start and on every change (H15-52). */
  onPlayer?(player: PlayerState): void;
  /** Called with the wake word listener's state: at the start and on every change (H15-68). */
  onWake?(wake: WakeWordState): void;
  /** Called when the stream is up (true) or lost and being retried (false). */
  onConnection?(connected: boolean): void;
}

/**
 * Follows `WatchEvents` until `signal` aborts, reconnecting with backoff (0.5 s doubling to 5 s)
 * whenever the daemon goes away. After a reconnect the stream starts over with fresh `attached`
 * events, so consumers should rebuild their device state from them.
 */
export async function watchDeviceEvents(handlers: WatchHandlers, signal: AbortSignal): Promise<void> {
  let delay = 500;
  while (!signal.aborted) {
    try {
      await followStream(handlers, signal, () => (delay = 500));
    } catch (err) {
      if (signal.aborted) return;
      console.warn("WatchEvents failed, retrying", err);
    }
    handlers.onConnection?.(false);
    await new Promise((resolve) => setTimeout(resolve, delay));
    delay = Math.min(delay * 2, 5000);
  }
}

/** Follows one `WatchEvents` stream until it ends or fails, calling `onResponse` for every response. */
async function followStream(handlers: WatchHandlers, signal: AbortSignal, onResponse: () => void): Promise<void> {
  // Connected once the daemon answers with response headers, not when the request starts:
  // a failed retry must not look like a (briefly) working stream.
  const stream = deviceClient.watchEvents({}, { signal, onHeader: () => handlers.onConnection?.(true) });
  for await (const res of stream) {
    onResponse();
    deliver(handlers, res);
  }
}

function deliver(handlers: WatchHandlers, res: WatchEventsResponse): void {
  if (res.event) handlers.onEvent(res.event, res.skipped);
  if (res.screen) handlers.onScreen?.(res.screen);
  if (res.activity) handlers.onActivity?.(res.activity);
  deliverState(handlers, res);
}

/** The latest-state messages: each replaces the previous one of its kind. */
function deliverState(handlers: WatchHandlers, res: WatchEventsResponse): void {
  if (res.audio) handlers.onAudio?.(res.audio);
  if (res.network) handlers.onNetwork?.(res.network);
  if (res.config) handlers.onConfig?.(res.config);
  deliverServices(handlers, res);
}

/** The states of the daemon's services: the built-in player, the wake word. */
function deliverServices(handlers: WatchHandlers, res: WatchEventsResponse): void {
  if (res.player) handlers.onPlayer?.(res.player);
  if (res.wake) handlers.onWake?.(res.wake);
}
