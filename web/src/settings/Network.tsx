// Wi-Fi and Bluetooth of the machine streamdockd runs on (H15-38), for the settings page. Shown only
// when the daemon manages the network (`[network] manage = true`, e.g. on the Luckfox). The state
// comes live from `WatchEvents` (H15-49): a headset that reconnects by itself shows up at once.
import { useCallback, useEffect, useMemo, useState } from "react";

import { deviceClient } from "../api/client.js";
import { useLive } from "../api/live.js";
import { BluetoothDetail } from "./BluetoothDetail.js";
import { Badge, Button, Panel, Toggle } from "../design/index.js";
import { AddressList } from "./Addresses.js";
import type {
  BluetoothDevice,
  BluetoothStatus,
  GetNetworkResponse,
  WifiNetwork,
  WifiStatus,
} from "../gen/streamdock/v1/device_pb.js";

type Status = { ok: boolean; text: string } | null;

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

/** Runs `action`, then shows `done` or the daemon's error, and reloads. */
function useAction(reload: () => void) {
  const [busy, setBusy] = useState<string | null>(null);
  const [status, setStatus] = useState<Status>(null);
  const run = (what: string, action: () => Promise<unknown>, done: string) => {
    setBusy(what);
    setStatus(null);
    action()
      .then(() => setStatus({ ok: true, text: done }))
      .catch((err: unknown) => setStatus({ ok: false, text: message(err) }))
      .finally(() => {
        setBusy(null);
        reload();
      });
  };
  return { busy, status, run };
}

/** The Bluetooth devices BlueZ knows and whether a scan runs, as last pushed. */
export interface BluetoothList {
  devices: BluetoothDevice[];
  scanning: boolean;
}

/**
 * What this machine's network looks like (`null` while loading), kept live by `WatchEvents`
 * (H15-49). `reload` reads it once more, e.g. where the daemon doesn't manage the network.
 */
export function useNetwork(): { network: GetNetworkResponse | null; bluetooth: BluetoothList; reload(): void } {
  const [loaded, setLoaded] = useState<GetNetworkResponse | null>(null);
  const live = useLive().network;
  const reload = useCallback(() => {
    deviceClient
      .getNetwork({})
      .then(setLoaded)
      .catch(() => setLoaded(null));
  }, []);
  useEffect(reload, [reload]);
  const bluetooth = useMemo(
    () => ({ devices: live?.bluetoothDevices ?? [], scanning: live?.bluetoothScanning ?? false }),
    [live],
  );
  return { network: live?.status ?? loaded, bluetooth, reload };
}

function signalText(signal: number): string {
  if (signal >= 70) return "▂▄▆█";
  if (signal >= 45) return "▂▄▆";
  if (signal >= 20) return "▂▄";
  return "▂";
}

export function WifiPanel({ status, reload }: { status: WifiStatus; reload(): void }) {
  const aside = status.connectedSsid ? `connected to ${status.connectedSsid}` : status.available ? "not connected" : "unavailable";
  return (
    <Panel title="Wi-Fi" aside={aside} className="sd-settings__module">
      {status.available ? <WifiBody status={status} reload={reload} /> : <p className="sd-setting__error">{status.problem}</p>}
    </Panel>
  );
}

function WifiFacts({ status }: { status: WifiStatus }) {
  return <>
      {status.addresses.length > 0 && (
        <div className="sd-setting">
          <span className="sd-setting__label">Addresses</span>
          <AddressList addresses={status.addresses} details={status.addressDetails} />
        </div>
      )}
      <dl className="sd-settings__daemon">
        <div>
          <dt>Gateway</dt>
          <dd>{[status.gateway, status.gateway6].filter((g) => g !== "").join(", ") || "—"}</dd>
        </div>
        <div>
          <dt>DNS</dt>
          <dd>{status.dns.join(", ") || "—"}</dd>
        </div>
      </dl>
  </>;
}

function WifiBody({ status, reload }: { status: WifiStatus; reload(): void }) {
  const [networks, setNetworks] = useState<WifiNetwork[] | null>(null);
  const [chosen, setChosen] = useState<string | null>(null);
  const action = useAction(reload);
  const list = (rescan: boolean) =>
    deviceClient
      .listWifiNetworks({ rescan })
      .then((res) => setNetworks(res.networks))
      .catch((err: unknown) => action.run("list", () => Promise.reject(err), ""));
  useEffect(() => {
    void list(false);
    // On first show and when the connection changes (live, H15-49); the Scan button rescans.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [status.connectedSsid]);

  return (
    <>
      {!status.radioOn && (
        <p className="sd-settings__help">
          The Wi-Fi radio is off (see Wi-Fi mode above). To join a network, switch the mode to "Both", join, then switch
          back.
        </p>
      )}
      <WifiFacts status={status} />
      <div className="sd-settings__actions">
        <Button
          disabled={action.busy !== null || !status.radioOn}
          onClick={() => {
            setNetworks(null);
            void list(true);
          }}
        >
          {networks === null ? "Scanning…" : "Scan"}
        </Button>
        {status.connectedSsid && (
          <Button
            disabled={action.busy !== null}
            onClick={() => action.run("disconnect", () => deviceClient.disconnectWifi({}), "Disconnected.")}
          >
            Disconnect
          </Button>
        )}
      </div>
      {action.status && <p className={action.status.ok ? "sd-setting__ok" : "sd-setting__error"}>{action.status.text}</p>}
      <ul className="sd-netlist">
        {(networks ?? []).map((n) => (
          <WifiRow
            key={n.ssid}
            network={n}
            open={chosen === n.ssid}
            busy={action.busy !== null}
            onChoose={() => setChosen(chosen === n.ssid ? null : n.ssid)}
            onConnect={(password) =>
              action.run("connect", () => deviceClient.connectWifi({ ssid: n.ssid, password }), `Connected to ${n.ssid}.`)
            }
            onForget={() => action.run("forget", () => deviceClient.forgetWifi({ ssid: n.ssid }), `Forgot ${n.ssid}.`)}
          />
        ))}
      </ul>
    </>
  );
}

interface WifiRowProps {
  network: WifiNetwork;
  open: boolean;
  busy: boolean;
  onChoose(): void;
  onConnect(password: string): void;
  onForget(): void;
}

function WifiRow({ network: n, open, busy, onChoose, onConnect, onForget }: WifiRowProps) {
  return (
    <li className="sd-netlist__item" data-active={n.inUse || undefined}>
      <button type="button" className="sd-netlist__head" onClick={onChoose} aria-expanded={open}>
        <WifiSummary network={n} />
      </button>
      {open && !n.inUse && <WifiConnect network={n} busy={busy} onConnect={onConnect} onForget={onForget} />}
    </li>
  );
}

function WifiSummary({ network: n }: { network: WifiNetwork }) {
  const band = n.frequencyMhz >= 5000 ? "5 GHz" : "2.4 GHz";
  return (
    <>
      <span className="sd-netlist__signal" aria-label={`signal ${n.signal} %`}>
        {signalText(n.signal)}
      </span>
      <span className="sd-netlist__name">{n.ssid}</span>
      {n.inUse && <Badge tone="success">connected</Badge>}
      {n.saved && !n.inUse && <Badge>saved</Badge>}
      <span className="sd-setting__help">
        {band}
        {n.secure ? " · secured" : " · open"}
      </span>
    </>
  );
}

function WifiConnect({
  network: n,
  busy,
  onConnect,
  onForget,
}: {
  network: WifiNetwork;
  busy: boolean;
  onConnect(password: string): void;
  onForget(): void;
}) {
  const [password, setPassword] = useState("");
  const needsPassword = n.secure && !n.saved;
  return (
    <div className="sd-setting__row sd-netlist__form">
      {needsPassword && (
        <input
          className="sd-input"
          type="password"
          autoComplete="off"
          placeholder="password"
          aria-label={`Password for ${n.ssid}`}
          value={password}
          onChange={(e) => setPassword(e.currentTarget.value)}
        />
      )}
      <Button variant="primary" size="sm" disabled={busy || (needsPassword && password === "")} onClick={() => onConnect(password)}>
        Connect
      </Button>
      {n.saved && (
        <Button size="sm" disabled={busy} onClick={onForget}>
          Forget
        </Button>
      )}
    </div>
  );
}

export function BluetoothPanel({ status, list, reload }: { status: BluetoothStatus; list: BluetoothList; reload(): void }) {
  const { devices, scanning } = list;
  const action = useAction(reload);
  const [shown, setShown] = useState<string | null>(null);

  const aside = !status.available ? "unavailable" : status.powered ? "on" : "off";
  return (
    <Panel title="Bluetooth" aside={aside} className="sd-settings__module">
      {!status.available && <p className="sd-setting__error">{status.problem}</p>}
      {status.available && (
        <>
          <div className="sd-settings__actions">
            <Toggle
              label="Bluetooth"
              checked={status.powered}
              disabled={action.busy !== null}
              onChange={(on) => action.run("power", () => deviceClient.setBluetoothPower({ on }), on ? "Bluetooth on." : "Bluetooth off.")}
            />
            <Button
              disabled={action.busy !== null}
              onClick={() =>
                action.run(
                  "scan",
                  () => deviceClient.listBluetoothDevices({ scan: true }),
                  "Scanning for 30 s: put the speaker or headset into pairing mode now.",
                )
              }
            >
              {scanning ? "Scanning… (again to extend)" : "Scan"}
            </Button>
          </div>
          {action.status && <p className={action.status.ok ? "sd-setting__ok" : "sd-setting__error"}>{action.status.text}</p>}
          <ul className="sd-netlist">
            {devices.map((d) => (
              <BluetoothRow key={d.address} device={d} busy={action.busy !== null} run={action.run} onDetails={() => setShown(d.address)} />
            ))}
          </ul>
          <BluetoothDetail address={shown} devices={devices} onClose={() => setShown(null)} />
        </>
      )}
    </Panel>
  );
}

function BluetoothRow({
  device: d,
  busy,
  run,
  onDetails,
}: {
  device: BluetoothDevice;
  busy: boolean;
  run(what: string, action: () => Promise<unknown>, done: string): void;
  onDetails(): void;
}) {
  const address = { address: d.address };
  // Unnamed while BlueZ still resolves the name during a scan (H15-45).
  const label = d.name || "Unknown device";
  return (
    <li className="sd-netlist__item" data-active={d.connected || undefined}>
      <div className="sd-netlist__head">
        <span className="sd-netlist__name">{label}</span>
        <BluetoothBadges device={d} />
        <span className="sd-setting__help sd-netlist__address">{d.address}</span>
      </div>
      <div className="sd-settings__actions sd-netlist__form">
        {d.connected ? (
          <Button size="sm" disabled={busy} onClick={() => run("disconnect", () => deviceClient.disconnectBluetooth(address), `${label} disconnected.`)}>
            Disconnect
          </Button>
        ) : (
          <Button variant="primary" size="sm" disabled={busy} onClick={() => run("connect", () => deviceClient.connectBluetooth(address), `${label} connected.`)}>
            {d.paired ? "Connect" : "Pair and connect"}
          </Button>
        )}
        {d.paired && (
          <Button size="sm" disabled={busy} onClick={() => run("remove", () => deviceClient.removeBluetooth(address), `${label} removed.`)}>
            Remove
          </Button>
        )}
        <Button size="sm" onClick={onDetails}>
          Details
        </Button>
      </div>
    </li>
  );
}

function BluetoothBadges({ device: d }: { device: BluetoothDevice }) {
  return (
    <>
      {d.connected && <Badge tone="success">connected</Badge>}
      {d.paired && !d.connected && <Badge>paired</Badge>}
      {d.codec ? <Badge tone="muted">{d.codec}</Badge> : d.audio && <span className="sd-setting__help">audio</span>}
    </>
  );
}

const WIFI_MODES: ReadonlyArray<{ id: string; label: string; help: string }> = [
  { id: "fallback", label: "Fallback", help: "Wi-Fi only while the Ethernet cable is unplugged or down; off again when it's back." },
  { id: "both", label: "Both", help: "Ethernet and Wi-Fi stay connected." },
  { id: "off", label: "Off", help: "The Wi-Fi radio is off, so Bluetooth has the chip to itself." },
];

/** When the Wi-Fi radio is on: off, fallback (only without Ethernet) or both (H15-44). */
export function WifiModePanel({ mode, radioOn, reload }: { mode: string; radioOn: boolean; reload(): void }) {
  const action = useAction(reload);
  return (
    <Panel title="Wi-Fi mode" aside={`radio ${radioOn ? "on" : "off"}`} className="sd-settings__module">
      <div className="sd-netlist" role="radiogroup" aria-label="Wi-Fi mode">
        {WIFI_MODES.map((m) => (
          <label key={m.id} className="sd-netlist__item sd-netlist__head" data-active={m.id === mode || undefined}>
            <input
              type="radio"
              name="wifi-mode"
              value={m.id}
              checked={m.id === mode}
              disabled={action.busy !== null}
              onChange={() => action.run("mode", () => deviceClient.setWifiMode({ mode: m.id }), `Wi-Fi mode: ${m.label}.`)}
            />
            <span className="sd-netlist__name">{m.label}</span>
            <span className="sd-setting__help">{m.help}</span>
          </label>
        ))}
      </div>
      {action.status && <p className={action.status.ok ? "sd-setting__ok" : "sd-setting__error"}>{action.status.text}</p>}
    </Panel>
  );
}
