#!/usr/bin/env node
// Renders the extension's icons (extension/icons/16.png, 32, 48, 128; manifest.json "icons" and the toolbar's
// "default_icon") from the one source of the mark, browser/icons/redline.svg, with Playwright's Chromium.
//   node browser/icons.cjs
// Run it after changing the mark; the PNGs are committed (installing needs no build). The mark is 4:3: it fills
// the width of the small toolbar sizes (16, 32: the even-pixel geometry keeps its edges crisp there) and keeps
// Chrome's recommended transparent margin at 48 and 128 (the 128 icon's art is 96 px wide).
// LUMEN_EXTENSION_CHROMIUM, if set, is the Chromium executable to use.
'use strict';
const fs = require('node:fs'), path = require('node:path');
const { chromium } = require('playwright');

const svg = fs.readFileSync(path.join(__dirname, 'icons', 'redline.svg'), 'utf8').replace(/<!--[\s\S]*?-->/g, '');
const out = path.join(__dirname, '..', 'extension', 'icons');
// size → transparent margin on the left and right (px)
const SIZES = [[16, 0], [32, 0], [48, 4], [128, 16]];

(async () => {
  fs.mkdirSync(out, { recursive: true });
  const browser = await chromium.launch(process.env.LUMEN_EXTENSION_CHROMIUM ? { executablePath: process.env.LUMEN_EXTENSION_CHROMIUM } : {});
  for (const [size, pad] of SIZES) {
    const w = size - 2 * pad, h = w * 24 / 32;
    const art = svg.replace(/ width="\d+" height="\d+"/, ` width="${w}" height="${h}"`);
    const page = await browser.newPage({ viewport: { width: size, height: size }, deviceScaleFactor: 1 });
    await page.setContent(`<!doctype html><style>html,body{margin:0;width:${size}px;height:${size}px;background:transparent;overflow:hidden}
svg{position:absolute;left:${pad}px;top:${(size - h) / 2}px;display:block}</style>${art}`);
    const f = path.join(out, `${size}.png`);
    await page.screenshot({ path: f, omitBackground: true });
    await page.close();
    console.log(f);
  }
  await browser.close();
})().catch(e => { console.error(e); process.exit(1); });
