// Ethernet of the machine streamdockd runs on (H15-42): status, and DHCP or a static IPv4 address.
// A change is only tried: the daemon reverts it after 60 s unless this page confirms it, so a wrong
// address can't lock the board out. After a new static address, confirm from that address.
import { useEffect, useRef, useState } from "react";

import { deviceClient } from "../api/client.js";
import { Badge, Button, Panel, Toggle } from "../design/index.js";
import type { EthernetStatus } from "../gen/streamdock/v1/device_pb.js";
import { AddressList } from "./Addresses.js";
import { changed, formOf, ipv6Choice, requestOf, type Form } from "./ethernetForm.js";

type Status = { ok: boolean; text: string } | null;

function message(err: unknown): string {
  return err instanceof Error ? err.message.replace(/^\[\w+\]\s*/, "") : String(err);
}

export function EthernetPanel({ status, reload }: { status: EthernetStatus; reload(): void }) {
  const aside = !status.available ? "unavailable" : status.connected ? status.address || "connected" : "no link";
  return (
    <Panel title="Ethernet" aside={aside} className="sd-settings__module">
      {status.available ? <EthernetBody status={status} reload={reload} /> : <p className="sd-setting__error">{status.problem}</p>}
    </Panel>
  );
}

function EthernetBody({ status, reload }: { status: EthernetStatus; reload(): void }) {
  const [form, setForm] = useState<Form>(() => formOf(status));
  const baseline = useRef(status);
  useEffect(() => {
    const previous = baseline.current;
    baseline.current = status;
    setForm((current) => changed(current, previous) ? current : formOf(status));
  }, [status]);
  const [result, setResult] = useState<Status>(null);
  const [busy, setBusy] = useState(false);
  const left = useCountdown(status.revertInSeconds, reload);

  const apply = () => {
    setBusy(true);
    setResult(null);
    deviceClient
      .setEthernet(requestOf(form, status))
      .then(() => {
        setResult(trialHint(form, status));
        reload();
      })
      .catch((err: unknown) => setResult({ ok: false, text: message(err) }))
      .finally(() => setBusy(false));
  };
  const keep = () => {
    deviceClient
      .confirmEthernet({})
      .then(() => setResult({ ok: true, text: "Kept. The setting is saved and survives a reboot." }))
      .catch((err: unknown) => setResult({ ok: false, text: message(err) }))
      .finally(reload);
  };

  return (
    <>
      <EthernetFacts status={status} />
      {left > 0 && (
        <div className="sd-trial" role="alert">
          <strong>Keep this network setting?</strong> It is reverted in {left} s unless you keep it.
          <Button variant="primary" size="sm" onClick={keep}>
            Keep
          </Button>
        </div>
      )}
      <EthernetForm form={form} onChange={setForm} />
      {result && <p className={result.ok ? "sd-setting__ok" : "sd-setting__error"}>{result.text}</p>}
      <div className="sd-settings__actions">
        <Button variant="primary" disabled={busy || left > 0 || !changed(form, status)} onClick={apply}>
          Try this setting
        </Button>
        <span className="sd-setting__help">Reverted after 60 s unless you keep it, so a mistake can't lock you out.</span>
      </div>
    </>
  );
}

/** Where to confirm the change from: the new address after a static change, else here. */
function trialHint(form: Form, status: EthernetStatus): Status {
  if (!form.dhcp && form.address.trim() !== status.address) {
    const url = `http://${form.address.trim()}${window.location.port ? `:${window.location.port}` : ""}/settings#network`;
    return { ok: true, text: `On trial. Open ${url} within 60 s and press Keep there, or it goes back to ${status.address}.` };
  }
  return { ok: true, text: "On trial. If the page still works, press Keep." };
}

/** Counts down from `seconds` (reset when the daemon reports a new value); reloads at zero. */
function useCountdown(seconds: number, onDone: () => void): number {
  const [left, setLeft] = useState(seconds);
  useEffect(() => {
    setLeft(seconds);
    if (seconds <= 0) return;
    const started = Date.now();
    const timer = setInterval(() => {
      const now = Math.max(0, seconds - Math.floor((Date.now() - started) / 1000));
      setLeft(now);
      if (now === 0) {
        clearInterval(timer);
        onDone();
      }
    }, 1000);
    return () => clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- restart only when the daemon's value changes
  }, [seconds]);
  return left;
}

function EthernetFacts({ status }: { status: EthernetStatus }) {
  return (
    <dl className="sd-settings__daemon">
      <div>
        <dt>Link</dt>
        <dd>
          {status.interface} {status.carrier ? <Badge tone="success">cable</Badge> : <Badge tone="muted">no cable</Badge>}{" "}
          {status.dhcp ? "DHCP" : "static"}
        </dd>
      </div>
      <div>
        <dt>Addresses</dt>
        <dd>
          <AddressList addresses={status.addresses} details={status.addressDetails} />
        </dd>
      </div>
      <div>
        <dt>Gateway</dt>
        <dd>{[status.gateway, status.gateway6].filter((g) => g !== "").join(", ") || "—"}</dd>
      </div>
      <div>
        <dt>IPv6</dt>
        <dd>{ipv6Choice(status.ipv6Method)}</dd>
      </div>
      <div>
        <dt>DNS</dt>
        <dd>{status.dns.join(", ") || "—"}</dd>
      </div>
    </dl>
  );
}

function EthernetForm({ form, onChange }: { form: Form; onChange(f: Form): void }) {
  const set = (key: keyof Form) => (e: React.ChangeEvent<HTMLInputElement>) => onChange({ ...form, [key]: e.currentTarget.value });
  return (
    <>
      <div className="sd-setting">
        <span className="sd-setting__label">IPv4</span>
        <Toggle label="IPv4 method" offLabel="Static" onLabel="DHCP" checked={form.dhcp} onChange={(dhcp) => onChange({ ...form, dhcp })} />
        {!form.dhcp && (
          <div className="sd-action__row">
            <Field label="Address" value={form.address} onChange={set("address")} placeholder="192.168.1.145" />
            <Field label="Prefix" value={form.prefix} onChange={set("prefix")} placeholder="24" />
            <Field label="Gateway" value={form.gateway} onChange={set("gateway")} placeholder="192.168.1.2" />
            <Field label="DNS" value={form.dns} onChange={set("dns")} placeholder="192.168.1.2, 1.1.1.1" />
          </div>
        )}
      </div>
      <Ipv6Form form={form} onChange={onChange} />
    </>
  );
}

const IPV6_METHODS: ReadonlyArray<{ id: string; label: string }> = [
  { id: "auto", label: "Automatic (router advertisements)" },
  { id: "dhcp", label: "DHCPv6" },
  { id: "static", label: "Static" },
  { id: "disabled", label: "Off" },
];

function Ipv6Form({ form, onChange }: { form: Form; onChange(f: Form): void }) {
  const set = (key: keyof Form) => (e: React.ChangeEvent<HTMLInputElement>) => onChange({ ...form, [key]: e.currentTarget.value });
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor="eth-ipv6">
        IPv6
      </label>
      <select id="eth-ipv6" className="sd-input" value={form.ipv6} onChange={(e) => onChange({ ...form, ipv6: e.currentTarget.value })}>
        {IPV6_METHODS.map((m) => (
          <option key={m.id} value={m.id}>
            {m.label}
          </option>
        ))}
      </select>
      {form.ipv6 === "static" && (
        <div className="sd-action__row">
          <Field label="IPv6 address" value={form.ipv6Address} onChange={set("ipv6Address")} placeholder="2001:db8::145" />
          <Field label="IPv6 prefix" value={form.ipv6Prefix} onChange={set("ipv6Prefix")} placeholder="64" />
          <Field label="IPv6 gateway" value={form.ipv6Gateway} onChange={set("ipv6Gateway")} placeholder="fe80::1" />
          <Field label="IPv6 DNS" value={form.ipv6Dns} onChange={set("ipv6Dns")} placeholder="2001:db8::53, fe80::1" />
        </div>
      )}
    </div>
  );
}

function Field(props: { label: string; value: string; placeholder: string; onChange(e: React.ChangeEvent<HTMLInputElement>): void }) {
  const id = `eth-${props.label.toLowerCase().replace(/\s+/g, "-")}`;
  return (
    <div className="sd-setting">
      <label className="sd-setting__label" htmlFor={id}>
        {props.label}
      </label>
      <input id={id} className="sd-input" value={props.value} placeholder={props.placeholder} onChange={props.onChange} />
    </div>
  );
}
