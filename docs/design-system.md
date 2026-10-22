# streamdock design system

The look of every streamdock UI: Cyberpunk 2077 menus (H15-11 has the reference screenshots), in
**blue and green only, no red**. It is built from tokens so it can be refined in one place and reused
by other frontends (for example the ESP32 spin-off's web UI).

- **Live style guide:** `/design` on a running daemon (every token and component, with an accent switcher).
- **Tokens:** `web/public/design/tokens.css`, mirrored in `web/src/design/tokens.ts`.
- **Components:** `web/public/design/components.css`, with React wrappers in `web/src/design/components.tsx`
  (`import { Panel, Button, … } from "../design/index.js"`).
- **Page theme:** `web/public/theme.css` imports both and styles the proa shell; `dashboard.css` lays out the
  dashboard with the same tokens.

Everything is plain CSS custom properties plus classes, so it also works without React: copy
`web/public/design/` and use the class names below.

## Principles

1. **One hue at a time.** A screen uses the accent (cyan by default) for everything interactive and
   `accent-2` (green) for secondary highlights. Status colors are the only other colors.
2. **No red, ever.** Critical states are bright ice white with a pulse (and a striped hazard fill where
   there is room). Warning is acid lime. Never introduce red, orange or magenta tokens.
   **The one exception** (H15-97): the colour temperature bar on a light key runs amber →
   warm white → cold blue, because it previews the light's own colour. It lives only in
   `render.rs` (`WARM`, `NEUTRAL`, `COLD`), is not a token, and never marks a state or a warning.
3. **Cut corners, no radii.** Frames are chamfered (top-right and bottom-left), lines are 1 px, and glow is
   reserved for active or focused things.
4. **Uppercase, tracked condensed type** for labels, headings and values. Mono for numbers and IDs.
5. **Mobile first.** One column below 768 px, every control at least 44 px high (`--sd-touch`), no
   horizontal page scroll (only tab strips scroll).
6. **Motion is short and optional.** 90 to 320 ms; all durations become 0 with `prefers-reduced-motion`.

## Tokens

Names are `--sd-<group>-<name>`. Components only use **semantic** tokens; primitives exist to define them.

| Group | Tokens | Notes |
|---|---|---|
| Primitives | `--sd-cyan-50…900`, `--sd-green-300…800`, `--sd-blue-400/600/800`, `--sd-lime-400`, `--sd-ice-100`, `--sd-ink-300…950` | ink = the dark blue-green neutrals |
| Color roles | `--sd-color-accent`, `-accent-strong`, `-accent-muted`, `-accent-faint`, `-on-accent`, `-accent-2`, `-bg`, `-surface`, `-surface-raised`, `-surface-sunken`, `-hover`, `-line`, `-line-strong`, `-line-faint`, `-grid`, `-text`, `-text-strong`, `-text-muted`, `-text-dim`, `-label`, `-success`, `-warning`, `-critical`, `-info`, `-focus` | retheme here |
| Type | `--sd-font-display` (SD Condensed = Fira Sans Condensed), `--sd-font-mono`, `--sd-font-size-xs…3xl`, `--sd-font-weight-*`, `--sd-tracking-normal/wide/wider`, `--sd-line-height` | |
| Space | `--sd-space-1…8` = 4, 8, 12, 16, 24, 32, 48, 64 px | 4 px grid |
| Shape | `--sd-cut-sm/md/lg` = 6, 12, 20 px, `--sd-border`, `--sd-border-strong`, `--sd-touch` (44 px) | |
| Effects | `--sd-glow-sm`, `--sd-glow-md`, `--sd-glow-text`, `--sd-scanline-opacity` | |
| Motion | `--sd-dur-fast/base/slow` = 90, 160, 320 ms, `--sd-ease`, `--sd-ease-step` | |
| Layout | `--sd-container` (1080 px), `--sd-z-header`, `--sd-z-overlay`; breakpoints sm 480, md 768, lg 1080 px | breakpoints are in `tokens.ts` (`mq("md")`); CSS media queries can't read custom properties |

**Accent themes:** `data-sd-accent="green"` or `"blue"` on any element (usually `<html>`) swaps the accent
roles; cyan is the default.

**Keeping CSS and TS in sync:** `pnpm check:tokens` (also part of `pnpm build`) compares `tokens.ts` with
`tokens.css` and fails on any difference. Change both together.

## Components

| Component | Classes | React | From the references |
|---|---|---|---|
| Frame | `.sd-frame` (+ `--sd-cut`, `--sd-frame-line`, `--sd-frame-fill`) | (base of the others) | every box |
| Panel | `.sd-panel`, `--accent`, `__header`, `__header-aside` | `Panel` | settings / briefing panels with barcode header |
| Section title | `.sd-section-title` | `SectionTitle` | "Performance", "Miscellaneous" |
| Tabs | `.sd-tabs`, `__tab[aria-selected]` | `Tabs` | top menu with `[1] … [3]` |
| Keycap | `.sd-keycap` | `Keycap` | `[ESC]`, `[F1]`, `[X]` hints |
| Button | `.sd-button`, `--primary`, `--ghost`, `--sm` | `Button` | DEFAULTS, GAMMA CORRECTION |
| Menu list | `.sd-list`, `__row[data-selected]`, `__label`, `__control` | `MenuList`, `MenuRow` | settings rows, quickhack list |
| Toggle | `.sd-toggle[aria-checked]`, `__off`, `__on` | `Toggle` | OFF / ON segmented switch |
| Stepper | `.sd-stepper`, `__arrow`, `__value` | `Stepper` | ◁ High ▷ |
| Slider | `.sd-slider` (+ `--sd-value`) | `Slider` (`onChange` while dragging, `onCommit` once on pointer/key up) | |
| XY pad | `.sd-xypad` (+ `--sd-x`, `--sd-y`) | `XYPad` (2D −1..1, labels at the four edges, `onChange` / `onCommit`, arrow keys) | sound character (H15-54) |
| Band table | `.sd-bands` (table of number inputs) | — (plain `<table>`) | correction EQ bands (H15-54) |
| Badge | `.sd-badge[data-tone]` (success, warning, critical, info, muted) | `Badge` | EQUIPPED, LEGENDARY |
| Readout | `.sd-readout`, `__value`, `__unit`, `--sm/--lg` | `Readout` | 27 LEVEL, 003 ammo |
| Meter | `.sd-meter[data-tone]` (+ `--sd-value` 0..1), `__fill` | `Meter` | health bar |
| Now playing | `.sd-nowplaying[data-state]` (playing / paused / idle / offline), `--slice` (+ `--sd-np-count`, `--sd-np-index`), `__band`, `__cover`, `__text` (`.is-cut`), `__progress`, `__bar` | `NowPlaying` (`marqueeOffset`) | the current track: one line that scrolls when too long (requestAnimationFrame, the dock's timing; cut with "…" under reduced motion), cover behind, progress bar; `slice` draws a tile's part of a band (H15-88) |
| Data list | `.sd-datalist`, `--inline`, `__item` | `DataList` | briefing card fields |
| Hint bar | `.sd-hints`, `__item` | `HintBar` | `[ESC] Close  [F1] Restore Defaults` |
| Disclosure | `details.sd-disclosure[open]`, `__summary`, `__title`, `__aside`, `__body` | `Disclosure` (`title`, `aside`, `open`: initially open) | foldable sections of a long dialog (Bluetooth details, H15-53) |
| Key tile | `.sd-keytile[data-state="pressed"]`, `--button`, `__number`, `__image`, `__flash` | `KeyTile` (`pulse`: change it to replay the release flash) | the dock's LCD keys and buttons |
| Knob | `.sd-knob[data-state="pressed"]` or `.is-pressed`, `__grip`, `__dial` (+ `.is-cw/.is-ccw`), `__indicator`, `__label`, `__pos`; `--control`, `__steps`, `__step` | `Knob` (`turn` −1/0/1 plays the turn flash, `pulse` replays it per detent; `onTurn(detents)` + `onPress()` make it a control: drag around the dial, wheel, arrow keys, ‹ › buttons, click/Enter presses) | the dock's knobs; the dashboard's dock (H15-51) |
| Text input | `.sd-input` (also `<select>`) | | |
| Context menu | `.sd-context-menu`, `__title`, `__item`, `__hint` | `ContextMenu` + `useContextMenuTrigger(open)` (right click, and a 500 ms long press on touch) | right-click menus (H15-32) |
| Dialog | `.sd-dialog` on `<dialog>`, `__body`, `__actions` | `Dialog` (native modal: focus trap, Escape, backdrop click) | |
| Inline edit | `.sd-inline-edit`, `__input` | `InlineEdit` (double click to edit, Enter saves, Escape cancels) | renaming tabs |
| Picker | `.sd-picker`, `__item[aria-selected]`, `__meta` | | searchable choice list in a dialog |
| Tab add button | `.sd-tabs__add` (sticky at the end of a scrolling tab strip) | | |
| Effects | `.sd-glitch[data-text]`, `.sd-hazard`, `.sd-flash` | `Glitch` | |
| Layout | `.sd-app`, `.sd-container`, `.sd-stack`, `.sd-cluster`, `.sd-grid` (+ `--sd-gap`, `--sd-grid-min`), `.sd-visually-hidden` | | |

State goes through ARIA or data attributes (`aria-checked`, `aria-selected`, `aria-pressed`,
`data-state`, `data-tone`), so React and plain markup style the same way.

## Per-event animation

Live UIs animate each event, even when the same one repeats (three clockwise detents in a row). The
components take a **`pulse`** number for that: pass anything that changes per event, for example an event
counter. The component uses it as a React `key` on the animated element, so each new value replays the
animation without remounting the control (focus stays put).

- `KeyTile pulse={n}`: green release flash over the tile.
- `Knob turn={1 | -1} pulse={n}`: clockwise or counter-clockwise flash on the dial. The dial sits in
  `__grip`, which keeps a drag's pointer capture while the dial remounts.
- `Slider onCommit={send}`: fires once when the user lets go, so a command isn't sent on every pixel.

## Contrast

Body text colors pass WCAG AA (4.5:1) on `bg`, `surface` and `surface-raised`: `text` 14:1, `text-muted`
7:1, `text-dim` 5.2:1 or more (`ink-400` was raised to `#5f98a3` for this), the accent and status colors
5:1 or more. `accent-muted` (about 2.8:1) is for lines and fills only, never for text.

## Fonts

`SD Condensed` is **Fira Sans Condensed** (SIL OFL 1.1, license in `web/public/design/fonts/OFL.txt`),
subset to Latin and arrows and self-hosted as WOFF2 (about 20 KB per weight; 400, 500 and 600). No font
CDN. Regenerate with:

```sh
uvx --from 'fonttools[woff]' pyftsubset /usr/share/fonts/TTF/FiraSansCondensed-Medium.ttf \
  --unicodes="U+0020-007E,U+00A0-00FF,U+2013-2014,U+2018-201D,U+2022,U+2026,U+2190-2193,U+25B2-25C1,U+2212,U+00B0" \
  --layout-features='kern,liga,tnum,lnum' --flavor=woff2 --output-file=fira-sans-condensed-medium.woff2
```

## Changing the design

- Retheme: change semantic roles in `tokens.css` (and `tokens.ts` if you touch primitives or scales).
- New component: add CSS to `components.css` using only tokens, a wrapper in `components.tsx`, a demo in
  `web/src/islands/DesignSystem.tsx`, and a row in the table above.
- Check `/design` at desktop width and at a phone width (390 px) before merging.

## Network settings

Ethernet and Wi-Fi list every IPv4/IPv6 address in `.sd-addresses`, with wrapping mono text and
muted scope badges: global, link-local, private (IPv4), unique-local (IPv6), or loopback.
Temporary/privacy and stable-privacy badges use the kernel's per-address flags; address spelling
alone never implies privacy. Both links show their IPv4/IPv6 gateways and DNS.

The Ethernet form keeps IPv4 and IPv6 DNS separate. Static IPv6 address, prefix, gateway and DNS
read back from the configured profile, rather than from temporary or router-learned values.
Trying only one family preserves the complete other-family profile. The existing “Try this
setting” / “Keep” actions retain the 60-second automatic rollback; a failed save keeps rollback
armed. Live readouts come from `WatchEvents`; only the local countdown uses a UI timer.
