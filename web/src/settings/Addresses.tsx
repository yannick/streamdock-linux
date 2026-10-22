// A link's IP addresses (H15-44): IPv4 and IPv6, link-local ones marked (they only work on that link).
import type { NetworkAddress } from "../gen/streamdock/v1/device_pb.js";
import { Badge } from "../design/index.js";

/** Whether "fe80::1/64" or "169.254.1.2/16" is link-local. */
export function isLinkLocal(address: string): boolean {
  const ip = address.split("/")[0]!.toLowerCase();
  return /^fe[89ab][0-9a-f]:/.test(ip) || ip.startsWith("169.254.");
}

export function labelsOf(address: string, details: readonly NetworkAddress[]): string[] {
  const detail = details.find((d) => d.cidr === address);
  if (!detail) return isLinkLocal(address) ? ["link-local"] : [];
  return [detail.scope, detail.temporary ? "temporary / privacy" : "", detail.stablePrivacy ? "stable privacy" : ""]
    .filter((label) => label !== "");
}

export function AddressList({ addresses, details = [] }: { addresses: readonly string[]; details?: readonly NetworkAddress[] }) {
  if (addresses.length === 0) return <>—</>;
  return (
    <ul className="sd-addresses">
      {addresses.map((a) => (
        <li key={a}>
          <code>{a}</code>{" "}
          {labelsOf(a, details).map((label) => <Badge key={label} tone="muted">{label}</Badge>)}
        </li>
      ))}
    </ul>
  );
}
