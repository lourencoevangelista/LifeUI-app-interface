// WCAG contrast check for the colour pairs that matter in web/style.css. Run: node scripts/contrast.mjs
// Reads the HSL tokens straight from the stylesheet (light = :root, dark = the prefers-color-scheme block),
// so a palette change is checked against the real values. Text needs 4.5:1 (3:1 for large or bold UI text).
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const css = readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "web", "style.css"), "utf8");
const darkStart = css.indexOf("@media (prefers-color-scheme: dark)");
const blocks = { light: css.slice(css.indexOf(":root {"), darkStart), dark: css.slice(darkStart, css.indexOf("*, *::before")) };

function tokens(block) {
  const out = {};
  for (const m of block.matchAll(/--([a-z-]+):\s*([\d.]+)\s+([\d.]+)%\s+([\d.]+)%/g)) out[m[1]] = [Number(m[2]), Number(m[3]), Number(m[4])];
  return out;
}
function rgb([h, s, l]) {
  s /= 100; l /= 100;
  const k = (n) => (n + h / 30) % 12, a = s * Math.min(l, 1 - l);
  return [0, 8, 4].map((n) => 255 * (l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)))));
}
const lum = (c) => { const [r, g, b] = rgb(c).map((v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
// over(top, alpha, bottom): the colour of `top` at `alpha` painted over `bottom`
const over = (top, alpha, bottom) => { const t = rgb(top), b = rgb(bottom); const m = t.map((v, i) => v * alpha + b[i] * (1 - alpha)); return rgbToHsl(m); };
function rgbToHsl([r, g, b]) {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b), l = (max + min) / 2, d = max - min;
  if (!d) return [0, 0, l * 100];
  const s = d / (1 - Math.abs(2 * l - 1));
  const h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [(h * 60 + 360) % 360, s * 100, l * 100];
}

let failed = 0;
for (const [scheme, block] of Object.entries(blocks)) {
  const t = { ...tokens(blocks.light), ...(scheme === "dark" ? tokens(block) : {}) };
  const white = [0, 0, 100];
  const pairs = [
    ["body text on page", t.foreground, t.background, 4.5],
    ["body text on card", t.foreground, t.card, 4.5],
    ["muted text on page", t["muted-foreground"], t.background, 4.5],
    ["muted text on card", t["muted-foreground"], t.card, 4.5],
    ["button text on primary (terracotta)", t["primary-foreground"], t.primary, 4.5],
    ["white on the terracotta add button", white, t.primary, 4.5],
    ["white on the olive done tick", white, t.green, 4.5],
    ["link / active tab (ring) on page", t.ring, t.background, 4.5],
    ["link / active tab (ring) on card", t.ring, t.card, 4.5],
    ["error text (bad) on page", t.bad, t.background, 4.5],
    ["error text (bad) on its tinted box", t.bad, over(t.red, 0.1, t.card), 4.5],
    ["Secret badge (bad) on its tint", t.bad, over(t.bad, 0.13, t.card), 4.5],
    ["Public badge (ok) on its tint", t.ok, over(t.ok, 0.13, t.card), 4.5],
    ["Sensitive badge (ring) on its tint", t.ring, over(t.primary, 0.16, t.card), 4.5],
    ["secondary button text", t["secondary-foreground"], t.secondary, 4.5],
    ["terracotta pop (active tab bar) on page [graphic]", t.primary, t.background, 3],
    ["red due-date dot on page [graphic]", t.red, t.background, 3],
    ["terracotta event dot on card [graphic]", t.primary, t.card, 3],
    ["input border on page [graphic]", t.input, t.background, 1.5],
  ];
  console.log(`\n${scheme.toUpperCase()}`);
  for (const [name, fg, bg, need] of pairs) {
    const r = ratio(fg, bg);
    const ok = r >= need;
    if (!ok) failed++;
    console.log(`${ok ? "ok  " : "LOW "} ${r.toFixed(2).padStart(5)}:1  (needs ${need})  ${name}`);
  }
}
console.log(failed ? `\n${failed} pair(s) below the target` : "\nall pairs meet their target");
process.exit(failed ? 1 : 0);
