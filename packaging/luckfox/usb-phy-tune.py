#!/usr/bin/python3
"""Raises the RV1106 USB PHY's high-speed eye height so the dock works on USB-A without a hub (H15-98).

Directly on the Luckfox Pico Ultra W's USB-A (through the U10 USB switch), the dock failed the
high-speed reset ("usb1-port1: Cannot enable") with the kernel's tuning (eye height 3'b011, 462 mV,
`rv1106_usb2phy_tuning` in phy-rockchip-inno-usb2.c). Measured on the board: 3'b100 0/3,
3'b101 3/3, 3'b110 3/3 enumerations; this sets 3'b110 (one step of margin, below the maximum).
A hub in between still works. If the dock is missing after the change, the root hub is
re-enumerated once so a dock that failed at boot comes up.

This runs too late for the boot itself: at the kernel's value the dock hangs at ~1 s until it is
replugged. The board's kernel therefore carries the same value (h15/luckfox/sdk, branch
h15-streamdock, `rv1106_usb2phy_tuning`, kernel #7 since 2026-10-19); this service is the fallback
for a board flashed with a stock kernel.
"""

import mmap
import os
import struct
import time

PHY_BASE = 0xFF3E0000
EYE_HEIGHT = 0x124  # bits 4:2, Tx HS eye height
EYE_MASK = 0b111 << 2
EYE_VALUE = 0b110 << 2
ROOT_HUB = "/sys/bus/usb/devices/usb1/authorized"
DOCK = "6603:1003"


def dock_present():
    for dev in os.listdir("/sys/bus/usb/devices"):
        base = os.path.join("/sys/bus/usb/devices", dev)
        try:
            with open(os.path.join(base, "idVendor")) as v, open(os.path.join(base, "idProduct")) as p:
                if f"{v.read().strip()}:{p.read().strip()}" == DOCK:
                    return True
        except OSError:
            continue
    return False


def main():
    fd = os.open("/dev/mem", os.O_RDWR | os.O_SYNC)
    try:
        regs = mmap.mmap(fd, 0x1000, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE, offset=PHY_BASE)
        old = struct.unpack("<I", regs[EYE_HEIGHT:EYE_HEIGHT + 4])[0]
        new = (old & ~EYE_MASK) | EYE_VALUE
        regs[EYE_HEIGHT:EYE_HEIGHT + 4] = struct.pack("<I", new)
        print(f"usb phy eye height: {old:#x} -> {new:#x}")
    finally:
        os.close(fd)
    if dock_present():
        return
    with open(ROOT_HUB, "w") as f:
        f.write("0")
    time.sleep(1)
    with open(ROOT_HUB, "w") as f:
        f.write("1")
    print("re-enumerated usb1")


if __name__ == "__main__":
    main()
