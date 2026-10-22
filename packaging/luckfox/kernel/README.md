# Luckfox kernel patches (ADR-004)

The production Luckfox Pico Ultra W runs our own kernel. These patches are the whole difference to
Luckfox's SDK [`LuckfoxTECH/luckfox-pico`](https://github.com/LuckfoxTECH/luckfox-pico) at commit
`824b817f8` ("Pullrequest 260319 (#355)"). The working copy is the local branch `h15-streamdock` in
`h15/luckfox/sdk`; the files here are its backup in git.

| Patch | What | Issue |
|---|---|---|
| `0001-luckfox-pico-ultra-streamdock-board.patch` | hidraw + USB HID, USB host mode, model "Ultra W", CMA 1M | H15-34 |
| `0002-rv1106-usb2phy-eye-height.patch` | USB PHY HS eye height 3'b110: the dock works directly on USB-A | H15-98 |

Recreate the SDK tree, build and flash as in [ADR-004](../../../docs/adr/0004-luckfox-board-kernel.md):

```sh
git clone https://github.com/LuckfoxTECH/luckfox-pico sdk && cd sdk && git checkout 824b817f8
git apply ../streamdock/packaging/luckfox/kernel/*.patch
```

Change a patch here whenever `h15-streamdock` changes (`git diff 824b817f8 h15-streamdock -- <paths>`).
