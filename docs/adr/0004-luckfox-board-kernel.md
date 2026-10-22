# ADR-004: Our own kernel on the Luckfox, built from a patched SDK branch

- Status: proposed (H15-98, 2026-10-19; the maintainer asked for this ADR on H15-47)
- Deciders: the maintainer (product, "Flash now" on H15-98), the lead agent
- Issues: H15-34 (board image), H15-98 (dock directly on USB-A)

## Context

The production board is a Luckfox Pico Ultra W (RV1106, eMMC) running Ubuntu with `streamdockd`
(H15-34). Luckfox's stock kernel isn't enough for us:

- It has no hidraw/USB HID, and the board's USB runs in device (OTG) mode.
- The CMA reservation for the camera eats ~65 MB of the 234 MB RAM.
- **H15-98:** the USB PHY's high-speed signal ("eye height") is too weak for the StreamDock directly
  on USB-A, behind the board's USB switch U10:
  - The stock tuning (3'b011, 462 mV, `rv1106_usb2phy_tuning`) fails ("usb1-port1: Cannot enable",
    `-71`).
  - The dock then **hangs until it is replugged**: USB-A's 5 V is hardwired, so the board can't
    power-cycle it.
  - Measured with runtime writes to `0xff3e0124` bits 4:2: 3'b100 0/3 enumerations, 3'b101–3'b111
    3/3. Pre-emphasis, ODT, squelch and disconnect mode didn't matter.
  - Userspace (a boot service) is too late: the kernel probes the PHY at 0.16 s and the dock hangs
    by ~1 s. U-Boot is overwritten by the driver's probe.

## Decision

1. **The board runs our own kernel**, built from the Luckfox SDK (`LuckfoxTECH/luckfox-pico`) on
   the local branch **`h15-streamdock`** in `h15/luckfox/sdk`. Our patches, one commit each:
   - **H15-34:** hidraw + USB HID in the defconfig; USB host mode in the Pico Ultra device tree; the
     model string Ultra W (the Wi-Fi/BT init scripts check it); CMA 1M.
   - **H15-98:** USB PHY eye height 3'b110 in `rv1106_usb2phy_tuning`. That's one step of margin
     above the minimum, not the maximum. The kernel is #7, built 2026-10-19.
2. **Build:** rootless in the `luckfox-build` container (`h15/luckfox/container/Containerfile`,
   Ubuntu 22.04):
   ```sh
   podman run --rm --userns=keep-id --user $(id -u):$(id -g) -v $PWD/sdk:/sdk:Z luckfox-build \
     bash -c 'cd /sdk && ./build.sh kernel'
   ```
   The result is `sdk/output/image/boot.img`.
3. **Flashing only the boot partition** (`mmcblk0p4`, 32 MB) over SSH, never the whole eMMC for a
   kernel change:
   1. Back up the partition to `/root/` on the board **and** to `h15/luckfox/`. `/userdata` is only
      540 KB.
   2. Check the backup's sha256 against the previous `boot.img`.
   3. `dd … conv=fsync`, then read back and compare the sha256.
   4. Reboot. Check `uname -v`, the dock enumerating without USB errors, and `streamdockd` opening
      the dock.
4. **Rollback:** `dd if=/root/boot-p4-before-<issue>.img of=/dev/mmcblk0p4 && reboot`. If the board
   doesn't boot, the maintainer puts it into maskrom mode (boot button, USB-C to a host computer), and it's
   reflashed with `upgrade_tool`.
5. **Rules:**
   - A kernel flash needs the maintainer's decision.
   - Never write an older `boot.img` to `mmcblk0p4`: the current build is always the tip of
     `h15-streamdock`.
   - A rebuild without source changes must be byte-identical to the board's partition. Check that
     before changing anything, so you know the patch is the only difference.
6. **Fallback in userspace:** `usb-phy-tune.service` (`packaging/luckfox/`, installed by
   `install.sh`) sets the same eye height and re-enumerates USB if the dock is missing. It matters
   only on a board with a stock kernel, where the dock then needs one replug.

## Consequences

- The dock runs directly on USB-A, without a hub, and comes up by itself at boot (two reboots
  tested, 0 USB errors).
- The board depends on our kernel patches. They live in the local SDK branch `h15-streamdock` and
  as files in this repo, `packaging/luckfox/kernel/*.patch` against Luckfox's `824b817f8` (H15-98), so they're
  versioned with the code. Keep them in sync when the branch changes.
- Updating the SDK means rebasing our commits onto a newer Luckfox release, rebuilding and checking
  the USB tuning again.
- A higher eye height is outside Rockchip's default but within the PHY's range. Other USB devices
  (the hub, flash drives) keep working; this was checked with the hub.

## Alternatives

- **Keep a USB 2.0 hub** (it refreshes the HS signal): works with the stock kernel, but it clutters
  the setup (H15-98).
- **Userspace tuning only:** too late at boot (the dock hangs before userspace runs).
- **A device-tree property:** the BSP driver has none for the RV1106 eye height.
- **U-Boot `mw.l`:** the kernel's PHY probe overwrites it.
