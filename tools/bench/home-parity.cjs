// Live: does the shape of the Home request change YouTube's answer? In one signed-out profile
// (tools/bench/profile.cjs), without the extension, Home is asked in turns with YouTube's own context
// (ytcfg's INNERTUBE_CONTEXT, what its web app sends) and with Redline's (and variants), and the
// answers are compared: overlap of the top 24 within and across shapes, and how often YouTube's
// sticky picks appear. Talks to the real youtube.com: run by hand, rarely.
//   node tools/bench/home-parity.cjs [--n=8] [--shapes=youtube,lumen,lumen+tz]
'use strict';
const { seeded, launch, homeIds } = require('./profile.cjs');
const arg = (k, d) => { const a = process.argv.find(x => x.startsWith(`--${k}=`)); return a ? a.slice(k.length + 3) : d; };
const N = +arg('n', 8), SHAPES = arg('shapes', 'youtube,lumen,lumen+tz,lumen+client').split(',');
const overlap = (a, b) => { a = a.slice(0, 24); b = b.slice(0, 24); const B = new Set(b); return a.length && b.length ? a.filter(x => B.has(x)).length / Math.min(a.length, b.length) : NaN; };

(async () => {
  const ctx = await launch(await seeded(arg('profile', 'main')), null);
  const page = ctx.pages()[0] || await ctx.newPage();
  await page.goto('https://www.youtube.com/', { waitUntil: 'domcontentloaded' });
  await page.waitForFunction(() => window.ytcfg && ytcfg.get('INNERTUBE_CONTEXT'), null, { timeout: 30000 });
  const ask = shape => page.evaluate(async shape => {
    const yc = ytcfg.get('INNERTUBE_CONTEXT'), vd = ytcfg.get('VISITOR_DATA'), ver = ytcfg.get('INNERTUBE_CLIENT_VERSION');
    // Redline's request as src/api.rs builds it (context + headers).
    const lumen = { client: { clientName: 'WEB', clientVersion: ver, hl: 'en', platform: 'DESKTOP', originalUrl: 'https://www.youtube.com/', visitorData: vd }, user: { lockedSafetyMode: false } };
    const c = yc.client;
    const ctx = shape === 'youtube' ? yc
      : shape === 'lumen' ? lumen
      : shape === 'lumen+tz' ? { ...lumen, client: { ...lumen.client, timeZone: c.timeZone, utcOffsetMinutes: c.utcOffsetMinutes } }
      : shape === 'lumen+client' ? { ...lumen, client: { ...lumen.client, gl: c.gl, timeZone: c.timeZone, utcOffsetMinutes: c.utcOffsetMinutes, userAgent: c.userAgent, browserName: c.browserName, browserVersion: c.browserVersion, osName: c.osName, osVersion: c.osVersion, deviceMake: c.deviceMake, screenWidthPoints: innerWidth, screenHeightPoints: innerHeight, windowWidthPoints: innerWidth, screenPixelDensity: 1, screenDensityFloat: 1, userInterfaceTheme: c.userInterfaceTheme, clientFormFactor: c.clientFormFactor, mainAppWebInfo: { graftUrl: '/', webDisplayMode: 'WEB_DISPLAY_MODE_BROWSER', isWebNativeShareAvailable: true } } }
      : shape === 'yt-noconfig' ? { ...yc, client: { ...c, configInfo: undefined, deviceExperimentId: undefined, rolloutToken: undefined } }
      : shape === 'yt-notrack' ? { ...yc, clickTracking: undefined, adSignalsInfo: undefined, request: undefined }
      : shape.startsWith('lumen+G') ? (() => {
        const G = { G1: ['remoteHost', 'gl'], G2: ['userAgent', 'browserName', 'browserVersion', 'osName', 'osVersion', 'deviceMake', 'deviceModel', 'acceptHeader', 'memoryTotalKbytes'],
          G3: ['screenWidthPoints', 'screenHeightPoints', 'windowWidthPoints', 'screenDensityFloat', 'screenPixelDensity', 'clientFormFactor', 'userInterfaceTheme', 'applicationState', 'mainAppWebInfo'],
          G4: ['configInfo', 'deviceExperimentId', 'rolloutToken', 'timeZone', 'utcOffsetMinutes'] };
        const add = {}; for (const g of shape.slice(6).split('+')) for (const k of G[g]) if (c[k] !== undefined) add[k] = c[k];
        return { ...lumen, client: { ...lumen.client, ...add } };
      })()
      : shape === 'yt-lumenheaders' ? yc
      : shape === 'lumen+yt-client' ? { ...lumen, client: { ...c } }
      : shape === 'lumen+config' ? { ...lumen, client: { ...lumen.client, configInfo: c.configInfo, deviceExperimentId: c.deviceExperimentId, rolloutToken: c.rolloutToken } }
      : null;
    const headers = shape.startsWith('yt') && shape !== 'yt-lumenheaders'
      ? { 'Content-Type': 'application/json', 'X-Youtube-Client-Name': '1', 'X-Youtube-Client-Version': ver, 'X-Goog-Visitor-Id': vd, 'X-Youtube-Bootstrap-Logged-In': 'false' }
      : { 'Content-Type': 'application/json', 'X-Youtube-Client-Name': '1', 'X-Youtube-Client-Version': ver, 'X-Origin': 'https://www.youtube.com', 'X-Goog-AuthUser': '0', 'X-Goog-Visitor-Id': vd };
    const r = await fetch('/youtubei/v1/browse?prettyPrint=false', { method: 'POST', credentials: 'include', headers, body: JSON.stringify({ context: ctx, browseId: 'FEwhat_to_watch' }) });
    return r.json();
  }, shape).then(homeIds);
  await page.evaluate(() => window.stop());
  const got = Object.fromEntries(SHAPES.map(s => [s, []]));
  for (let i = 0; i < N; i++) for (const s of SHAPES) { got[s].push(await ask(s)); await page.waitForTimeout(800); }
  const mean = p => p.length ? Math.round(p.reduce((a, b) => a + b, 0) / p.length * 100) + ' %' : 'n/a';
  const within = A => { const p = []; for (let i = 0; i < A.length; i++) for (let j = i + 1; j < A.length; j++) p.push(overlap(A[i], A[j])); return p; };
  const cross = (A, B) => { const p = []; for (const a of A) for (const b of B) p.push(overlap(a, b)); return p; };
  const Y = got.youtube || got[SHAPES[0]];
  // Sticky picks from the even YouTube-context answers; every shape is scored on answers that did not
  // choose them (YouTube's own on its odd answers), so selection does not flatter it.
  const pick = Y.filter((_, i) => i % 2 === 0), held = { youtube: Y.filter((_, i) => i % 2 === 1) };
  const cy = new Map(); for (const ids of pick) for (const id of ids.slice(0, 24)) cy.set(id, (cy.get(id) || 0) + 1);
  const sticky = [...cy].filter(([, n]) => n >= 0.75 * pick.length).map(([id]) => id);
  console.log(`n=${N} per shape; top 24; sticky = in ≥ 75 % of the even YouTube-context answers (${sticky.length}), scored on the others`);
  for (const s of SHAPES) {
    const A = held[s] || got[s];
    const freq = sticky.map(id => Math.round(A.filter(ids => ids.slice(0, 24).includes(id)).length / A.length * 100));
    console.log(`${s.padEnd(13)} items ${Math.round(got[s].reduce((a, x) => a + x.length, 0) / got[s].length)} · within ${mean(within(got[s]))} · × youtube ${s === 'youtube' ? '-' : mean(cross(got[s], Y))} · sticky ${freq.join('/')} (mean ${freq.length ? Math.round(freq.reduce((a, b) => a + b, 0) / freq.length) : '-'} %)`);
  }
  await ctx.close();
})().catch(e => { console.error(e); process.exit(1); });
