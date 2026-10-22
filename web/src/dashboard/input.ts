// Control the dock from the dashboard (H15-51): clicks and knob turns go to `SendInput`, which runs
// them through the same path as the dock's own input. The pressed and turned looks come back
// through WatchEvents like any other input, so every open dashboard shows them.
import { useMemo, useState } from "react";

import { deviceClient } from "../api/client.js";

/** The daemon takes at most this many detents per request. */
const MAX_TURN = 20;

function message(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Sends one dock's input. Knob turns are coalesced: one request in flight per knob, the rest summed. */
export class DockInput {
  private readonly pending = new Map<number, number>();
  private readonly busy = new Set<number>();

  constructor(
    private readonly deviceId: string,
    private readonly onError: (message: string | null) => void,
  ) {}

  /** A key or button click: pressed, then released. */
  async click(key: number): Promise<void> {
    await this.send({ case: "key", value: { key, pressed: true } });
    await this.send({ case: "key", value: { key, pressed: false } });
  }

  /** A knob press: down, then up. */
  async press(knob: number): Promise<void> {
    await this.send({ case: "knob", value: { knob, action: { case: "pressed", value: true } } });
    await this.send({ case: "knob", value: { knob, action: { case: "pressed", value: false } } });
  }

  /** Turns `knob` by `detents` (positive clockwise). */
  turn(knob: number, detents: number): void {
    this.pending.set(knob, (this.pending.get(knob) ?? 0) + detents);
    if (!this.busy.has(knob)) void this.flush(knob);
  }

  private async flush(knob: number): Promise<void> {
    this.busy.add(knob);
    try {
      for (;;) {
        const total = this.pending.get(knob) ?? 0;
        const step = Math.max(-MAX_TURN, Math.min(MAX_TURN, total));
        if (step === 0) break;
        this.pending.set(knob, total - step);
        await this.send({ case: "knob", value: { knob, action: { case: "turned", value: step } } });
      }
    } finally {
      this.busy.delete(knob);
    }
  }

  private async send(input: Parameters<typeof deviceClient.sendInput>[0]["input"]): Promise<void> {
    try {
      await deviceClient.sendInput({ deviceId: this.deviceId, input });
      this.onError(null);
    } catch (err) {
      this.onError(message(err));
    }
  }
}

/** A `DockInput` for `deviceId` and the last error it ran into (null when the last input went through). */
export function useDockInput(deviceId: string): [DockInput, string | null] {
  const [error, setError] = useState<string | null>(null);
  const input = useMemo(() => new DockInput(deviceId, setError), [deviceId]);
  return [input, error];
}
