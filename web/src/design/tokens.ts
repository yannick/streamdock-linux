// TS mirror of web/public/design/tokens.css (H15-11). Use it for values that JS needs (breakpoints,
// durations, colors for canvas drawing) and `cssVar()` to reference a token in inline styles.
// `pnpm check:tokens` fails the build when this file and tokens.css disagree.

export const color = {
  cyan: {
    50: "#e8fdff",
    100: "#b9f6fb",
    300: "#6ff0f7",
    400: "#2ee6f2",
    500: "#10c4d6",
    600: "#0b93a3",
    700: "#0d6773",
    800: "#0c3f48",
    900: "#082a31",
  },
  green: {
    300: "#8dffbf",
    400: "#3bff95",
    500: "#1fd977",
    600: "#12a95b",
    700: "#0e6f3f",
    800: "#0b3d25",
  },
  blue: { 400: "#4f8dff", 600: "#2756c4", 800: "#142a5c" },
  lime: { 400: "#d4ff3a" },
  ice: { 100: "#eafaff" },
  ink: {
    950: "#02070a",
    900: "#050f14",
    850: "#08161c",
    800: "#0c1f27",
    700: "#132d36",
    600: "#1d404b",
    500: "#2c5c69",
    400: "#5f98a3",
    300: "#7fb2ba",
  },
} as const;

/** Semantic color roles (`--sd-color-<role>`). Values come from CSS so accent themes apply. */
export const colorRoles = [
  "accent",
  "accent-strong",
  "accent-muted",
  "accent-faint",
  "on-accent",
  "accent-2",
  "bg",
  "surface",
  "surface-raised",
  "surface-sunken",
  "hover",
  "line",
  "line-strong",
  "line-faint",
  "grid",
  "text",
  "text-strong",
  "text-muted",
  "text-dim",
  "label",
  "success",
  "warning",
  "critical",
  "info",
  "focus",
] as const;
export type ColorRole = (typeof colorRoles)[number];

export const space = { 1: 4, 2: 8, 3: 12, 4: 16, 5: 24, 6: 32, 7: 48, 8: 64 } as const;
export const cut = { sm: 6, md: 12, lg: 20 } as const;
export const fontSize = {
  xs: "0.6875rem",
  sm: "0.8125rem",
  md: "0.9375rem",
  lg: "1.125rem",
  xl: "1.5rem",
  "2xl": "2.25rem",
  "3xl": "3.25rem",
} as const;
export const duration = { fast: 90, base: 160, slow: 320 } as const;
export const touch = 44;
/** Media query breakpoints in px (min-width). */
export const breakpoint = { sm: 480, md: 768, lg: 1080 } as const;

/** Accent themes, set with `data-sd-accent` on any ancestor (cyan is the default). */
export const accents = ["cyan", "green", "blue"] as const;
export type Accent = (typeof accents)[number];

/** `var(--sd-…)` for a token name without the prefix, e.g. `cssVar("color-accent")`. */
export function cssVar(name: string): string {
  return `var(--sd-${name})`;
}

/** Media query string for a breakpoint, e.g. `useMedia(mq("md"))`. */
export function mq(bp: keyof typeof breakpoint): string {
  return `(min-width: ${breakpoint[bp]}px)`;
}
