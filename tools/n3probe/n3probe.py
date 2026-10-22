#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow>=10"]
# ///
"""Probe tool for the StreamDock N3 (USB 6603:1003) over Linux hidraw.

Talks to the vendor interface (usage page 0xFFA0) directly through /dev/hidrawN,
so no hidapi binding is needed. See docs/protocol.md for the protocol.

Usage:
  n3probe.py info                       show device node, descriptor and firmware
  n3probe.py init                       wake the screen (DIS + LIG)
  n3probe.py listen [--raw]             print decoded input events (Ctrl-C to stop)
  n3probe.py brightness <0-100>         set LCD brightness
  n3probe.py image <key 1-6> <file|color> [--size N] [--rot DEG]
                                        draw an image or a solid color on one LCD key
  n3probe.py clear [key 1-6]            clear one key, or all keys
  n3probe.py sleep                      put the screen to sleep (HAN)
  n3probe.py raw <hex...>               send one raw CRT command, e.g. `raw 4c 49 47 00 00 32`
"""

from __future__ import annotations

import argparse
import fcntl
import glob
import io
import os
import select
import sys
import time

VID, PID = 0x6603, 0x1003
USAGE_PAGE = 0xFFA0
OUT_REPORT = 1024  # output report payload size (from the report descriptor)
IN_REPORT = 512  # input report size
CRT = b"CRT\x00\x00"

# Hardware input codes (byte 9 of an ACK input report) -> event name.
INPUTS = {
    0x01: "key1", 0x02: "key2", 0x03: "key3",
    0x04: "key4", 0x05: "key5", 0x06: "key6",
    0x25: "button7", 0x30: "button8", 0x31: "button9",
    0x33: "knob1-press", 0x34: "knob2-press", 0x35: "knob3-press",
    0x90: "knob1-left", 0x91: "knob1-right",
    0x60: "knob2-left", 0x61: "knob2-right",
    0x50: "knob3-left", 0x51: "knob3-right",
}


def find_node() -> str:
    """Return the hidraw node of the vendor interface of the first N3 found."""
    for sysdir in sorted(glob.glob("/sys/class/hidraw/hidraw*")):
        try:
            with open(f"{sysdir}/device/uevent") as f:
                uevent = f.read()
            with open(f"{sysdir}/device/report_descriptor", "rb") as f:
                desc = f.read()
        except OSError:
            continue
        if f"HID_ID=0003:{VID:08X}:{PID:08X}" not in uevent:
            continue
        # Usage Page (Vendor 0xFFA0): 06 a0 ff
        if desc.startswith(bytes([0x06, USAGE_PAGE & 0xFF, USAGE_PAGE >> 8])):
            return "/dev/" + os.path.basename(sysdir)
    sys.exit("no StreamDock N3 (6603:1003) vendor interface found")


class Dock:
    def __init__(self, node: str | None = None):
        self.node = node or find_node()
        self.fd = os.open(self.node, os.O_RDWR)

    def close(self):
        os.close(self.fd)

    def write_report(self, payload: bytes):
        """Write one output report: report id 0 + payload padded to OUT_REPORT."""
        if len(payload) > OUT_REPORT:
            raise ValueError("payload too long")
        os.write(self.fd, b"\x00" + payload.ljust(OUT_REPORT, b"\x00"))

    def cmd(self, body: bytes):
        """Send a CRT command (`CRT\\0\\0` + body)."""
        self.write_report(CRT + body)

    def read_report(self, timeout: float | None = None) -> bytes | None:
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return None
        return os.read(self.fd, IN_REPORT)

    # Commands

    def wake(self):
        self.cmd(b"DIS")
        self.cmd(b"LIG\x00\x00\x00\x00")

    def brightness(self, percent: int):
        self.cmd(b"LIG\x00\x00" + bytes([max(0, min(100, percent))]))

    def clear(self, key: int = 0xFF):
        self.cmd(b"CLE\x00\x00\x00" + bytes([key]))
        self.cmd(b"STP")

    def image(self, key: int, jpeg: bytes):
        n = len(jpeg)
        self.cmd(b"BAT" + bytes([0, 0, n >> 8, n & 0xFF, key]))
        for off in range(0, n, OUT_REPORT):
            self.write_report(jpeg[off : off + OUT_REPORT])
        self.cmd(b"STP")

    def keep_alive(self):
        self.cmd(b"CONNECT")

    def sleep(self):
        self.cmd(b"HAN")

    def firmware(self) -> str | None:
        """Read the firmware string with HIDIOCGFEATURE(report 1); None if unsupported."""
        buf = bytearray(64)
        buf[0] = 0x01
        HIDIOCGFEATURE = 0xC0004807 | (len(buf) << 16)
        try:
            n = fcntl.ioctl(self.fd, HIDIOCGFEATURE, buf, True)
        except OSError:
            return None
        return bytes(buf[:n]).split(b"\x00")[0].decode(errors="replace")


# Rotation candidates for the `pattern` command, one per key 1-6.
PATTERN_ROTATIONS = [0, 90, 180, 270, 90, 270]


def decode(report: bytes) -> str:
    if not report.startswith(b"ACK"):
        return f"non-ACK report: {report[:16].hex(' ')}"
    code, state = report[9], report[10]
    name = INPUTS.get(code, f"unknown-0x{code:02x}")
    st = {0x01: "down", 0x00: "up", 0x02: "up"}.get(state, f"state=0x{state:02x}")
    return f"{name:<12} {st}"


FONT = "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf"


def make_jpeg(src: str, size: int, rot: int, label: str = "") -> bytes:
    from PIL import Image, ImageColor, ImageDraw, ImageFont

    if os.path.exists(src):
        img = Image.open(src).convert("RGB").resize((size, size))
    else:
        img = Image.new("RGB", (size, size), ImageColor.getrgb(src))
        d = ImageDraw.Draw(img)
        # Triangle in the top-left corner so rotation/flip is visible on the device.
        d.polygon([(0, 0), (size // 3, 0), (0, size // 3)], fill="white")
        if label:
            try:
                font = ImageFont.truetype(FONT, size // 2)
            except OSError:
                font = ImageFont.load_default()
            d.text((size // 2, size // 2), label, fill="white", font=font, anchor="mm")
    if rot:
        img = img.rotate(rot, expand=True)
    out = io.BytesIO()
    img.save(out, "JPEG", quality=90, subsampling=0)
    return out.getvalue()


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--node", help="hidraw node (default: autodetect)")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("info")
    sub.add_parser("init")
    s = sub.add_parser("listen")
    s.add_argument("--raw", action="store_true")
    s.add_argument("--seconds", type=float, default=0, help="stop after N seconds (0 = forever)")
    s = sub.add_parser("brightness")
    s.add_argument("percent", type=int)
    s = sub.add_parser("image")
    s.add_argument("key", type=int)
    s.add_argument("src", help="image file or color name (e.g. red, #00ffcc)")
    s.add_argument("--size", type=int, default=64)
    s.add_argument("--rot", type=int, default=270, help="counter-clockwise rotation in degrees (270 = upright on the N3)")
    s.add_argument("--label", default="", help="text drawn on a color image")
    s = sub.add_parser("pattern", help="label keys 1-6 with their number, each with another rotation")
    s.add_argument("--size", type=int, default=64)
    s = sub.add_parser("clear")
    s.add_argument("key", type=int, nargs="?", default=0xFF)
    sub.add_parser("sleep")
    s = sub.add_parser("raw")
    s.add_argument("hex", nargs="+")
    a = p.parse_args()

    dock = Dock(a.node)
    try:
        if a.cmd == "info":
            print("node:", dock.node)
            sysdir = f"/sys/class/hidraw/{os.path.basename(dock.node)}/device"
            print(open(f"{sysdir}/uevent").read().strip())
            print("descriptor:", open(f"{sysdir}/report_descriptor", "rb").read().hex())
            print("firmware:", dock.firmware())
        elif a.cmd == "init":
            dock.wake()
        elif a.cmd == "listen":
            dock.wake()
            end = time.monotonic() + a.seconds if a.seconds else None
            last_ka = time.monotonic()
            while end is None or time.monotonic() < end:
                rep = dock.read_report(1.0)
                if time.monotonic() - last_ka > 10:
                    dock.keep_alive()
                    last_ka = time.monotonic()
                if rep is None:
                    continue
                line = decode(rep)
                if a.raw:
                    line += "   " + rep[:16].hex(" ")
                print(time.strftime("%H:%M:%S"), line, flush=True)
        elif a.cmd == "brightness":
            dock.wake()
            dock.brightness(a.percent)
        elif a.cmd == "image":
            dock.wake()
            jpeg = make_jpeg(a.src, a.size, a.rot, a.label)
            dock.image(a.key, jpeg)
            print(f"sent {len(jpeg)} bytes JPEG {a.size}x{a.size} rot {a.rot} to key {a.key}")
        elif a.cmd == "pattern":
            dock.wake()
            colors = ["red", "green", "blue", "purple", "orange", "teal"]
            for key, (color, rot) in enumerate(zip(colors, PATTERN_ROTATIONS), start=1):
                dock.image(key, make_jpeg(color, a.size, rot, str(key)))
                print(f"key {key}: {color:<6} label '{key}' rotated {rot} deg")
        elif a.cmd == "clear":
            dock.wake()
            dock.clear(a.key)
        elif a.cmd == "sleep":
            dock.sleep()
        elif a.cmd == "raw":
            dock.cmd(bytes.fromhex("".join(a.hex)))
    except KeyboardInterrupt:
        pass
    finally:
        dock.close()


if __name__ == "__main__":
    main()
