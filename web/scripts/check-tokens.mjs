// Fails when web/src/design/tokens.ts and web/public/design/tokens.css disagree (H15-11).
// Run by `pnpm check:tokens` and as part of `pnpm build`. Needs Node with native TS imports (>= 23).
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../public/design/tokens.css", import.meta.url), "utf8");
const t = await import(new URL("../src/design/tokens.ts", import.meta.url).href);

const vars = new Map();
const root = css.slice(css.indexOf("\n:root {"), css.indexOf("\n[data-sd-accent"));
for (const m of root.matchAll(/--sd-([a-z0-9-]+):\s*([^;]+);/g)) vars.set(m[1], m[2].trim());

const errors = [];
const expect = (name, value) => {
  const actual = vars.get(name);
  if (actual === undefined) errors.push(`tokens.css lacks --sd-${name} (tokens.ts: ${value})`);
  else if (actual.toLowerCase() !== String(value).toLowerCase())
    errors.push(`--sd-${name}: tokens.css ${actual}, tokens.ts ${value}`);
};

for (const [hue, steps] of Object.entries(t.color))
  for (const [step, hex] of Object.entries(steps)) expect(`${hue}-${step}`, hex);
for (const role of t.colorRoles) if (!vars.has(`color-${role}`)) errors.push(`tokens.css lacks --sd-color-${role}`);
for (const [k, v] of Object.entries(t.space)) expect(`space-${k}`, `${v}px`);
for (const [k, v] of Object.entries(t.cut)) expect(`cut-${k}`, `${v}px`);
for (const [k, v] of Object.entries(t.fontSize)) expect(`font-size-${k}`, v);
for (const [k, v] of Object.entries(t.duration)) expect(`dur-${k}`, `${v}ms`);
expect("touch", `${t.touch}px`);
for (const a of t.accents.filter((a) => a !== "cyan"))
  if (!css.includes(`[data-sd-accent="${a}"]`)) errors.push(`tokens.css lacks the ${a} accent theme`);

if (errors.length) {
  console.error(`design tokens out of sync:\n  ${errors.join("\n  ")}`);
  process.exit(1);
}
console.log(`design tokens in sync (${vars.size} CSS custom properties)`);
