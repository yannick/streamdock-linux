// Everything BlueZ says about one Bluetooth device (H15-49): flags, battery, profiles, the audio
// streams with their codec, and the controller. Read again whenever the pushed device row changes,
// so a codec switch or a reconnect shows up while the dialog is open.
import { useEffect, useState, type ReactNode } from "react";

import { deviceClient } from "../api/client.js";
import { Badge, Button, DataList, Dialog, Disclosure } from "../design/index.js";
import type { BluetoothDevice, BluetoothDeviceDetail, BluetoothTransport } from "../gen/streamdock/v1/device_pb.js";
import { CodecChooser } from "./BluetoothCodec.js";

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

/** The pushed row of `address`, as a key that changes whenever the row does. */
function rowKey(devices: BluetoothDevice[], address: string | null): string {
  const d = devices.find((x) => x.address === address);
  return d ? [d.connected, d.paired, d.codec, d.name].join("|") : "";
}

/** While the dialog shows a connected device, a tick every few seconds re-reads its live link. */
const LINK_REFRESH_MS = 3000;

function useTick(active: boolean): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!active) return;
    const id = window.setInterval(() => setTick((t) => t + 1), LINK_REFRESH_MS);
    return () => window.clearInterval(id);
  }, [active]);
  return tick;
}

function useDetail(address: string | null, devices: BluetoothDevice[]) {
  const [detail, setDetail] = useState<BluetoothDeviceDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const tick = useTick(address !== null && detail?.device?.connected === true);
  const [reads, setReads] = useState(0);
  const key = rowKey(devices, address);
  useEffect(() => {
    if (address === null) {
      setDetail(null);
      setError(null);
      return;
    }
    let live = true;
    deviceClient
      .getBluetoothDevice({ address })
      .then((res) => {
        if (!live) return;
        setDetail(res.device ?? null);
        setError(null);
      })
      .catch((err: unknown) => live && setError(message(err)));
    return () => {
      live = false;
    };
  }, [address, key, tick, reads]);
  return { detail, error, reload: () => setReads((n) => n + 1) };
}

export function BluetoothDetail({ address, devices, onClose }: { address: string | null; devices: BluetoothDevice[]; onClose(): void }) {
  const { detail, error, reload } = useDetail(address, devices);
  const name = detail?.device?.name || "Unknown device";
  return (
    <Dialog
      open={address !== null}
      title={detail ? name : "Bluetooth device"}
      onClose={onClose}
      actions={
        <Button variant="primary" onClick={onClose}>
          Close
        </Button>
      }
    >
      {error && <p className="sd-setting__error">{error}</p>}
      {!detail && !error && <p className="sd-setting__help">Loading…</p>}
      {detail && <DetailBody detail={detail} onChanged={reload} />}
    </Dialog>
  );
}

function yesNo(on: boolean): ReactNode {
  return on ? <Badge tone="success">yes</Badge> : <Badge tone="muted">no</Badge>;
}

type Item = { term: ReactNode; value: ReactNode };

/** The optional facts, only where BlueZ reports them. */
function extraItems(d: BluetoothDeviceDetail): Item[] {
  const battery = d.battery !== undefined && `${d.battery} %${d.batterySource ? ` (via ${d.batterySource})` : ""}`;
  const items: Array<Item | false> = [
    d.bonded !== undefined && { term: "Bonded", value: yesNo(d.bonded) },
    d.blocked && { term: "Blocked", value: yesNo(true) },
    d.legacyPairing && { term: "Legacy pairing", value: yesNo(true) },
    d.rssi !== undefined && { term: "Signal", value: `${d.rssi} dBm (scan)` },
    battery !== false && { term: "Battery", value: battery },
  ];
  return items.filter((i): i is Item => i !== false);
}

const dbm = (v: number | undefined) => (v === undefined ? "—" : `${v} dBm`);

/** The live link of a connected device (H15-53), refreshed while the dialog is open. */
function linkItems(d: BluetoothDeviceDetail): Item[] {
  const link = d.link;
  if (!link) return [];
  if (link.problem) return [{ term: "Live signal", value: <span className="sd-setting__help">{link.problem}</span> }];
  const max = link.maxTxPower === undefined ? "" : ` (max ${link.maxTxPower} dBm)`;
  return [
    { term: "Live signal", value: dbm(link.rssi) },
    { term: "Our TX power", value: `${dbm(link.txPower)}${max}` },
  ];
}

function deviceItems(d: BluetoothDeviceDetail): Item[] {
  const dev = d.device;
  const type = d.addressType ? ` (${d.addressType})` : "";
  return [
    { term: "Address", value: `${dev?.address ?? ""}${type}` },
    { term: "Alias", value: d.alias || "—" },
    { term: "Class", value: d.class ? `${d.classText} (${d.class})` : "—" },
    { term: "Connected", value: yesNo(dev?.connected === true) },
    { term: "Paired", value: yesNo(dev?.paired === true) },
    { term: "Trusted", value: yesNo(d.trusted) },
    ...linkItems(d),
    ...extraItems(d),
  ];
}

/** What the device says about itself: IDs and what it advertises. */
function identityItems(d: BluetoothDeviceDetail): Item[] {
  const items: Array<Item | false> = [
    d.deviceId !== "" && { term: "Device ID", value: d.deviceId },
    d.modalias !== "" && { term: "Modalias", value: <code>{d.modalias}</code> },
    d.appearance !== "" && { term: "Appearance", value: d.appearance },
    d.txPower !== undefined && { term: "TX power", value: `${d.txPower} dBm (advertised)` },
    { term: "Services read", value: yesNo(d.servicesResolved) },
    { term: "May wake us", value: yesNo(d.wakeAllowed) },
    ...d.manufacturerData.map((m) => ({
      term: `Vendor data ${m.company || `0x${m.companyId.toString(16).padStart(4, "0")}`}`,
      value: <code>{m.dataHex || "—"}</code>,
    })),
  ];
  return items.filter((i): i is Item => i !== false);
}

function transportItems(t: BluetoothTransport): Item[] {
  const items: Item[] = [
    { term: "Our profile", value: t.profile },
    { term: "State", value: t.state === "active" ? <Badge tone="success">streaming</Badge> : t.state },
    { term: "Codec", value: t.codec || "—" },
    { term: "Configuration", value: t.configuration || "—" },
    { term: "Bitrate", value: t.bitrate || "—" },
    { term: "Raw", value: <code>{t.configurationHex}</code> },
  ];
  if (t.delayMs !== undefined) items.push({ term: "Delay", value: `${t.delayMs.toFixed(1)} ms (reported by the device)` });
  if (t.volume !== undefined) items.push({ term: "Volume", value: `${Math.round((t.volume * 100) / 127)} % (${t.volume}/127)` });
  return items;
}

function controllerItems(d: BluetoothDeviceDetail): Item[] {
  const items: Array<Item | false> = [
    { term: "Adapter", value: `${d.adapter}${d.adapterBus ? ` (${d.adapterBus})` : ""}` },
    { term: "Address", value: d.adapterAddress || "—" },
    d.adapterName !== "" && { term: "Name", value: d.adapterName },
    d.adapterDeviceId !== "" && { term: "Device ID", value: d.adapterDeviceId },
    d.adapterRoles.length > 0 && { term: "Roles", value: d.adapterRoles.join(", ") },
    { term: "Discoverable", value: yesNo(d.adapterDiscoverable) },
    { term: "Pairable", value: yesNo(d.adapterPairable) },
  ];
  return items.filter((i): i is Item => i !== false);
}

function streamsAside(d: BluetoothDeviceDetail): string {
  const active = d.transports.find((t) => t.state === "active");
  return active ? `${active.codec} · ${active.bitrate || "streaming"}` : `${d.transports.length}`;
}

function DetailBody({ detail: d, onChanged }: { detail: BluetoothDeviceDetail; onChanged(): void }) {
  return (
    <div className="sd-btdetail">
      <Disclosure title="Device" open>
        <DataList inline items={deviceItems(d)} />
      </Disclosure>
      <Disclosure title="Audio streams" aside={streamsAside(d)} open>
        {d.transports.length === 0 && <p className="sd-setting__help">No audio stream (not connected, or not an audio device).</p>}
        {d.transports.map((t, i) => (
          <DataList key={i} inline items={transportItems(t)} />
        ))}
        <CodecChooser key={`${d.preferred?.codec ?? ""}/${d.preferred?.ldacQuality ?? ""}`} detail={d} onChanged={onChanged} />
      </Disclosure>
      <Disclosure title="Identity and advertising">
        <DataList inline items={identityItems(d)} />
      </Disclosure>
      <Disclosure title="Profiles" aside={`${d.profiles.length}`}>
        <ul className="sd-btdetail__profiles">
          {d.profiles.map((p) => (
            <li key={p.uuid}>
              {p.name || "Vendor service"} <code className="sd-setting__help">{p.uuid}</code>
            </li>
          ))}
        </ul>
      </Disclosure>
      <Disclosure title="Controller" aside={d.adapter}>
        <DataList inline items={controllerItems(d)} />
      </Disclosure>
    </div>
  );
}
