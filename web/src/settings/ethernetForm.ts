// Configured Ethernet values and family-specific changes (H15-63).
import type { EthernetStatus } from "../gen/streamdock/v1/device_pb.js";

export interface Form {
  dhcp: boolean;
  address: string;
  prefix: string;
  gateway: string;
  dns: string;
  ipv6: string;
  ipv6Address: string;
  ipv6Prefix: string;
  ipv6Gateway: string;
  ipv6Dns: string;
}

export function ipv6Choice(method: string): string {
  if (method === "manual") return "static";
  if (method === "disabled" || method === "ignore") return "disabled";
  return method === "dhcp" ? "dhcp" : "auto";
}

function ipv6Form(s: EthernetStatus) {
  const config = s.ipv6Config ?? { method: s.ipv6Method, address: "", prefix: 64, gateway: s.gateway6, dns: [] };
  return {
    ipv6: ipv6Choice(config.method || s.ipv6Method),
    ipv6Address: config.address,
    ipv6Prefix: config.prefix ? String(config.prefix) : "64",
    ipv6Gateway: config.gateway,
    ipv6Dns: config.dns.join(", "),
  };
}

export function formOf(s: EthernetStatus): Form {
  return {
    dhcp: s.dhcp,
    address: s.address,
    prefix: s.prefix ? String(s.prefix) : "24",
    gateway: s.gateway,
    dns: (s.ipv4Dns.length ? s.ipv4Dns : s.dns.filter((d) => !d.includes(":"))).join(", "),
    ...ipv6Form(s),
  };
}

function dnsList(text: string): string[] {
  return text.split(/[\s,]+/).filter((d) => d !== "");
}

function ipv4Changed(form: Form, status: EthernetStatus): boolean {
  if (form.dhcp !== status.dhcp) return true;
  if (form.dhcp) return false;
  const now = formOf(status);
  return form.address.trim() !== now.address || Number(form.prefix) !== Number(now.prefix) ||
    form.gateway.trim() !== now.gateway || dnsList(form.dns).join(",") !== dnsList(now.dns).join(",");
}

function ipv6Changed(form: Form, status: EthernetStatus): boolean {
  const now = formOf(status);
  if (form.ipv6 !== now.ipv6) return true;
  if (form.ipv6 !== "static") return false;
  return form.ipv6Address.trim() !== now.ipv6Address || Number(form.ipv6Prefix) !== Number(now.ipv6Prefix) ||
    form.ipv6Gateway.trim() !== now.ipv6Gateway || dnsList(form.ipv6Dns).join(",") !== dnsList(now.ipv6Dns).join(",");
}

export function changed(form: Form, status: EthernetStatus): boolean {
  return ipv4Changed(form, status) || ipv6Changed(form, status);
}

export function requestOf(form: Form, status: EthernetStatus) {
  const keepIpv4 = !ipv4Changed(form, status);
  const changeIpv6 = ipv6Changed(form, status);
  return {
    keepIpv4,
    ...(keepIpv4 ? {} : {
      dhcp: form.dhcp, address: form.address.trim(), prefix: Number(form.prefix),
      gateway: form.gateway.trim(), dns: dnsList(form.dns),
    }),
    ipv6Method: changeIpv6 ? form.ipv6 : "",
    ...(changeIpv6 ? {
      ipv6Address: form.ipv6Address.trim(), ipv6Prefix: Number(form.ipv6Prefix),
      ipv6Gateway: form.ipv6Gateway.trim(), ipv6Dns: dnsList(form.ipv6Dns),
    } : {}),
  };
}
