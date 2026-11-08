'use strict';
const assert = require('node:assert/strict');

module.exports = async ({ step, tvOpen, tvClose, tvGo, tvAudit, TVSIZES }) => {
  await step('TV motion: remote focus keeps card geometry steady, rail opens immediately, and delayed watch content never starts a desktop entrance', async () => {
    const tv = await tvOpen();
    try {
      for (const [width, height] of TVSIZES) {
        await tv.setViewportSize({ width, height });
        for (const reducedMotion of ['no-preference', 'reduce']) {
          await tv.emulateMedia({ reducedMotion });
          await tvGo(tv, 'code', 'https://www.youtube.com/results?search_query=tv+motion');
          await tv.waitForSelector('#ytc-list .c a.t');
          await tv.focus('#ytc-list .c:first-child a.t');
          const box = () => tv.evaluate(() => {
            const card = document.querySelector('#ytc-list .c:nth-child(2)');
            const r = card.getBoundingClientRect();
            return [r.x, r.y, r.width, r.height];
          });
          // (After the first rendered frame: until then a card in view still has its content-visibility placeholder size.)
          await tv.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
          const before = await box();
          await tv.keyboard.press('ArrowRight');
          assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-list .c:nth-child(2) a.t')), 'Right focuses the next card');
          assert.deepEqual(await box(), before, 'focus does not change card geometry on the input frame');
          await tv.waitForTimeout(240);
          assert.deepEqual(await box(), before, 'focus does not zoom after the input frame');
          assert.deepEqual(await tvAudit(tv, `steady focus ${width} ${reducedMotion}`), []);
          await tv.focus('#ytc-side .rl[data-nav=home]');
          assert.equal(await tv.evaluate(() => getComputedStyle(document.querySelector('#ytc-side .rl[data-nav=home] span')).opacity), '1', 'rail labels are immediately visible');
          assert.equal(await tv.evaluate(() => document.getElementById('ytc-side').getAnimations({ subtree: true }).filter(a => a.playState === 'running').length), 0, 'rail does not animate under remote focus');

          // Delay next beyond the first-frame motion gate. A TV must not inherit the
          // desktop's asynchronous stagger/fade once its metadata eventually lands.
          await tv.route('**/youtubei/v1/next?*', async route => {
            if (!route.request().postDataJSON()?.continuation) await new Promise(resolve => setTimeout(resolve, 500));
            await route.fallback();
          });
          await tv.focus('#ytc-list .c[data-v] a.t');
          await tv.keyboard.press('Enter');
          await tv.waitForSelector('#ytc-tvcm-body .yc-h', { state: 'attached' });
          const moving = await tv.evaluate(() => ['ytc-info', 'ytc-desc', 'ytc-list'].flatMap(id => {
            const el = document.getElementById(id);
            return el ? el.getAnimations({ subtree: true }).filter(a => a.playState === 'running').map(a => ({ id, name: a.animationName || 'web-animation' })) : [];
          }));
          assert.deepEqual(moving, [], 'late watch content is steady and usable immediately');
          await tv.unroute('**/youtubei/v1/next?*');
        }
      }
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });
};
