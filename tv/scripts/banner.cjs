#!/usr/bin/env node
// Renders Redline for TV's launcher art from HTML with Playwright's Chromium.
//
//   node tv/scripts/banner.cjs [name]           banner only (default name "Redline")
//   node tv/scripts/banner.cjs --icons          launcher icon PNGs only
//   node tv/scripts/banner.cjs --all [name]     both
//
// The mark comes from its one source, browser/icons/redline.svg (a red parallelogram with a chevron cut out).
//
// Banner: 320×180 dp, written at xhdpi (640×360 px) to res/drawable-xhdpi/banner.png: the red mark and the white
// wordmark beside it, centred as a group on graphite with everything ≥ 10 % inside each edge (launchers crop and
// round banners).
//
// Icons (all under tv/shell/src-tauri/): the legacy squircle and round PNGs (mipmap-*/ic_launcher.png,
// ic_launcher_round.png, 48 dp: the red mark on a graphite tile), the adaptive-icon foreground PNGs
// (mipmap-*/ic_launcher_foreground.png, 108 dp, the mark alone on transparent; the graphite comes from
// @color/ic_launcher_background) and icons/icon.png (512 px). The adaptive icon itself uses the vector
// drawable-v24/ic_launcher_foreground.xml, which holds the same path at the same place (scaled 1.875×, centred on
// the 108 dp canvas, inside the 66 dp safe zone): keep it in step with the SVG.
//
// LUMEN_EXTENSION_CHROMIUM, if set, is the Chromium executable to use.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');

const args = process.argv.slice(2);
const flags = new Set(args.filter(a => a.startsWith('--')));
const name = args.find(a => !a.startsWith('--')) || 'Redline';
const doBanner = !flags.has('--icons') || flags.has('--all');
const doIcons = flags.has('--icons') || flags.has('--all');

const tauri = path.join(__dirname, '..', 'shell', 'src-tauri');
const res = path.join(tauri, 'gen', 'android', 'app', 'src', 'main', 'res');

// The palette: graphite surfaces (client.css --bg / --pn), white text, the brand red (in the SVG).
const BG = '#0f0f0f', TILE = '#181818', TEXT = '#f1f1f1';
const SVG = fs.readFileSync(path.join(__dirname, '..', '..', 'browser', 'icons', 'redline.svg'), 'utf8').replace(/<!--[\s\S]*?-->/g, '');
const D = /\sd="([^"]+)"/.exec(SVG)[1];
// The mark (viewBox 0 0 32 24) as a group placed by `transform`.
const GLYPH = (t = '') => `<path fill="#ff0033" fill-rule="evenodd" transform="${t}" d="${D}"/>`;

const esc = s => s.replace(/[&<>"]/g, c => `&#${c.charCodeAt(0)};`);
const page0 = (w, h, body, css = '') => `<!doctype html><meta charset="utf-8"><style>
html,body{margin:0;width:${w}px;height:${h}px;overflow:hidden;background:transparent}
svg{display:block}${css}</style>${body}`;

// The wordmark is system-ui (SF on macOS; Inter and "SF Pro Display" are not installed and would fall
// back to Times in Playwright's Chromium). No "TV" label: it vanished at launcher-row size.
const banner = page0(640, 360, `<div class="b"><svg viewBox="0 0 32 24" width="152" height="114">${GLYPH()}</svg><div class="w">${esc(name)}</div></div>`, `
html,body{background:${BG}}
body{display:flex;align-items:center;justify-content:center}
.b{display:flex;align-items:center;gap:26px}
.w{color:${TEXT};font:700 92px/1 system-ui,"Helvetica Neue",Arial,sans-serif;letter-spacing:-.035em;padding-bottom:6px}`);

// Icon pages, each `size` px square. On the 48-unit tile the mark is 36 units wide (scale 1.125, centred).
const TILE_MARK = GLYPH('translate(6 10.5) scale(1.125)');
const icon = (size, kind) => {
  if (kind === 'squircle') return page0(size, size, `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 48 48"><rect width="48" height="48" rx="14" fill="${TILE}"/>${TILE_MARK}</svg>`);
  if (kind === 'round') return page0(size, size, `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 48 48"><circle cx="24" cy="24" r="24" fill="${TILE}"/>${TILE_MARK}</svg>`);
  // adaptive foreground: the 108 dp canvas, the mark scaled 1.875× and centred (as the vector drawable)
  return page0(size, size, `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 108 108">${GLYPH('translate(24 31.5) scale(1.875)')}</svg>`);
};

(async () => {
  const browser = await chromium.launch(process.env.LUMEN_EXTENSION_CHROMIUM ? { executablePath: process.env.LUMEN_EXTENSION_CHROMIUM } : {});
  const shot = async (w, h, html, out, transparent) => {
    const page = await browser.newPage({ viewport: { width: w, height: h } });
    await page.setContent(html);
    await page.screenshot({ path: out, omitBackground: !!transparent });
    await page.close();
    console.log(out);
  };
  if (doBanner) await shot(640, 360, banner, path.join(res, 'drawable-xhdpi', 'banner.png'));
  if (doIcons) {
    for (const [d, s] of [['mdpi', 48], ['hdpi', 72], ['xhdpi', 96], ['xxhdpi', 144], ['xxxhdpi', 192]]) {
      const dir = path.join(res, `mipmap-${d}`);
      await shot(s, s, icon(s, 'squircle'), path.join(dir, 'ic_launcher.png'), true);
      await shot(s, s, icon(s, 'round'), path.join(dir, 'ic_launcher_round.png'), true);
      const f = s * 108 / 48;
      await shot(f, f, icon(f, 'foreground'), path.join(dir, 'ic_launcher_foreground.png'), true);
    }
    await shot(512, 512, icon(512, 'squircle'), path.join(tauri, 'icons', 'icon.png'), true);
  }
  await browser.close();
})().catch(e => { console.error(e); process.exit(1); });
