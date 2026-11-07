#!/usr/bin/env node
// Renders every 1920×1080 frame of tv/design/look-sheet.html to a PNG (one file per screen id).
//   node tv/design/render.cjs [out-dir]     (default /tmp/redline-tv-look)
'use strict';
const path = require('node:path');
const fs = require('node:fs');
const { chromium } = require('playwright');
const out = process.argv[2] || '/tmp/redline-tv-look';
(async () => {
  fs.mkdirSync(out, { recursive: true });
  const browser = await chromium.launch(process.env.LUMEN_EXTENSION_CHROMIUM ? { executablePath: process.env.LUMEN_EXTENSION_CHROMIUM } : {});
  const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
  await page.goto('file://' + path.join(__dirname, 'look-sheet.html'));
  const ids = await page.$$eval('section.screen', s => s.map(e => e.id));
  for (const [i, id] of ids.entries()) {
    const f = path.join(out, `${String(i + 1).padStart(2, '0')}-${id}.png`);
    await page.locator('section#' + id).screenshot({ path: f });
    console.log(f);
  }
  await browser.close();
})().catch(e => { console.error(e); process.exit(1); });
