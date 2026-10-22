import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";

import { EthernetStatusSchema } from "../gen/streamdock/v1/device_pb.js";
import { changed, formOf, requestOf } from "./ethernetForm.js";

const status = () => create(EthernetStatusSchema, {
  dhcp: false, address: "192.168.1.145", prefix: 24, gateway: "192.168.1.1",
  dns: ["192.168.1.53", "2001:db8::53"], ipv4Dns: ["192.168.1.53"],
  ipv6Method: "manual",
  ipv6Config: { method: "manual", address: "2001:db8::145", prefix: 64, gateway: "fe80::1", dns: ["2001:db8::53"] },
});

describe("Ethernet form", () => {
  it("reads configured static IPv6 and keeps DNS families separate", () => {
    const s = status();
    const form = formOf(s);
    expect(form.dns).toBe("192.168.1.53");
    expect(form.ipv6Address).toBe("2001:db8::145");
    expect(form.ipv6Prefix).toBe("64");
    expect(form.ipv6Gateway).toBe("fe80::1");
    expect(form.ipv6Dns).toBe("2001:db8::53");
    expect(changed(form, s)).toBe(false);
  });
  it("sends an IPv6-only change with an explicit IPv4 preservation flag", () => {
    const s = status();
    const request = requestOf({ ...formOf(s), ipv6: "disabled" }, s);
    expect(request.keepIpv4).toBe(true);
    expect(request.ipv6Method).toBe("disabled");
    expect(Object.hasOwn(request, "dhcp")).toBe(false);
    expect(Object.hasOwn(request, "address")).toBe(false);
    expect(Object.hasOwn(request, "dns")).toBe(false);
  });
  it("does not rebuild IPv6 when only IPv4 DNS changes", () => {
    const s = status();
    const request = requestOf({ ...formOf(s), dns: "192.168.1.54" }, s);
    expect(request.keepIpv4).toBe(false);
    expect(request.ipv6Method).toBe("");
    expect(Object.hasOwn(request, "ipv6Address")).toBe(false);
    expect(request.dns).toEqual(["192.168.1.54"]);
  });
});
