// Choosing a Bluetooth headset's audio codec (H15-78): the codecs PipeWire offers while it is
// connected, LDAC's quality, and going back to PipeWire's automatic choice. The choice is
// remembered per device and applied again on every connect. Also the link's sample rate: kept at
// the player's 48 kHz unless the device may keep the rate the headset chooses.
import { useState } from "react";

import { deviceClient } from "../api/client.js";
import { Button } from "../design/index.js";
import type { BluetoothDeviceDetail } from "../gen/streamdock/v1/device_pb.js";

const LDAC_QUALITIES = [
  { id: "auto", label: "Adaptive (follows the radio link)" },
  { id: "hq", label: "Best (990 kbit/s)" },
  { id: "sq", label: "Standard (660 kbit/s)" },
  { id: "mq", label: "Robust (330 kbit/s)" },
];

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

/** The remembered choice, else what runs now. */
function initial(d: BluetoothDeviceDetail): { codec: string; quality: string } {
  const p = d.preferred;
  if (p) return { codec: p.codec, quality: p.ldacQuality || "auto" };
  return { codec: "", quality: d.ldacQuality || "auto" };
}

export function CodecChooser({ detail: d, onChanged }: { detail: BluetoothDeviceDetail; onChanged(): void }) {
  const [choice, setChoice] = useState(initial(d));
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  if (d.codecs.length === 0) return null;
  const address = d.device?.address ?? "";
  const apply = () => {
    setBusy(true);
    setNote(null);
    deviceClient
      .setBluetoothCodec({ address, choice: { codec: choice.codec, ldacQuality: choice.codec === "ldac" ? choice.quality : "" } })
      .then(() => {
        setNote({ ok: true, text: "Switched; it is used again whenever the headset connects." });
        onChanged();
      })
      .catch((err: unknown) => setNote({ ok: false, text: message(err) }))
      .finally(() => setBusy(false));
  };
  return (
    <div className="sd-btdetail__codec">
      <div className="sd-setting">
        <label className="sd-setting__label" htmlFor="bt-codec">
          Codec
        </label>
        <select id="bt-codec" className="sd-input" value={choice.codec} onChange={(e) => setChoice({ ...choice, codec: e.currentTarget.value })}>
          <option value="">Automatic (PipeWire picks the best)</option>
          {d.codecs.map((c) => (
            <option key={c.id} value={c.id}>
              {c.label}
              {c.id === d.activeCodec ? " · in use" : ""}
            </option>
          ))}
        </select>
      </div>
      {choice.codec === "ldac" && (
        <div className="sd-setting">
          <label className="sd-setting__label" htmlFor="bt-ldac">
            LDAC quality
          </label>
          <select id="bt-ldac" className="sd-input" value={choice.quality} onChange={(e) => setChoice({ ...choice, quality: e.currentTarget.value })}>
            {LDAC_QUALITIES.map((q) => (
              <option key={q.id} value={q.id}>
                {q.label}
              </option>
            ))}
          </select>
        </div>
      )}
      <p className="sd-setting__help">
        Switching the codec interrupts the sound for a moment. Only the codecs this device and PipeWire both support are listed. LDAC
        sounds best but its encoder costs about 7 % of the board&apos;s CPU; a fixed &ldquo;Best&rdquo; can drop out when the radio
        link is weak, &ldquo;Adaptive&rdquo; steps down instead.
      </p>
      {note && <p className={note.ok ? "sd-setting__ok" : "sd-setting__error"}>{note.text}</p>}
      <Button size="sm" disabled={busy} onClick={apply}>
        {busy ? "Switching…" : "Use this codec"}
      </Button>
      <LinkRate detail={d} address={address} onChanged={onChanged} />
    </div>
  );
}

function khz(rate: number): string {
  return rate % 1000 === 0 ? `${rate / 1000} kHz` : `${(rate / 1000).toFixed(1)} kHz`;
}

/** The link's sample rate now and whether it is kept at 48 kHz. */
function LinkRate({ detail: d, address, onChanged }: { detail: BluetoothDeviceDetail; address: string; onChanged(): void }) {
  const [any, setAny] = useState(d.anyLinkRate);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  // Also offered unchanged while a 48 kHz device runs at another rate: renegotiates now.
  const offTarget = !any && d.linkRate !== 0 && d.linkRate !== 48000;
  const apply = () => {
    setBusy(true);
    setNote(null);
    deviceClient
      .setBluetoothLinkRate({ address, anyRate: any })
      .then(() => {
        setNote({ ok: true, text: any ? "Kept as the headset chooses." : "The link runs at 48 kHz." });
        onChanged();
      })
      .catch((err: unknown) => setNote({ ok: false, text: message(err) }))
      .finally(() => setBusy(false));
  };
  return (
    <>
      <div className="sd-setting">
        <label className="sd-setting__label" htmlFor="bt-rate">
          Link rate{d.linkRate ? ` · now ${khz(d.linkRate)}` : ""}
        </label>
        <select id="bt-rate" className="sd-input" value={any ? "any" : "48"} onChange={(e) => setAny(e.currentTarget.value === "any")}>
          <option value="48">48 kHz (the player&apos;s rate)</option>
          <option value="any">As the headset chooses</option>
        </select>
      </div>
      <p className="sd-setting__help">
        The player runs at 48 kHz. A headset that connects by itself may choose another rate (the WH-1000XM4 picks 96 kHz), and the
        board then resamples every sample. At 48 kHz such a link is renegotiated once per connect, a second gap in the sound.
      </p>
      {note && <p className={note.ok ? "sd-setting__ok" : "sd-setting__error"}>{note.text}</p>}
      <Button size="sm" disabled={busy || (any === d.anyLinkRate && !offTarget)} onClick={apply}>
        {busy ? "Switching…" : "Use this rate"}
      </Button>
    </>
  );
}
