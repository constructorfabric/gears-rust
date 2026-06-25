// make-title-card.mjs — render a full-screen demo title card to a PNG via
// headless Chromium. This repo's ffmpeg has no drawtext filter and there's no
// ImageMagick/librsvg, so Chromium (already present for Playwright) is the
// portable text renderer for the cross-site compositor's act dividers.
//
//   node .agents/skills/kitsoki-ui-demo/scripts/make-title-card.mjs \
//     <out.png> <title> [subtitle] [kicker] [accent]
//
// ESM resolves bare specifiers from the MODULE's directory, not cwd, and
// @playwright/test lives in tools/runstatus/node_modules — so resolve it
// explicitly from there via createRequire (works regardless of where node runs).
import { createRequire } from "module";
import { fileURLToPath } from "url";
import path from "path";
const here = path.dirname(fileURLToPath(import.meta.url));
const runstatus = path.resolve(here, "../../../..", "tools", "runstatus");
const require = createRequire(path.join(runstatus, "package.json"));
const { chromium } = require("@playwright/test");

const [out, title, subtitle = "", kicker = "", accent = "#fbbf24"] = process.argv.slice(2);
if (!out || !title) {
  console.error("usage: make-title-card.mjs <out.png> <title> [subtitle] [kicker] [accent]");
  process.exit(2);
}

const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" }[c]));
const html = `<!doctype html><meta charset="utf-8"><body style="margin:0">
<div style="width:1600px;height:900px;background:#070d1a;display:flex;flex-direction:column;
  align-items:center;justify-content:center;font-family:ui-sans-serif,system-ui,-apple-system,sans-serif;
  color:#e2e8f0;text-align:center;gap:20px">
  ${kicker ? `<div style="color:${esc(accent)};font:700 26px/1 ui-sans-serif;letter-spacing:.28em;text-transform:uppercase">${esc(kicker)}</div>` : ""}
  <div style="font:800 64px/1.12 ui-sans-serif;max-width:1200px;letter-spacing:-.01em">${esc(title)}</div>
  ${subtitle ? `<div style="font:400 28px/1.4 ui-sans-serif;color:#94a3b8;max-width:1040px">${esc(subtitle)}</div>` : ""}
  <div style="margin-top:14px;width:120px;height:5px;border-radius:3px;background:${esc(accent)}"></div>
</div></body>`;

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1600, height: 900 }, deviceScaleFactor: 2 });
await page.setContent(html, { waitUntil: "load" });
await page.screenshot({ path: out });
await browser.close();
console.log(`make-title-card: wrote ${out}`);
