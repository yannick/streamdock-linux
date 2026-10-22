import { create } from "@bufbuild/protobuf";
import { NetworkAddressSchema } from "../gen/streamdock/v1/device_pb.js";
import { describe, expect, it } from "vitest";

import { isLinkLocal, labelsOf } from "./Addresses.js";

describe("isLinkLocal", () => {
  it("marks link-local IPv6 and IPv4 addresses", () => {
    expect(isLinkLocal("fe80::2a3d:e8ff:fe83:5930/64")).toBe(true);
    expect(isLinkLocal("FEB0::1/64")).toBe(true);
    expect(isLinkLocal("169.254.3.4/16")).toBe(true);
  });
  it("leaves routable addresses alone", () => {
    expect(isLinkLocal("2a02:168:e678::145/64")).toBe(false);
    expect(isLinkLocal("192.168.1.145/24")).toBe(false);
    expect(isLinkLocal("fec0::1/64")).toBe(false);
  });
});

describe("address metadata", () => {
  it("shows scope and actual temporary/privacy flags", () => {
    const details = [create(NetworkAddressSchema, { cidr: "2001:db8::1/64", scope: "global", temporary: true })];
    expect(labelsOf("2001:db8::1/64", details)).toEqual(["global", "temporary / privacy"]);
    expect(labelsOf("fe80::1/64", details)).toEqual(["link-local"]);
    expect(labelsOf("2001:db8::2/64", details)).toEqual([]);
  });
});
