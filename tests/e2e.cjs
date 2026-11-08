// End-to-end test: loads the real unpacked extension (extension/) in Chromium.
// Every request is answered by context.route (stub youtube.com documents with a
// fake "YouTube app", InnerTube fixtures, stub embed/images); nothing reaches the
// network. Run: npm run test:e2e (or node tests/e2e.cjs).
// Env (optional): LUMEN_EXTENSION_CHROMIUM (a full Chrome/Chromium that loads
// unpacked extensions; default: Playwright's Chromium), YTC_ONLY=<regexp> (run
// matching steps only), YTC_EXT (another extension folder).
// Public build: the debug-center steps that need development-build telemetry are
// skipped, and the public build's own promises are checked instead (no telemetry,
// nothing kept for a collector). Incidents still reach the test through a forwarder
// appended to a temporary copy of the public background worker.
'use strict';
const { chromium } = require('playwright');
const fs = require('node:fs'), os = require('node:os'), path = require('node:path'), zlib = require('node:zlib');
const assert = require('node:assert/strict');
const { ensureMedia, SECONDS } = require('./make-media.cjs');
const { chromiumOptions, extensionDir, isDevBuild } = require('./browser.cjs');
const { ensureLive } = require('./make-live.cjs');
const wsMock = require('./ws-mock.cjs');

// YTC_EXT: another client folder (e.g. an older build) for the Home timing table;
// YTC_TIMING_ONLY=1 skips the checks and prints only that table.
const EXT = extensionDir();
const DEV_BUILD = isDevBuild(EXT);
const TIMING_ONLY = !!process.env.YTC_TIMING_ONLY;
const FIX = path.join(__dirname, 'fixtures');
const fixture = name => zlib.gunzipSync(fs.readFileSync(path.join(FIX, name + '.gz')));
const json = name => JSON.parse(fixture(name).toString('utf8'));
const EVIL = `<img src=x onerror="window.__xss=1">"'&amp; <script>window.__xss=2</script>`;
const GIF = Buffer.from('R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7', 'base64');
// Roughly YouTube's real document CSP, including Trusted Types enforcement.
const CSP = "require-trusted-types-for 'script';trusted-types default goog#html polymer-html-literal;script-src 'nonce-n1' 'unsafe-eval' 'strict-dynamic' https: http: 'unsafe-inline';object-src 'none';base-uri 'self'";
// YouTube's own watch page (native mode cleanup): DOM assembled from the old extension's verified fixtures.
const NATIVE_PAGE = fs.readFileSync(path.join(FIX, 'native-watch.html'), 'utf8');
const STUB = `<!DOCTYPE html><html lang="en" style="font-size:10px" dark system-icons><head>
<script nonce="n1">window.__ytAppRan = true;</script>
<link rel="stylesheet" href="/s/desktop/abc123/cssbin/www-main-desktop-watch-page-skeleton.css">
<script nonce="n1" src="/s/desktop/abc123/jsbin/desktop_polymer.vflset/desktop_polymer.js"></script>
</head><body><ytd-app>NATIVE YOUTUBE APP</ytd-app>${'<!-- padding -->'.repeat(2000)}</body></html>`;

function findFirst(o, pred) {
  if (o && typeof o === 'object') {
    if (pred(o)) return o;
    for (const v of Object.values(o)) { const r = findFirst(v, pred); if (r) return r; }
  }
  return null;
}
// Hostile variants of real fixtures for the escaping test.
const search = json('search.json');
const evilSearch = json('search.json');
const vr = findFirst(evilSearch, o => o.videoId && o.longBylineText && !o.navigationEndpoint?.reelWatchEndpoint);
vr.title = { runs: [{ text: EVIL }] };
vr.longBylineText.runs[0].text = EVIL;
const EVIL_ID = vr.videoId;
const next = json('next.json');
const evilNext = json('next.json');
const sec = findFirst(evilNext, o => o.attributedDescription)?.attributedDescription;
sec.content = EVIL + ' bad-link tail';
// "tail" is a real resource, so the hostile line is one the description panel keeps.
sec.commandRuns = [{ startIndex: EVIL.length + 1, length: 8, onTap: { innertubeCommand: { urlEndpoint: { url: 'javascript:window.__xss=3' } } } },
  { startIndex: EVIL.length + 10, length: 4, onTap: { innertubeCommand: { urlEndpoint: { url: 'https://example.com/tail' } } } }];
findFirst(evilNext, o => o.videoPrimaryInfoRenderer).videoPrimaryInfoRenderer.title = { runs: [{ text: EVIL }] };

// A description linking (through YouTube's redirect) to example.net, for the blocked-websites checks (example.com appears in other fixtures).
const linkedNext = json('next.json');
{
  const ad = findFirst(linkedNext, o => o.attributedDescription).attributedDescription;
  ad.content = 'Get the plugin: example.net/ref and more';
  ad.commandRuns = [{ startIndex: 16, length: 15, onTap: { innertubeCommand: { urlEndpoint: { url: 'https://www.youtube.com/redirect?event=video_description&q=https%3A%2F%2Fwww.example.net%2Fref' } } } }];
}
const linkedNextStr = JSON.stringify(linkedNext);
// A description exercising the old extension's description rules: prose, hashtags (text and
// links), a same-video timestamp, a bare link labelled by the line above, another video's link, a blank-line gap.
const descNext = json('next.json');
{
  const ad = findFirst(descNext, o => o.attributedDescription).attributedDescription;
  ad.content = 'Intro prose that should vanish. #intro\nJoin my Discord: Discord #gaming\nProject files:\nDownload\n#tag 0:30 opening\n\nRelated tutorial\nThanks for watching. #thanks';
  const run = (text, cmd, from = 0) => ({ startIndex: ad.content.indexOf(text, from), length: text.length, onTap: { innertubeCommand: cmd } });
  const tag = t => ({ commandMetadata: { webCommandMetadata: { url: '/hashtag/' + t } }, browseEndpoint: { browseId: 'FEhashtag' } });
  ad.commandRuns = [
    run('Discord', { urlEndpoint: { url: 'https://www.youtube.com/redirect?event=video_description&q=https%3A%2F%2Fdiscord.example%2Fjoin' } }, ad.content.indexOf('Discord:') + 8),
    run('#gaming', tag('gaming')),
    run('Download', { urlEndpoint: { url: 'https://files.example/dl' } }),
    run('#tag', tag('tag')),
    run('0:30', { watchEndpoint: { videoId: 'descRULES01', startTimeSeconds: 30 } }),
    run('Related tutorial', { watchEndpoint: { videoId: 'aqz-KE-bpKQ' } }),
  ];
}
const descNextStr = JSON.stringify(descNext);
const noneNext = json('next.json');
findFirst(noneNext, o => o.attributedDescription).attributedDescription.commandRuns = [];
const noneNextStr = JSON.stringify(noneNext);
// Features state for the mocks: Watch later membership, websites checks, Home music/live labels.
// A solid-color 16x9 PNG (late thumbnails in the motion step; the default stub GIF is transparent).
const PNG = (() => {
  const chunk = (type, data) => { const b = Buffer.alloc(12 + data.length); b.writeUInt32BE(data.length, 0); b.write(type, 4, 'ascii'); data.copy(b, 8); b.writeUInt32BE(zlib.crc32(b.subarray(4, 8 + data.length)), 8 + data.length); return b; };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(16, 0); ihdr.writeUInt32BE(9, 4); ihdr[8] = 8; ihdr[9] = 2;
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array(16).fill([74, 120, 140]).flat())]);
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', zlib.deflateSync(Buffer.concat(Array(9).fill(row)))), chunk('IEND', Buffer.alloc(0))]);
})();
const IMG = { delay: 0, search: 0 }; // motion step: late thumbnails / a slow search (skeleton geometry)
const NET = { down: false, probes: 0 };
// googlevideo chunk requests of these videos answer `ms` late (only `itag`'s, when given).
const SLOWSEG = new Map();
// Hedged reads: the first matching InnerTube request never answers (`{ ep, q | v, n }`).
const HEDGE = { stall: null };
const FX = { bot: null, botVideo: null, homeChips: false, wlMock: false, wl: new Set(), wlFail: false, linked: new Set(), checkDelay: 0, checksInFlight: 0, checksMax: 0, homeLabels: false, homeShorts: false };
// An owned Watch later playlist: saved oldest-first initially, with a native sort command and
// enough pages to prove that newest-first applies to the entire library, not each loaded chunk.
const LIBRARY = { on: false, newest: false, ids: [], delay: 0, fail: false, sortFail: false };
const libraryId = n => 'savedWL' + String(n).padStart(4, '0');
// `hl`: the request's language; another language than English labels the sort menu in German.
const libraryPage = (offset, hl = 'en') => {
  const de = hl !== 'en';
  const ids = LIBRARY.newest ? [...LIBRARY.ids].reverse() : LIBRARY.ids;
  const rows = ids.slice(offset, offset + 12).map((id, i) => ({ playlistVideoRenderer: {
    videoId: id, title: { simpleText: ['Drawing expressive characters', 'Making a short film', 'Learning to animate', 'A walk through the studio'][i % 4] },
    shortBylineText: { simpleText: 'Studio notes' }, lengthText: { simpleText: '12:34' },
    thumbnail: { thumbnails: [{ url: `https://i.ytimg.com/vi/${id}/hqdefault.jpg`, width: 480, height: 360 }] },
    // These upload ages deliberately disagree with the save order.
    videoInfo: { runs: [{ text: '12K views' }, { text: ' · ' }, { text: i % 2 ? '1 day ago' : '3 years ago' }] },
  } }));
  if (offset + 12 < ids.length) rows.push({ continuationItemRenderer: { continuationEndpoint: { continuationCommand: { token: `WLLIB:${offset + 12}` } } } });
  const sorts = { sortFilterSubMenuRenderer: { subMenuItems: [
    { title: de ? 'Hinzugefügt (älteste)' : 'Date added (oldest)', selected: !LIBRARY.newest, serviceEndpoint: { playlistEditEndpoint: { playlistId: 'WL', actions: [{ action: 'ACTION_SET_PLAYLIST_VIDEO_ORDER', playlistVideoOrder: 2 }], params: 'fixtureOrderContext' } } },
    { title: de ? 'Hinzugefügt (neueste)' : 'Date added (newest)', selected: LIBRARY.newest, serviceEndpoint: { playlistEditEndpoint: { playlistId: 'WL', actions: [{ action: 'ACTION_SET_PLAYLIST_VIDEO_ORDER', playlistVideoOrder: 1 }], params: 'fixtureOrderContext' } } },
  ] } };
  if (offset) return { onResponseReceivedActions: [{ appendContinuationItemsAction: { continuationItems: rows } }] };
  return { header: { playlistHeaderRenderer: { title: { simpleText: 'Watch later' }, ownerText: { simpleText: 'Fixture owner' }, numVideosText: { simpleText: `${ids.length} videos` }, viewCountText: { simpleText: 'No views' } } },
    contents: { sectionListRenderer: { contents: [{ itemSectionRenderer: { contents: [{ playlistVideoListRenderer: { contents: rows } }] } }], header: sorts } } };
};
// ---- SponsorBlock (sponsor.ajay.app): the k-anonymity API answers a whole sha256-prefix bucket. Our fixture video
// sits in the real bucket 671e (its other entries, 2 Nov 2026, are the decoys; aqz-KE-bpKQ hashes there too).
const sha4 = id => require('node:crypto').createHash('sha256').update(id).digest('hex').slice(0, 4);
const SB_ID = 'sponsorBB9G', SB_ID2 = 'sponsorBNjX';
const SB = { delay: 0, log: [], videos: { [SB_ID]: [['intro', 0, 2], ['sponsor', 8, 14], ['interaction', 14, 15.5], ['selfpromo', 26, 30], ['poi_highlight', 34, 34]] } };
const SB_DECOYS = json('sponsorblock_671e.json');
async function sponsorBlock(route, req, url) {
  const m = /^\/api\/skipSegments\/([0-9a-f]{4})$/.exec(url.pathname);
  const e = { url: req.url(), headers: req.headers(), sent: Date.now(), prefix: m && m[1] };
  SB.log.push(e);
  if (SB.delay) await new Promise(r => setTimeout(r, SB.delay));
  e.done = Date.now();
  const cors = { 'access-control-allow-origin': '*' };
  const mine = m ? Object.entries(SB.videos).filter(([id]) => sha4(id) === m[1]) : [];
  if (!mine.length) return route.fulfill({ status: 404, headers: cors, contentType: 'text/plain', body: 'Not Found' });
  const seg = ([category, a, b], i) => ({ category, actionType: category === 'poi_highlight' ? 'poi' : 'skip', segment: [a, b], UUID: 'e2e' + i, videoDuration: SECONDS, locked: 0, votes: 1, description: '' });
  return route.fulfill({ status: 200, headers: cors, contentType: 'application/json', body: JSON.stringify([...SB_DECOYS, ...mine.map(([videoID, segs]) => ({ videoID, segments: segs.map(seg) }))]) });
}
// ---- actions (comments, like/subscribe/save/share, notifications, account): stubbed endpoints.
const ACTS = { fail: new Set(), delay: 0, signedIn: false };
const cmTok = (o) => o.continuationItemRenderer?.continuationEndpoint?.continuationCommand?.token;
const T_FIRST = findFirst(next, o => o.sectionIdentifier === 'comment-item-section').contents.map(cmTok)[0];
const sortItems = findFirst(next, o => o.sortFilterSubMenuRenderer).sortFilterSubMenuRenderer.subMenuItems;
const T_TOP = sortItems[0].serviceEndpoint.continuationCommand.token, T_NEW = sortItems[1].serviceEndpoint.continuationCommand.token;
const comments1 = json('comments.json');
const cmItems = comments1.onResponseReceivedEndpoints[1].reloadContinuationItemsCommand.continuationItems;
const T_PAGE2 = cmTok(cmItems[cmItems.length - 1]);
const T_REP = cmTok(cmItems[0].commentThreadRenderer.replies.commentRepliesRenderer.contents[0]);
// A timestamp (0:12 → this video) and hostile text in the second comment.
const ent2 = comments1.frameworkUpdates.entityBatchUpdate.mutations.find(m => m.payload?.commentEntityPayload?.properties.commentId === cmItems[1].commentThreadRenderer.commentViewModel.commentViewModel.commentId).payload.commentEntityPayload;
ent2.properties.content = { content: 'see 0:12 ' + EVIL, commandRuns: [{ startIndex: 4, length: 4, onTap: { innertubeCommand: { watchEndpoint: { videoId: '-Bxc4gJluC0', startTimeSeconds: 12 } } } }] };
const comments1Str = JSON.stringify(comments1);
const newest = json('comments.json');
newest.frameworkUpdates.entityBatchUpdate.mutations.forEach(m => { const c = m.payload?.commentEntityPayload; if (c) c.author.displayName = '@newest-e2e'; });
const newestStr = JSON.stringify(newest);
const comments2 = json('comments2.json');
const c2items = comments2.onResponseReceivedEndpoints[0].appendContinuationItemsAction.continuationItems;
c2items.splice(c2items.length - 1, 1); // last page: no continuation → End
const comments2Str = JSON.stringify(comments2);
const chapters = json('chapters_next.json');
findFirst(chapters, o => o.markersMap).markersMap[0].value.chapters[1].chapterRenderer.timeRangeStartMillis = 20000;
findFirst(chapters, o => o.markersMap).markersMap[0].value.chapters[2].chapterRenderer.timeRangeStartMillis = 30000;
// Descriptions with timecodes (next.json has no chapters of its own): one by YouTube's rules, one not (5 s chapter).
const chapDescNext = content => { const n = json('next.json'); const ad = findFirst(n, o => o.attributedDescription).attributedDescription; ad.content = content; ad.commandRuns = []; return JSON.stringify(n); };
const descChapStr = chapDescNext('Chapters below.\nMore at https://example.com/page?t=1:00\n\n0:00 Intro\n0:12 - Setup\n1. 0:26 Build https://example.com\n\nThanks!');
const descNoChapStr = chapDescNext('0:00 Intro\n0:05 Too short\n0:26 Build');
const chaptersStr = JSON.stringify(chapters);
const SIGNED_TOPBAR = { desktopTopbarRenderer: { topbarButtons: [
  { notificationTopbarButtonRenderer: { notificationCount: 3 } },
  { topbarMenuButtonRenderer: { avatar: { thumbnails: [{ url: 'https://yt3.ggpht.com/e2e-avatar=s88' }] } } }] } };
const notif = (i, extra) => ({ notificationRenderer: { shortMessage: { simpleText: `Notification ${i}` }, sentTimeText: { simpleText: `${i} hours ago` }, read: false,
  thumbnail: { thumbnails: [{ url: 'https://yt3.ggpht.com/n' }] }, navigationEndpoint: { commandMetadata: { webCommandMetadata: { url: '/watch?v=-Bxc4gJluC0' } }, watchEndpoint: { videoId: '-Bxc4gJluC0' } },
  recordClickEndpoint: { recordNotificationInteractionsEndpoint: { serializedInteractionsRequest: `SER${i}` } }, ...extra } });
function actFixture(ep, body) {
  const ok = (o) => ACTS.fail.has(ep) ? { status: 500, body: '{"error":{"code":500,"message":"Backend Error"}}' } : { status: 200, body: JSON.stringify(o) };
  if (ep === 'next' && body.continuation) {
    const t = body.continuation;
    if (t === T_FIRST || t === T_TOP) return { status: 200, body: comments1Str };
    if (t === T_NEW) return { status: 200, body: newestStr };
    if (t === T_PAGE2) return { status: 200, body: comments2Str };
    if (t === T_REP) return { status: 200, body: fixture('replies.json') };
    return null;
  }
  if (ep === 'next' && body.videoId === 'kCc8FmEb1nY') return { status: 200, body: chaptersStr };
  if (ep.startsWith('like/')) return ok({});
  if (ep.startsWith('subscription/')) return ok({ actions: [{ updateSubscribeButtonAction: { subscribed: ep === 'subscription/subscribe' } }] });
  if (ep === 'playlist/get_add_to_playlist') return ok({ contents: [{ addToPlaylistRenderer: { playlists: [
    { playlistAddToOptionRenderer: { playlistId: 'WL', title: { simpleText: 'Watch later' }, privacy: 'PRIVATE', containsSelectedVideos: 'ALL' } },
    { playlistAddToOptionRenderer: { playlistId: 'PLe2e', title: { simpleText: EVIL }, privacy: 'PUBLIC', containsSelectedVideos: 'NONE' } }] } }] });
  if (ep === 'browse/edit_playlist') return ok({ status: 'STATUS_SUCCEEDED' });
  if (ep === 'playlist/create') return ok({ playlistId: 'PLnew123' });
  if (ep === 'notification/get_notification_menu') return ok(body.ctoken
    ? { actions: [{ appendContinuationItemsAction: { continuationItems: [notif(3)] } }] }
    : { actions: [{ openPopupAction: { popup: { multiPageMenuRenderer: { sections: [{ multiPageMenuNotificationSectionRenderer: { items: [notif(1, { shortMessage: { simpleText: EVIL } }), notif(2),
        { continuationItemRenderer: { continuationEndpoint: { getNotificationMenuEndpoint: { ctoken: 'CTOK' } } } }] } }] } } } }] });
  if (ep === 'notification/record_interactions') return ok({});
  if (ep === 'account/account_menu') return ok({ actions: [{ openPopupAction: { popup: { multiPageMenuRenderer: { header: { activeAccountHeaderRenderer: {
    accountName: { simpleText: 'E2E Viewer' }, channelHandle: { simpleText: '@e2e' }, accountPhoto: { thumbnails: [{ url: 'https://yt3.ggpht.com/e2e-avatar=s88' }] } } } } } } }] });
  return null;
}

// Pre-serialized so the route handler adds as little latency as possible.
const searchStr = JSON.stringify(search), evilSearchStr = JSON.stringify(evilSearch), nextStr = JSON.stringify(next), evilNextStr = JSON.stringify(evilNext);
const cache = new Map();
const fx = name => { if (!cache.has(name)) cache.set(name, fixture(name)); return cache.get(name); };
const log = { warm: [], suggest: [], api: [], docs: [], assets: [], external: [], embeds: [], gv: [], stats: [], timedtext: [], live: [], sb: [], imp: [] };

// ---- own player fixtures: VISIONOS streams (synthetic local WebM), WEB metadata, captions.
let media = null;
const GV = 'https://rr1---sn-fixture.googlevideo.com/videoplayback';
const playerGen = new Map();
// The googlevideo URLs' `expire=` (Unix seconds): six hours ahead, like YouTube's (a fixed date would
// have passed); staleurl…'s first URLs and expirenoext's expire in 20 minutes (the player goes by
// expiresInSeconds, not by this date against the device clock).
const EXPIRE = Math.floor(Date.now() / 1000) + 6 * 3600;
const expireFor = (v, gen) => (v.startsWith('staleurl') && gen === 1) || v === 'expirenoext' ? Math.floor(Date.now() / 1000) + 20 * 60 : EXPIRE;
// AV1 renditions of the fixture's VP9 ones (av1fault…: each rung also as AV1 at 60 fps, so the
// ladder prefers it). Their bytes are the VP9 files: the browser refuses them for an AV1
// SourceBuffer ("Video stream codec vp9 doesn't match SourceBuffer codecs"), an AV1-only failure.
const AV1_OF = { 243: 396, 242: 395, 278: 394 }, AV1_FILE = { 396: '243', 395: '242', 394: '278' };
// Media errors once the picture is there (midplayerr…): a video/audio request past the first segment
// is held until MIDERR.go, then answered with its first cluster id zeroed (the browser refuses the
// append). midplayerr1/003: only the first player response's URLs (fresh URLs answer cleanly);
// midplayerr2: every answer. staleurl…: the first URLs' later segments never answer (stale URLs).
const MIDERR = { go: false, held: [] };
const midRelease = () => { MIDERR.go = true; for (const p of MIDERR.held.splice(0)) Promise.resolve().then(() => googlevideo(p.route, p.url)).catch(() => {}); };
const failures = { gone: false };
// Stuck episodes (stalls are not failures): googlevideo requests of these videos are
// parked (no answer) while held; `unstall` answers the parked ones and lets new ones through. `chunks`:
// only media chunks are held (the index answers: data but no picture → the MediaSource is rebuilt).
const STALL = { hold: new Map(), parked: [] };
const unstall = v => { STALL.hold.delete(v); for (const p of STALL.parked.splice(0)) Promise.resolve().then(() => googlevideo(p.route, p.url)).catch(() => {}); };
// YouTube's bot check (2 Nov 2026 wording) and the visitor the visitor_id fixture mints.
const BOT = { responseContext: {}, playabilityStatus: { status: 'LOGIN_REQUIRED', reason: 'Sign in to confirm you’re not a bot' } };
const FRESH_VD = 'CgtGcmVzaEUyRTAwMSjAgICABjIKCgJERRIEEgAgQw%3D%3D';
// FX.bot: 'visitor' = bot check unless the fresh visitor; 'account' = unless signed (SAPISIDHASH); 'all' = always.
const botChecked = (body, headers) => (!FX.botVideo || body.videoId === FX.botVideo) && (FX.bot === 'visitor' ? body.context.client.visitorData !== FRESH_VD : FX.bot === 'account' ? !headers.authorization : FX.bot === 'all');
function visionFixture(v) {
  if (v === 'unplayable1') return { playabilityStatus: { status: 'UNPLAYABLE', reason: 'Forced failure (e2e)' } };
  if (v === 'members0001') return { playabilityStatus: { status: 'UNPLAYABLE', reason: 'Join this channel to get access to members-only content like this video, and other exclusive perks.' } };
  if (v === 'agerestrict') return { playabilityStatus: { status: 'LOGIN_REQUIRED', reason: 'Sign in to confirm your age' } };
  if (v === 'liveupcomin') return { playabilityStatus: { status: 'LIVE_STREAM_OFFLINE', reason: 'Premieres in 2 hours' }, videoDetails: { videoId: v, isUpcoming: true, isLiveContent: true } };
  if (LIVE_IDS.includes(v)) {
    if (!LIVE.streams.has(v)) LIVE.streams.set(v, { t0: Date.now() });
    const adaptiveFormats = LIVE.fx.formats.map(f => ({ ...f, url: `${GV}?expire=${EXPIRE}&ei=E2E&ip=0.0.0.0&id=${v}.2&itag=${f.itag}&source=yt_live_broadcast&live=1&v=${v}&c=VISIONOS` }));
    return { playabilityStatus: { status: 'OK' }, streamingData: { expiresInSeconds: '21540', adaptiveFormats, hlsManifestUrl: `https://manifest.googlevideo.com/api/manifest/hls_variant/expire/1793600000/id/${v}.2/source/yt_live_broadcast/file/index.m3u8`, serverAbrStreamingUrl: 'https://rr1---sn-fixture.googlevideo.com/sabr' },
      videoDetails: { videoId: v, title: 'Fixture live stream', lengthSeconds: '0', isLive: true, isLiveContent: true, author: 'Fixture' } };
  }
  const gen = (playerGen.get(v) || 0) + 1;
  playerGen.set(v, gen);
  const fmts = v.startsWith('av1fault') ? [...media.formats, ...media.formats.filter(f => AV1_OF[f.itag]).map(f => ({ ...f, itag: AV1_OF[f.itag], mimeType: 'video/webm; codecs="av01.0.05M.08"', fps: 60 }))] : media.formats;
  const adaptiveFormats = fmts.map(f => ({ ...f, url: `${GV}?expire=${expireFor(v, gen)}&ei=E2E&ip=0.0.0.0&id=o-fixture&mn=sn-fixture%2Csn-fixture2&fvip=1&itag=${f.itag}&source=youtube&v=${v}&gen=${gen}&mime=${encodeURIComponent(f.mimeType.split(';')[0])}&clen=${f.contentLength}&c=VISIONOS` }));
  // staleurl… (first response) and expirenoext (every response): URLs that run out in 20 minutes.
  const life = (v.startsWith('staleurl') && gen === 1) || v === 'expirenoext' || v === 'expirehang1' ? '1200' : '21540';
  return { responseContext: {}, playabilityStatus: { status: 'OK' }, streamingData: { expiresInSeconds: life, adaptiveFormats },
    videoDetails: { videoId: v, title: 'Fixture video', lengthSeconds: String(SECONDS), author: 'Fixture' } };
}
// A Japanese video: ASR + human Japanese tracks, YouTube can translate to English.
const CC_JA = { captionTracks: ['a.ja', '.ja'].map(id => ({ baseUrl: `https://www.youtube.com/api/timedtext?v=ccjapanese1&ei=E2E&lang=ja${id[0] === 'a' ? '&kind=asr' : ''}`,
  name: { simpleText: id[0] === 'a' ? 'Japanese (auto-generated)' : 'Japanese' }, vssId: id, languageCode: 'ja', kind: id[0] === 'a' ? 'asr' : undefined, isTranslatable: true })),
  audioTracks: [{ captionTrackIndices: [0, 1], defaultCaptionTrackIndex: 1 }], defaultAudioTrackIndex: 0,
  translationLanguages: [{ languageCode: 'de', languageName: { simpleText: 'German' } }, { languageCode: 'en', languageName: { simpleText: 'English' } }] };
// Storyboards (the WEB response's seek-preview sprite sheets, real spec format): every VOD fixture but
// descnochap1 has one; frames every 1 s. The sheets are served as SVGs whose cells show their frame number.
const SB_LEVELS = [[48, 27, 100, 10, 10, 0, 'default'], [80, 45, SECONDS, 10, 10, 1000, 'M$M'], [160, 90, SECONDS, 5, 5, 1000, 'M$M'], [320, 180, SECONDS, 3, 3, 1000, 'M$M']];
const sbSpec = v => `https://i.ytimg.com/sb/${v}/storyboard3_L$L/$N.jpg?sqp=E2E|` + SB_LEVELS.map((l, i) => [...l, `rs$E2E${i}`].join('#')).join('|');
function sbSheet(url) {
  const m = /^\/sb\/([\w-]{11})\/storyboard3_L(\d)\/(default|M(\d+))\.jpg$/.exec(url.pathname), L = m && SB_LEVELS[+m[2]];
  if (!L || url.searchParams.get('sigh') !== `rs$E2E${m[2]}` || url.searchParams.get('sqp') !== 'E2E') return null;
  const [w, h, count, cols, rows] = L, per = cols * rows, page = m[4] ? +m[4] : 0, left = Math.min(per, count - page * per);
  const cw = Math.min(left, cols), rh = Math.ceil(left / cols);
  let cells = '';
  for (let k = 0; k < left; k++) {
    const n = page * per + k, x = (k % cols) * w, y = Math.floor(k / cols) * h;
    cells += `<rect x="${x}" y="${y}" width="${w}" height="${h}" fill="hsl(${(n * 47) % 360} 45% 32%)"/><rect x="${x + 0.5}" y="${y + 0.5}" width="${w - 1}" height="${h - 1}" fill="none" stroke="#fff" stroke-opacity=".5"/><text x="${x + w / 2}" y="${y + h * 0.62}" font-size="${h * 0.42}" font-family="Arial" font-weight="700" fill="#fff" text-anchor="middle">${n}</text>`;
  }
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${cw * w}" height="${rh * h}">${cells}</svg>`;
}
function webFixture(v) {
  const q = `cl=1&docid=${v}&ei=E2E&fexp=&ns=yt&plid=P&el=detailpage&len=${SECONDS}&of=OF&vm=VM`;
  return { playabilityStatus: { status: 'OK' }, streamingData: { serverAbrStreamingUrl: 'https://rr1---sn-fixture.googlevideo.com/sabr' },
    playbackTracking: { videostatsPlaybackUrl: { baseUrl: `https://s.youtube.com/api/stats/playback?${q}` }, videostatsWatchtimeUrl: { baseUrl: `https://s.youtube.com/api/stats/watchtime?${q}` },
      videostatsScheduledFlushWalltimeSeconds: [10, 20, 30], videostatsDefaultFlushIntervalSeconds: 40 },
    captions: { playerCaptionsTracklistRenderer: v === 'ccjapanese1' ? CC_JA : { captionTracks: [{ baseUrl: `https://www.youtube.com/api/timedtext?v=${v}&ei=E2E&lang=en&e2e=1`, name: { runs: [{ text: 'English' }] }, vssId: '.en', languageCode: 'en' }] } },
    ...(v === 'descnochap1' ? {} : { storyboards: { playerStoryboardSpecRenderer: { spec: sbSpec(v), recommendedLevel: 2 } } }),
    videoDetails: { videoId: v, title: 'Fixture video', lengthSeconds: String(SECONDS) } };
}
const json3 = JSON.stringify({ wireMagic: 'pb3', events: [{ tStartMs: 0, dDurationMs: SECONDS * 1000, id: 1, wpWinPosId: 1, wsWinStyleId: 1 },
  ...Array.from({ length: SECONDS / 2 }, (_, i) => ({ tStartMs: i * 2000, dDurationMs: 2000, wWinId: 1, segs: [{ utf8: `Caption line ${i}` }, { utf8: ` <b>&amp;</b>`, tOffsetMs: 300 }] }))] });
// ---- live fixtures: HLS master/media playlists (sliding window that grows in real
// time, 3.2 s segments) + fMP4 segments by sequence number (make-live.cjs).
const LIVE_IDS = ['livestream1', 'liveended01'];
const LIVE = { fx: null, streams: new Map(), base: 1000, window: 8 };
const liveHead = st => LIVE.base + 5 + Math.floor((Date.now() - st.t0) / (LIVE.fx.SEG * 1000));
function liveManifest(route, url) {
  const p = url.pathname, v = (p.match(/\/id\/([\w-]{11})\.2\//) || [])[1], st = LIVE.streams.get(v);
  log.live.push({ p, v, t: Date.now() });
  const cors = { 'access-control-allow-origin': 'https://www.youtube.com', 'content-type': 'application/vnd.apple.mpegurl' };
  if (!st) return route.fulfill({ status: 404, headers: cors, body: '' });
  if (p.includes('/hls_variant/')) {
    const vs = LIVE.fx.VIDEO.map(r => `#EXT-X-STREAM-INF:BANDWIDTH=${r.bitrate + 70000},CODECS="${r.codec},mp4a.40.2",RESOLUTION=${r.w}x${r.h},FRAME-RATE=25,VIDEO-RANGE=SDR,AUDIO="234",CLOSED-CAPTIONS=NONE\nhttps://manifest.googlevideo.com/api/manifest/hls_playlist/expire/1793600000/id/${v}.2/itag/${r.itag === 134 ? 230 : 229}/source/yt_live_broadcast/playlist_type/DVR/file/index.m3u8`);
    return route.fulfill({ status: 200, headers: cors, body: `#EXTM3U\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MEDIA:URI="https://manifest.googlevideo.com/api/manifest/hls_playlist/id/${v}.2/itag/234/file/index.m3u8",TYPE=AUDIO,GROUP-ID="234",NAME="Default",DEFAULT=YES,AUTOSELECT=YES\n${vs.join('\n')}\n` });
  }
  let head = liveHead(st), ended = false;
  if (v === 'liveended01' && head >= LIVE.base + 6) { head = LIVE.base + 6; ended = true; }
  const first = Math.max(LIVE.base, head - LIVE.window + 1);
  let out = `#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:${Math.ceil(LIVE.fx.SEG)}\n#EXT-X-MEDIA-SEQUENCE:${first}\n#EXT-X-DISCONTINUITY-SEQUENCE:0\n`;
  for (let sq = first; sq <= head; sq++) out += `#EXT-X-PROGRAM-DATE-TIME:${new Date(st.t0 + (sq - LIVE.base - 5) * LIVE.fx.SEG * 1000).toISOString()}\n#EXTINF:${LIVE.fx.SEG.toFixed(3)},\nhttps://rr1---sn-fixture.googlevideo.com/videoplayback/id/${v}.2/itag/230/source/yt_live_broadcast/playlist/index.m3u8/sq/${sq}/dur/${LIVE.fx.SEG}/file/seg.ts\n`;
  if (ended) out += '#EXT-X-ENDLIST\n';
  return route.fulfill({ status: 200, headers: cors, body: out });
}
function liveSegment(route, url) {
  const v = url.searchParams.get('v'), itag = +url.searchParams.get('itag'), sq = +url.searchParams.get('sq'), st = LIVE.streams.get(v);
  log.gv.push({ itag: String(itag), v, sq, live: true, t: Date.now() });
  const cors = { 'access-control-allow-origin': 'https://www.youtube.com', 'content-type': 'video/mp4' };
  if (!st || sq > liveHead(st) || sq < LIVE.base) return route.fulfill({ status: 404, headers: cors, body: '' });
  const body = LIVE.fx.segment(itag, sq, st.t0 + (sq - LIVE.base - 5) * LIVE.fx.SEG * 1000);
  return body ? route.fulfill({ status: 200, headers: cors, body }) : route.fulfill({ status: 404, headers: cors, body: '' });
}
const mediaFiles = new Map();
function googlevideo(route, url) {
  if (url.hostname === 'manifest.googlevideo.com') return liveManifest(route, url);
  if (url.searchParams.get('live') === '1') return liveSegment(route, url);
  // The TV app's connection warm-up (through its shell): a tiny request, answered 204.
  if (url.pathname === '/generate_204') { log.warm.push(url.hostname); return route.fulfill({ status: 204, headers: { 'access-control-allow-origin': '*' }, body: '' }); }
  const itag = url.searchParams.get('itag'), v = url.searchParams.get('v'), gen = +url.searchParams.get('gen');
  const [a, b] = (url.searchParams.get('range') || '').split('-').map(Number);
  log.gv.push({ itag, v, gen, a, b, host: url.hostname, t: Date.now() });
  const cors = { 'access-control-allow-origin': 'https://www.youtube.com', 'content-type': 'application/octet-stream' };
  if (STALL.hold.has(v) && (STALL.hold.get(v) !== 'chunks' || a > 0)) return new Promise(() => STALL.parked.push({ route, url }));
  if ((v === 'refresh4033' && gen === 1 && a > 0) || (v === 'gone403gone' && failures.gone)) return route.fulfill({ status: 403, headers: cors, body: '' });
  // Live 1 Nov 2026: high rungs can 302 to plain http, which the browser blocks (status 0).
  if ((v === 'deadupgrade' || v === 'deadstart01') && itag === '243') return route.abort('failed');
  // googlevideo never answers the first player response's URLs (stale cached response / stuck node).
  // A stuck node: never answers on its own host, only on the mirror YouTube lists in mn.
  if (v === 'gvnodehang1' && url.hostname === 'rr1---sn-fixture.googlevideo.com') return new Promise(() => {});
  if (v === 'gvhangfirst' && gen === 1) return new Promise(() => {});
  // A slow node (throttled, not dead): its own host answers only after 4 s; the mirror at once.
  if (v === 'gvslowfirst' && url.hostname === 'rr1---sn-fixture.googlevideo.com') {
    const file = fs.readFileSync(path.join(media.dir, `itag-${itag}.webm`));
    return new Promise(r => setTimeout(r, 4000)).then(() => route.fulfill({ status: 200, headers: cors, body: file.subarray(a, Math.min(b, file.length - 1) + 1) })).catch(() => {});
  }
  // A dropped connection: every request to the video server fails at once (HTTP 0).
  if (v === 'netdown0001' && NET.down) return route.abort('internetdisconnected');
  if (v === 'gvhangcach1' && gen === 1) return new Promise(() => {});
  // A momentary 503 from one node on the starting format's index: asked again, no fallback.
  if (v === 'flaky503vid' && itag === '243' && a === 0 && !failures.flaky503) { failures.flaky503 = true; return route.fulfill({ status: 503, headers: cors, body: '' }); }
  // Chunks of one rung failing mid-video (random redirects): the player steps down.
  if (v === 'stepdown001' && itag === '243' && a > 120000) return route.abort('failed');
  const fileItag = AV1_FILE[itag] || itag;
  if (!mediaFiles.has(fileItag)) mediaFiles.set(fileItag, fs.readFileSync(path.join(media.dir, `itag-${fileItag}.webm`)));
  const file = mediaFiles.get(fileItag);
  {
    const f = media.formats.find(x => String(x.itag) === itag), later = f && a > +f.indexRange.end + 1 && a < file.length;
    if (later && v === 'staleurl001' && gen === 1) return new Promise(() => {});
    if (later && v === 'staleurl002' && gen === 1) return new Promise(() => {});
    const mid = f && f.mimeType.startsWith('video/') && later && (((v === 'midplayerr1' || v === 'midplayerr3') && gen === 1) || v === 'midplayerr2');
    if (mid && !MIDERR.go) return new Promise(() => MIDERR.held.push({ route, url }));
    if (mid) {
      const body = Buffer.from(file.subarray(a, Math.min(b, file.length - 1) + 1));
      body.fill(0, 0, 4);
      return route.fulfill({ status: 200, headers: cors, body }).catch(() => {});
    }
  }
  // Slow media requests (seek cancel / seek-bar prefetch): every chunk request answers this many ms late.
  const slow = SLOWSEG.get(v);
  // A media error before the first frame: the first video cluster's id bytes zeroed (no valid WebM
  // element, so the browser refuses the append) in every answer for mediaerr001's first player
  // response (its fresh URLs answer cleanly) and in every answer for mediaerr002.
  if ((v === 'mediaerr001' && gen === 1) || v === 'mediaerr002') {
    const f = media.formats.find(x => String(x.itag) === itag);
    if (f && f.mimeType.startsWith('video/') && a < file.length) {
      const body = Buffer.from(file.subarray(a, Math.min(b, file.length - 1) + 1)), at = +f.indexRange.end + 1;
      for (let p = Math.max(at, a); p < at + 4 && p - a < body.length; p++) body[p - a] = 0;
      return route.fulfill({ status: 200, headers: cors, body });
    }
  }
  if (slow && (!slow.itag || slow.itag === itag) && a > 0 && a < file.length) return new Promise(r => setTimeout(r, slow.ms)).then(() => route.fulfill({ status: 200, headers: cors, body: file.subarray(a, Math.min(b, file.length - 1) + 1) })).catch(() => {});
  if (!(a >= 0 && b >= a && a < file.length)) return route.fulfill({ status: 416, headers: cors, body: '' });
  return route.fulfill({ status: 200, headers: cors, body: file.subarray(a, Math.min(b, file.length - 1) + 1) });
}
// ---- signed-in-like Home: real channel lockups as page 1, then five browse
// continuations with fresh video ids (HOMECONT1…5) = 180 videos.
const HOME = { rich: false, delay: 0, latency: 0, stamp: false, served: 0, stall: 0, aborted: 0, docs: 0, doc: 'home', nav: false };
// YouTube's Home document (what a reload of its page gets; src/homedoc.rs): the Home data the browse fixture
// would answer, inside the page's ytcfg head and its ytInitialData script. HOME.doc: 'home' | 'consent' (a page
// without Home data) | 'redirect' (to the consent site).
const HOME_VD = 'CgtlMmVGYWtlVmlzMCjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D';
const homeDoc = (data, n) => `<!DOCTYPE html><html lang="en"><head><script nonce="n1">var ytcfg={d:function(){return window.yt&&yt.config_||ytcfg.data_||(ytcfg.data_={})},get:function(k,o){return k in ytcfg.d()?ytcfg.d()[k]:o},set:function(){}};
ytcfg.set({"EVENT_ID":"e2eHomeEvent${String(n).padStart(4, '0')}","INNERTUBE_CLIENT_VERSION":"2.20261102.01.00","INNERTUBE_CONTEXT_GL":"DE","LOGGED_IN":false,"VISITOR_DATA":"${HOME_VD}"});window.__ytAppRan=1;</script></head>
<body><script nonce="n1">var ytInitialData = ${data};</script><script nonce="n1">window.__ytAppRan=1;</script></body></html>`;
const gunzipJSON = buf => { try { return JSON.parse(zlib.gunzipSync(buf).toString('utf8')); } catch { try { return JSON.parse(buf.toString('utf8')); } catch { return null; } } };
// Simulated `next` latency (speed table).
const NEXT = { latency: 0, contLatency: 0 };
const ids11 = str => [...new Set([...str.matchAll(/"contentId":"([A-Za-z0-9_-]{11})"/g)].map(m => m[1]))];
const homeId = (id, n) => id.slice(0, 8) + 'H' + n + 'z';
const homePage = (str, n, next) => {
  for (const id of ids11(str)) str = str.split(id).join(homeId(id, n));
  return str.replace(/"continuationCommand":\{"token":"[^"]+"/g, `"continuationCommand":{"token":"${next}"`);
};
const homeFirst = homePage(fixture('channel_videos.json').toString('utf8'), 0, 'HOMECONT1');
const homeConts = [1, 2, 3, 4, 5].map(n => homePage(fixture('browse_cont.json').toString('utf8'), n, n < 5 ? `HOMECONT${n + 1}` : 'HOMEEND'));
const HOME_ID0 = homeId(ids11(fixture('channel_videos.json').toString('utf8'))[0], 0);
// The same first Home page with an explicit "Music" badge on video 0 and a LIVE badge on video 1.
const homeLabeled = (() => {
  const j = JSON.parse(homeFirst), vids = [];
  (function f(o) { if (o && typeof o === 'object') { if (o.lockupViewModel?.contentType === 'LOCKUP_CONTENT_TYPE_VIDEO') vids.push(o.lockupViewModel); for (const v of Object.values(o)) f(v); } })(j);
  vids[0].metadata.lockupMetadataViewModel.metadata.contentMetadataViewModel.metadataRows[0].badges = [{ badgeViewModel: { badgeText: 'Music' } }];
  const b = vids[1].contentImage.thumbnailViewModel.overlays.find(o => o.thumbnailBottomOverlayViewModel).thumbnailBottomOverlayViewModel.badges[0].thumbnailBadgeViewModel;
  b.text = 'LIVE'; b.badgeStyle = 'THUMBNAIL_OVERLAY_BADGE_STYLE_LIVE';
  return { str: JSON.stringify(j), music: vids[0].contentId, live: vids[1].contentId };
})();
// The same first Home page with a Shorts shelf (the recorded search response's real
// shortsLockupViewModel items, fresh ids) after the first two videos.
const homeShorts = (() => {
  const j = JSON.parse(homeFirst);
  const shelf = findFirst(search, o => o.gridShelfViewModel).gridShelfViewModel.contents;
  let str = JSON.stringify(shelf);
  const ids = [...new Set(shelf.map(it => it.shortsLockupViewModel.onTap.innertubeCommand.reelWatchEndpoint.videoId))];
  const fresh = ids.map((id, i) => id.slice(0, 8) + 'S' + i + 'z');
  ids.forEach((id, i) => { str = str.split(id).join(fresh[i]); });
  const items = JSON.parse(str).map(it => ({ richItemRenderer: { content: it } }));
  findFirst(j, o => o.richGridRenderer).richGridRenderer.contents.splice(2, 0, { richSectionRenderer: { content: { richShelfRenderer: { title: { runs: [{ text: 'Shorts' }] }, contents: items } } } });
  return { str: JSON.stringify(j), ids: fresh, title: shelf[0].shortsLockupViewModel.overlayMetadata.primaryText.content };
})();
// YouTube's Home chip bar (signed-in shape, hand-built: signed-out Home has none) and a chip's feed.
const ncloud = (label, token, sel) => ({ chipCloudChipRenderer: { text: { runs: [{ text: label }] }, isSelected: !!sel, ...(token ? { navigationEndpoint: { continuationCommand: { token } } } : {}) } });
const homeChips = (() => {
  const j = JSON.parse(homeFirst);
  findFirst(j, o => o.richGridRenderer).richGridRenderer.header = { feedFilterChipBarRenderer: { contents: [ncloud('All', null, true), ncloud('Gaming', 'CHIPGAMING'), ncloud('Podcasts', 'CHIPPOD0')] } };
  return JSON.stringify(j);
})();
const chipFeed = (n, sel) => {
  const items = JSON.parse(homePage(fixture('browse_cont.json').toString('utf8'), n, 'HOMEEND')).onResponseReceivedActions[0].appendContinuationItemsAction.continuationItems;
  return JSON.stringify({ onResponseReceivedActions: [
    { reloadContinuationItemsCommand: { slot: 'RELOAD_CONTINUATION_SLOT_HEADER', continuationItems: [{ feedFilterChipBarRenderer: { contents: [ncloud('All', 'CHIPALL0'), ncloud('Gaming', sel === 'g' ? null : 'CHIPGAMING2', sel === 'g'), ncloud('Podcasts', sel === 'p' ? null : 'CHIPPOD2', sel === 'p')] } }] } },
    { reloadContinuationItemsCommand: { slot: 'RELOAD_CONTINUATION_SLOT_BODY', continuationItems: items } }] });
};
// YTC_AUDIT: worst-case data (break-ui) for the layout audit: long and short titles, non-Latin
// scripts, emoji, huge counts, a missing avatar. Served for the query "worst" and the video WORST_ID.
const AUDIT = { worst: false };
const WT = [
  'How to make SMOOTH animations in ROBLOX STUDIO! [Moon Animator Tutorial] — Complete Beginner to Advanced Guide (2026 Edition) with R6 + R15 Rigs, IK, Camera Work & Exporting to Blender',
  '【MV】YOASOBI「アイドル」(Idol) Official Music Video — 4K 60fps 原作: 赤坂アカ × 横槍メンゴ「【推しの子】」',
  'شرح كامل لبرنامج روبلوكس ستوديو للمبتدئين خطوة بخطوة ٢٠٢٦',
  'Hi',
  '🔥🔥 I Built a 1:1 Replica of Tokyo in Roblox Studio (100 Days) 🗼🇯🇵',
  'RobloxStudio_AnimationExport_FINAL_final_v2_rendered_1080p60_noaudio.mp4',
  'Как сделать плавную анимацию в Roblox Studio: полный курс для начинающих',
];
const WC = ['Aleksandra Wiśniewska-Kowalczyk Official Music & Gaming Channel', 'Jo', 'Курцгезагт – В двух словах', 'قناة المبرمج العربي الرسمية', 'MonkeyDev', 'TheVeryLongChannelNameWithoutAnySpacesAtAll2026', 'さくらみこ / Sakura Miko'];
const worstSearchStr = (() => {
  const j = json('search.json'); let i = 0;
  const walk = o => { if (o && typeof o === 'object') { if (o.videoRenderer && i < WT.length) { const v = o.videoRenderer, k = i++; v.title = { runs: [{ text: WT[k] }] }; for (const f of ['longBylineText', 'ownerText', 'shortBylineText']) if (v[f]?.runs?.[0]) v[f].runs[0].text = WC[k]; v.viewCountText = { simpleText: k % 2 ? '1,284,736,912 views' : '1 view' }; v.shortViewCountText = { simpleText: k % 2 ? '1.2B views' : '1 view' }; } for (const x of Object.values(o)) walk(x); } };
  walk(j); return JSON.stringify(j);
})();
const WORST_ID = 'UURYhAVph5g';
const worstNextStr = (() => {
  const j = json('next.json');
  findFirst(j, o => o.videoPrimaryInfoRenderer).videoPrimaryInfoRenderer.title = { runs: [{ text: WT[0] }] };
  const own = findFirst(j, o => o.videoOwnerRenderer).videoOwnerRenderer;
  own.title.runs[0].text = WC[0]; own.subscriberCountText = { simpleText: '312M subscribers' }; delete own.thumbnail;
  let i = 0;
  const walk = o => { if (o && typeof o === 'object') { if (o.lockupViewModel && i < WT.length) { const m = o.lockupViewModel.metadata?.lockupMetadataViewModel; if (m) { const k = i++; m.title.content = WT[(k + 1) % WT.length]; const rows = m.metadata?.contentMetadataViewModel?.metadataRows || []; if (rows[0]?.metadataParts?.[0]) rows[0].metadataParts[0].text.content = WC[k]; if (rows[1]?.metadataParts?.[0]) rows[1].metadataParts[0].text.content = k % 2 ? '1,284,736,912 views' : '1 view'; } } for (const x of Object.values(o)) walk(x); } };
  walk(j);
  return JSON.stringify(j).slice(0, -1) + `,"currentVideoEndpoint":{"watchEndpoint":{"videoId":"${WORST_ID}"}}}`;
})();
// Search with YouTube's recommendation shelves (synthetic, built from the recorded search): the recorded
// "People also watched" shelf, plus "For you", "People also search for" and a channel's linked
// "Latest from …" shelf (a real result shelf), and the same shelves in the continuation.
const SHELF_CONT = 'SHELVES-CONTINUATION-TOKEN-0123456789-abcdefghijklmnopqrstuvwxyz';
const SHELF_IDS = { rec: ['shelfrec001', 'shelfrec002', 'shelfrec003'], keep: ['shelfkeep01', 'shelfkeep02'] };
const shelfVr = (id, title) => { const v = JSON.parse(JSON.stringify(findFirst(json('search.json'), o => o.videoId && o.longBylineText && !o.navigationEndpoint?.reelWatchEndpoint))); v.videoId = id; v.title = { runs: [{ text: title }] }; v.navigationEndpoint = { commandMetadata: { webCommandMetadata: { url: '/watch?v=' + id } }, watchEndpoint: { videoId: id } }; delete v.expandableMetadata; return { videoRenderer: v }; };
const shelvesExtra = () => [
  { shelfRenderer: { title: { simpleText: 'For you' }, content: { verticalListRenderer: { items: [shelfVr(SHELF_IDS.rec[0], 'For you pick'), shelfVr(SHELF_IDS.rec[1], 'Another pick')] } } } },
  { horizontalCardListRenderer: { header: { richListHeaderRenderer: { title: { simpleText: 'People also search for' } } }, cards: [{ searchRefinementCardRenderer: { query: { runs: [{ text: 'roblox animation' }] }, searchEndpoint: { searchEndpoint: { query: 'roblox animation' } } } }] } },
  { shelfRenderer: { title: { runs: [{ text: 'Latest from MonkeyDev', navigationEndpoint: { browseEndpoint: { browseId: 'UCmonkeydevchannel000001', canonicalBaseUrl: '/@MonkeyDev' } } }] }, endpoint: { browseEndpoint: { browseId: 'UCmonkeydevchannel000001', canonicalBaseUrl: '/@MonkeyDev' } }, content: { verticalListRenderer: { items: [shelfVr(SHELF_IDS.keep[0], 'Latest one'), shelfVr(SHELF_IDS.keep[1], 'Latest two')] } } } },
];
const shelvesSearchStr = (() => {
  const j = json('search.json');
  const list = j.contents.twoColumnSearchResultsRenderer.primaryContents.sectionListRenderer.contents[0].itemSectionRenderer.contents;
  list.splice(3, 0, ...shelvesExtra());
  // Its own continuation token, so the continuation request is recognisable.
  findFirst(j, o => o.continuationItemRenderer).continuationItemRenderer.continuationEndpoint.continuationCommand.token = SHELF_CONT;
  return JSON.stringify(j);
})();
const shelvesContStr = (() => {
  const j = json('search_cont.json');
  const list = j.onResponseReceivedCommands[0].appendContinuationItemsAction.continuationItems[0].itemSectionRenderer.contents;
  list.splice(2, 0, { shelfRenderer: { title: { simpleText: 'People also watched' }, content: { verticalListRenderer: { items: [shelfVr(SHELF_IDS.rec[2], 'Watched pick')] } } } });
  return JSON.stringify(j);
})();
function apiFixture(ep, body) {
  if (AUDIT.worst && ep === 'search' && body.query === 'worst' && !body.continuation && !body.params) return worstSearchStr;
  if (AUDIT.worst && ep === 'next' && body.videoId === WORST_ID && !body.continuation && !body.playlistId) return worstNextStr;
  if (ep === 'browse' && /^CHIP(GAMING|POD)/.test(body.continuation || '')) return body.continuation.startsWith('CHIPGAMING') ? chipFeed(7, 'g') : chipFeed(8, 'p');
  if (ep === 'browse' && body.continuation?.startsWith('HOME')) return homeConts[+body.continuation.slice(8) - 1] || '{"onResponseReceivedActions":[]}';
  if (ep === 'browse' && body.browseId === 'FEwhat_to_watch' && HOME.rich) {
    if (HOME.stamp) { const j = JSON.parse(homeFirst); findFirst(j, o => o.lockupViewModel?.contentType === 'LOCKUP_CONTENT_TYPE_VIDEO').lockupViewModel.metadata.lockupMetadataViewModel.title.content = `Fresh #${++HOME.served}`; return JSON.stringify(j); }
    return FX.homeShorts ? homeShorts.str : FX.homeLabels ? homeLabeled.str : FX.homeChips ? homeChips : homeFirst;
  }
  if (ep === 'playlist/get_add_to_playlist') {
    const v = body.videoIds[0];
    return JSON.stringify({ contents: [{ addToPlaylistRenderer: { playlists: [{ playlistAddToOptionRenderer: { playlistId: 'PLother', containsSelectedVideos: 'NONE' } }, { playlistAddToOptionRenderer: { playlistId: 'WL', containsSelectedVideos: FX.wl.has(v) ? 'ALL' : 'NONE' } }] } }] });
  }
  if (ep === 'browse/edit_playlist') {
    if (FX.wlFail) return { status: 401, body: JSON.stringify({ error: { code: 401, message: 'Request is missing required authentication credential.', status: 'UNAUTHENTICATED' } }) };
    const a = body.actions[0];
    if (a.action === 'ACTION_ADD_VIDEO') FX.wl.add(a.addedVideoId); else FX.wl.delete(a.removedVideoId);
    return JSON.stringify({ status: 'STATUS_SUCCEEDED' });
  }
  // Any filtered search answers with the recorded "This year" response (1 Nov 2026).
  if (ep === 'search' && body.continuation === SHELF_CONT) return shelvesContStr;
  if (ep === 'search' && body.query === 'shelves' && !body.params && !body.continuation) return shelvesSearchStr;
  if (ep === 'search') return body.continuation ? fx('search_cont.json') : body.query === 'xss' ? evilSearchStr : body.params ? fx('search_filtered.json') : searchStr;
  if (ep === 'next') {
    // (NEXT.contLatency: the scroll measurement, which needs fresh related videos on every page.)
    if (body.continuation) return NEXT.contLatency ? homePage(fx('next_cont.json').toString('utf8'), (NEXT.contN = (NEXT.contN || 0) + 1) % 10, 'RELCONT' + NEXT.contN) : fx('next_cont.json');
    // In a playlist: the recorded list= response (1 Nov 2026), reporting the requested video as current.
    if (body.playlistId) return fx('next_list.json').toString('utf8').slice(0, -1) + `,"currentVideoEndpoint":{"watchEndpoint":{"videoId":"${body.videoId}"}}}`;
    if (body.videoId === EVIL_ID) return evilNextStr;
    if (body.videoId === 'descNONE001') return noneNextStr.slice(0, -1) + `,"currentVideoEndpoint":{"watchEndpoint":{"videoId":"descNONE001"}}}`;
    if (body.videoId === 'descRULES01') return descNextStr.slice(0, -1) + `,"currentVideoEndpoint":{"watchEndpoint":{"videoId":"descRULES01"}}}`;
    if (body.videoId === 'descchaps01' || body.videoId === 'descnochap1') return (body.videoId === 'descchaps01' ? descChapStr : descNoChapStr).slice(0, -1) + `,"currentVideoEndpoint":{"watchEndpoint":{"videoId":"${body.videoId}"}}}`;
    if (body.videoId === 'blockLINK01' || FX.linked.has(body.videoId)) return linkedNextStr.slice(0, -1) + `,"currentVideoEndpoint":{"watchEndpoint":{"videoId":"${body.videoId}"}}}`;
    // Later duplicate key wins in serde_json: report the requested video as current.
    return nextStr.slice(0, -1) + `,"currentVideoEndpoint":{"watchEndpoint":{"videoId":"${body.videoId}"}}}`;
  }
  if (ep === 'navigation/resolve_url') return fx('resolve.json');
  if (ep === 'player') return JSON.stringify(body.context.client.clientName === 'VISIONOS' ? visionFixture(body.videoId) : webFixture(body.videoId));
  if (ep === 'browse') {
    if (body.continuation) return body.continuation.startsWith('4qmFsgJbEiRWTFBM') ? '{"onResponseReceivedActions":[]}' : fx('browse_cont.json');
    if (body.browseId === 'FEwhat_to_watch') return fx('home.json');
    if (body.browseId?.startsWith('UC')) return fx('channel_videos.json');
    if (body.browseId?.startsWith('VL')) return fx('playlist.json');
  }
  return null;
}

// The website check's own `next` mask starts with `contents/`; the watch page's (`api::NEXT_FIELDS`) does not.
const isCheck = url => (url.searchParams.get('fields') || '').startsWith('contents/');
// A `fields=` mask applied the way InnerTube applies it (arrays transparent, a bare name keeps its subtree).
function parseMask(s) {
  let i = 0;
  const list = m => { for (;;) { item(m); if (s[i] === ',') i++; else return m; } };
  const item = m => {
    const st = i; while (i < s.length && /[\w*]/.test(s[i])) i++;
    const sub = m[s.slice(st, i)] ||= {};
    if (s[i] === '/') { i++; item(sub); } else if (s[i] === '(') { i++; list(sub); i++; }
  };
  return list({});
}
function applyMask(m, v) {
  if (!Object.keys(m).length || v === null || typeof v !== 'object') return v;
  if (Array.isArray(v)) return v.map(x => applyMask(m, x));
  const o = {}; for (const k of Object.keys(v)) if (m[k]) o[k] = applyMask(m[k], v[k]); return o;
}
// NEXT.refuse: YouTube answers a masked watch `next` with HTTP 400 (a field it no longer knows).
const NEXT_MASK = { refused: 0, served: 0 };
// A selected VISIONOS request can reject fields; unrelated background requests still answer.
const VISION_MASK = { refuse: null, refused: 0, served: 0 };

async function handle(route) {
  const req = route.request(), url = new URL(req.url());
  if (url.hostname.endsWith('.googlevideo.com')) return googlevideo(route, url);
  if (url.hostname === '127.0.0.1') return route.continue(); // the test debug-center collector
  if (url.hostname === 'www.youtube.com' && url.pathname === '/generate_204') { NET.probes++; return NET.down ? route.abort('internetdisconnected') : route.fulfill({ status: 204, body: '' }); }
  if (url.hostname === 'sponsor.ajay.app') return sponsorBlock(route, req, url);
  if (url.hostname !== 'www.youtube.com') {
    log.external.push(req.url());
    if (url.hostname === 'i.ytimg.com' && url.pathname.startsWith('/sb/')) {
      log.sb.push({ url: req.url(), t: Date.now() });
      const svg = sbSheet(url);
      return svg ? route.fulfill({ status: 200, contentType: 'image/svg+xml', headers: { 'cache-control': 'public, max-age=7200' }, body: svg }) : route.fulfill({ status: 404, body: '' });
    }
    if (/(^|\.)ytimg\.com$|ggpht\.com$|googleusercontent\.com$/.test(url.hostname)) {
      if (IMG.delay) { await new Promise(r => setTimeout(r, IMG.delay)); return route.fulfill({ status: 200, contentType: 'image/png', headers: { 'cache-control': 'public, max-age=7200' }, body: PNG }); }
      return route.fulfill({ status: 200, contentType: 'image/gif', body: GIF });
    }
    return route.abort();
  }
  const p = url.pathname;
  // Redline's "shown" signals (src/impress.rs): gzip'd log_event batches.
  if (p === '/youtubei/v1/log_event') {
    log.imp.push({ t: Date.now(), url: req.url(), headers: req.headers(), body: gunzipJSON(req.postDataBuffer() || Buffer.alloc(0)) });
    return route.fulfill({ status: 200, contentType: 'application/json', body: '{"responseContext":{}}' });
  }
  // YouTube's Home document fetched by the page (not a navigation): logged as the Home request it stands for.
  const homeDocReq = p === '/' && req.method() === 'GET' && req.resourceType() !== 'document';
  if (homeDocReq && HOME.doc !== 'home') {
    log.api.push({ ep: 'browse', body: { browseId: 'FEwhat_to_watch' }, headers: req.headers(), t: Date.now(), url: req.url(), doc: HOME.doc });
    if (HOME.doc === 'redirect') return route.fulfill({ status: 302, headers: { location: 'https://consent.youtube.com/m?continue=https%3A%2F%2Fwww.youtube.com%2F' }, body: '' });
    return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: '<!DOCTYPE html><html><body><h1>Before you continue to YouTube</h1></body></html>' });
  }
  if (p.startsWith('/youtubei/v1/') || homeDocReq) {
    const ep = homeDocReq ? 'browse' : p.slice('/youtubei/v1/'.length);
    const body = homeDocReq ? { browseId: 'FEwhat_to_watch' } : req.postDataJSON() || {};
    const docN = homeDocReq ? ++HOME.docs : 0;
    log.api.push({ ep, body, postData: req.postData(), headers: req.headers(), t: Date.now(), url: req.url(), ...(homeDocReq ? { doc: 'home', eid: 'e2eHomeEvent' + String(docN).padStart(4, '0') } : {}) });
    // (The Home document's answer: the same data inside YouTube's page.)
    const ct = homeDocReq ? 'text/html; charset=utf-8' : 'application/json';
    const wrap = out => homeDocReq ? homeDoc(out, docN) : out;
    // expirehang1: every VISIONOS request after its first never answers (a background refresh that hangs).
    if (ep === 'player' && body.videoId === 'expirehang1' && body.context?.client?.clientName === 'VISIONOS' && log.api.filter(a => a.ep === 'player' && a.body.videoId === 'expirehang1' && a.body.context?.client?.clientName === 'VISIONOS').length > 1) return new Promise(() => {});
    const hs = HEDGE.stall;
    if (hs && hs.n > 0 && ep === hs.ep && !body.continuation && !isCheck(url) && (hs.home ? body.browseId === 'FEwhat_to_watch' : hs.q ? body.query === hs.q : body.videoId === hs.v)) {
      hs.n--;
      await new Promise(r => setTimeout(r, 6000));
      try { await route.fulfill({ status: 200, contentType: ct, body: wrap('{}') }); } catch {}
      return;
    }
    const masked = ep === 'next' && isCheck(url) && body.videoId && !body.continuation;
    const pageMask = ep === 'next' && !isCheck(url) && url.searchParams.get('fields');
    if (pageMask && NEXT.refuse) { NEXT_MASK.refused++; return route.fulfill({ status: 400, contentType: 'application/json', body: JSON.stringify({ error: { code: 400, message: 'Request contains an invalid argument.', status: 'INVALID_ARGUMENT' } }) }); }
    const visionMask = ep === 'player' && body.context?.client?.clientName === 'VISIONOS' && url.searchParams.get('fields');
    if (visionMask && VISION_MASK.refuse?.(body, req.headers())) { VISION_MASK.refused++; return route.fulfill({ status: 400, contentType: 'application/json', body: '{"error":{"code":400,"status":"INVALID_ARGUMENT"}}' }); }
    if (masked && FX.checkDelay) {
      const home = /H\dz$/.test(body.videoId); // Home fixture ids (counted apart from other pages' checks)
      FX.checksInFlight++; FX.checksMax = Math.max(FX.checksMax, FX.checksInFlight);
      if (home) { FX.homeInFlight = (FX.homeInFlight || 0) + 1; FX.homeMax = Math.max(FX.homeMax || 0, FX.homeInFlight); }
      await new Promise(r => setTimeout(r, FX.checkDelay));
      FX.checksInFlight--; if (home) FX.homeInFlight--;
    }
    if (LIBRARY.on) {
      if (ep === 'browse' && (body.browseId === 'VLWL' || body.continuation?.startsWith('WLLIB:'))) return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(libraryPage(body.continuation ? +body.continuation.split(':')[1] : 0, body.context?.client?.hl)) });
      if (ep === 'playlist/get_add_to_playlist') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ contents: [{ addToPlaylistRenderer: { playlists: [{ playlistAddToOptionRenderer: { playlistId: 'WL', title: { simpleText: 'Watch later' }, containsSelectedVideos: LIBRARY.ids.includes(body.videoIds[0]) ? 'ALL' : 'NONE' } }] } }] }) });
      if (ep === 'browse/edit_playlist' && body.playlistId === 'WL') {
        const a = body.actions[0], sort = a.action === 'ACTION_SET_PLAYLIST_VIDEO_ORDER';
        if (LIBRARY.delay && !sort) await new Promise(r => setTimeout(r, LIBRARY.delay));
        if ((sort && LIBRARY.sortFail) || (!sort && LIBRARY.fail)) return route.fulfill({ status: 503, contentType: 'application/json', body: '{"error":{"message":"Try again"}}' });
        if (sort) { assert.equal(a.playlistVideoOrder, 1); assert.equal(body.params, 'fixtureOrderContext'); LIBRARY.newest = true; }
        else if (a.action === 'ACTION_REMOVE_VIDEO_BY_VIDEO_ID') LIBRARY.ids = LIBRARY.ids.filter(id => id !== a.removedVideoId);
        else if (a.action === 'ACTION_ADD_VIDEO' && !LIBRARY.ids.includes(a.addedVideoId)) LIBRARY.ids.push(a.addedVideoId);
        return route.fulfill({ status: 200, contentType: 'application/json', body: '{"status":"STATUS_SUCCEEDED"}' });
      }
    }
    // The card Watch later checks use their own membership mock (FX.wlMock).
    const act = FX.wlMock && /^(playlist\/get_add_to_playlist|browse\/edit_playlist)$/.test(ep) ? null : actFixture(ep, body);
    if (act) {
      if (ACTS.delay && /^(like|subscription|browse\/edit_playlist)/.test(ep)) await new Promise(r => setTimeout(r, ACTS.delay));
      return route.fulfill({ status: act.status, contentType: 'application/json', body: act.body });
    }
    if (ep === 'search' && IMG.search) await new Promise(r => setTimeout(r, IMG.search));
    if (ep === 'visitor_id') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ responseContext: { visitorData: FRESH_VD, serviceTrackingParams: [] } }) });
    // A hung player request: the first VISIONOS request for this video never answers (the hedge must).
    if (ep === 'player' && body.videoId === 'vishang0001' && body.context?.client?.clientName === 'VISIONOS' && !FX.visHung) { FX.visHung = true; return new Promise(() => {}); }
    if (ep === 'player' && FX.bot) log.api.at(-1).done = Date.now(); // answered now (the recovery tests order calls by it)
    if (ep === 'player' && FX.bot && body.context?.client?.clientName === 'VISIONOS' && botChecked(body, req.headers())) return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(BOT) });
    let out = apiFixture(ep, body);
    if (out && out.status) return route.fulfill({ status: out.status, contentType: 'application/json', body: out.body });
    if (out && ACTS.signedIn && (ep === 'next' || ep === 'browse') && !body.continuation) { const o = JSON.parse(out); o.topbar = SIGNED_TOPBAR; out = JSON.stringify(o); }
    const home = ep === 'browse' && (body.browseId === 'FEwhat_to_watch' || body.continuation?.startsWith('HOME'));
    // A dropped connection fails Home at once; HOME.stall: the next N first-page requests hang (a hedge test).
    if (home && NET.down) return route.abort('internetdisconnected');
    if (home && !body.continuation && HOME.stall > 0) {
      HOME.stall--;
      await new Promise(r => setTimeout(r, 12000));
      return route.fulfill({ status: 200, contentType: ct, body: wrap(out) }).catch(() => { HOME.aborted++; });
    }
    if (home && (HOME.latency || (HOME.delay && !body.continuation))) await new Promise(r => setTimeout(r, HOME.latency + (body.continuation ? 0 : HOME.delay)));
    if (ep === 'next' && NEXT.latency && !body.continuation && !masked) await new Promise(r => setTimeout(r, NEXT.latency));
    if (ep === 'next' && NEXT.contLatency && body.continuation) await new Promise(r => setTimeout(r, NEXT.contLatency));
    if (out && pageMask) { NEXT_MASK.served++; out = JSON.stringify(applyMask(parseMask(pageMask), JSON.parse(out))); }
    if (out && visionMask) { VISION_MASK.served++; out = JSON.stringify(applyMask(parseMask(visionMask), JSON.parse(out))); }
    return out ? route.fulfill({ status: 200, contentType: ct, body: wrap(out) }).catch(() => {}) : route.fulfill({ status: 404, body: '{}' });
  }
  if (p.startsWith('/api/stats/')) { log.stats.push(req.url()); return route.fulfill({ status: 204, body: '' }); }
  if (p === '/api/timedtext') { log.timedtext.push(req.url()); return route.fulfill({ status: 200, contentType: 'application/json', body: json3 }); }
  if (p === '/complete/search') {
    log.suggest.push(req.url());
    return route.fulfill({ status: 200, contentType: 'application/json; charset=UTF-8', body: JSON.stringify([url.searchParams.get('q'), [['roblox', 0, [512]], ['roblox <b>evil</b>" x="', 0, [512]]], { k: 1 }]) });
  }
  if (p === '/sw.js_data') { log.api.push({ ep: 'sw.js_data' }); return route.fulfill({ status: 200, contentType: 'application/json', body: fixture('sw_js_data.txt') }); }
  if (p.startsWith('/embed/')) { log.embeds.push(req.url()); return route.fulfill({ status: 200, contentType: 'text/html', body: `<body style="background:#000;color:#fff">EMBED ${p.slice(7)}</body>` }); }
  if (p.startsWith('/s/')) { log.assets.push(req.url()); return route.fulfill({ status: 200, contentType: p.endsWith('.css') ? 'text/css' : 'text/javascript', body: p.endsWith('.css') ? '' : 'window.__nativeBundle=(window.__nativeBundle||0)+1;' }); }
  if (req.resourceType() === 'document') {
    log.docs.push(req.url());
    // HOME.nav: the tab's own Home document is YouTube's Home page ('home') or a YouTube page without Home data
    // ('nodata'), as a navigation answers (the client reads it instead of asking again; src/app/homeready.rs).
    if (HOME.nav && p === '/' && !url.searchParams.has('yt-native')) {
      const docN = ++HOME.docs;
      log.api.push({ ep: 'browse', body: { browseId: 'FEwhat_to_watch' }, headers: req.headers(), t: Date.now(), url: req.url(), doc: HOME.nav === 'home' ? 'navigation' : HOME.nav, eid: 'e2eHomeEvent' + String(docN).padStart(4, '0') });
      const html = HOME.nav === 'home' ? homeDoc(apiFixture('browse', { browseId: 'FEwhat_to_watch' }), docN) : homeDoc('{}', docN).replace(/<script nonce="n1">var ytInitialData = [\s\S]*?<\/script>/, '');
      return route.fulfill({ status: 200, headers: { 'content-type': 'text/html; charset=utf-8', 'content-security-policy': CSP }, body: html });
    }
    if (url.searchParams.get('v') === 'ntvfixture1' && url.searchParams.get('yt-native') === '1') return route.fulfill({ status: 200, headers: { 'content-type': 'text/html; charset=utf-8', 'content-security-policy': CSP }, body: NATIVE_PAGE });
    return route.fulfill({ status: 200, headers: { 'content-type': 'text/html; charset=utf-8', 'content-security-policy': CSP }, body: STUB });
  }
  return route.fulfill({ status: 204, body: '' });
}

const results = [];
let dumpDebug = null, holdWatch = false;
// YTC_ONLY=<regex>: run only the matching steps (quicker iteration; some depend on earlier state).
const ONLY = process.env.YTC_ONLY ? new RegExp(process.env.YTC_ONLY) : null;
async function step(name, fn) {
  if (TIMING_ONLY || (ONLY && !ONLY.test(name))) return;
  if (name.startsWith('dev build: ') && !DEV_BUILD) { results.push(['skip', name]); console.log(`skip ${name} (public build)`); return; }
  const t = Date.now();
  try { await fn(); results.push(['ok', name, Date.now() - t]); console.log(`ok   ${name}`); }
  catch (e) {
    results.push(['FAIL', name]); console.log(`FAIL ${name}\n     ${e.stack.split('\n').slice(0, 8).join('\n     ')}`);
    if (/^own (player|live)/.test(name) && dumpDebug) await dumpDebug().catch(() => {});
  }
}
const median = a => { const s = a.filter(x => !Number.isNaN(x)).sort((x, y) => x - y); return s.length ? s[Math.floor(s.length / 2)] : NaN; };

(async () => {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'ytc-e2e-'));
  // The public build's worker ignores incident messages; a temporary copy gets a forwarder to the
  // test collector (named by the same storage key the development build uses), so the player and
  // action checks see their incidents in both builds. The copy is otherwise byte-identical.
  let load = EXT;
  if (!DEV_BUILD) {
    load = fs.mkdtempSync(path.join(os.tmpdir(), 'ytc-e2e-ext-'));
    fs.cpSync(EXT, load, { recursive: true, filter: f => !f.includes('_metadata') });
    fs.appendFileSync(path.join(load, 'background.js'), `
// ---- e2e only (appended to a temporary copy of the public build).
chrome.runtime.onMessage.addListener(m => {
  if (!m || (typeof m.ytcIncident !== 'string' && typeof m.ytcPerf !== 'string')) return;
  chrome.storage.local.get('ytcCollectorV1').then(g => g.ytcCollectorV1 && fetch(g.ytcCollectorV1, { method: 'POST', headers: { 'content-type': 'application/json' }, body: m.ytcIncident || m.ytcPerf })).catch(() => {});
});
`);
  }
  console.log(`extension: ${EXT} (${DEV_BUILD ? 'development' : 'public'} build)`);
  const ctx = await chromium.launchPersistentContext(profile, {
    headless: true, ...chromiumOptions(), viewport: { width: 1400, height: 900 },
    args: [`--disable-extensions-except=${load}`, `--load-extension=${load}`, '--autoplay-policy=no-user-gesture-required',
      // Voice search: a fake microphone (a beep), granted without the prompt.
      '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream'],
    // YTC_SCROLLBARS=1: classic always-visible scrollbars (macOS "Show scroll bars: Always"), which take layout width.
    ...(process.env.YTC_SCROLLBARS ? { ignoreDefaultArgs: ['--hide-scrollbars'] } : {}),
  });
  media = await ensureMedia(ctx);
  LIVE.fx = await ensureLive(ctx);
  await ctx.route('**/*', handle);
  await ctx.route('chrome-extension://**/*', route => route.continue());
  // Headless Chromium decodes in software and drops frames under load; the
  // dropped-frame watchdog has its own unit test, so keep it out of e2e timing.
  await ctx.addInitScript(() => { try { localStorage.setItem('ytc-test-nowatchdog', '1'); } catch {} });
  // Debug center: a test collector, so e2e incidents never reach the real one.
  const incidents = [];
  const collector = require('node:http').createServer((req, res) => {
    let body = ''; req.on('data', c => body += c); req.on('end', () => {
      res.writeHead(req.method === 'OPTIONS' ? 204 : 200, { 'access-control-allow-origin': '*', 'access-control-allow-headers': 'content-type', 'access-control-allow-private-network': 'true' });
      if (req.method === 'POST') { try { incidents.push(JSON.parse(body)); } catch {} }
      res.end('ok');
    });
  }).listen(0, '127.0.0.1');
  await new Promise(r => collector.once('listening', r));
  const sw = ctx.serviceWorkers()[0] || await ctx.waitForEvent('serviceworker');
  // The ready Home response, one per browser in the extension's memory-only session storage (src/ready.rs).
  const readyGet = () => sw.evaluate(() => chrome.storage.session.get('ytc:ready').then(o => o['ytc:ready'] ? JSON.parse(o['ytc:ready']) : null));
  const readySet = r => sw.evaluate(v => chrome.storage.session.set({ 'ytc:ready': v }), JSON.stringify(r));
  // `end`: first end the tab's document, whose Home fetched ahead may still be on its way (on a busy machine it
  // lands after the removal and the next Home paints it instead of the fixture under test).
  const readyDel = async end => { if (end) await page.goto('about:blank').catch(() => {}); return sw.evaluate(() => chrome.storage.session.remove('ytc:ready')); };
  const readyWait = async (min = 30, timeout = 8000) => {
    for (const t = Date.now(); ;) { const r = await readyGet(); if (r && r.i.length > min) return r; if (Date.now() - t > timeout) throw new Error('no ready response in time'); await new Promise(x => setTimeout(x, 100)); }
  };
  await sw.evaluate(port => chrome.storage.local.set({ ytcCollectorV1: `http://127.0.0.1:${port}/incident` }), collector.address().port);
  // Voice search: a local stand-in for Deepgram's streaming endpoint (the relay is pointed there), so the
  // suite never reaches the real service.
  const VKEY = 'e2eDeepgramKey0123456789abcdefabcdef0123';
  const dg = await wsMock.start({ key: VKEY });
  await sw.evaluate(u => chrome.storage.local.set({ ytcSockTestV1: u }), dg.url);
  const page = ctx.pages()[0] || await ctx.newPage();
  // The client never leaves for YouTube's page by itself: every native watch
  // document must be one the test opened itself or asked for with the panel's Open on YouTube.
  const nativeOk = [];
  const wrapGoto = p => { const g = p.goto.bind(p); p.goto = (u, o) => { if (String(u).includes('yt-native=1')) nativeOk.push(String(u)); return g(u, o); }; };
  wrapGoto(page); ctx.on('page', wrapGoto);
  // Fetch priorities as Chromium assigns them (background preloads must be Low).
  const prio = [];
  const cdp = await ctx.newCDPSession(page);
  await cdp.send('Network.enable');
  cdp.on('Network.requestWillBeSent', e => {
    if (e.request.url.includes('/youtubei/v1/browse')) prio.push({ post: e.request.postData || '', p: e.request.initialPriority });
    // YouTube's Home document fetched by the page: the Home request (FEwhat_to_watch) it stands for.
    else if (e.request.url === 'https://www.youtube.com/' && e.type === 'Fetch') prio.push({ post: 'FEwhat_to_watch (document)', p: e.request.initialPriority });
  });
  const errors = [];
  page.on('pageerror', e => errors.push(String(e)));
  page.on('console', m => { if (m.type() === 'error' && !/ERR_BLOCKED_BY_CLIENT|Failed to load resource/.test(m.text())) errors.push(m.text()); });
  dumpDebug = async () => {
    console.log('     page: ' + page.url() + ' gv: ' + JSON.stringify(log.gv.slice(-6)));
    const inc = incidents[incidents.length - 1];
    if (inc && inc.player) console.log('     last incident: ' + inc.reason + '\n       ' + inc.player.notes.join('\n       '));
    if (inc && inc.player && process.env.YTC_DUMP) console.log('     PLAYER ' + JSON.stringify(inc.player));
    if (!(await page.locator('#ytc-video').count())) return;
    await page.keyboard.press('Backquote');
    console.log('     debug overlay:\n' + (await page.locator('#ytc-debug').textContent()).split('\n').filter(l => /player|ms  |fallback/.test(l)).join('\n'));
    await page.keyboard.press('Backquote');
  };
  // Incidents since `from`, without the tab's `perf` timing reports: those are sent when a
  // document is left (at most once a minute) and can land during any later step.
  // Speed telemetry (development builds): schema-2 event batches (src/telemetry.rs).
  const telBatches = from => incidents.slice(from).filter(x => x.kind === 'perf' && x.schema === 2);
  const problems = from => incidents.slice(from).filter(x => x.kind !== 'perf').map(x => x.kind + ': ' + x.reason);
  const ds = () => page.evaluate(() => ({ ...document.documentElement.dataset }));
  const apiCount = ep => log.api.filter(a => a.ep === ep && !a.body?.continuation).length;
  // Wait until the extension's content script is registered.
  for (let i = 0; i < 20; i++) {
    await page.goto('https://www.youtube.com/results?search_query=warmup', { waitUntil: 'commit' });
    await page.waitForTimeout(300);
    if (await page.locator('#ytc-top').count()) break;
    await page.waitForTimeout(250);
  }

  await step('search page: app stopped, shell + results from InnerTube', async () => {
    log.api.length = 0;
    await page.goto('https://www.youtube.com/results?search_query=roblox+studio+animation', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    assert.equal(await page.evaluate(() => window.__ytAppRan), undefined, 'native inline script ran');
    assert.equal(await page.evaluate(() => window.__nativeBundle), undefined, 'native bundle ran');
    assert.equal(await page.locator('ytd-app').count(), 0);
    assert.equal(await page.evaluate(() => document.documentElement.getAttribute('style')), null, 'YouTube <html> attributes cleared');
    const n = await page.locator('#ytc-list .c').count();
    assert.ok(n >= 12, `cards ${n}`);
    // The recorded "People also watched" shelf is YouTube recommending: none of its videos are results.
    assert.equal(await page.locator('#ytc-list .c[data-v="AH30avEEC9A"], #ytc-list .c[data-v="ILaV0JYHzwY"]').count(), 0, 'People also watched dropped');
    const titles = await page.locator('#ytc-list .c .t').allTextContents();
    assert.ok(titles.includes('Roblox Animation Tutorials'), 'playlist card');
    assert.ok(!titles.some(t => t.includes('How to Make Roblox Animations! #roblox')), 'Short was hidden');
    const s = log.api.find(a => a.ep === 'search');
    assert.equal(s.body.query, 'roblox studio animation');
    assert.equal(s.body.context.client.clientName, 'WEB');
    assert.equal(s.headers['x-youtube-client-name'], '1');
    assert.ok(s.headers['x-youtube-client-version'].startsWith('2.'));
    assert.equal(s.headers['x-origin'], 'https://www.youtube.com');
    assert.equal(s.headers['x-goog-authuser'], '0');
    assert.equal(s.headers.authorization, undefined, 'no SAPISID cookie → no Authorization');
    assert.equal(await page.inputValue('#ytc-q'), 'roblox studio animation');
    assert.ok((await page.title()).includes('roblox studio animation'));
  });

  // Since site bundles are no longer network-blocked (see below), the preload
  // scanner can race window.stop() and download one; what matters is that YouTube's
  // app never executes on a client page.
  await step('YouTube site bundles and inline app never run on client pages', async () => {
    assert.equal(await page.evaluate(() => window.__nativeBundle), undefined, 'site bundle executed');
    assert.equal(await page.evaluate(() => window.__ytAppRan), undefined, 'inline app executed');
    assert.equal(await page.locator('#ytc-top').count(), 1);
  });

  // Site bundles are not network-blocked: YouTube's service worker fetches them
  // outside the page frame, so a block broke the native fallback page (found
  // 1 Nov 2026). window.stop() already keeps them from running on client pages.
  await step('DNR: site bundles, /s/player and /api/stats reachable; telemetry blocked', async () => {
    const r = await page.evaluate(async () => {
      const t = async u => { try { await fetch(u); return 'ok'; } catch { return 'blocked'; } };
      return { desktop: await t('/s/desktop/abc/jsbin/x.js'), underscore: await t('/s/_/ytmainappweb/x.js'), player: await t('/s/player/abc/player_ias.vflset/en_US/base.js'), stats: await t('/api/stats/watchtime?x=1'), log: await t('/youtubei/v1/log_event?alt=json') };
    });
    assert.deepEqual(r, { desktop: 'ok', underscore: 'ok', player: 'ok', stats: 'ok', log: 'blocked' });
  });

  await step('infinite scroll loads a search continuation', async () => {
    // The sentinel is observed with a 1500px margin, so on a tall window the
    // first continuation can start before any scrolling.
    await page.evaluate(() => scrollTo(0, document.body.scrollHeight));
    // (14 cards on the first page now that the recorded "People also watched" shelf is dropped, + 10.)
    await page.waitForFunction(() => document.querySelectorAll('#ytc-list .c').length > 20, null, { timeout: 5000 });
    assert.ok(log.api.some(a => a.ep === 'search' && a.body.continuation));
    // The fixture continuation repeats itself: all duplicates → paging stops instead of looping.
    await page.waitForSelector('#ytc-more.done', { state: 'attached', timeout: 5000 });
    assert.ok(log.api.filter(a => a.ep === 'search' && a.body.continuation).length <= 2);
    await page.evaluate(() => scrollTo(0, 0));
  });

  let watchId;
  await step('hover prefetches next; click opens watch via SPA (no document load)', async () => {
    const docs = log.docs.length;
    const card = page.locator('#ytc-list .c').filter({ has: page.locator('a.th[href^="/watch"]') }).nth(1);
    watchId = new URL(await card.locator('a.th').getAttribute('href'), 'https://www.youtube.com').searchParams.get('v');
    await card.locator('a.th').hover();
    await page.waitForFunction(() => true);
    for (let i = 0; i < 20 && !log.api.some(a => a.ep === 'next' && a.body.videoId === watchId); i++) await page.waitForTimeout(25);
    assert.ok(log.api.some(a => a.ep === 'next' && a.body.videoId === watchId), 'prefetch on hover');
    const nexts = apiCount('next');
    await card.locator('a.t').click();
    await page.waitForSelector('#ytc-info h1.wt');
    assert.equal(new URL(page.url()).searchParams.get('v'), watchId);
    assert.equal(log.docs.length, docs, 'no document navigation');
    // (Other cards' next calls may follow: speculative prefetch of the related cards in view.)
    assert.equal(log.api.filter(a => a.ep === 'next' && !a.body.continuation && a.body.videoId === watchId).length, 1, 'prefetched next reused');
    void nexts;
    assert.equal(await page.locator('#ytc-info h1.wt').textContent(), 'How to make SMOOTH animations in ROBLOX STUDIO! [Moon Animator Tutorial]');
    await page.waitForSelector('#ytc-video');
    await page.waitForFunction(() => document.documentElement.dataset.ytcFirstFrame);
    const vis = log.api.filter(a => a.ep === 'player' && a.body.videoId === watchId && a.body.context.client.clientName === 'VISIONOS');
    assert.equal(vis.length, 1, 'VISIONOS prefetched on hover and reused by the click');
    assert.ok(await page.locator('#ytc-list .c').count() >= 20, 'related');
    assert.ok((await page.locator('#ytc-desc').textContent()).includes('MOON ANIMATOR'));
    assert.ok(!(await page.locator('#ytc-desc').textContent()).includes("In today's video"), 'description keeps only resource lines');
    assert.ok(await page.locator('#ytc-desc a[href^="https://create.roblox.com/"]').count() >= 1, 'redirect links unwrapped');
    assert.equal(log.embeds.length, 0, 'no embed');
  });

  await step('channel link uses known browseId (no resolve_url) and renders videos', async () => {
    const resolves = apiCount('navigation/resolve_url');
    await page.click('#ytc-info a.wc');
    await page.waitForSelector('.ch h1');
    assert.equal(new URL(page.url()).pathname, '/@monkeydeveloper');
    assert.equal(await page.locator('.ch h1').textContent(), 'MonkeyDev');
    // 30 from the first response (a continuation may already have appended more).
    assert.ok(await page.locator('#ytc-list .c').count() >= 30);
    assert.equal(apiCount('navigation/resolve_url'), resolves);
    const b = log.api.filter(a => a.ep === 'browse' && !a.body.continuation).pop();
    assert.equal(b.body.browseId, 'UCaZO-66UcqyVT9btiqdDryA');
    assert.equal(b.body.params, 'EgZ2aWRlb3PyBgQKAjoA');
  });

  await step('back/forward restore pages from memory without new requests', async () => {
    await page.waitForTimeout(300); // let the channel's first continuation settle
    // The watch page's player restarts on back/forward (its MediaSource cannot be restored); only page data must come from memory.
    // Speculative next calls for other cards in view are not page requests.
    const pageReqs = () => log.api.filter(a => a.ep !== 'sw.js_data' && a.ep !== 'player' && !a.body?.continuation && (a.ep !== 'next' || a.body.videoId === watchId));
    // Park the pointer on blank header space: hovering links prefetches pages (tested separately).
    await page.mouse.move(2, 2);
    await page.waitForTimeout(150);
    const n = pageReqs().length, docs = log.docs.length;
    await page.evaluate(() => history.back());
    await page.waitForFunction(id => location.search.includes(id) && document.querySelector('#ytc-info h1.wt'), watchId);
    await page.evaluate(() => history.back());
    await page.waitForFunction(() => location.pathname === '/results' && document.querySelectorAll('#ytc-list .c').length > 20);
    assert.equal(await page.inputValue('#ytc-q'), 'roblox studio animation');
    await page.evaluate(() => history.forward());
    await page.waitForFunction(() => document.querySelector('#ytc-info h1.wt'));
    assert.equal(pageReqs().length, n, 'no page requests for back/forward: ' + JSON.stringify(pageReqs().slice(n).map(a => [a.ep, a.body])));
    assert.equal(log.docs.length, docs);
  });

  await step('top-bar search submits through the SPA', async () => {
    const docs = log.docs.length;
    await page.fill('#ytc-q', 'xss');
    await page.press('#ytc-q', 'Enter');
    // Title is set when the results render (the previous watch page's related list also has > 5 cards).
    await page.waitForFunction(() => location.search === '?search_query=xss' && document.title.startsWith('xss') && document.querySelectorAll('#ytc-list .c').length > 5);
    assert.equal(log.docs.length, docs);
  });

  await step('hostile titles/descriptions are escaped', async () => {
    const t = await page.locator(`#ytc-list a.t[href="/watch?v=${EVIL_ID}"]`).textContent();
    assert.equal(t, EVIL);
    assert.equal(await page.locator('#ytc-main img[src="x"]').count(), 0);
    assert.equal(await page.locator('#ytc-main script').count(), 0);
    await page.click(`#ytc-list a.t[href="/watch?v=${EVIL_ID}"]`);
    // The header first shows the remembered card title, then the next response's title.
    await page.waitForFunction(e => document.querySelector('#ytc-info h1.wt')?.textContent === e && document.getElementById('ytc-desc'), EVIL);
    assert.equal(await page.locator('#ytc-info h1.wt').textContent(), EVIL);
    assert.ok((await page.locator('#ytc-desc').textContent()).includes(EVIL));
    assert.equal(await page.locator('#ytc-main a[href^="javascript"]').count(), 0);
    assert.equal(await page.locator('#ytc-main img[src="x"], #ytc-main script').count(), 0);
    assert.equal(await page.evaluate(() => window.__xss), undefined);
    assert.ok((await page.title()).startsWith('<img'), 'title set as text');
  });

  await step('watch next is field-masked (YouTube applies the mask, the page renders everything); a refused mask (HTTP 400) is resent whole at once, then no mask in that document', async () => {
    // The steps above ran on masked answers (the stub applies the mask like YouTube): title, related, description, chapters, comments.
    const pageNexts = log.api.filter(a => a.ep === 'next' && !a.body.continuation && !a.url.includes('fields=contents%2F'));
    assert.ok(pageNexts.length > 0 && pageNexts.every(a => a.url.includes('&fields=responseContext%2CcurrentVideoEndpoint')), 'every watch next carries the mask');
    assert.ok(NEXT_MASK.served > 0, 'masked answers served');
    assert.ok(log.api.filter(a => a.ep === 'next' && a.body.continuation).every(a => !new URL(a.url).searchParams.has('fields')), 'continuations unmasked');
    assert.ok(log.api.filter(a => a.ep === 'search' || a.ep === 'browse').every(a => !new URL(a.url).searchParams.has('fields')), 'other endpoints unmasked');
    const p2 = await ctx.newPage();
    NEXT.refuse = true; NEXT_MASK.refused = 0;
    const inc = incidents.length;
    try {
      const v = 'maskREF0001';
      const from = log.api.length;
      await p2.goto(`https://www.youtube.com/watch?v=${v}`, { waitUntil: 'commit' });
      await p2.waitForFunction(() => document.querySelector('#ytc-info h1.wt')?.textContent && document.querySelectorAll('#ytc-list .c').length >= 20);
      const mine = log.api.slice(from).filter(a => a.ep === 'next' && a.body.videoId === v && !a.body.continuation);
      assert.equal(mine.length, 2, 'refused once, resent once: ' + mine.map(a => a.url).join(' '));
      assert.ok(mine[0].url.includes('fields=') && !mine[1].url.includes('fields='), 'the resend is whole');
      assert.equal(mine[1].postData ?? JSON.stringify(mine[1].body), mine[0].postData ?? JSON.stringify(mine[0].body), 'same body');
      // Later watch pages in that document go out whole (no second refusal).
      await p2.locator('#ytc-list .c a.t[href^="/watch"]').first().click();
      await p2.waitForFunction(v => !location.search.includes(v) && document.querySelector('#ytc-info h1.wt'), v);
      await p2.waitForTimeout(300);
      assert.equal(NEXT_MASK.refused, 1, 'one refusal per document');
      assert.equal(incidents.length, inc, 'no incident (it would carry the page URL); telemetry has the 400');
    } finally {
      NEXT.refuse = false;
      await p2.close();
    }
  });

  await step('VISIONOS field mask: direct playback keeps formats; HTTP 400 retries the identical anonymous, fresh-visitor or account request and disables only that mask', async () => {
    const p2 = await ctx.newPage();
    const vision = (v, from) => log.api.slice(from).filter(a => a.ep === 'player' && a.body.videoId === v && a.body.context?.client?.clientName === 'VISIONOS');
    const hasMask = a => new URL(a.url).searchParams.has('fields');
    const frame = () => p2.waitForFunction(() => +document.documentElement.dataset.ytcFirstFrame > 0 && document.getElementById('ytc-video')?.readyState >= 2, null, { timeout: 8000 });
    const had = (await ctx.cookies('https://www.youtube.com')).find(c => c.name === 'SAPISID');
    const inc = incidents.length;
    try {
      const from = log.api.length, served = VISION_MASK.served;
      await p2.goto('https://www.youtube.com/watch?v=vismaskOK01', { waitUntil: 'commit' });
      await frame();
      const normal = vision('vismaskOK01', from);
      assert.equal(normal.length, 1);
      assert.equal(new URL(normal[0].url).searchParams.get('fields'), 'responseContext,playabilityStatus,streamingData,captions,videoDetails');
      assert.ok(VISION_MASK.served > served, 'the stub applied the mask to a playable answer');
      assert.ok(log.api.slice(from).some(a => a.ep === 'player' && a.body.context?.client?.clientName === 'WEB' && !hasMask(a)), 'WEB metadata call stays whole');
      // Each navigation is a new document, so each route starts with its own enabled mask.
      for (const [v, route] of [['vismaskAN01', 'anonymous'], ['vismaskFR01', 'fresh'], ['vismaskAC01', 'account']]) {
        // Only this video's recovery is under test; unrelated related-card prefetches
        // must not turn the document's separate bot-check hold on afterwards.
        FX.botVideo = v;
        FX.bot = route === 'fresh' ? 'visitor' : route === 'account' ? 'account' : null;
        if (route === 'account') await ctx.addCookies([{ name: 'SAPISID', value: 'e2e-mask-sapisid', domain: '.youtube.com', path: '/', secure: true }]);
        const selected = (b, h) => b.videoId === v && (route === 'account' ? !!h.authorization : route === 'fresh' ? b.context.client.visitorData === FRESH_VD : true);
        VISION_MASK.refuse = selected;
        const mark = log.api.length, refused = VISION_MASK.refused;
        await p2.goto(`https://www.youtube.com/watch?v=${v}`, { waitUntil: 'commit' });
        await frame();
        const pair = vision(v, mark).filter(a => selected(a.body, a.headers));
        assert.equal(pair.length, 2, `${route}: one refusal and one immediate resend`);
        assert.ok(hasMask(pair[0]) && !hasMask(pair[1]), `${route}: resend is whole`);
        assert.equal(pair[0].postData, pair[1].postData, `${route}: exact body, including cpn/session fields`);
        assert.deepEqual(pair[0].headers, pair[1].headers, `${route}: exact auth and visitor headers`);
        assert.equal(VISION_MASK.refused, refused + 1);
        if (route !== 'account') assert.ok(pair.every(a => !a.headers.authorization && !a.headers.cookie), 'anonymous credentials remain omitted');
        else assert.match(pair[1].headers.authorization, /^SAPISIDHASH \d+_[0-9a-f]{40}$/);
        assert.ok(log.api.slice(mark).some(a => a.ep === 'next' && a.body.videoId === v && hasMask(a)), 'next mask remains enabled');
        // App's hover prefetch shares Own's disable for subsequent requests in this document.
        const later = v.slice(0, -1) + '2';
        await p2.waitForSelector('#ytc-list .c a.t[href^="/watch"]');
        await p2.mouse.move(2, 2);
        await p2.evaluate(id => document.querySelector('#ytc-list .c a.t[href^="/watch"]').setAttribute('href', '/watch?v=' + id), later);
        await p2.hover(`#ytc-list .c a.t[href="/watch?v=${later}"]`);
        for (let i = 0; i < 60 && !vision(later, mark).length; i++) await p2.waitForTimeout(20);
        const after = vision(later, mark);
        assert.equal(after.length, 1, `${route}: later prefetch asked once`);
        assert.ok(after.every(a => !hasMask(a)), 'document stays unmasked after refusal');
        FX.bot = null; FX.botVideo = null; VISION_MASK.refuse = null;
      }
      assert.deepEqual(problems(inc), [], 'a refused mask is not an incident');
    } finally {
      FX.bot = null; FX.botVideo = null; VISION_MASK.refuse = null;
      if (had) await ctx.addCookies([had]); else await ctx.clearCookies({ name: 'SAPISID' });
      await p2.close();
    }
  });

  await step('VISIONOS field mask: a refused hover prefetch retries whole; the click reuses it and future player requests stay unmasked', async () => {
    const p2 = await ctx.newPage();
    const v = 'vismaskPRE1', later = 'vismaskNXT1';
    const from = log.api.length, inc = incidents.length;
    const vision = id => log.api.slice(from).filter(a => a.ep === 'player' && a.body.videoId === id && a.body.context?.client?.clientName === 'VISIONOS');
    VISION_MASK.refuse = b => b.videoId === v;
    try {
      await p2.goto('https://www.youtube.com/results?search_query=vision+mask+prefetch', { waitUntil: 'commit' });
      await p2.waitForSelector('#ytc-list .c a.t');
      await p2.mouse.move(2, 2);
      await p2.evaluate(id => document.querySelector('#ytc-list .c a.t').setAttribute('href', '/watch?v=' + id), v);
      const link = p2.locator(`#ytc-list .c a.t[href="/watch?v=${v}"]`);
      await link.hover();
      for (let i = 0; i < 100 && log.gv.filter(g => g.v === v).length < 2; i++) await p2.waitForTimeout(20);
      assert.equal(log.gv.filter(g => g.v === v).length, 2, 'prefetch includes both media tracks');
      const pair = vision(v);
      assert.equal(pair.length, 2);
      assert.ok(new URL(pair[0].url).searchParams.has('fields') && !new URL(pair[1].url).searchParams.has('fields'));
      assert.equal(pair[0].postData, pair[1].postData, 'the exact prefetch body');
      assert.deepEqual(pair[0].headers, pair[1].headers, 'the exact prefetch headers');
      await link.click();
      await p2.waitForFunction(() => +document.documentElement.dataset.ytcFirstFrame > 0 && document.getElementById('ytc-video')?.readyState >= 2, null, { timeout: 8000 });
      assert.equal(vision(v).length, 2, 'the click consumed the successful prefetch');
      // A direct SPA request exercises Own with App's document-level disable.
      await p2.waitForSelector('#ytc-list .c a.t[href^="/watch"]');
      await p2.evaluate(id => document.querySelector('#ytc-list .c a.t[href^="/watch"]').setAttribute('href', '/watch?v=' + id), later);
      await p2.locator(`#ytc-list .c a.t[href="/watch?v=${later}"]`).evaluate(a => a.click());
      await p2.waitForFunction(id => new URLSearchParams(location.search).get('v') === id && +document.documentElement.dataset.ytcFirstFrame > 0, later, { timeout: 8000 });
      assert.equal(vision(later).length, 1);
      assert.ok(!new URL(vision(later)[0].url).searchParams.has('fields'));
      assert.deepEqual(problems(inc), [], 'no incident');
    } finally { VISION_MASK.refuse = null; await p2.close(); }
  });

  await step('a typing pause fetches the search results, so Enter paints from cache', async () => {
    await page.fill('#ytc-q', '');
    await page.type('#ytc-q', 'roblox warm');
    for (let i = 0; i < 30 && !log.api.some(a => a.ep === 'search' && a.body.query === 'roblox warm'); i++) await page.waitForTimeout(50);
    const before = log.api.filter(a => a.ep === 'search' && a.body.query === 'roblox warm').length;
    assert.equal(before, 1, 'fetched during the pause');
    await page.waitForTimeout(250);
    await page.press('#ytc-q', 'Enter');
    await page.waitForFunction(() => new URLSearchParams(location.search).get('search_query') === 'roblox warm' && document.querySelectorAll('#ytc-list .c').length > 5);
    assert.equal(log.api.filter(a => a.ep === 'search' && a.body.query === 'roblox warm').length, 1, 'Enter used the fetched results');
  });

  await step('search fetched ahead: low priority, one at a time (further typing aborts it), ≤ 10 a minute; arrowing onto a suggestion fetches its results and Enter paints them from memory; the text is never stored', async () => {
    const failed = [];
    const onFail = r => { if (r.url().includes('/youtubei/v1/search')) failed.push(r.postDataJSON()?.query); };
    page.on('requestfailed', onFail);
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Network.enable');
    const prio = [];
    cdp.on('Network.requestWillBeSent', e => { if (e.request.url.includes('/youtubei/v1/search')) prio.push({ q: JSON.parse(e.request.postData || '{}').query, p: e.request.initialPriority }); });
    const searches = q => log.api.filter(a => a.ep === 'search' && a.body.query === q && !a.body.continuation).length;
    try {
      IMG.search = 1500;
      await page.fill('#ytc-q', '');
      await page.type('#ytc-q', 'cats warm');
      for (let i = 0; i < 30 && !searches('cats warm'); i++) await page.waitForTimeout(25);
      assert.equal(searches('cats warm'), 1, 'fetched during the pause');
      await page.type('#ytc-q', 'er');
      for (let i = 0; i < 40 && !failed.includes('cats warm'); i++) await page.waitForTimeout(25);
      assert.ok(failed.includes('cats warm'), 'further typing aborts the search fetched ahead');
      for (let i = 0; i < 40 && !searches('cats warmer'); i++) await page.waitForTimeout(25);
      assert.equal(searches('cats warmer'), 1, 'the new text is fetched after its own pause');
      assert.ok(prio.filter(x => x.q === 'cats warmer').every(x => x.p === 'Low'), 'low priority: ' + JSON.stringify(prio));
      IMG.search = 0;
      // Arrowing onto a suggestion: its results are fetched after a short rest; Enter then needs no request.
      await page.fill('#ytc-q', '');
      await page.type('#ytc-q', 'rob');
      await page.waitForSelector('#ytc-sug li');
      await page.waitForTimeout(400);
      await page.keyboard.press('ArrowDown');
      const pick = await page.inputValue('#ytc-q');
      assert.ok(pick && pick !== 'rob', 'a row is chosen: ' + pick);
      for (let i = 0; i < 30 && !searches(pick); i++) await page.waitForTimeout(25);
      assert.equal(searches(pick), 1, 'the chosen suggestion is fetched ahead');
      await page.waitForTimeout(150);
      await page.keyboard.press('Enter');
      await page.waitForFunction(q => new URLSearchParams(location.search).get('search_query') === q && document.querySelectorAll('#ytc-list .c').length > 5, pick);
      assert.equal(searches(pick), 1, 'Enter used the results fetched ahead');
      // Fetched ahead, never submitted: in memory only (no tab or extension storage holds the text).
      const stored = await page.evaluate(async () => JSON.stringify(sessionStorage) + JSON.stringify(localStorage));
      assert.ok(!stored.includes('cats warm'), 'not in this tab\'s storage');
      const ext = await sw.evaluate(() => new Promise(r => chrome.storage.local.get(null, x => r(JSON.stringify(x)))));
      assert.ok(!ext.includes('cats warm'), 'not in chrome.storage');
    } finally {
      IMG.search = 0;
      page.off('requestfailed', onFail);
      await cdp.detach().catch(() => {});
    }
  });

  await step('direct channel load resolves the handle', async () => {
    const resolves = apiCount('navigation/resolve_url');
    await page.goto('https://www.youtube.com/@monkeydeveloper/videos', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    assert.equal(apiCount('navigation/resolve_url'), resolves + 1);
    assert.equal(log.api.filter(a => a.ep === 'navigation/resolve_url').pop().body.url, 'https://www.youtube.com/@monkeydeveloper');
    // Preloaded items enter the DOM in chunks as the user scrolls.
    await page.waitForFunction(() => { scrollTo(0, document.body.scrollHeight); return document.querySelectorAll('#ytc-list .c').length >= 60; }, null, { timeout: 5000, polling: 100 });
  });

  await step('playlist page', async () => {
    await page.goto('https://www.youtube.com/playlist?list=PL_pGtHsOvhtxX3WHo-Hi268L78XBziem_', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    assert.equal(await page.locator('.feed h1').textContent(), 'Roblox Animation Tutorials');
    assert.equal(await page.locator('#ytc-list .c').count(), 6);
    // (Home/Subscriptions may refresh in the background meanwhile.)
    assert.equal(log.api.filter(a => a.ep === 'browse' && !a.body.continuation && !/^FE(what_to_watch|subscriptions)$/.test(a.body.browseId)).pop().body.browseId, 'VLPL_pGtHsOvhtxX3WHo-Hi268L78XBziem_');
  });

  await step('signed-out Home shows the search-first page; WL asks to sign in', async () => {
    await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    await page.waitForSelector('.hero');
    assert.ok((await page.locator('.hero').textContent()).includes('Your YouTube history is off'));
    assert.equal(await page.getAttribute('#ytc', 'data-nav'), 'home');
    const n = log.api.length;
    await page.click('nav a[data-nav=wl]');
    await page.waitForSelector('.msg');
    assert.ok((await page.locator('.msg h1').textContent()).includes('Sign in'));
    assert.equal(log.api.length, n, 'no request without a session');
  });

  await step('Home preloads 100 videos in the background (Low priority); the DOM gets them in chunks as you scroll', async () => {
    HOME.rich = true;
    await page.evaluate(() => sessionStorage.clear());
    await readyDel();
    const before = log.api.length; prio.length = 0;
    await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    await page.waitForFunction(() => +document.documentElement.dataset.ytcItems >= 100, null, { timeout: 8000 });
    await page.waitForTimeout(300);
    const shown = await page.locator('#ytc-list .c').count();
    assert.ok(shown >= 30 && shown < 100, `cards in DOM ${shown}: preloaded items wait for scrolling`);
    const conts = log.api.slice(before).filter(a => a.body?.continuation?.startsWith('HOME'));
    assert.ok(conts.length >= 3 && conts.length <= 4, `continuations ${conts.length}`);
    const bg = prio.filter(x => x.post.includes('HOMECONT'));
    assert.ok(bg.filter(x => x.p === 'Low').length >= 2, JSON.stringify(bg.map(x => x.p)));
    assert.equal(prio.find(x => x.post.includes('FEwhat_to_watch'))?.p, 'High', 'first page at normal priority');
    // Scrolling needs no further requests up to 100 cards.
    const n = log.api.length;
    await page.waitForFunction(() => { scrollBy(0, 1200); return document.querySelectorAll('#ytc-list .c').length >= 100; }, null, { timeout: 5000, polling: 50 });
    assert.ok(log.api.slice(n).filter(a => a.body?.continuation).length <= 1, 'scroll used preloaded items');
    await page.evaluate(() => scrollTo(0, 0));
  });

  await step('thumbnails: srcset w descriptors (vi_webp 320w … 720w), sizes, first row fetchpriority=high, lazy below, preconnect', async () => {
    const imgs = await page.$$eval('#ytc-list .c a.th img', els => els.map(e => ({ src: e.getAttribute('src'), srcset: e.getAttribute('srcset') || '', sizes: e.getAttribute('sizes') || '', loading: e.getAttribute('loading'), fp: e.getAttribute('fetchpriority'), dec: e.getAttribute('decoding') })));
    assert.ok(imgs.length >= 100);
    assert.ok(imgs.every(i => i.dec === 'async'));
    const f = imgs[0];
    assert.match(f.srcset, /^https:\/\/i\.ytimg\.com\/vi_webp\/[\w-]{11}\/mqdefault\.webp 320w, https:\/\/i\.ytimg\.com\/vi\/[^ ]+ 360w, https:\/\/i\.ytimg\.com\/vi\/[^ ]+ 720w$/, f.srcset);
    // `sizes` is the card's real width: at 1400 px a 1x screen takes the 320w webp, not the 720w file.
    const w = await page.$eval('#ytc-list .c a.th img', e => e.getBoundingClientRect().width);
    assert.ok(f.sizes.includes('(max-width:1700px) calc(25vw - 75px)'), f.sizes);
    const said = await page.evaluate(() => innerWidth * 0.25 - 75); // 1400 px: four columns
    assert.ok(Math.abs(said - w) < 12 && said < 320, `sizes says ${said} px, the card is ${w} px`);
    assert.ok((await page.$eval('#ytc-list .c a.th img', e => e.currentSrc)).includes('/mqdefault.webp'), '1x: the 320w file');
    assert.equal(f.loading, null, 'first screen not lazy');
    // 1400 px viewport → 4 grid columns (5 above 1700 px) → first row high priority.
    assert.deepEqual(imgs.slice(0, 6).map(i => i.fp), ['high', 'high', 'high', 'high', null, null]);
    assert.ok(imgs.slice(30).every(i => i.loading === 'lazy' && !i.fp));
    const pre = await page.$$eval('link[rel=preconnect]', ls => ls.map(l => l.href));
    // Image hosts from the shell; video servers are warmed separately once a player response is known.
    assert.deepEqual(pre.filter(h => !h.endsWith('.googlevideo.com/')), ['https://i.ytimg.com/', 'https://yt3.ggpht.com/']);
  });

  await step('fast scrolling never reaches the end of Home while more is on its way: two continuations ahead, asked at once while scrolling (400 ms YouTube)', async () => {
    HOME.rich = true; HOME.latency = 400;
    try {
      await page.evaluate(() => sessionStorage.clear());
      await readyDel(true);
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForFunction(() => +document.documentElement.dataset.ytcItems >= 100, null, { timeout: 12000 });
      await page.waitForTimeout(300);
      // A fast flick-scroll (about 5,000 px/s; YTC_STEP changes it) down to card 170 of 180. "Waited": the end of the list in view
      // while more is still loading.
      const r = await page.evaluate(async STEP => {
        const more = document.getElementById('ytc-more'); let since = 0, waited = 0, waits = 0;
        const io = new IntersectionObserver(es => { for (const e of es) { if (e.isIntersecting && !more.classList.contains('done')) { if (!since) { since = performance.now(); waits++; } } else if (since) { waited += performance.now() - since; since = 0; } } });
        io.observe(more);
        const t0 = performance.now();
        while (document.querySelectorAll('#ytc-list .c').length < 170 && performance.now() - t0 < 20000) { scrollBy(0, STEP); await new Promise(r => setTimeout(r, 120)); }
        if (since) waited += performance.now() - since;
        io.disconnect();
        return { waits, waited: Math.round(waited), cards: document.querySelectorAll('#ytc-list .c').length, ms: Math.round(performance.now() - t0) };
      }, +(process.env.YTC_STEP || 600));
      console.log('  fast scroll through Home: ' + JSON.stringify(r));
      assert.ok(r.cards >= 170, JSON.stringify(r));
      assert.ok(r.waited < 200, `the end was in view for ${r.waited} ms (${r.waits}×)`);
      await page.evaluate(() => scrollTo(0, 0));
    } finally { HOME.latency = 0; }
  });

  await step('scrolling a watch page: the related column\'s next page is on its way before its end comes into view (900 ms YouTube)', async () => {
    NEXT.contLatency = 900;
    try {
      await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      await page.waitForTimeout(2500);
      const r = await page.evaluate(async STEP => {
        const more = document.getElementById('ytc-more'); let since = 0, waited = 0, waits = 0;
        const io = new IntersectionObserver(es => { for (const e of es) { if (e.isIntersecting && !more.classList.contains('done')) { if (!since) { since = performance.now(); waits++; } } else if (since) { waited += performance.now() - since; since = 0; } } });
        io.observe(more);
        const t0 = performance.now();
        while (document.querySelectorAll('#ytc-list .c').length < 100 && performance.now() - t0 < 20000) { scrollBy(0, STEP); await new Promise(r => setTimeout(r, 120)); }
        if (since) waited += performance.now() - since;
        io.disconnect();
        return { waits, waited: Math.round(waited), cards: document.querySelectorAll('#ytc-list .c').length, ms: Math.round(performance.now() - t0) };
      }, +(process.env.YTC_STEP || 250));
      console.log('  scroll through related: ' + JSON.stringify(r));
      assert.ok(r.cards >= 100, JSON.stringify(r));
      // 0.13.6 (nothing ahead on watch pages): the end in view 3× for ~570 ms at this speed.
      assert.ok(r.waited < 200, `the end was in view for ${r.waited} ms (${r.waits}×)`);
    } finally { NEXT.contLatency = 0; await page.evaluate(() => scrollTo(0, 0)).catch(() => {}); }
  });

  await step('Home reload: YouTube\'s answer on the first frame, from the ready response (fetched ahead once, used once, never the old Home first); expiry, sign-in change and watching discard it', async () => {
    await page.evaluate(() => scrollTo(0, 0));
    const wasRich = HOME.rich;
    HOME.rich = true; HOME.stamp = true;
    // Every document: the first frame that shows Home cards, and every write of #ytc-main.
    await page.addInitScript(() => {
      const w = window.__home = { first: null, frames: 0, writes: 0 };
      const tick = () => {
        const t = document.querySelector('#ytc-list .c[data-v] .t');
        if (t && w.first === null) { w.first = t.textContent; w.firstAt = performance.now(); w.upd = document.getElementById('ytc-main')?.dataset.upd || ''; }
        if (++w.frames < 2000) requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
      new MutationObserver(ms => { for (const m of ms) if (m.target.id === 'ytc-main') w.writes++; }).observe(document, { childList: true, subtree: true });
    });
    const homeReqs = () => log.api.filter(a => a.ep === 'browse' && a.body?.browseId === 'FEwhat_to_watch');
    const stamp = t => +((/Fresh #(\d+)/.exec(t || '') || [])[1] || 0);
    const ready = async () => { const r = await readyGet(); return r && { n: r.i.length, first: r.f, title: r.i[0].t, at: r.t, cont: !!r.c, eid: r.e, track: (r.k || []).length }; };
    const waitReady = () => readyWait();
    const home = () => page.evaluate(() => ({ ...window.__home, src: document.documentElement.dataset.ytcHomeSrc, title: document.querySelector('#ytc-list .c[data-v] .t')?.textContent }));
    try {
      // (A ready response fetched before the stamps were on would be the page: start without one.)
      await readyDel();
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForFunction(() => /Fresh #/.test(document.querySelector('#ytc-list .c .t')?.textContent || ''));
      // 1. After the paint, in idle time, the Home request once more (low priority) and its first continuation.
      await waitReady();
      const r1 = await ready();
      assert.ok(r1.first > 0 && r1.n > r1.first && r1.cont, `first page + its continuation ${JSON.stringify(r1)}`);
      assert.ok(/^e2eHomeEvent\d{4}$/.test(r1.eid) && r1.track > 30, `its answer id and the cards' tracking ids (the "shown" signals) ${JSON.stringify(r1)}`);
      const shown = stamp((await home()).title);
      assert.ok(stamp(r1.title) > shown, 'a new answer, not the one on screen');
      const lowReady = prio.filter(x => x.post.includes('FEwhat_to_watch')).at(-1);
      assert.equal(lowReady.p, 'Low', 'fetched at low priority');
      // 2. Reload: the ready response is the page on the first frame with cards, without a Home request
      //    before it; exactly one Home request afterwards (the next ready response), sent after the paint.
      HOME.delay = 700;
      const n0 = homeReqs().length, t0 = Date.now();
      await page.reload({ waitUntil: 'commit' });
      await page.waitForFunction(() => window.__home && window.__home.first !== null);
      let h = await home();
      assert.equal(stamp(h.first), stamp(r1.title), 'the first cards are the ready response');
      assert.equal(h.src, 'ready');
      assert.equal(await page.evaluate(() => document.documentElement.dataset.ytcRepaint), undefined, 'no snapshot repaint');
      assert.equal(await readyGet(), null, 'used up: taken out at once');
      const after = homeReqs().slice(n0);
      assert.ok(after.every(a => a.t - t0 > h.firstAt - 50), 'no Home request before the paint');
      // 3. Reloading again before the next one has landed: the skeleton and YouTube's answer (never the old Home first).
      await page.waitForTimeout(150);
      await page.reload({ waitUntil: 'commit' });
      await page.waitForFunction(() => window.__home && window.__home.first !== null, null, { timeout: 5000 });
      h = await home();
      assert.equal(h.src, 'network');
      assert.ok(stamp(h.first) > stamp(r1.title), `the first cards are a new answer (${h.first})`);
      HOME.delay = 0;
      // 4. Expired (older than 10 min): not used.
      await waitReady();
      { const r = await readyGet(); r.t -= 11 * 60000; await readySet(r); }
      await page.reload({ waitUntil: 'commit' });
      await page.waitForFunction(() => window.__home && window.__home.first !== null);
      assert.equal((await home()).src, 'network', 'an expired ready response is not used');
      // 5. Signing in (another account key): not used.
      await waitReady();
      await ctx.addCookies([{ name: 'SAPISID', value: 'e2e-ready-account', domain: '.youtube.com', path: '/', secure: true }]);
      try {
        await page.reload({ waitUntil: 'commit' });
        await page.waitForFunction(() => window.__home && window.__home.first !== null);
        assert.equal((await home()).src, 'network', 'another account: not used');
      } finally { await ctx.clearCookies({ name: 'SAPISID' }); }
      await page.reload({ waitUntil: 'commit' });
      await page.waitForFunction(() => window.__home && window.__home.first !== null);
      // 6. Opening a video discards it (watching changes what YouTube recommends); nothing is fetched
      //    ahead on the watch page; Home afterwards asks YouTube.
      await waitReady();
      await page.click('#ytc-list a.t[href^="/watch"]');
      await page.waitForSelector('#ytc-info h1.wt');
      assert.equal(await readyGet(), null, 'watching discards it');
      const n1 = homeReqs().length;
      await page.waitForTimeout(2500);
      assert.equal(homeReqs().length, n1, 'nothing fetched ahead on a watch page');
      await page.mouse.move(700, 450);
      const tc = Date.now();
      await page.click('#ytc-logo');
      await page.waitForFunction(() => /Fresh #/.test(document.querySelector('#ytc-list .c .t')?.textContent || ''));
      assert.ok(homeReqs().length > n1 && homeReqs()[n1].t >= tc - 100, 'Home asked YouTube when it was opened');
      assert.notEqual(await page.evaluate(() => document.documentElement.dataset.ytcHomeSrc), 'ready');
      // 7. Memory only: the extension's session storage (never on disk: checked with chrome.storage in the next
      //    steps), not this tab's sessionStorage or localStorage.
      await waitReady();
      assert.deepEqual(await page.evaluate(() => Object.keys(localStorage).filter(k => k.startsWith('ytc:'))), []);
      assert.equal(await page.evaluate(() => sessionStorage.getItem('ytc:ready')), null, 'not in the tab');
      assert.equal(await sw.evaluate(() => chrome.storage.local.get('ytc:ready').then(o => o['ytc:ready'])), undefined, 'not in chrome.storage.local');
    } finally { HOME.delay = 0; HOME.stamp = false; HOME.rich = wasRich; }
  });

  await step('Home request hedged: a stalled first request gets an identical second one at the deadline; the first answer paints once and the other is aborted; past 4 s the old Home shows dimmed ("Updating…"), offline it waits for the connection', async () => {
    const wasRich = HOME.rich;
    HOME.rich = true; HOME.stamp = true;
    const homeReqs = () => log.api.filter(a => a.ep === 'browse' && a.body?.browseId === 'FEwhat_to_watch');
    const failed = [];
    const onFail = r => { if (r.url().includes('/youtubei/v1/browse') || r.url() === 'https://www.youtube.com/') failed.push(r.failure()?.errorText); };
    page.on('requestfailed', onFail);
    try {
      await readyDel();
      await page.evaluate(() => { localStorage.setItem('ytc-test-noready', '1'); sessionStorage.setItem('ytc:bt', '120,140,150,160,170,180'); });
      // 1. The first request hangs: the hedge goes out at this tab's p90 (clamped to ≥ 0.7 s) and paints.
      HOME.stall = 1; HOME.aborted = 0;
      const n0 = homeReqs().length;
      await page.reload({ waitUntil: 'commit' });
      await page.waitForFunction(() => window.__home && window.__home.first !== null, null, { timeout: 5000 });
      const h = await page.evaluate(() => ({ ...window.__home, hedge: document.documentElement.dataset.ytcHedge }));
      const reqs = homeReqs().slice(n0);
      assert.equal(reqs.length, 2, 'the request and its hedge');
      assert.equal(JSON.stringify(reqs[0].body), JSON.stringify(reqs[1].body), 'identical requests');
      const gap = reqs[1].t - reqs[0].t;
      // Measured where the fixtures see the requests: a busy machine can log the first one late (the hedge's own
      // deadline is ≥ 700 ms from its send in Rust), so the lower bound leaves room for that.
      assert.ok(gap >= 500 && gap < 1200, `hedge after ${gap} ms`);
      assert.equal(h.hedge, 'hedge', 'the hedge won');
      assert.ok(h.firstAt < 2500, `painted at ${h.firstAt.toFixed(0)} ms`);
      await page.waitForFunction(() => document.querySelector('#ytc-list .c[data-v]'));
      await page.waitForTimeout(300);
      assert.ok(failed.includes('net::ERR_ABORTED'), `the first request was aborted (${failed.join()})`);
      const writes = await page.evaluate(() => window.__home.writes);
      assert.ok(writes <= 2, `#ytc-main written ${writes} times (skeleton shell, then the page once)`);
      // 2. Both late (> 4 s): the old Home dimmed with "Updating…", replaced in place when YouTube answers.
      HOME.delay = 5200;
      await page.evaluate(() => sessionStorage.setItem('ytc:bt', '3000,3000,3000,3000,3000,3000'));
      await page.reload({ waitUntil: 'commit' });
      await page.waitForFunction(() => document.getElementById('ytc-main')?.dataset.upd === '1', null, { timeout: 6000 });
      const dim = await page.evaluate(() => ({ cards: document.querySelectorAll('#ytc-list .c[data-v]').length, op: getComputedStyle(document.querySelector('#ytc-main>.feed')).opacity, note: getComputedStyle(document.getElementById('ytc-main'), '::after').content, at: performance.now() }));
      assert.ok(dim.cards > 10 && +dim.op < 0.6 && /Updating/.test(dim.note) && dim.at >= 3900, JSON.stringify(dim));
      await page.waitForFunction(() => document.getElementById('ytc-main').dataset.upd === '' && document.documentElement.dataset.ytcFresh, null, { timeout: 4000 });
      HOME.delay = 0;
      // 3. Offline: the old Home dimmed with "No connection", probed every 3 s; back → it loads by itself.
      NET.down = true; NET.probes = 0;
      await page.reload({ waitUntil: 'commit' });
      await page.waitForFunction(() => document.getElementById('ytc-main')?.dataset.upd === '2', null, { timeout: 5000 });
      assert.match(await page.evaluate(() => getComputedStyle(document.getElementById('ytc-main'), '::after').content), /No connection/);
      assert.equal(await page.evaluate(() => location.pathname), '/', 'stays in the client');
      NET.down = false;
      await page.waitForFunction(() => document.getElementById('ytc-main').dataset.upd === '' && document.documentElement.dataset.ytcFresh, null, { timeout: 7000 });
      assert.ok(NET.probes >= 1);
    } finally {
      page.off('requestfailed', onFail);
      HOME.stall = 0; HOME.delay = 0; HOME.stamp = false; HOME.rich = wasRich; NET.down = false;
      await page.evaluate(() => { localStorage.removeItem('ytc-test-noready'); sessionStorage.removeItem('ytc:bt'); }).catch(() => {});
    }
  });

  await step('back from another document restores Home at its scroll position without refetching', async () => {
    const noReadyBefore = await page.evaluate(() => localStorage.getItem('ytc-test-noready'));
    try {
      // Ready Home can refill its own continuation while Back restores this snapshot. Its
      // separately tested prefetch is unrelated to the restoration's zero-request guarantee.
      await page.evaluate(() => localStorage.setItem('ytc-test-noready', '1'));
      await page.waitForFunction(() => +document.documentElement.dataset.ytcItems >= 100, null, { timeout: 8000 });
      // The fresh feed may show only its first chunk (page shorter than 1500 + viewport):
      // scroll on until more cards have been appended and the position sticks.
      await page.waitForFunction(() => { scrollTo(0, 1500); return scrollY === 1500; }, null, { timeout: 8000, polling: 50 });
      await page.waitForTimeout(100);
      await page.goto('https://www.youtube.com/results?search_query=back+test', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      const n = apiCount('browse');
      await page.goBack({ waitUntil: 'commit' });
      await page.waitForFunction(() => document.querySelectorAll('#ytc-list .c').length >= 30 && location.pathname === '/');
      await page.waitForTimeout(300);
      const y = await page.evaluate(() => scrollY);
      assert.ok(Math.abs(y - 1500) < 60, `scrollY ${y}`);
      assert.equal(apiCount('browse'), n, 'no Home request');
      await page.evaluate(() => scrollTo(0, 0));
    } finally {
      await page.evaluate(previous => {
        if (previous === null) localStorage.removeItem('ytc-test-noready');
        else localStorage.setItem('ytc-test-noready', previous);
      }, noReadyBefore);
    }
  });

  await step('feeds and continuation tokens never reach chrome.storage or localStorage', async () => {
    const ls = await page.evaluate(() => Object.keys(localStorage).filter(k => k.startsWith('ytc:')));
    assert.deepEqual(ls, []);
    const dir = path.join(profile, 'Default', 'Local Extension Settings');
    const bytes = fs.readdirSync(dir).flatMap(x => fs.readdirSync(path.join(dir, x)).map(f => path.join(dir, x, f))).filter(f => fs.statSync(f).isFile()).map(f => fs.readFileSync(f).toString('latin1')).join('');
    assert.ok(bytes.includes('ytcConfigV1'), 'storage readable (config present)');
    assert.ok(!bytes.includes(HOME_ID0) && !bytes.includes('HOMECONT') && !bytes.includes('ytc:snap'), 'feed data in chrome.storage');
  });

  // Tracking ids of the first Home page's cards (homeFirst): id → [grid item's, card's own].
  const homeTrack = (() => {
    const m = {};
    (function f(o) {
      if (!o || typeof o !== 'object') return;
      const lk = o.richItemRenderer?.content?.lockupViewModel;
      if (lk?.contentType === 'LOCKUP_CONTENT_TYPE_VIDEO') m[lk.contentId] = [o.richItemRenderer.trackingParams, lk.rendererContext?.loggingContext?.loggingDirectives?.trackingParams];
      for (const v of Object.values(o)) f(v);
    })(JSON.parse(homeFirst));
    return m;
  })();
  const until = async (fn, ms = 5000, what = 'condition') => { for (const t = Date.now(); !(await fn());) { if (Date.now() - t > ms) throw new Error('timed out: ' + what); await new Promise(r => setTimeout(r, 50)); } };
  // (Never empties log.api: later steps read requests made long before them, e.g. the first sw.js_data.)
  let apiFrom = 0;
  const homeOnly = () => log.api.slice(apiFrom).filter(a => a.body?.browseId === 'FEwhat_to_watch');

  await step('Home is YouTube\'s own Home document (GET / like its page reload: its ytInitialData, then browse with its continuation); scripts in it never run; no Home data (a consent page, a redirect) → browse for the rest of the document; never on TV', async () => {
    const wasRich = HOME.rich;
    HOME.rich = true;
    try {
      await readyDel();
      await page.evaluate(() => localStorage.setItem('ytc-test-noready', '1'));
      apiFrom = log.api.length;
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      const first = homeOnly()[0];
      assert.equal(first.doc, 'home', 'the Home request is the document');
      assert.equal(first.url, 'https://www.youtube.com/');
      assert.match(first.headers.accept || '', /^text\/html/, 'asked like a page');
      assert.ok(homeOnly().every(a => a.doc), 'no browse FEwhat_to_watch');
      assert.equal(await page.evaluate(() => window.__ytAppRan), undefined, 'the document\'s scripts never run');
      assert.equal(await page.getAttribute('#ytc-list .c[data-v]', 'data-v'), HOME_ID0, 'its ytInitialData is the page');
      await page.waitForFunction(() => +document.documentElement.dataset.ytcItems >= 60, null, { timeout: 8000 });
      assert.ok(log.api.slice(apiFrom).some(a => a.ep === 'browse' && a.body?.continuation === 'HOMECONT1' && !a.doc), 'the continuation: browse with the document\'s token');
      for (const mode of ['consent', 'redirect']) {
        HOME.doc = mode;
        const n = log.api.length;
        await page.reload({ waitUntil: 'commit' });
        await page.waitForSelector('#ytc-list .c[data-v]');
        assert.equal(await page.getAttribute('#ytc-list .c[data-v]', 'data-v'), HOME_ID0, mode + ': Home from browse');
        assert.deepEqual(log.api.slice(n).filter(a => a.body?.browseId === 'FEwhat_to_watch').map(a => a.doc || 'browse'), [mode, 'browse'], mode);
        assert.equal(await page.evaluate(() => location.href), 'https://www.youtube.com/', 'stays in the client');
        HOME.doc = 'home';
        // The rest of this document: browse (Home on Home asks YouTube again).
        const m = log.api.length;
        await page.evaluate(() => document.getElementById('ytc-logo').click());
        await until(() => log.api.slice(m).some(a => a.body?.browseId === 'FEwhat_to_watch'), 4000, 'Home again');
        assert.ok(log.api.slice(m).filter(a => a.body?.browseId === 'FEwhat_to_watch').every(a => !a.doc), mode + ': browse for the rest of the document');
      }
      await page.evaluate(() => localStorage.setItem('ytc-test-nodoc', '1'));
      const k = log.api.length;
      await page.reload({ waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      assert.ok(log.api.slice(k).filter(a => a.body?.browseId === 'FEwhat_to_watch').every(a => !a.doc), 'switched off (tests): browse');
    } finally {
      HOME.doc = 'home'; HOME.rich = wasRich;
      await page.evaluate(() => { localStorage.removeItem('ytc-test-noready'); localStorage.removeItem('ytc-test-nodoc'); }).catch(() => {});
    }
  });

  await step('Home sends the "shown" signals YouTube\'s own Home sends (log_event: screenCreated with its answer id, then visualElementStateChanged + visualElementShown per card ≥ half in view, once, and scrollEvent), gzip, batched ~1 s; the next ready response waits for the first batch; a later batch makes the waiting one stale; nothing on other pages or with the switch off', async () => {
    const wasRich = HOME.rich;
    HOME.rich = true;
    try {
      await page.goto('https://www.youtube.com/results?search_query=imp', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      // (A lazy refill may go out on this page: let it land, then start Home from the network path.)
      await page.waitForTimeout(2500);
      await readyDel();
      log.imp.length = 0; apiFrom = log.api.length;
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      const painted = Date.now();
      assert.equal(await page.evaluate(() => document.documentElement.dataset.ytcHomeSrc), 'network');
      await until(() => log.imp.length >= 1, 4000, 'the first batch');
      const b = log.imp[0];
      assert.ok(b.t - painted >= 600 && b.t - painted < 2500, `first batch ${b.t - painted} ms after the paint`);
      assert.equal(b.url, 'https://www.youtube.com/youtubei/v1/log_event?alt=json&prettyPrint=false');
      assert.equal(b.headers['content-encoding'], 'gzip');
      assert.equal(b.headers['x-youtube-client-name'], '1');
      assert.equal(b.headers['x-goog-visitor-id'] !== undefined, true);
      assert.ok(b.body, 'a gzip JSON body');
      const kind = e => Object.keys(e).find(k => k !== 'eventTimeMs' && k !== 'context');
      const ev = b.body.events;
      const eid = homeOnly().find(a => a.doc === 'home').eid;
      assert.equal(kind(ev[0]), 'screenCreated');
      const csn = ev[0].screenCreated.csn;
      assert.match(csn, /^[\w-]{16}$/);
      assert.deepEqual(ev[0].screenCreated.pageVe, { veType: 3854, youtubeData: { servletData: { serializedServletEventId: eid } } }, 'names the answer it shows');
      assert.equal(b.body.serializedClientEventId.serializedEventId, eid);
      assert.equal(b.body.context.client.clientName, 1);
      assert.ok(ev.every(e => /^\d+$/.test(e.context.lastActivityMs)));
      assert.deepEqual([...new Set(ev.map(kind))].sort(), ['screenCreated', 'visualElementShown', 'visualElementStateChanged']);
      // Exactly the cards ≥ half in the viewport, each once, with their grid place (4 columns at 1400 px).
      const vis = await page.$$eval('#ytc-list .c[data-v]', els => els.map((e, i) => { const r = e.getBoundingClientRect(); return { id: e.dataset.v, i, in: Math.min(r.bottom, innerHeight) - Math.max(r.top, 0) >= r.height / 2 }; }));
      const onScreen = vis.filter(v => v.in);
      assert.ok(onScreen.length >= 4 && onScreen.length < vis.length, `${onScreen.length} of ${vis.length} cards in view`);
      const shown = ev.filter(e => e.visualElementShown).map(e => e.visualElementShown);
      assert.ok(shown.every(x => x.eventType === 1 && x.csn === csn && x.clientData.viewData.widthPoints > 100), JSON.stringify(shown[0]));
      assert.deepEqual(shown.map(x => x.ve.trackingParams).sort(), onScreen.flatMap(v => homeTrack[v.id]).sort(), 'shown: the cards in view (grid item + own ids), nothing else');
      const grid = ev.filter(e => e.visualElementStateChanged).map(e => e.visualElementStateChanged);
      assert.deepEqual(grid.map(g => [g.ve.trackingParams, g.clientData.gridData.veColumnCoordinate, g.clientData.gridData.veRowCoordinate]).sort(), onScreen.map(v => [homeTrack[v.id][0], v.i % 4, Math.floor(v.i / 4)]).sort());
      // The ready response is fetched after YouTube has that batch.
      const fill = await readyWait();
      const fillReq = homeOnly().find(a => a.t > painted);
      assert.ok(fillReq.t >= b.t, `the refill went out ${fillReq.t - b.t} ms after the first batch`);
      // Scrolling: a scroll event and the newly shown cards (never one twice); YouTube's next answer depends on them,
      // so the waiting ready response goes.
      await page.evaluate(() => scrollTo(0, 1100));
      await until(() => log.imp.length >= 2, 4000, 'the scroll batch');
      const ev2 = log.imp[1].body.events;
      const sc = ev2.find(e => e.scrollEvent).scrollEvent;
      assert.deepEqual(sc, { clientScreenNonce: csn, orientation: 'SCROLL_ORIENTATION_VERTICAL', startContentOffsetPoints: 1100, netDisplacementPoints: 1100 });
      const tp2 = ev2.filter(e => e.visualElementShown).map(e => e.visualElementShown.ve.trackingParams);
      assert.ok(tp2.length > 0 && !tp2.some(t => shown.some(x => x.ve.trackingParams === t)), 'new cards only');
      await page.waitForTimeout(150);
      const after = await readyGet();
      assert.ok(!after || after.t > fill.t, 'the ready response fetched before those cards were shown is gone');
      // Leaving Home: what is queued goes; nothing on other pages.
      await page.goto('https://www.youtube.com/results?search_query=imp2', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      const k = log.imp.length;
      await page.evaluate(() => scrollTo(0, 900));
      await page.waitForTimeout(1600);
      assert.equal(log.imp.length, k, 'no signals on a search page');
      // YouTube's own log_event (its pages) stays blocked; only Redline's batch URL passes the request rules.
      const r = await page.evaluate(async () => {
        const t = async u => { try { await fetch(u, { method: 'POST', body: '{}' }); return 'ok'; } catch { return 'blocked'; } };
        return { youtube: await t('/youtubei/v1/log_event?alt=json'), lumen: await t('/youtubei/v1/log_event?alt=json&prettyPrint=false') };
      });
      assert.deepEqual(r, { youtube: 'blocked', lumen: 'ok' });
      log.imp.length = 0;
      // Switched off (tests): none.
      await page.evaluate(() => localStorage.setItem('ytc-test-noimp', '1'));
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.waitForTimeout(1600);
      assert.equal(log.imp.filter(x => x.url.includes('prettyPrint')).length, 0, 'switched off: no signals');
    } finally {
      HOME.rich = wasRich;
      await page.evaluate(() => localStorage.removeItem('ytc-test-noimp')).catch(() => {});
    }
  });

  await step('a new tab or window paints the ready Home response shared by the tabs (extension memory only): no Home request before it; used once for every tab; watching in any tab, expiry or another account → the network path', async () => {
    const wasRich = HOME.rich;
    HOME.rich = true; HOME.stamp = true;
    const stamp = t => +((/Fresh #(\d+)/.exec(t || '') || [])[1] || 0);
    const title = p => p.evaluate(() => document.querySelector('#ytc-list .c[data-v] .t')?.textContent);
    const open = async () => {
      const t = await ctx.newPage();
      await t.addInitScript(() => { const tick = () => { if (document.querySelector('#ytc-list .c[data-v]')) window.__firstAt = performance.timeOrigin + performance.now(); else requestAnimationFrame(tick); }; requestAnimationFrame(tick); });
      await t.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await t.waitForFunction(() => /Fresh #/.test(document.querySelector('#ytc-list .c[data-v] .t')?.textContent || '') && document.documentElement.dataset.ytcHomeSrc);
      return t;
    };
    const tabs = [];
    try {
      await readyDel();
      await page.bringToFront();
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForFunction(() => /Fresh #/.test(document.querySelector('#ytc-list .c .t')?.textContent || ''));
      const r1 = await readyWait();
      // 1. A new tab: the shared ready response on its first frame, no Home request before it, gone for all tabs.
      const n0 = homeOnly().length;
      const t2 = await open(); tabs.push(t2);
      const m = await t2.evaluate(() => ({ src: document.documentElement.dataset.ytcHomeSrc, at: window.__firstAt }));
      assert.equal(m.src, 'ready');
      assert.equal(stamp(await title(t2)), stamp(r1.i[0].t), 'the ready response');
      assert.ok(homeOnly().slice(n0).every(a => a.t > m.at - 30), 'no Home request before the paint');
      const left = await readyGet();
      assert.ok(!left || left.t > r1.t, 'used once: gone for every tab');
      // The first tab's copy went with it: its logo-Home never shows r1 again.
      await page.bringToFront();
      await page.mouse.move(700, 450);
      await page.evaluate(() => scrollTo(0, 0));
      await page.click('#ytc-logo');
      await page.waitForTimeout(1500);
      assert.notEqual(stamp(await title(page)), stamp(r1.i[0].t), 'not shown twice');
      // 2. Watching in one tab discards it for all.
      const r2 = await readyWait();
      await t2.bringToFront();
      await t2.click('#ytc-list a.t[href^="/watch"]');
      await t2.waitForSelector('#ytc-info h1.wt');
      await until(async () => (await readyGet()) === null, 2000, 'discarded');
      await page.bringToFront();
      const k = homeOnly().length;
      await page.click('#ytc-logo');
      await until(() => homeOnly().length > k, 3000, 'asked YouTube');
      await page.waitForTimeout(800);
      assert.notEqual(stamp(await title(page)), stamp(r2.i[0].t), 'the other tab watched: not the response from before');
      // 3. Expired or another account's: not used by a new tab. The other tabs fill nothing meanwhile (t2 watches
      //    and page shows Home: either would put a fresh response in, and on a busy machine it lands in this window).
      await page.evaluate(() => localStorage.setItem('ytc-test-noready', '1'));
      await t2.close(); tabs.splice(tabs.indexOf(t2), 1);
      await page.waitForTimeout(600);
      for (const change of [r => { r.t -= 11 * 60000; }, r => { r.a = 'sother:0:en'; }]) {
        const r = JSON.parse(JSON.stringify(r2));
        r.t = Date.now();
        change(r);
        await readySet(r);
        const t = await open(); tabs.push(t);
        assert.equal(await t.evaluate(() => document.documentElement.dataset.ytcHomeSrc), 'network');
        await t.close(); tabs.pop();
      }
    } finally {
      for (const t of tabs) await t.close().catch(() => {});
      await page.evaluate(() => localStorage.removeItem('ytc-test-noready')).catch(() => {});
      await page.bringToFront();
      HOME.stamp = false; HOME.rich = wasRich;
    }
  });

  await step('a new tab\'s Home asks YouTube at once: its Home document carries the visitor (no wait for the stored config or sw.js_data); the visitor and client version are learned from it', async () => {
    const wasRich = HOME.rich;
    HOME.rich = true;
    const cfg = await sw.evaluate(() => chrome.storage.local.get('ytcConfigV1'));
    const tabs = [];
    try {
      await readyDel();
      await sw.evaluate(() => chrome.storage.local.remove('ytcConfigV1'));
      const a0 = log.api.length;
      const t = await ctx.newPage(); tabs.push(t);
      await t.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await t.waitForSelector('#ytc-list .c[data-v]');
      const reqs = log.api.slice(a0), home = reqs.findIndex(a => a.doc === 'home'), swd = reqs.findIndex(a => a.ep === 'sw.js_data');
      assert.ok(home >= 0 && (swd < 0 || swd > home), `Home first (${reqs.map(a => a.doc ? 'doc' : a.ep).join(',')})`);
      await until(async () => (await sw.evaluate(() => chrome.storage.local.get('ytcConfigV1'))).ytcConfigV1?.vd === HOME_VD, 3000, 'visitor learned');
      const c = (await sw.evaluate(() => chrome.storage.local.get('ytcConfigV1'))).ytcConfigV1;
      assert.equal(c.v, '2.20261102.01.00', 'the document\'s client version');
    } finally {
      for (const t of tabs) await t.close().catch(() => {});
      await sw.evaluate(c => chrome.storage.local.set(c), cfg);
      await page.bringToFront();
      HOME.rich = wasRich;
    }
  });

  await step('nothing ready: Home is read from the tab\'s own document (no second request): held out of the page while it streams (its scripts never run, nothing of it renders, one head and body: the client\'s), its answer id for the "shown" signals; a YouTube page without Home data → browse at once; a ready response stops the document', async () => {
    const wasRich = HOME.rich;
    HOME.rich = true;
    const state = () => page.evaluate(() => ({ doc: document.documentElement.dataset.ytcHomeDoc || '', src: document.documentElement.dataset.ytcHomeSrc, ran: window.__ytAppRan, ytd: document.querySelectorAll('ytd-app').length, bodies: document.querySelectorAll('body').length, heads: document.querySelectorAll('head').length, cards: document.querySelectorAll('#ytc-list .c[data-v]').length }));
    try {
      await readyDel();
      await page.evaluate(() => localStorage.setItem('ytc-test-noready', '1'));
      HOME.nav = 'home';
      const a0 = log.api.length;
      log.imp.length = 0;
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      const reqs = log.api.slice(a0).filter(a => a.body?.browseId === 'FEwhat_to_watch');
      assert.deepEqual(reqs.map(a => a.doc), ['navigation'], 'the tab\'s own document is the only Home request');
      const st = await state();
      assert.deepEqual({ ...st, cards: st.cards > 10 }, { doc: 'navigation', src: 'network', ran: undefined, ytd: 0, bodies: 1, heads: 1, cards: true }, 'its scripts never ran, nothing of it is in the page');
      assert.equal(await page.getAttribute('#ytc-list .c[data-v]', 'data-v'), HOME_ID0, 'its ytInitialData is the page');
      await page.waitForFunction(() => +document.documentElement.dataset.ytcItems >= 60, null, { timeout: 8000 });
      assert.ok(log.api.slice(a0).some(a => a.body?.continuation === 'HOMECONT1'), 'its continuation');
      await until(() => log.imp.length >= 1, 4000, 'the first "shown" batch');
      assert.equal(log.imp[0].body.events[0].screenCreated.pageVe.youtubeData.servletData.serializedServletEventId, reqs[0].eid, 'the signals name its answer');
      // A YouTube page without Home data: browse at once (a fetch of the same page would not have it either).
      HOME.nav = 'nodata';
      const a1 = log.api.length;
      await page.reload({ waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      assert.deepEqual(log.api.slice(a1).filter(a => a.body?.browseId === 'FEwhat_to_watch' && !a.body.continuation).map(a => a.doc || 'browse'), ['nodata', 'browse']);
      assert.equal((await state()).ran, undefined);
      // A ready response: painted at once; the document is stopped and nothing of it ran.
      HOME.nav = 'home';
      await page.evaluate(() => localStorage.removeItem('ytc-test-noready'));
      await page.reload({ waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await readyWait();
      await page.reload({ waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.waitForTimeout(300);
      const s3 = await state();
      assert.deepEqual([s3.src, s3.doc, s3.ran, s3.ytd, s3.bodies], ['ready', '', undefined, 0, 1]);
    } finally {
      HOME.nav = false; HOME.rich = wasRich;
      await page.evaluate(() => localStorage.removeItem('ytc-test-noready')).catch(() => {});
    }
  });

  await step('a playing video: once playback is healthy (8 s played, 20 s or the rest buffered ahead) one low-priority Home request refills the ready response (no continuation), once per watch page; a new window paints it; the next video discards it for every tab', async () => {
    const wasRich = HOME.rich;
    HOME.rich = true;
    const tabs = [];
    try {
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.mouse.move(700, 450);
      await page.click('#ytc-list a.t[href^="/watch"]');
      await page.waitForFunction(() => document.documentElement.dataset.ytcFirstFrame);
      assert.equal(await readyGet(), null, 'opening the video discarded Home\'s');
      const a0 = log.api.length, t0 = Date.now();
      prio.length = 0;
      await until(async () => (await readyGet()) !== null, 30000, 'refilled while the video plays');
      const reqs = log.api.slice(a0).filter(a => a.body?.browseId === 'FEwhat_to_watch');
      assert.equal(reqs.length, 1, 'one request');
      assert.ok(reqs[0].t - t0 >= 7000, `after the video played a while (${reqs[0].t - t0} ms)`);
      assert.equal(prio.filter(x => x.post.startsWith('FEwhat_to_watch')).at(-1)?.p, 'Low', 'low priority');
      assert.equal(await page.evaluate(() => document.querySelector('video') && !document.querySelector('video').paused), true, 'still playing');
      await page.waitForTimeout(1500);
      assert.ok(!log.api.slice(a0).some(a => a.body?.continuation?.startsWith('HOME')), 'no continuation while a video plays');
      // A new window while it plays: the ready response on its first frame.
      const r = await readyGet();
      const t = await ctx.newPage(); tabs.push(t);
      await t.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await t.waitForFunction(() => document.querySelector('#ytc-list .c[data-v]') && document.documentElement.dataset.ytcHomeSrc);
      assert.equal(await t.evaluate(() => document.documentElement.dataset.ytcHomeSrc), 'ready');
      assert.equal(await t.evaluate(() => document.querySelector('#ytc-list .c[data-v] .t').textContent), r.i[0].t);
      // Once per watch page: used, it is not fetched again here (refills are at most once a minute anyway).
      const a1 = log.api.length;
      await page.waitForTimeout(11000);
      assert.equal(log.api.slice(a1).filter(a => a.body?.browseId === 'FEwhat_to_watch' && a.t > 0 && !a.body.continuation && a.headers.referer?.includes('watch')).length, 0, 'not again on this page');
      // The next video is a personalizing event: what the new window fetched since is discarded for every tab.
      await readyWait(10, 8000);
      await page.bringToFront();
      await page.locator('#ytc-main a.t[href^="/watch"]').first().click();
      await until(async () => (await readyGet()) === null, 3000, 'discarded by the next video');
    } finally {
      for (const t of tabs) await t.close().catch(() => {});
      await page.bringToFront();
      HOME.rich = wasRich;
    }
  });

  await step('logo: resting on it opens Home from memory (the ready response, else a request on enter; 250 ms dwell, the pill fills); a pass-over or a shorter rest never navigates, a new rest starts over; click instant; keyboard unchanged; nothing while typing; Home on Home: top, then a new Home from YouTube', async () => {
    const saved = { ...HOME };
    HOME.rich = true; HOME.latency = 150; HOME.delay = 0;
    const box = await page.locator('#ytc-logo').boundingBox();
    const lc = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
    const homeReqs = () => log.api.filter(a => a.ep === 'browse' && a.body?.browseId === 'FEwhat_to_watch');
    // Page-side watch: when the pointer entered the logo, whether the pill was filling mid-dwell, and the
    // first frame showing Home (cards from memory, or a skeleton waiting for the network).
    const watch = () => page.evaluate(() => {
      const w = window.__dw = { enter: null, mid: null, nav: null };
      document.getElementById('ytc-logo').addEventListener('pointerover', () => {
        if (w.enter !== null) return;
        w.enter = performance.now(); w.enterEpoch = Date.now();
        setTimeout(() => { const l = document.getElementById('ytc-logo'); w.mid = l && l.classList.contains('dw') && getComputedStyle(l, '::before').opacity === '1'; }, 120);
      });
      const tick = () => {
        if (location.pathname === '/') { w.nav = performance.now(); w.navEpoch = Date.now(); w.cards = document.querySelectorAll('#ytc-list .c[data-v]').length; w.sk = !!document.querySelector('#ytc-main .sk'); w.dw = document.getElementById('ytc-logo').classList.contains('dw'); }
        else requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
    const results = async q => {
      await page.mouse.move(700, 450);
      await page.goto('https://www.youtube.com/results?search_query=' + q, { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.evaluate(() => { sessionStorage.removeItem('ytc:snap:/'); });
      await readyDel();
    };
    try {
      // 1. Rest on the logo from a results page (no Home snapshot): Home's first page is requested the moment
      // the pointer enters (not after the 70 ms hover delay) and the dwell ends in Home painted from memory.
      // (The background refresh, which would have fetched Home already, is off for this part.)
      await page.evaluate(() => { localStorage.setItem('ytc-test-nobg', '1'); localStorage.setItem('ytc-test-noready', '1'); });
      const dwellMs = [];
      for (let i = 0; i < 3; i++) {
        await results('dwell' + i);
        await watch();
        const n0 = homeReqs().length;
        await page.mouse.move(lc.x, lc.y);
        await page.waitForFunction(() => window.__dw.nav !== null, null, { timeout: 3000 });
        const w = await page.evaluate(() => window.__dw);
        const reqs = homeReqs().slice(n0);
        assert.equal(reqs.length, 1, 'one Home request, sent on enter (none at the end of the dwell)');
        assert.ok(reqs[0].t - w.enterEpoch < 50, `Home requested ${reqs[0].t - w.enterEpoch} ms after entering the logo`);
        assert.ok(w.cards >= 20 && !w.sk, `Home painted from memory on the dwell's frame (cards ${w.cards}, skeleton ${w.sk})`);
        assert.ok(w.nav - w.enter >= 240 && w.nav - w.enter < 450, `dwell ${(w.nav - w.enter).toFixed(0)} ms`);
        assert.equal(w.mid, true, 'the pill fills during the dwell');
        assert.equal(w.dw, false, 'the pill is gone when Home opens');
        dwellMs.push(w.nav - w.enter);
        // Still resting there: no second dwell (the pointer has to leave first).
        const h = await page.evaluate(() => history.length);
        await page.waitForTimeout(400);
        assert.equal(await page.evaluate(() => history.length), h, 'no second dwell while the pointer stays');
      }
      await page.evaluate(() => { localStorage.removeItem('ytc-test-nobg'); localStorage.removeItem('ytc-test-noready'); });
      console.log(`     logo rest → Home cards (pointer enters the logo; 150 ms InnerTube latency): median ${median(dwellMs).toFixed(0)} ms`);
      // 1b. With a ready Home response waiting, the dwell paints it: no request on enter or at the end
      // (only the next ready response afterwards, once Home is on screen).
      HOME.stamp = true;
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      const readyTitle = (await readyWait()).i[0].t;
      await page.mouse.move(700, 450);
      await page.goto('https://www.youtube.com/results?search_query=dwellready', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await watch();
      const n0 = homeReqs().length;
      await page.mouse.move(lc.x, lc.y);
      await page.waitForFunction(() => window.__dw.nav !== null, null, { timeout: 3000 });
      const wr = await page.evaluate(() => window.__dw);
      const sent = homeReqs().filter(a => a.t <= wr.navEpoch + 5);
      assert.equal(sent.length, n0, 'no Home request on enter or at the end of the dwell');
      assert.ok(wr.cards >= 20 && !wr.sk, 'painted on the dwell\'s frame');
      assert.equal(await page.evaluate(() => document.querySelector('#ytc-list .c[data-v] .t').textContent), readyTitle, 'the ready response');
      HOME.stamp = false;
      // 2. Passing over (sidebar → search, fast and slow) and resting shorter than the dwell never navigate.
      await results('pass');
      await page.mouse.move(4, lc.y);
      await page.mouse.move(lc.x + 400, lc.y, { steps: 8 });
      for (let x = box.x - 20; x < box.x + box.width + 30; x += 30) { await page.mouse.move(x, lc.y); await page.waitForTimeout(20); }
      await page.mouse.move(lc.x + 400, lc.y + 300);
      await page.waitForTimeout(450);
      assert.equal(await page.evaluate(() => location.pathname), '/results', 'a pass-over did not navigate');
      assert.equal(await page.evaluate(() => document.getElementById('ytc-logo').classList.contains('dw')), false, 'the pill retracted on leaving');
      // A new rest starts over: 150 ms, out, 150 ms again → still here; then the full dwell → Home.
      await page.mouse.move(lc.x, lc.y); await page.waitForTimeout(150);
      await page.mouse.move(lc.x, lc.y + 60); await page.waitForTimeout(40);
      await page.mouse.move(lc.x, lc.y); await page.waitForTimeout(150);
      assert.equal(await page.evaluate(() => location.pathname), '/results', 'leaving restarted the dwell');
      await page.waitForFunction(() => location.pathname === '/', null, { timeout: 1000 });
      // 3. A click is instant (the dwell started by the pointer's arrival is dropped).
      await results('click');
      const h0 = await page.evaluate(() => history.length);
      await page.mouse.move(lc.x, lc.y);
      const t = await page.evaluate(() => new Promise(res => { const t0 = performance.now(); document.getElementById('ytc-logo').click(); res({ path: location.pathname, cards: document.querySelectorAll('#ytc-list .c').length, ms: performance.now() - t0 }); }));
      assert.equal(t.path, '/', 'the click navigated in its own task');
      await page.waitForTimeout(400);
      assert.equal(await page.evaluate(() => history.length), h0 + 1, 'one history entry: the dwell did not navigate again');
      // 4. Keyboard unchanged: focus + Enter opens Home at once (no pill).
      await results('keys');
      await page.focus('#ytc-logo');
      await page.keyboard.press('Enter');
      assert.equal(await page.evaluate(() => location.pathname), '/');
      assert.equal(await page.evaluate(() => document.getElementById('ytc-logo').classList.contains('dw')), false);
      // 5. Nothing while typing in the search field (or with its dropdown open).
      await results('typing');
      await page.click('#ytc-q'); await page.keyboard.type('abc');
      await page.mouse.move(lc.x, lc.y); await page.waitForTimeout(450);
      assert.equal(await page.evaluate(() => location.pathname), '/results', 'no dwell while typing');
      assert.equal(await page.evaluate(() => document.getElementById('ytc-logo').classList.contains('dw')), false);
      if (process.env.YTC_SHOTS) {
        // The cue mid-dwell (125 of 250 ms), frozen: the pointer rests on the logo (its hover look) while typing
        // keeps the real dwell from running; the class is the one Rust sets.
        await page.evaluate(() => { document.getElementById('ytc-logo').classList.add('dw'); });
        await page.evaluate(() => { for (const a of document.getAnimations()) if (a.transitionProperty === 'clip-path' || a.transitionProperty === 'opacity') { a.pause(); a.currentTime = 125; } });
        await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'logo-dwell-mid.png'), clip: { x: box.x - 24, y: box.y - 16, width: box.width + 48, height: box.height + 32 } });
        await page.evaluate(() => { for (const a of document.getAnimations()) a.finish(); });
        await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'logo-dwell-full.png'), clip: { x: box.x - 24, y: box.y - 16, width: box.width + 48, height: box.height + 32 } });
        await page.evaluate(() => document.getElementById('ytc-logo').classList.remove('dw'));
      }
      await page.keyboard.press('Escape');
      await page.evaluate(() => document.activeElement.blur());
      await page.mouse.move(lc.x, lc.y + 300);
      await page.mouse.move(lc.x, lc.y);
      await page.waitForFunction(() => location.pathname === '/', null, { timeout: 1000 });
      // 5b. Watch pages scroll the top bar away: the logo coming back under a resting pointer is no rest on it.
      await results('scrollunder');
      const href = await page.getAttribute('#ytc-list a.t[href^="/watch"]', 'href');
      await page.goto('https://www.youtube.com' + href, { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      await page.evaluate(() => scrollTo(0, 400));
      await page.waitForTimeout(100);
      await page.mouse.move(lc.x, lc.y); await page.waitForTimeout(400);
      const away = await page.evaluate(([x, y]) => !document.elementFromPoint(x, y)?.closest('#ytc-logo'), [lc.x, lc.y]);
      await page.evaluate(() => { window.__over = 0; document.getElementById('ytc-logo').addEventListener('pointerover', () => window.__over++); scrollTo(0, 0); });
      await page.waitForTimeout(550);
      const back = await page.evaluate(([x, y]) => ({ on: !!document.elementFromPoint(x, y)?.closest('#ytc-logo'), over: window.__over, dw: document.getElementById('ytc-logo').classList.contains('dw') }), [lc.x, lc.y]);
      assert.ok(away && back.on, 'the top bar scrolled away from the pointer and back under it');
      assert.equal(back.dw, false, 'no pill for a logo that came under a still pointer');
      assert.equal(await page.evaluate(() => location.pathname), '/watch', 'the logo scrolling under a still pointer did not navigate');
      console.log(`     scroll-under: pointerover events on the logo after the scroll: ${back.over}`);
      await page.mouse.move(lc.x + 3, lc.y);
      await page.waitForFunction(() => location.pathname === '/', null, { timeout: 1000 });
      // 6. Home on Home (like YouTube's logo on its Home): back to the top at once, no history entry, and a new
      // Home from YouTube: the ready response when one is waiting (no request), else the request sent on enter.
      HOME.stamp = true;
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.mouse.move(700, 450);
      const next = (await readyWait()).i[0].t;
      await page.evaluate(() => scrollTo(0, 900));
      const before = { h: await page.evaluate(() => history.length), n: homeReqs().length };
      await page.mouse.move(lc.x, lc.y); await page.waitForTimeout(400);
      let s = await page.evaluate(() => ({ y: scrollY, h: history.length, path: location.pathname, t: document.querySelector('#ytc-list .c[data-v] .t').textContent }));
      assert.deepEqual(s, { y: 0, h: before.h, path: '/', t: next }, 'Home on Home: top, the ready response');
      assert.ok(homeReqs().slice(before.n).every(a => a.t > Date.now() - 350), 'no request at the dwell (only the next ready response after it)');
      // Without a ready response, the click asks YouTube and replaces the page in place.
      await readyDel();
      await page.evaluate(() => { localStorage.setItem('ytc-test-noready', '1'); scrollTo(0, 900); });
      const k0 = HOME.served, n1 = homeReqs().length;
      await page.evaluate(() => document.getElementById('ytc-logo').click());
      s = await page.evaluate(() => ({ y: scrollY, h: history.length }));
      assert.deepEqual(s, { y: 0, h: before.h }, 'click on Home: top at once, no history entry');
      await page.waitForFunction(k => { const m = /Fresh #(\d+)/.exec(document.querySelector('#ytc-list .c .t')?.textContent || ''); return m && +m[1] > k; }, k0, { timeout: 2000 });
      assert.equal(homeReqs().length, n1 + 1, 'one request');
    } finally {
      Object.assign(HOME, saved);
      await page.evaluate(() => { sessionStorage.removeItem('ytc:bgage'); localStorage.removeItem('ytc-test-nobg'); localStorage.removeItem('ytc-test-noready'); }).catch(() => {});
      await page.mouse.move(700, 450);
    }
  });

  await step('search box dropdown: history (recent on focus, keyboard, remove) + suggestions, escaped', async () => {
    await page.fill('#ytc-q', '');
    await page.click('#ytc-q');
    // 'xss' was submitted through the top bar earlier: recent searches show on focus.
    await page.waitForSelector('#ytc-sug li.hs');
    assert.ok((await page.locator('#ytc-sug li.hs span').allTextContents()).includes('xss'));
    assert.equal(await page.getAttribute('#ytc-q', 'aria-expanded'), 'true');
    await page.type('#ytc-q', 'rob', { delay: 30 });
    await page.waitForFunction(() => document.querySelectorAll('#ytc-sug li:not(.hs)').length === 2);
    const vals = await page.$$eval('#ytc-sug li:not(.hs) span', o => o.map(x => x.textContent));
    assert.deepEqual(vals, ['roblox', 'roblox <b>evil</b>" x="']);
    assert.equal(await page.locator('#ytc-sug b, #ytc-sug [x]').count(), 0);
    const u = new URL(log.suggest.at(-1));
    assert.equal(u.origin + u.pathname, 'https://www.youtube.com/complete/search');
    assert.equal(u.searchParams.get('q'), 'rob');
    // Submit, then it is the first history match for "rob".
    await page.fill('#ytc-q', 'roblox lua tables');
    await page.press('#ytc-q', 'Enter');
    await page.waitForFunction(() => location.search === '?search_query=roblox+lua+tables' || location.search === '?search_query=roblox%20lua%20tables');
    await page.waitForSelector('#ytc-list .c');
    await page.fill('#ytc-q', '');
    await page.click('#ytc-q');
    await page.type('#ytc-q', 'rob', { delay: 20 });
    await page.waitForSelector('#ytc-sug li.hs');
    assert.equal(await page.locator('#ytc-sug li.hs span').first().textContent(), 'roblox lua tables');
    // Arrow keys select rows (input shows the row), Delete removes a history row.
    await page.press('#ytc-q', 'ArrowDown');
    assert.equal(await page.inputValue('#ytc-q'), 'roblox lua tables');
    assert.equal(await page.getAttribute('#ytc-q', 'aria-activedescendant'), 'ytc-sg0');
    assert.equal(await page.locator('#ytc-sug li.on').count(), 1);
    await page.press('#ytc-q', 'Delete');
    await page.waitForFunction(() => ![...document.querySelectorAll('#ytc-sug li.hs span')].some(s => s.textContent === 'roblox lua tables'));
    assert.equal(await page.inputValue('#ytc-q'), 'rob', 'typed text back after removing');
    // Escape closes.
    await page.press('#ytc-q', 'Escape');
    assert.equal(await page.locator('#ytc-sug li').count(), 0);
    // Mouse: remove button keeps the box focused; a row click searches.
    await page.fill('#ytc-q', '');
    await page.click('#ytc-q');
    await page.waitForSelector('#ytc-sug li.hs');
    const before = await page.locator('#ytc-sug li.hs').count();
    await page.hover('#ytc-sug li.hs >> nth=0');
    await page.click('#ytc-sug li.hs >> nth=0 >> .sgx');
    await page.waitForFunction(n => document.querySelectorAll('#ytc-sug li.hs').length === n - 1, before);
    assert.equal(await page.evaluate(() => document.activeElement.id), 'ytc-q');
    if (before > 1) {
      const q = await page.locator('#ytc-sug li.hs span').first().textContent();
      await page.click('#ytc-sug li.hs >> nth=0 >> a');
      await page.waitForFunction(q => document.title.startsWith(q), q);
    }
    await page.fill('#ytc-q', '');
    await page.keyboard.press('Escape');
  });

  await step('hover: animated preview on a thumbnail; channel links prefetched and reused on click', async () => {
    await page.goto('https://www.youtube.com/results?search_query=roblox+studio+animation', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    const th = page.locator('#ytc-list .c a.th[href^="/watch"]').first();
    await th.hover();
    await page.waitForSelector('#ytc-list a.th img.pv', { timeout: 3000 });
    assert.ok((await page.getAttribute('#ytc-list a.th img.pv', 'src')).startsWith('https://i.ytimg.com/an_webp/'));
    await page.mouse.move(2, 2);
    await page.waitForSelector('img.pv', { state: 'detached', timeout: 2000 });
    // (It may already be in the 2-minute prefetch cache from an earlier hover.)
    const ch = page.locator('#ytc-list .c:not(.plc):not(.chc) .m a[href^="/@"]').first();
    const href = await ch.getAttribute('href');
    await ch.hover();
    await page.waitForTimeout(300);
    const n = apiCount('browse');
    await ch.click();
    await page.waitForSelector('.ch h1');
    assert.equal(new URL(page.url()).pathname, href);
    assert.equal(apiCount('browse'), n, 'click rendered the hover-prefetched channel');
    await page.keyboard.press('Backquote');
    assert.match(await page.locator('#ytc-debug').textContent(), /prefetched/);
    await page.keyboard.press('Backquote');
    HOME.rich = false;
  });

  await step('/shorts/ID opens as a normal watch page', async () => {
    await page.goto('https://www.youtube.com/shorts/DQcrB2wLu04', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-video');
    assert.equal(new URL(page.url()).pathname + new URL(page.url()).search, '/watch?v=DQcrB2wLu04');
    await page.waitForFunction(() => document.documentElement.dataset.ytcFirstFrame);
  });

  const vstate = () => page.evaluate(() => { const v = document.getElementById('ytc-video'); if (!v) return null;
    const q = v.getVideoPlaybackQuality(); return { t: v.currentTime, paused: v.paused, ended: v.ended, rs: v.readyState, h: v.videoHeight, rate: v.playbackRate, dropped: q.droppedVideoFrames, frames: q.totalVideoFrames }; });
  const waitT = (pred, arg, timeout = 10000) => page.waitForFunction(pred, arg, { timeout, polling: 100 });
  // The watch-open morph has landed (layout assertions read the painted box).
  const morphDone = () => page.waitForFunction(() => !document.getElementById('ytc-player')?.getAnimations().some(a => a.id === 'flip'), null, { timeout: 3000 });
  const shot = async name => { if (process.env.YTC_SHOTS) await page.screenshot({ path: path.join(process.env.YTC_SHOTS, name + '.png'), animations: 'disabled' }); };
  const keys = async (k, n = 1) => { for (let i = 0; i < n; i++) await page.keyboard.press(k); };
  // 90 % through the video by a seek-bar click (0–9 are blocked keys now), then the pointer leaves.
  const nearEnd = async () => { const b = await page.locator('.pseek').boundingBox(); await page.mouse.click(b.x + b.width * 0.9, b.y + b.height / 2); await page.mouse.move(5, 5); };

  await step('timestamp: t= starts our player at that time; the description lists no timestamp links', async () => {
    await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0&t=30', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-info h1.wt');
    await waitT(() => document.documentElement.dataset.ytcFirstFrame);
    await waitT(() => document.getElementById('ytc-video').currentTime >= 30);
    await page.waitForSelector('#ytc-desc:not(.sk)');
    assert.equal(await page.locator('#ytc-desc a[data-act="seek"]').count(), 0, 'timestamps are not description resources');
  });

  const ff = { spa: [] };
  await step('own player: first frame from VISIONOS streams, WEB metadata call, no YouTube player JS', async () => {
    log.gv.length = 0; log.stats.length = 0;
    const assets = log.assets.length, mark0 = log.api.length;
    await page.goto('https://www.youtube.com/watch?v=aqz-KE-bpKQ', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-video');
    // A cold page has no warm first frame: the dark player surface until the first frame, never the 320 px thumbnail scaled up.
    assert.equal(await page.getAttribute('#ytc-video', 'poster'), null, 'no thumbnail poster');
    assert.equal(await page.locator('#ytc-player img, #ytc-player .pwf').count(), 0, 'no still in the player box');
    // The caption cue's quick fade-in (a cue at 0 s can show before the first frame) is not a loading indicator.
    assert.equal(await page.evaluate(() => [...document.querySelectorAll('#ytc-player *:not(#ytc-cc)')].filter(e => getComputedStyle(e).animationName !== 'none').length), 0, 'no loading spinner/animation in the player');
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    ff.direct = await page.evaluate(() => +document.documentElement.dataset.ytcFirstFrame);
    const players = log.api.slice(mark0).filter(a => a.ep === 'player' && a.body.videoId === 'aqz-KE-bpKQ');
    const vis = players.find(a => a.body.context.client.clientName === 'VISIONOS');
    const web = players.find(a => a.body.context.client.clientName === 'WEB');
    assert.ok(vis && web, 'both player calls');
    assert.equal(players.length, 2, 'happy path: one VISIONOS + one WEB player call');
    assert.equal(log.api.slice(mark0).filter(a => a.ep === 'visitor_id').length, 0, 'happy path: no visitor_id');
    assert.ok(!vis.headers.authorization && !vis.headers.cookie, 'happy path: VISIONOS anonymous');
    assert.equal(vis.headers['x-youtube-client-name'], '101');
    assert.ok(vis.headers['x-goog-visitor-id']?.startsWith('Cg'), 'visitor data');
    assert.equal(vis.body.context.client.visitorData, vis.headers['x-goog-visitor-id']);
    assert.equal(web.headers['x-youtube-client-name'], '1');
    assert.ok(web.body.playbackContext.contentPlaybackContext.signatureTimestamp >= 20753);
    assert.equal(web.body.cpn.length, 16);
    // First segment rides along with the index (one request per track before the first frame).
    const first = log.gv.filter(g => g.a === 0);
    assert.ok(first.some(g => g.itag === '249' && g.b > +media.formats.find(f => f.itag === 249).indexRange.end), 'audio head + first segment');
    const st = await vstate();
    assert.ok(st.h > 0 && st.rs >= 2, JSON.stringify(st));
    assert.equal(log.assets.length, assets, 'no /s/ assets');
    assert.equal(await page.locator('iframe').count(), 0, 'no iframe');
  });

  await step('own player: plays ≥ 10 s; speed with < > (2×)', async () => {
    await waitT(() => document.getElementById('ytc-video').currentTime >= 3);
    await keys('>', 4);
    let st = await vstate();
    assert.equal(st.rate, 2);
    assert.equal(await page.locator('#ytc-speed').textContent(), '2×');
    await waitT(() => document.getElementById('ytc-video').currentTime >= 10, null, 8000);
    st = await vstate();
    assert.ok(!st.paused && st.rs >= 3, JSON.stringify(st));
  });

  await step('own player: seek by clicking the seek bar and with j', async () => {
    const box = await page.locator('.pseek').boundingBox();
    await page.mouse.click(box.x + box.width * 0.75, box.y + box.height / 2);
    await waitT(() => { const v = document.getElementById('ytc-video'); return v.currentTime > 30.5 && v.readyState >= 3; });
    await keys('j');
    await waitT(() => { const v = document.getElementById('ytc-video'); return v.currentTime > 19 && v.currentTime < 25; });
    // 0–9 are blocked: no jump, the key is taken (default prevented), nothing else happens.
    const t0 = await page.evaluate(() => document.getElementById('ytc-video').currentTime);
    const prevented = await page.evaluate(() => ['0', '5', '9'].map(k => { const e = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true }); document.body.dispatchEvent(e); return e.defaultPrevented; }));
    await keys('0'); await keys('9'); await keys('5');
    await page.waitForTimeout(300);
    const t1 = await page.evaluate(() => document.getElementById('ytc-video').currentTime);
    assert.deepEqual(prevented, [true, true, true], 'digits taken on the watch page');
    assert.ok(t1 >= t0 && t1 - t0 < 2, `no jump on 0/9/5: ${t0} → ${t1}`);
    await keys('<', 4);
    assert.equal((await vstate()).rate, 1);
  });

  await step('own player: quality switch from the menu (360p → 144p) during playback', async () => {
    assert.equal((await vstate()).h, 360, 'Auto picked the best rung that fits');
    assert.equal(await page.textContent('#ytc-qlbl'), '360', 'the Quality button shows the playing rung');
    if (process.env.YTC_SHOTS) { await page.mouse.move(700, 400); await page.waitForTimeout(100); const b = await page.locator('#ytc-player').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'player-toolbar.png'), clip: { x: b.x, y: b.y + b.height - 70, width: b.width, height: 70 } }); }
    const w0 = await page.evaluate(() => [document.getElementById('ytc-qual').getBoundingClientRect().width, document.getElementById('ytc-speed').getBoundingClientRect().x]);
    await page.click('#ytc-qual');
    await page.waitForSelector('#ytc-player.menu #ytc-menu button');
    await page.locator('#ytc-menu button', { hasText: '144p' }).click();
    await waitT(() => document.getElementById('ytc-video').videoHeight === 144, null, 9000);
    const st = await vstate();
    assert.ok(!st.paused && st.rs >= 3, JSON.stringify(st));
    assert.ok(log.gv.some(g => g.itag === '278' && g.a > 0), '144p segments fetched');
    assert.equal(await page.textContent('#ytc-qlbl'), '144');
    const w1 = await page.evaluate(() => [document.getElementById('ytc-qual').getBoundingClientRect().width, document.getElementById('ytc-speed').getBoundingClientRect().x]);
    assert.deepEqual(w1, w0, 'label changes never move or resize the buttons');
    assert.equal(await page.evaluate(() => document.getElementById('ytc-player').scrollLeft + document.getElementById('ytc-player').scrollTop), 0, 'the player box never scrolls');
  });

  await step('own player: captions (json3 overlay) and history pings', async () => {
    assert.ok(await page.locator('#ytc-player.cc').count(), 'captions on by default');
    await waitT(() => /^Caption line \d+ <b>&amp;<\/b>$/.test(document.getElementById('ytc-cc').textContent), null, 5000);
    assert.equal(await page.locator('#ytc-cc b').count(), 0, 'caption text is not HTML');
    assert.ok(log.timedtext.some(u => u.includes('fmt=json3') && u.includes('lang=en')));
    await keys('k');
    await waitT(() => document.getElementById('ytc-video').paused);
    const paused = () => log.stats.filter(u => u.includes('/watchtime') && u.includes('state=paused')).pop();
    for (let i = 0; i < 20 && !paused(); i++) await page.waitForTimeout(100);
    const pb = log.stats.find(u => u.includes('/api/stats/playback'));
    const wt = paused();
    assert.ok(pb && wt, log.stats.join('\n'));
    assert.ok(pb.startsWith('https://www.youtube.com/api/stats/playback?cl=1&docid=aqz-KE-bpKQ'), 's.youtube.com → www');
    const p = new URL(pb).searchParams, w = new URL(wt).searchParams;
    assert.equal(p.get('ver'), '2');
    assert.equal(p.get('cpn').length, 16);
    assert.equal(p.get('c'), 'WEB');
    assert.equal(w.get('cpn'), p.get('cpn'));
    assert.equal(w.get('state'), 'paused');
    assert.ok(w.get('st') && w.get('et') && w.get('st').split(',').length === w.get('et').split(',').length, wt);
    assert.ok(Math.max(...w.get('et').split(',').map(Number)) > 20, 'played time reported');
  });

  await step('own player: end of stream (ended fires at the end; the app gets Out::Ended once)', async () => {
    await page.evaluate(() => delete document.documentElement.dataset.ytcEnded);
    // (0–9 are blocked now: jump near the end with the seek bar.)
    const sb = await page.locator('.pseek').boundingBox();
    await page.mouse.click(sb.x + sb.width * 0.9, sb.y + sb.height / 2);
    await page.mouse.move(5, 5);
    await keys('k');
    await waitT(() => document.getElementById('ytc-video').ended, null, 8000);
    const st = await vstate();
    assert.ok(st.t > SECONDS - 1, JSON.stringify(st));
    await waitT(() => document.documentElement.dataset.ytcEnded, null, 2000);
  });

  await step('captions control: click toggles immediately, hover only opens options, keyboard reaches languages and the icon follows remembered state', async () => {
    const before = await sw.evaluate(() => chrome.storage.local.get('ytcConfigV1'));
    const t = await ctx.newPage();
    try {
      await t.goto('https://www.youtube.com/watch?v=ccjapanese1&t=4', { waitUntil: 'commit' });
      await t.waitForFunction(() => document.documentElement.dataset.ytcFirstFrame, null, { timeout: 8000 });
      const on = () => t.$eval('#ytc-player', p => p.classList.contains('cc'));
      const wasOn = await on(), paused = await t.$eval('#ytc-video', v => v.paused);
      const media = await t.$eval('#ytc-video', v => v.currentSrc);
      const requests = log.api.filter(a => a.ep === 'player').length;
      await t.click('#ytc-ccb');
      assert.equal(await on(), !wasOn, 'the button toggles in its click task');
      assert.equal(await t.getAttribute('#ytc-ccb', 'aria-pressed'), String(!wasOn));
      await t.hover('#ytc-ccb'); await t.waitForSelector('#ytc-player.menu .mcc');
      assert.equal(await t.locator('#ytc-menu [data-arg="cc:toggle"]').count(), 0, 'no duplicate on/off row');
      assert.equal(await t.getAttribute('#ytc-ccb', 'aria-expanded'), 'true');
      await t.hover('#ytc-menu [data-arg="cc:1"]'); await t.waitForTimeout(280);
      assert.equal(await on(), !wasOn, 'hovering a language never enables or selects it');
      await t.click('#ytc-menu [data-arg="cc:1"]');
      assert.equal(await on(), true, 'choosing a language enables captions');
      await t.waitForFunction(() => document.getElementById('ytc-cc').textContent.length > 0);
      const captionRequests = log.timedtext.length;
      await t.click('#ytc-ccb');
      assert.equal(await on(), false); assert.equal(await t.textContent('#ytc-cc'), '');
      if (process.env.YTC_SHOTS) { await t.waitForTimeout(200); await t.screenshot({ path: path.join(process.env.YTC_SHOTS, 'captions-click-off.png') }); }
      await t.click('#ytc-ccb');
      assert.equal(await on(), true);
      assert.equal(log.timedtext.length, captionRequests, 'toggling reuses loaded captions');
      assert.equal(log.api.filter(a => a.ep === 'player').length, requests, 'no player reload');
      assert.equal(await t.$eval('#ytc-video', v => v.currentSrc), media);
      assert.equal(await t.$eval('#ytc-video', v => v.paused), paused);
      await t.mouse.move(5, 5); await t.waitForSelector('#ytc-player:not(.menu)');
      await t.focus('#ytc-ccb'); await t.keyboard.press('Enter');
      assert.equal(await on(), false); assert.equal(await t.evaluate(() => document.activeElement.id), 'ytc-ccb');
      assert.equal(await t.$eval('#ytc-ccb .i-ccoff', e => getComputedStyle(e).opacity), '1', 'off icon');
      await t.keyboard.press('Space');
      assert.equal(await on(), true); assert.equal(await t.getAttribute('#ytc-ccb', 'aria-pressed'), 'true');
      assert.equal(await t.$eval('#ytc-ccb .i-ccon', e => getComputedStyle(e).clipPath), 'inset(0px)', 'on icon is filled immediately for keyboard');
      const volume = await t.$eval('#ytc-video', v => v.volume);
      for (const key of ['ArrowDown', 'ArrowUp']) {
        await t.keyboard.press(key); await t.waitForSelector('#ytc-player.menu .mcc');
        assert.ok(await t.evaluate(() => document.getElementById('ytc-menu').contains(document.activeElement)), 'options receive keyboard focus');
        assert.deepEqual(await t.$eval('#ytc-menu', e => { const s = getComputedStyle(e); return [s.opacity, s.transform]; }), ['1', 'none'], 'keyboard options are immediate');
        await t.keyboard.press('Escape');
        assert.equal(await t.getAttribute('#ytc-ccb', 'aria-expanded'), 'false');
        assert.equal(await t.evaluate(() => document.activeElement.id), 'ytc-ccb');
      }
      assert.equal(await t.$eval('#ytc-video', v => v.volume), volume, 'options arrows do not change volume');
      if (process.env.YTC_SHOTS) { await t.hover('#ytc-ccb'); await t.waitForTimeout(200); await t.screenshot({ path: path.join(process.env.YTC_SHOTS, 'captions-click-on-options.png') }); await t.keyboard.press('Escape'); }
      await t.focus('#ytc-ccb'); await t.keyboard.press('Enter');
      for (let i = 0; i < 40 && (await sw.evaluate(() => chrome.storage.local.get('ytcConfigV1'))).ytcConfigV1?.p?.cc !== false; i++) await t.waitForTimeout(50);
      assert.equal((await sw.evaluate(() => chrome.storage.local.get('ytcConfigV1'))).ytcConfigV1.p.cc, false);
      await t.reload({ waitUntil: 'commit' }); await t.waitForSelector('#ytc-ccb[aria-pressed="false"]');
      assert.equal(await on(), false, 'off survives reload');
    } finally {
      await t.close(); await page.bringToFront();
      await sw.evaluate(async cfg => { await chrome.storage.local.remove('ytcConfigV1'); await chrome.storage.local.set(cfg); }, before);
    }
  });

  await step('own player: captions on + quality remembered across a reload (chrome.storage)', async () => {
    await page.goto('https://www.youtube.com/watch?v=aqz-KE-bpKQ&t=4', { waitUntil: 'commit' });
    await waitT(() => document.getElementById('ytc-cc')?.textContent.startsWith('Caption line'), null, 8000);
    assert.ok(await page.locator('#ytc-player.cc').count());
    await waitT(() => document.getElementById('ytc-video').videoHeight === 144);
    await page.click('#ytc-qual');
    await page.locator('#ytc-menu button', { hasText: 'Auto' }).click();
    await keys('c');
    assert.equal(await page.locator('#ytc-player.cc').count(), 0);
  });

  await step('captions: Japanese source → English translation (tlang=en) by default; a deliberate choice sticks for that video only', async () => {
    const lastCc = () => log.timedtext.filter(u => u.includes('v=ccjapanese1')).pop() || '';
    await page.goto('https://www.youtube.com/watch?v=ccjapanese1', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    const wasOn = await page.evaluate(() => document.getElementById('ytc-player').classList.contains('cc'));
    if (!wasOn) await keys('c');
    await page.waitForFunction(() => document.getElementById('ytc-player').classList.contains('cc'));
    for (let i = 0; i < 40 && !lastCc(); i++) await page.waitForTimeout(100);
    assert.match(lastCc(), /lang=ja&tlang=en&fmt=json3$/, 'English translation of the human Japanese track');
    await page.hover('#ytc-ccb'); await page.waitForSelector('#ytc-menu .mls button');
    const items = page.locator('#ytc-menu .mls button');
    assert.deepEqual(await items.allTextContents(), ['Japanese (auto-generated)', 'Japanese', 'English (translated)']);
    assert.equal(await page.locator('#ytc-menu .mls button.on').textContent(), 'English (translated)');
    await items.nth(1).click();
    for (let i = 0; i < 40 && !/lang=ja&fmt/.test(lastCc()); i++) await page.waitForTimeout(100);
    assert.match(lastCc(), /lang=ja&fmt=json3$/);
    await page.keyboard.press('Escape');
    // Another video keeps the fixed preference; coming back (memory) keeps the choice.
    await page.locator('#ytc-list a[href^="/watch"]').first().click();
    await waitT(() => !location.search.includes('ccjapanese1'), null, 5000);
    const n = log.timedtext.length;
    await page.evaluate(() => history.back());
    await waitT(() => location.search.includes('ccjapanese1'), null, 5000);
    for (let i = 0; i < 40 && !log.timedtext.slice(n).some(u => u.includes('v=ccjapanese1')); i++) await page.waitForTimeout(100);
    assert.match(lastCc(), /lang=ja&fmt=json3$/, 'remembered for this video');
    if (!wasOn) {
      await keys('c');
      await page.waitForFunction(() => !document.getElementById('ytc-player').classList.contains('cc'));
    }
  });

  const rate = async () => (await vstate()).rate;
  await step('player menus: hover opens; Speed/Quality are vertical lists centred on their shortcut (1× / Auto at the bottom); passing over rows selects nothing, resting selects (dwell), a click is instant; departure closes; keyboard + Escape', async () => {
    await page.mouse.move(5, 5);
    await keys('<', 10); // back to the lowest step, then
    await keys('>', 3);  // 1×
    assert.equal(await rate(), 1);
    await page.hover('#ytc-speed');
    await page.waitForSelector('#ytc-player.menu #ytc-menu .msp', { timeout: 1000 });
    assert.equal(await page.locator('#ytc-menu input[type=range]').count(), 0, 'no horizontal slider');
    assert.deepEqual(await page.locator('#ytc-mpre button').allTextContents(), ['2.5×', '2×', '1.75×', '1.5×', '1.25×', '1×'], 'top → bottom, 1× next to the shortcut');
    // Geometry: the panel is centred over its shortcut and above it; the rows are one aligned column, top to bottom.
    const geo = id => page.evaluate(([sc, list]) => {
      const r = el => el.getBoundingClientRect(), m = r(document.getElementById('ytc-menu')), s = r(document.getElementById(sc));
      const rows = [...document.querySelectorAll('#' + list + ' button')].map(r);
      return { dc: (m.left + m.width / 2) - (s.left + s.width / 2), above: s.top - m.bottom, ys: rows.map(b => b.top), ls: rows.map(b => b.left), ws: rows.map(b => b.width) };
    }, id);
    let g = await geo(['ytc-speed', 'ytc-mpre']);
    assert.ok(Math.abs(g.dc) <= 1 && g.above > 0, 'Speed panel centred above its shortcut ' + JSON.stringify(g));
    assert.ok(g.ys.every((y, i) => !i || y > g.ys[i - 1]) && g.ls.every(x => Math.abs(x - g.ls[0]) < 0.5) && g.ws.every(w => Math.abs(w - g.ws[0]) < 0.5), 'rows aligned in one column ' + JSON.stringify(g));
    // Passing over rows on the way does not select: sweep 1× → 2× quickly (the rows in between are crossed).
    const row = l => page.locator('#ytc-mpre button', { hasText: new RegExp('^' + l.replace('.', '\\.') + '$') });
    const mid = async l => { const b = await row(l).boundingBox(); return [b.x + b.width / 2, b.y + b.height / 2]; };
    let [x, y] = await mid('1×');
    await page.mouse.move(x, y);
    [x, y] = await mid('2×');
    await page.mouse.move(x, y, { steps: 8 });
    await page.waitForTimeout(110);
    assert.equal(await rate(), 1, 'rows passed over select nothing (and the rested one not yet)');
    const fill = await page.evaluate(() => { const b = document.querySelector('#ytc-mpre .dw'); return b && { label: b.textContent, clip: getComputedStyle(b, '::before').clipPath, op: getComputedStyle(b, '::before').opacity }; });
    assert.ok(fill && fill.label === '2×' && fill.op !== '0', 'the rested row fills while it waits ' + JSON.stringify(fill));
    if (process.env.YTC_SHOTS) { const b = await page.locator('#ytc-menu').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'player-menu-speed-dwell.png'), clip: { x: b.x - 40, y: b.y - 20, width: b.width + 80, height: b.height + 100 } }); }
    await page.waitForTimeout(200);
    assert.equal(await rate(), 2, 'resting ~220 ms selects it');
    assert.equal(await page.locator('#ytc-mpre button.on').textContent(), '2×');
    assert.equal(await page.locator('#ytc-mpre .dw').count(), 0, 'the fill is gone once chosen');
    // A click is instant (no dwell wait), and wins over the dwell it started.
    await row('1.5×').click();
    assert.equal(await rate(), 1.5, 'click: at once');
    assert.equal(await page.locator('#ytc-mpre button.on').textContent(), '1.5×');
    await page.click('#ytc-menu .mrt button[aria-label=Faster]');
    assert.equal(await rate(), 1.55);
    assert.equal(await page.locator('#ytc-rate').textContent(), '1.55×', 'the readout carries the exact rate');
    assert.equal(await page.locator('#ytc-mpre button.on').count(), 0, 'an off-list rate selects no row');
    await page.mouse.move(5, 5);
    await page.waitForSelector('#ytc-player:not(.menu)', { state: 'attached', timeout: 1500 });
    await keys('>', 8);
    assert.equal(await rate(), 2.5, '> stops at 2.5');
    assert.equal(await page.locator('#ytc-rateb').textContent(), '2.5×');
    await keys('<', 6);
    assert.equal(await rate(), 1);
    assert.equal(await page.locator('#ytc-rateb').textContent(), '1×', 'the Speed button always shows the rate');
    // Quality: best at the top, Auto at the bottom next to the shortcut; centred on its shortcut.
    await page.hover('#ytc-qual');
    await page.waitForSelector('#ytc-menu .mq', { timeout: 1000 });
    assert.deepEqual(await page.locator('#ytc-mq button').allTextContents(), ['360p', '240p', '144p', 'Auto']);
    g = await geo(['ytc-qual', 'ytc-mq']);
    assert.ok(Math.abs(g.dc) <= 1 && g.above > 0, 'Quality panel centred above its shortcut ' + JSON.stringify(g));
    assert.ok(g.ys.every((y, i) => !i || y > g.ys[i - 1]) && g.ls.every(x => Math.abs(x - g.ls[0]) < 0.5), 'rows aligned ' + JSON.stringify(g));
    // Moving to another shortcut switches menus by hover.
    await page.hover('#ytc-ccb');
    await page.waitForSelector('#ytc-menu .mcc', { timeout: 1000 });
    assert.equal(await page.locator('#ytc-menu .mtab').count(), 0, 'one audio track: no tabs');
    assert.equal(await page.locator('#ytc-menu .msw').count(), 0, 'no duplicate captions switch');
    assert.equal(await page.getAttribute('#ytc-ccb', 'aria-pressed'), 'false');
    await page.click('#ytc-ccb');
    await page.waitForSelector('#ytc-player.cc');
    assert.equal(await page.getAttribute('#ytc-ccb', 'aria-pressed'), 'true');
    assert.ok(await page.locator('#ytc-player.menu').count(), 'stays open after a choice');
    await page.mouse.move(5, 5);
    await page.waitForSelector('#ytc-player:not(.menu)', { state: 'attached', timeout: 1500 });
    // Keyboard: Enter opens with focus inside, arrows move, Escape returns to the shortcut.
    await page.focus('#ytc-qual');
    await page.keyboard.press('Enter');
    await page.waitForSelector('#ytc-menu .mq');
    // Keyboard opens have no motion: fully shown on the frame it opens (motion law).
    assert.deepEqual(await page.evaluate(() => { const cs = getComputedStyle(document.getElementById('ytc-menu')); return [cs.opacity, cs.transform]; }), ['1', 'none'], 'keyboard open is instant');
    assert.ok(await page.evaluate(() => document.getElementById('ytc-menu').contains(document.activeElement)), 'focus in the menu');
    assert.equal(await page.evaluate(() => document.activeElement.textContent), 'Auto', 'the selected choice is focused');
    await page.keyboard.press('ArrowUp');
    assert.equal(await page.evaluate(() => document.activeElement.textContent), '144p', 'ArrowUp goes up the list');
    await page.mouse.move(5, 5);
    await page.waitForTimeout(500);
    assert.ok(await page.locator('#ytc-player.menu').count(), 'keyboard menu ignores pointer departure');
    await page.keyboard.press('Escape');
    await page.waitForSelector('#ytc-player:not(.menu)', { state: 'attached' });
    assert.equal(await page.evaluate(() => getComputedStyle(document.getElementById('ytc-menu')).visibility), 'hidden', 'Escape closes at once');
    assert.equal(await page.evaluate(() => document.activeElement.id), 'ytc-qual');
  });

  await step('captions: drag moves them, the size range scales them (0.5–2×), both remembered as one object', async () => {
    await waitT(() => document.getElementById('ytc-cc').textContent.startsWith('Caption line'), null, 5000);
    const box = await page.locator('#ytc-player').boundingBox();
    const cc = await page.locator('#ytc-cc').boundingBox();
    const paused = (await vstate()).paused;
    await page.mouse.move(cc.x + 4, cc.y + 4);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.4 + 4, box.y + box.height * 0.5 + 4, { steps: 5 });
    await page.mouse.up();
    const v = () => page.evaluate(() => { const s = getComputedStyle(document.getElementById('ytc-player')); return { x: +s.getPropertyValue('--cx'), y: +s.getPropertyValue('--cy'), s: +s.getPropertyValue('--cs') }; });
    let c = await v();
    assert.ok(Math.abs(c.x - 0.4) < 0.03 && Math.abs(c.y - 0.5) < 0.03, JSON.stringify(c));
    assert.equal((await vstate()).paused, paused, 'dragging captions does not toggle playback');
    const cc2 = await page.locator('#ytc-cc').boundingBox();
    assert.ok(Math.abs(cc2.x - (box.x + box.width * 0.4)) < 12, 'caption moved');
    await page.evaluate(() => { const r = document.getElementById('ytc-ccs'); r.value = '9'; r.dispatchEvent(new Event('input', { bubbles: true })); });
    assert.equal((await v()).s, 2, 'size capped at 2×');
    await page.evaluate(() => { const r = document.getElementById('ytc-ccs'); r.value = '1.5'; r.dispatchEvent(new Event('input', { bubbles: true })); });
    assert.equal((await v()).s, 1.5);
    // The mouse wheel over the caption resizes it (up = larger, 5 % a notch) and never scrolls the page.
    const ccb = await page.locator('#ytc-cc').boundingBox();
    await page.mouse.move(ccb.x + ccb.width / 2, ccb.y + ccb.height / 2);
    const y0 = await page.evaluate(() => scrollY);
    await page.mouse.wheel(0, -100); await page.mouse.wheel(0, -100);
    await page.waitForFunction(() => getComputedStyle(document.getElementById('ytc-player')).getPropertyValue('--cs') === '1.600');
    await page.mouse.wheel(0, 100);
    await page.waitForFunction(() => getComputedStyle(document.getElementById('ytc-player')).getPropertyValue('--cs') === '1.550');
    assert.equal(await page.evaluate(() => scrollY), y0, 'the wheel did not scroll the page');
    assert.equal(await page.locator('#ytc-ccv').textContent(), '155%');
    for (let i = 0; i < 12; i++) await page.mouse.wheel(0, -100);
    await page.waitForFunction(() => getComputedStyle(document.getElementById('ytc-player')).getPropertyValue('--cs') === '2.000');
    // Arrow keys while the size control is focused: ±5 %, no seeking or volume change.
    const before = await vstate();
    await page.focus('#ytc-ccs');
    await page.keyboard.press('ArrowDown');
    await page.waitForFunction(() => getComputedStyle(document.getElementById('ytc-player')).getPropertyValue('--cs') === '1.950');
    for (let i = 0; i < 10; i++) await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowUp');
    await page.waitForFunction(() => getComputedStyle(document.getElementById('ytc-player')).getPropertyValue('--cs') === '1.500');
    const after = await vstate();
    assert.ok(Math.abs(after.t - before.t) < 4, `no seek on the size control's arrows ${before.t} → ${after.t}`);
    await page.evaluate(() => document.activeElement.blur());
    await page.waitForTimeout(450); // batched preference write
    await page.reload({ waitUntil: 'commit' });
    await page.waitForSelector('#ytc-video');
    await waitT(() => getComputedStyle(document.getElementById('ytc-player')).getPropertyValue('--cs') === '1.500', null, 5000);
    c = await v();
    assert.ok(Math.abs(c.x - 0.4) < 0.03 && Math.abs(c.y - 0.5) < 0.03, 'position remembered ' + JSON.stringify(c));
  });

  await step('player zero clicks: the wheel over the Speed shortcut steps 1→1.25→1.5 with a toast and never scrolls the page; ends clamp with a nudge; the Speed readout steps 0.05; the wheel in the Quality menu moves the selection every step but switches the stream once it rests; dwell in Quality; keys unchanged', async () => {
    await page.goto('https://www.youtube.com/watch?v=aqz-KE-bpKQ', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await waitT(() => { const v = document.getElementById('ytc-video'); return !v.paused && v.currentTime > 0.5 && v.videoHeight === 360; }, null, 8000);
    await page.evaluate(() => scrollTo(0, 0));
    assert.equal(await rate(), 1);
    const center = async sel => { const b = await page.locator(sel).boundingBox(); return [b.x + b.width / 2, b.y + b.height / 2]; };
    const rateIs = r => waitT(want => document.getElementById('ytc-video').playbackRate === want, r, 1000);
    const toast = () => page.textContent('#ytc-toast');
    // 1. The menu closed (no hover): a wheel on the Speed shortcut is taken (no page scroll) and steps up once.
    const synth = await page.evaluate(() => { const e = new WheelEvent('wheel', { deltaY: -100, bubbles: true, cancelable: true }); document.getElementById('ytc-speed').dispatchEvent(e); return { prevented: e.defaultPrevented, open: document.getElementById('ytc-player').classList.contains('menu') }; });
    assert.deepEqual(synth, { prevented: true, open: false }, 'taken on the closed shortcut');
    assert.equal(await rate(), 1.25, 'one step up');
    assert.equal(await page.textContent('#ytc-rateb'), '1.25×');
    assert.equal(await toast(), '1.25×');
    await keys('<');
    assert.equal(await rate(), 1, 'keys unchanged and instant');
    // 2. A real wheel over the shortcut: 1 → 1.25 → 1.5, a toast each step, the page stays put.
    const y0 = await page.evaluate(() => scrollY);
    const [sx, sy] = await center('#ytc-speed');
    await page.mouse.move(sx, sy);
    const notch = async (dy, wait = 160) => { await page.mouse.wheel(0, dy); await page.waitForTimeout(wait); };
    await notch(-100);
    await rateIs(1.25);
    assert.equal(await toast(), '1.25×');
    await notch(-100);
    await rateIs(1.5);
    assert.equal(await toast(), '1.5×');
    assert.equal(await page.evaluate(() => scrollY), y0, 'the page never scrolls under the shortcut');
    assert.equal(await page.locator('#ytc-mpre button.on').textContent(), '1.5×', 'the open list follows');
    // The top end: 2.5× holds, the list nudges once.
    for (let i = 0; i < 3; i++) await notch(-100);
    await rateIs(2.5);
    await page.mouse.wheel(0, -100);
    const band = await page.evaluate(() => new Promise(res => requestAnimationFrame(() => res(document.getElementById('ytc-mpre').getAnimations().length))));
    assert.ok(band > 0, 'a nudge at the end');
    await page.waitForTimeout(160);
    assert.equal(await rate(), 2.5, 'clamped at the top');
    for (let i = 0; i < 6; i++) await notch(100);
    await rateIs(1);
    assert.equal(await page.locator('#ytc-mpre button.on').textContent(), '1×', 'clamped at the bottom (1×)');
    // The readout steps 0.05 (slower rates stay reachable with zero clicks).
    const [rx, ry] = await center('#ytc-rate');
    await page.mouse.move(rx, ry);
    await notch(100);
    await rateIs(0.95);
    assert.equal(await page.textContent('#ytc-rate'), '0.95×');
    await notch(-100);
    await rateIs(1);
    assert.equal(await page.evaluate(() => scrollY), y0, 'still no page scroll');
    // 3. Quality, menu open: the selection follows every step at once, the stream switches once, after the rest.
    await page.mouse.move(5, 5);
    await page.waitForSelector('#ytc-player:not(.menu)', { state: 'attached', timeout: 1500 });
    const [qx, qy] = await center('#ytc-qual');
    await page.mouse.move(qx, qy);
    await page.waitForSelector('#ytc-player.menu #ytc-menu .mq', { timeout: 1000 });
    const [ax, ay] = await center('#ytc-mq button.on');
    await page.mouse.move(ax, ay); // rests on Auto (already chosen: no dwell)
    const gv0 = log.gv.length, onRow = () => page.textContent('#ytc-mq button.on');
    const slid = () => page.evaluate(() => new Promise(res => requestAnimationFrame(() => requestAnimationFrame(() => { const m = document.querySelector('#ytc-mq > i.pmk'); res(m ? m.getAnimations().length : -1); }))));
    await page.mouse.wheel(0, -100);
    await waitT(() => document.querySelector('#ytc-mq button.on')?.textContent === '144p', null, 1000);
    assert.ok(await slid() > 0, 'the mark slides to 144p');
    assert.equal(await toast(), '144p');
    await page.waitForTimeout(60);
    await page.mouse.wheel(0, -100);
    const tw = Date.now();
    await waitT(() => document.querySelector('#ytc-mq button.on')?.textContent === '240p', null, 1000);
    assert.equal(await toast(), '240p');
    assert.deepEqual(log.gv.slice(gv0).filter(g => g.itag === '278' || g.itag === '242'), [], 'nothing switched while the wheel moves');
    await waitT(() => document.getElementById('ytc-video').videoHeight === 240, null, 9000);
    const got = log.gv.slice(gv0);
    assert.ok(!got.some(g => g.itag === '278'), 'the passed 144p was never fetched');
    const first = got.find(g => g.itag === '242');
    assert.ok(first && first.t - tw >= 150, 'switched once the wheel rested ' + (first && first.t - tw) + ' ms');
    assert.equal(await page.textContent('#ytc-qlbl'), '240');
    // 4. Dwell in Quality: resting on 360p selects it; then resting on Auto returns to Auto.
    const [hx, hy] = await center('#ytc-mq button >> nth=0');
    await page.mouse.move(hx, hy, { steps: 4 });
    await page.waitForTimeout(80);
    if (process.env.YTC_SHOTS) { const b = await page.locator('#ytc-menu').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'player-menu-quality-dwell.png'), clip: { x: b.x - 40, y: b.y - 20, width: b.width + 80, height: b.height + 100 } }); }
    assert.equal(await onRow(), '240p', 'not yet');
    await page.waitForTimeout(250);
    assert.equal(await onRow(), '360p', 'dwell chose 360p');
    await waitT(() => document.getElementById('ytc-video').videoHeight === 360, null, 9000);
    const [ax2, ay2] = await center('#ytc-mq button >> nth=-1');
    await page.mouse.move(ax2, ay2, { steps: 4 });
    await page.waitForTimeout(320);
    assert.equal(await onRow(), 'Auto', 'dwell chose Auto');
    // 5. The wheel elsewhere on the player still scrolls the page (only the shortcuts and menus take it).
    await page.mouse.move(5, 5);
    await page.waitForSelector('#ytc-player:not(.menu)', { state: 'attached', timeout: 1500 });
    const pl = await page.locator('#ytc-player').boundingBox();
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3);
    await page.mouse.wheel(0, 200);
    await waitT(y => scrollY > y, y0, 2000);
    await page.evaluate(() => scrollTo(0, 0));
  });

  await step('player motion: menu clickable on its opening frame, Speed→Quality→CC sweep never stuck or doubled, idle hide eased + instant wake, toast on the key frame, 1:1 scrub, no long tasks', async () => {
    await page.goto('https://www.youtube.com/watch?v=aqz-KE-bpKQ', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await waitT(() => { const v = document.getElementById('ytc-video'); return !v.paused && v.currentTime > 0.5; }, null, 8000);
    const center = async sel => { const b = await page.locator(sel).boundingBox(); return [b.x + b.width / 2, b.y + b.height / 2]; };
    const pl = await page.locator('#ytc-player').boundingBox();
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3);
    // 1. The class flip that opens the menu is observed before the next paint: the panel is visible, takes pointers and
    //    has its content on that same frame (only its opacity/scale is still easing in).
    await page.evaluate(() => {
      const box = document.getElementById('ytc-player'), m = document.getElementById('ytc-menu');
      const mo = new MutationObserver(() => {
        if (!box.classList.contains('menu')) return;
        mo.disconnect();
        const b = m.querySelector('button'), r = b && b.getBoundingClientRect(), cs = getComputedStyle(m);
        const hit = r && document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
        window.__open = { content: !!m.querySelector('.msp'), vis: cs.visibility, pe: cs.pointerEvents, hit: !!hit && (hit === b || b.contains(hit)), op: +cs.opacity, origin: cs.transformOrigin, mo: m.dataset.m };
      });
      mo.observe(box, { attributes: true, attributeFilter: ['class'] });
    });
    const [sx, sy] = await center('#ytc-speed');
    await page.mouse.move(sx, sy, { steps: 3 });
    await waitT(() => window.__open, null, 2000);
    const op = await page.evaluate(() => window.__open);
    assert.deepEqual([op.content, op.vis, op.pe, op.hit, op.mo], [true, 'visible', 'auto', true, 'speed'], 'menu usable on its first frame ' + JSON.stringify(op));
    assert.ok(op.op < 1, 'the first open eases in (opacity ' + op.op + ')');
    await page.waitForTimeout(300);
    assert.equal(await page.evaluate(() => getComputedStyle(document.getElementById('ytc-menu')).opacity), '1');
    if (process.env.YTC_SHOTS) for (const [sel, name] of [['#ytc-speed', 'speed'], ['#ytc-qual', 'quality'], ['#ytc-ccb', 'cc']]) {
      const [x, y] = await center(sel); await page.mouse.move(x, y); await page.waitForTimeout(250);
      await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `player-menu-${name}.png`), clip: { x: pl.x + pl.width - 380, y: pl.y + pl.height - 420, width: 380, height: 420 } });
    }
    if (process.env.YTC_SHOTS) { await page.mouse.move(sx, sy); await page.waitForTimeout(100); }
    // 2. Sweep Speed → Quality → CC along the toolbar: every frame shows exactly one menu, never none, never re-fading.
    await page.evaluate(() => {
      const box = document.getElementById('ytc-player'), m = document.getElementById('ytc-menu'), w = window;
      w.__sweep = { frames: 0, closed: 0, multi: 0, minOp: 1, kinds: [] }; w.__sweeping = true;
      const f = () => {
        if (!w.__sweeping) return;
        const k = ['msp', 'mq', 'mcc'].filter(c => m.querySelector('.' + c));
        const s = w.__sweep; s.frames++;
        if (!box.classList.contains('menu')) s.closed++;
        if (k.length !== 1) s.multi++;
        s.minOp = Math.min(s.minOp, +getComputedStyle(m).opacity);
        if (k[0] && s.kinds[s.kinds.length - 1] !== k[0]) s.kinds.push(k[0]);
        requestAnimationFrame(f);
      };
      requestAnimationFrame(f);
    });
    const [qx, qy] = await center('#ytc-qual'), [cx, cy] = await center('#ytc-ccb');
    await page.mouse.move(qx, qy, { steps: 4 });
    await page.waitForTimeout(60);
    await page.mouse.move(cx, cy, { steps: 6 });
    await page.waitForTimeout(150);
    await page.mouse.move(qx, qy, { steps: 6 });
    await page.mouse.move(sx, sy, { steps: 6 });
    await page.waitForTimeout(120);
    const sw = await page.evaluate(() => { window.__sweeping = false; return window.__sweep; });
    assert.equal(sw.closed, 0, 'never no menu while sweeping ' + JSON.stringify(sw));
    assert.equal(sw.multi, 0, 'always exactly one menu ' + JSON.stringify(sw));
    assert.equal(sw.minOp, 1, 'sweeping does not re-animate ' + JSON.stringify(sw));
    // Toolbar order is CC · Speed · Quality, so Quality → CC crosses Speed (16 ms intent opens it on the way).
    assert.ok(sw.kinds[1] === 'mq' && sw.kinds.includes('mcc'), 'each shortcut opened its menu ' + JSON.stringify(sw));
    assert.equal(sw.kinds[sw.kinds.length - 1], 'msp', 'ends on the menu under the pointer ' + JSON.stringify(sw));
    // Leaving closes it (grace), fading out with its content, then inert.
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3, { steps: 4 });
    await page.waitForSelector('#ytc-player:not(.menu)', { state: 'attached', timeout: 1500 });
    assert.equal(await page.evaluate(() => getComputedStyle(document.getElementById('ytc-menu')).pointerEvents), 'none', 'a closing menu takes no clicks');
    // 3. Idle: the still pointer lets the bar fade (eased), any move brings it back on the same frame.
    await waitT(() => document.getElementById('ytc-player').classList.contains('idle'), null, 4000);
    await page.waitForTimeout(80);
    const mid = +(await page.evaluate(() => getComputedStyle(document.querySelector('.pctl')).opacity));
    assert.ok(mid > 0 && mid < 1, 'idle hide eases out (opacity ' + mid + ' 80 ms in)');
    await page.waitForTimeout(600);
    assert.equal(await page.evaluate(() => getComputedStyle(document.querySelector('.pctl')).opacity), '0');
    const wake = () => page.evaluate(() => new Promise(res => {
      const box = document.getElementById('ytc-player');
      const mo = new MutationObserver(() => { if (!box.classList.contains('idle')) { mo.disconnect(); res(getComputedStyle(document.querySelector('.pctl')).opacity); } });
      mo.observe(box, { attributes: true, attributeFilter: ['class'] });
    }));
    let woke = wake();
    await page.mouse.move(pl.x + pl.width / 2 + 30, pl.y + pl.height / 3);
    assert.equal(await woke, '1', 'wake-up is instant');
    // 4. Leaving the player while playing hides the bar at once; moving elsewhere on the page does not wake it; coming back does.
    await page.mouse.move(5, 5);
    await waitT(() => document.getElementById('ytc-player').classList.contains('idle'), null, 500);
    await page.mouse.move(40, 8);
    await page.waitForTimeout(250);
    assert.ok(await page.evaluate(() => document.getElementById('ytc-player').classList.contains('idle')), 'moves outside the player do not wake it');
    woke = wake();
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3);
    assert.equal(await woke, '1', 'back over the player: shown at once');
    // 5. Keyboard feedback appears on the key's frame (no fade-in).
    const toast = page.evaluate(() => new Promise(res => {
      const box = document.getElementById('ytc-player');
      const mo = new MutationObserver(() => { if (box.classList.contains('toast')) { mo.disconnect(); res([getComputedStyle(document.getElementById('ytc-toast')).opacity, document.getElementById('ytc-toast').textContent]); } });
      mo.observe(box, { attributes: true, attributeFilter: ['class'] });
    }));
    await keys('l');
    assert.deepEqual(await toast, ['1', '+10 s']);
    // Frames: the same motion again with nothing but pointer moves and waits (no probes forcing style or layout):
    // menu open, sweep, close, idle fade and wake must produce no long tasks. (The largest frame gap is reported
    // only: headless Chromium paces rAF at 30–120 Hz and drops a frame now and then with nothing running.)
    await page.evaluate(() => {
      const w = window; w.__lt = []; w.__gap = 0; w.__probe = true;
      new PerformanceObserver(l => { if (w.__probe) w.__lt.push(...l.getEntries().map(e => Math.round(e.duration))); }).observe({ type: 'longtask' });
      let last = performance.now(); const f = t => { if (w.__probe) { w.__gap = Math.max(w.__gap, t - last); last = t; requestAnimationFrame(f); } }; requestAnimationFrame(f);
    });
    await page.mouse.move(sx, sy, { steps: 3 });
    await page.waitForTimeout(250);
    await page.mouse.move(qx, qy, { steps: 4 });
    await page.mouse.move(cx, cy, { steps: 6 });
    await page.waitForTimeout(150);
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3, { steps: 4 });
    await page.waitForTimeout(3600); // grace close, then idle (2.5 s) and its fade
    await page.mouse.move(pl.x + pl.width / 2 + 20, pl.y + pl.height / 3);
    await page.waitForTimeout(200);
    const probe = await page.evaluate(() => { window.__probe = false; return { lt: window.__lt, gap: Math.round(window.__gap), idle: document.getElementById('ytc-player').className }; });
    assert.deepEqual(probe.lt, [], 'no long tasks during menu open, sweep, close, idle fade and wake');
    console.log(`     motion frames: largest gap between animation frames ${probe.gap} ms, long tasks ${probe.lt.length}`);
    // 6. Progress: while playing the fill sits on the true position (glides, no lag); a scrub lands 1:1 on its frame.
    await page.waitForTimeout(400);
    const track = await page.evaluate(() => {
      const v = document.getElementById('ytc-video'), m = new DOMMatrix(getComputedStyle(document.querySelector('.ppos')).transform);
      return { shown: m.a * v.duration, t: v.currentTime, glide: document.getElementById('ytc-player').classList.contains('glide') };
    });
    assert.ok(track.glide && Math.abs(track.shown - track.t) < 0.35, 'the fill tracks playback ' + JSON.stringify(track));
    const bar = await page.locator('#ytc-seek').boundingBox();
    await page.mouse.move(bar.x + bar.width * 0.3, bar.y + bar.height / 2);
    await page.mouse.down();
    await page.mouse.move(bar.x + bar.width * 0.6, bar.y + bar.height / 2);
    const scrub = await page.evaluate(() => {
      const ppos = new DOMMatrix(getComputedStyle(document.querySelector('.ppos')).transform).a;
      const knob = document.querySelector('.pknob').getBoundingClientRect(), seek = document.getElementById('ytc-seek').getBoundingClientRect();
      return { ppos, knob: (knob.x - seek.x) / seek.width, drag: document.getElementById('ytc-player').classList.contains('drag') };
    });
    await page.mouse.up();
    assert.ok(scrub.drag && Math.abs(scrub.ppos - 0.6) < 0.01 && Math.abs(scrub.knob - 0.6) < 0.01, 'scrub tracks the pointer 1:1 ' + JSON.stringify(scrub));
    await waitT(() => Math.abs(document.getElementById('ytc-video').currentTime / document.getElementById('ytc-video').duration - 0.6) < 0.02, null, 3000);
  });

  await step('player morphs: play/pause icon morphs on a pointer click and flips at once on Space; mute/CC/theater morph (keys instant); menu panel morphs between menus, clickable mid-morph; selection mark slides (keys jump); center pulse; seek ripple', async () => {
    await page.goto('https://www.youtube.com/watch?v=aqz-KE-bpKQ', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await waitT(() => { const v = document.getElementById('ytc-video'); return !v.paused && v.currentTime > 0.3; }, null, 8000);
    const pl = await page.locator('#ytc-player').boundingBox();
    const center = async sel => { const b = await page.locator(sel).boundingBox(); return [b.x + b.width / 2, b.y + b.height / 2]; };
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3);
    const slow = async rate => { await cdp.send('Animation.enable'); await cdp.send('Animation.setPlaybackRate', { playbackRate: rate }); };
    const shots = process.env.YTC_SHOTS;
    const midShot = async (name, clip) => { if (shots) await page.screenshot({ path: path.join(shots, name + '.png'), ...(clip ? { clip } : {}) }); };
    // Every icon in the control bar sits on its button's centre (±1 px), in both states.
    const centred = () => page.evaluate(() => [...document.querySelectorAll('.prow .pb')].filter(b => b.offsetParent).flatMap(b => [...b.querySelectorAll('svg')].map(i => {
      const a = b.getBoundingClientRect(), r = i.getBoundingClientRect(); return { b: b.getAttribute('aria-label'), dx: +((r.left + r.width / 2) - (a.left + a.width / 2)).toFixed(1), dy: +((r.top + r.height / 2) - (a.top + a.height / 2)).toFixed(1) };
    })).filter(o => Math.abs(o.dx) > 1 || Math.abs(o.dy) > 1));
    assert.deepEqual(await centred(), [], 'player icons centred (playing)');
    // A state flip observed on the frame after the class changed: a pointer flip is mid-morph (a running `d`
    // transition, the shape between the two), a key flip is already final.
    const flip = (sel, cls) => page.evaluate(([sel, cls]) => new Promise(res => {
      const box = document.getElementById('ytc-player'), w = document.getElementById('ytc-watch'), el = document.querySelector(sel);
      const on = () => cls === 'theater' ? w.classList.contains('theater') : box.classList.contains(cls), was = on();
      const mo = new MutationObserver(() => {
        if (on() === was) return;
        mo.disconnect();
        requestAnimationFrame(() => requestAnimationFrame(() => {
          const mid = getComputedStyle(el).d, run = el.getAnimations().map(a => a.transitionProperty).filter(Boolean), kf = box.classList.contains('kf');
          setTimeout(() => res({ mid, end: getComputedStyle(el).d, run, kf }), 500);
        }));
      });
      mo.observe(box, { attributes: true, attributeFilter: ['class'] }); mo.observe(w, { attributes: true, attributeFilter: ['class'] });
      setTimeout(() => res(null), 3000);
    }), [sel, cls]);
    const P1 = '.prow .i-pp .p1';
    let f = flip(P1, 'paused');
    await page.click('.prow [data-act=p-toggle]');
    let r = await f;
    assert.ok(r && r.run.includes('d') && r.mid !== r.end && !r.kf, 'pointer pause: the icon morphs ' + JSON.stringify(r));
    assert.deepEqual(await centred(), [], 'player icons centred (paused)');
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3);
    f = flip(P1, 'paused');
    await page.keyboard.press(' ');
    r = await f;
    assert.ok(r && r.run.length === 0 && r.mid === r.end && r.kf, 'Space: the icon flips at once ' + JSON.stringify(r));
    // Mid-morph look (slowed 10×): play → pause.
    await slow(0.1);
    await page.click('.prow [data-act=p-toggle]');
    await page.waitForTimeout(700);
    await midShot('morph-playpause-mid', { x: pl.x, y: pl.y + pl.height - 56, width: 200, height: 56 });
    await slow(1);
    await page.waitForTimeout(400);
    if (await page.evaluate(() => document.getElementById('ytc-video').paused)) await page.keyboard.press('k');
    // Mute: the cross turns in and the waves leave (pointer); m flips at once.
    f = flip('.i-vm .vx', 'muted');
    await page.click('.prow [data-act=p-mute]');
    r = await page.evaluate(() => new Promise(res => requestAnimationFrame(() => res(document.querySelector('.i-vm .vx').getAnimations().map(a => a.transitionProperty)))));
    await f;
    assert.ok(r.includes('transform') || r.includes('opacity'), 'pointer mute morphs the icon ' + JSON.stringify(r));
    await page.keyboard.press('m');
    r = await page.evaluate(() => new Promise(res => requestAnimationFrame(() => res(document.querySelector('.i-vm .vx').getAnimations().length))));
    assert.equal(r, 0, 'm: instant');
    // Theater: the frame changes shape on a pointer click; t is instant.
    f = flip('.i-th path', 'theater');
    await page.click('.prow [data-act=p-theater]');
    r = await f;
    assert.ok(r && r.run.includes('d') && r.mid !== r.end, 'pointer theater: the frame morphs ' + JSON.stringify(r));
    f = flip('.i-th path', 'theater');
    await page.keyboard.press('t');
    r = await f;
    assert.ok(r && r.run.length === 0 && r.mid === r.end, 't: instant ' + JSON.stringify(r));
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3);
    // Menus: Speed open, then Quality: the panel keeps its opacity and morphs its surface (a clip on its ::before
    // between the two heights) while the new menu is in the DOM and its bottom row takes clicks on that frame.
    const [sx, sy] = await center('#ytc-speed');
    await page.mouse.move(sx, sy, { steps: 3 });
    await page.waitForSelector('#ytc-player.menu #ytc-menu .msp');
    await page.waitForTimeout(350);
    const h0 = await page.evaluate(() => document.getElementById('ytc-menu').getBoundingClientRect().height);
    const probe = page.evaluate(() => new Promise(res => {
      const m = document.getElementById('ytc-menu');
      const mo = new MutationObserver(() => {
        if (!m.querySelector('.mq')) return;
        mo.disconnect();
        const now = () => {
          const b = [...m.querySelectorAll('#ytc-mq button')].pop(), q = b.getBoundingClientRect(), hit = document.elementFromPoint(q.x + q.width / 2, q.y + q.height / 2);
          const surf = document.getAnimations().filter(a => a.effect && a.effect.target === m && a.effect.pseudoElement === '::before');
          return { clip: getComputedStyle(m, '::before').clipPath, mx: getComputedStyle(m).getPropertyValue('--mx'), h: m.getBoundingClientRect().height, op: getComputedStyle(m).opacity, hit: !!hit && (hit === b || b.contains(hit)), surf: surf.length };
        };
        const first = now();
        requestAnimationFrame(() => requestAnimationFrame(() => res({ first, next: now() })));
      });
      mo.observe(m, { childList: true, subtree: true });
      setTimeout(() => res(null), 2000);
    }));
    const [qx, qy] = await center('#ytc-qual');
    await page.mouse.move(qx, qy, { steps: 2 });
    const mp = await probe;
    assert.ok(mp, 'quality menu opened');
    assert.ok(mp.first.surf === 1 && mp.first.hit && mp.first.op === '1', 'the switch morphs the panel, content clickable on its frame ' + JSON.stringify(mp));
    // The surface (::before) is the panel's box extended by --mx (a shrinking morph still paints the old box) and
    // clipped by inset(top right bottom left): painted height = box + top/bottom extension − top/bottom clip.
    const ins = c => { const v = (/inset\(([^)]*?)(?: round [^)]*)?\)/.exec(c) || [0, '0'])[1].trim().split(/\s+/).map(parseFloat); return [v[0], v[1] ?? v[0], v[2] ?? v[0], v[3] ?? v[1] ?? v[0]]; };
    const ext = mx => { const v = (mx || '0').trim().split(/\s+/).map(x => -parseFloat(x) || 0); return [v[0], v[1] ?? v[0], v[2] ?? v[0], v[3] ?? v[1] ?? v[0]]; };
    const [ct, , cb] = ins(mp.next.clip), [et, , eb] = ext(mp.next.mx);
    const painted = mp.next.h + et + eb - ct - cb;
    assert.ok(Math.abs(mp.next.h - h0) > 4 && Math.min(h0, mp.next.h) - 1 <= painted && painted <= Math.max(h0, mp.next.h) + 1 && Math.abs(painted - mp.next.h) > 0.2,
      `the surface is between the two menus' heights mid-morph (old ${h0}, new ${mp.next.h}, painted ${painted}) ` + JSON.stringify(mp));
    await page.waitForTimeout(450);
    assert.equal(await page.evaluate(() => document.getElementById('ytc-menu').getAnimations({ subtree: true }).length + document.getAnimations().filter(a => a.effect && a.effect.pseudoElement === '::before' && a.effect.target.id === 'ytc-menu').length), 0, 'morph finished, nothing left running');
    // Mid-morph look (slowed): Quality → Speed.
    await slow(0.1);
    await page.mouse.move(sx, sy, { steps: 2 });
    await page.waitForTimeout(900);
    await midShot('morph-menu-mid', { x: pl.x + pl.width - 380, y: pl.y + pl.height - 420, width: 380, height: 420 });
    await slow(1);
    await page.waitForTimeout(500);
    // Selection mark (Speed presets): a pointer choice springs the tint from 1.0 to 1.5; a key jumps it.
    await page.locator('#ytc-mpre button', { hasText: /^1\.5×$/ }).click();
    const mk = await page.evaluate(() => new Promise(res => requestAnimationFrame(() => requestAnimationFrame(() => { const m = document.querySelector('#ytc-mpre > i.pmk'); res({ ok: !!m, run: m ? m.getAnimations().map(a => a.transitionProperty) : [] }); }))));
    assert.ok(mk.ok && mk.run.includes('clip-path'), 'the selection mark slides ' + JSON.stringify(mk));
    await page.waitForTimeout(450);
    const markOn = () => page.evaluate(() => { const m = document.querySelector('#ytc-mpre > i.pmk'), b = document.querySelector('#ytc-mpre button.on'), s = document.getElementById('ytc-mpre').getBoundingClientRect(), r = b.getBoundingClientRect(), v = (c => { const v = /inset\(([^)]*?)(?: round [^)]*)?\)/.exec(c)[1].trim().split(/\s+/).map(parseFloat); return [v[0], v[1] ?? v[0], v[2] ?? v[0], v[3] ?? v[1] ?? v[0]]; })(getComputedStyle(m).clipPath); return { dl: v[3] - (r.left - s.left), dt: v[0] - (r.top - s.top), dw: (s.width - v[1] - v[3]) - r.width, label: b.textContent, run: m.getAnimations().length }; });
    let mo = await markOn();
    assert.ok(Math.abs(mo.dl) < 0.6 && Math.abs(mo.dt) < 0.6 && Math.abs(mo.dw) < 0.6 && mo.label === '1.5×', 'the mark lands exactly on 1.5 ' + JSON.stringify(mo));
    await page.keyboard.press('>');
    mo = await page.evaluate(() => new Promise(res => requestAnimationFrame(() => requestAnimationFrame(() => res(document.querySelector('#ytc-mpre > i.pmk').getAnimations().length)))));
    assert.equal(mo, 0, 'a key moves the mark at once');
    mo = await markOn();
    assert.ok(Math.abs(mo.dl) < 0.6 && mo.label === '1.75×', 'the mark follows the key ' + JSON.stringify(mo));
    await page.locator('#ytc-mpre button', { hasText: /^1×$/ }).click();
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3, { steps: 3 });
    await page.waitForSelector('#ytc-player:not(.menu)', { state: 'attached', timeout: 1500 });
    // Center pulse: a click on the video leaves the action's icon growing and fading in the middle; Space does not.
    await page.waitForTimeout(300);
    await page.mouse.click(pl.x + pl.width / 2, pl.y + pl.height / 3);
    let pu = await page.evaluate(() => { const e = document.getElementById('ytc-pfx'); return { cls: e.className, run: e.getAnimations().map(a => a.animationName + ':' + a.playState) }; });
    assert.ok(/\bon\b/.test(pu.cls) && /\bpa\b/.test(pu.cls) && pu.run.includes('ytcpfx:running'), 'pause by click: pulse ' + JSON.stringify(pu));
    await page.waitForTimeout(700);
    await page.keyboard.press('k');
    pu = await page.evaluate(() => { const e = document.getElementById('ytc-pfx'); return { cls: e.className, run: e.getAnimations().map(a => a.animationName + ':' + a.playState) }; });
    assert.ok(/\bpa\b/.test(pu.cls) && !pu.run.includes('ytcpfx:running'), 'k: no pulse ' + JSON.stringify(pu));
    await slow(0.1);
    await page.mouse.click(pl.x + pl.width / 2, pl.y + pl.height / 3);
    await page.waitForTimeout(1200);
    await midShot('pulse-mid', { x: pl.x + pl.width / 2 - 120, y: pl.y + pl.height / 2 - 120, width: 240, height: 240 });
    await slow(1);
    await page.waitForTimeout(400);
    if (await page.evaluate(() => document.getElementById('ytc-video').paused)) await page.keyboard.press('k');
    // Seek ripple: pressing the bar swells a red dot where it landed (on pointerdown).
    const bar = await page.locator('#ytc-seek').boundingBox();
    await page.mouse.move(bar.x + bar.width * 0.4, bar.y + bar.height / 2);
    await page.mouse.down();
    const rp = await page.evaluate(() => { const e = document.getElementById('ytc-rip'), s = document.getElementById('ytc-seek'); return { run: e.getAnimations().map(a => a.animationName), r: +s.style.getPropertyValue('--r') }; });
    await page.mouse.up();
    assert.ok(rp.run.includes('ytcrip') && Math.abs(rp.r - 0.4) < 0.01, 'press ripple at the pointer ' + JSON.stringify(rp));
    // Captions mid-wipe (slowed): turning them on.
    await slow(0.1);
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3);
    await page.keyboard.press('c'); // (instant: the key)
    await page.keyboard.press('c');
    await page.waitForTimeout(500);
    await page.evaluate(() => scrollTo(0, 0));
    if (await page.locator('#ytc-player.cc').count()) await page.keyboard.press('c');
    await page.click('#ytc-ccb');
    await page.waitForTimeout(900);
    { const b = await page.locator('#ytc-player').boundingBox(); await midShot('morph-cc-mid', { x: b.x + b.width - 300, y: b.y + b.height - 56, width: 300, height: 56 }); }
    await slow(1);
    await page.waitForTimeout(500);
    await page.mouse.move(pl.x + pl.width / 2, pl.y + pl.height / 3, { steps: 3 });
    await page.keyboard.press('Escape');
    if (await page.evaluate(() => document.getElementById('ytc-player').classList.contains('cc'))) await page.keyboard.press('c');
    await page.waitForTimeout(100);
    assert.equal(await page.evaluate(() => document.getElementById('ytc-player').classList.contains('cc')), false, 'captions left off');
    for (const w of [1400, 2560]) {
      await page.setViewportSize({ width: w, height: w === 2560 ? 1440 : 900 });
      await page.waitForTimeout(300);
      const b = await page.locator('#ytc-player').boundingBox();
      await page.mouse.move(b.x + b.width / 2, b.y + b.height / 3);
      await page.evaluate(() => document.activeElement && document.activeElement.blur());
      await page.waitForTimeout(200);
      assert.deepEqual(await centred(), [], `player icons centred at ${w}px`);
      await midShot(`player-bar-${w}`, { x: b.x, y: b.y + b.height - 70, width: b.width, height: 70 });
    }
    await page.setViewportSize({ width: 1400, height: 900 });
  });

  // Chrome's headless action popup does not surface as a Playwright page. Load the exact manifest
  // popup document in an extension tab, with the watched YouTube tab active. Storage and tabs.sendMessage
  // are the real extension APIs; selecting or undoing through the popup never stubs a player action.
  const openToolbar = async () => {
    await page.bringToFront();
    const active = await sw.evaluate(() => chrome.tabs.query({ active: true, currentWindow: true }).then(ts => ts[0].id));
    const popup = await ctx.newPage();
    await popup.setViewportSize({ width: 360, height: 720 });
    const target = await sw.evaluate(() => chrome.runtime.getURL(chrome.runtime.getManifest().action.default_popup));
    await popup.goto(target);
    await popup.waitForSelector('#ytc-popup[data-ready=true]');
    await sw.evaluate(id => chrome.tabs.update(id, { active: true }), active);
    return popup;
  };
  const storedConfig = () => sw.evaluate(() => chrome.storage.local.get('ytcConfigV1').then(o => o.ytcConfigV1));
  // Every place a user reads the product's name: the text, titles and accessible names (aria-label, title, alt,
  // placeholder, aria-roledescription) of a document. The old name must be nowhere.
  const visibleNames = p => p.evaluate(() => {
    const out = [document.title, document.body ? document.body.innerText : ''];
    for (const e of document.querySelectorAll('*')) for (const a of ['aria-label', 'title', 'alt', 'placeholder', 'aria-roledescription']) if (e.hasAttribute(a)) out.push(e.getAttribute(a));
    for (const e of document.querySelectorAll('*')) for (const pseudo of ['::before', '::after']) { const c = getComputedStyle(e, pseudo).content; if (c && c !== 'none' && c !== 'normal') out.push(c); }
    return out.join('\n');
  });

  await step('Redline look: one palette (graphite, white, red) on every page, the toolbar and YouTube\'s own pages; no palette choice; a stored theme ("lumen"/"youtube") is harmless; YouTube\'s logo, name and titles inside the product, Redline only as the extension\'s name; no "Lumen" anywhere a user reads it', async () => {
    const saved = await storedConfig();
    let popup, other, native, fresh;
    try {
      // 0.13.3–0.13.8 stored the palette choice (and kept a new-tab hint in YouTube's localStorage, below).
      await sw.evaluate(cfg => chrome.storage.local.set({ ytcConfigV1: { ...(cfg || {}), theme: 'lumen' } }), saved);
      await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
      await page.evaluate(() => { window.paletteVideo = document.getElementById('ytc-video'); window.paletteVideo.pause(); });
      const before = await page.locator('#ytc-player').boundingBox();
      other = await ctx.newPage();
      await other.goto('https://www.youtube.com/results?search_query=palette', { waitUntil: 'commit' });
      await other.waitForSelector('#ytc-list .c');
      const manifest = await sw.evaluate(() => chrome.runtime.getManifest());
      assert.deepEqual([manifest.name, manifest.action.default_title], ['Redline', 'Redline preferences']);
      assert.ok(!/lumen/i.test(manifest.description) && /client for YouTube/.test(manifest.description) && /Not affiliated with YouTube or Google/.test(manifest.description), manifest.description);
      for (const [size, file] of Object.entries(manifest.icons)) {
        const dims = await sw.evaluate(f => fetch(chrome.runtime.getURL(f)).then(r => r.blob()).then(createImageBitmap).then(b => [b.width, b.height]), file);
        assert.deepEqual(dims, [+size, +size], `icon ${file}`);
      }
      assert.deepEqual(Object.keys(manifest.icons), ['16', '32', '48', '128']);
      assert.deepEqual(Object.keys(manifest.action.default_icon), ['16', '32']);

      popup = await openToolbar();
      assert.equal(await popup.locator('[id^=palette-], .palette-choice, [data-act=palette]').count(), 0, 'no palette choice');
      assert.deepEqual(await popup.evaluate(() => [document.title, document.querySelector('#ytc-popup header').textContent, getComputedStyle(document.querySelector('#ytc-popup header svg')).fill, !!document.getElementById('popup-sponsor')]), ['Redline', 'Redline', 'rgb(255, 0, 51)', true]);
      for (const p of [page, other, popup]) {
        assert.equal(await p.evaluate(() => document.documentElement.hasAttribute('data-ytc-palette')), false, 'no palette switching');
        assert.equal(await p.evaluate(() => getComputedStyle(document.body).backgroundColor), 'rgb(15, 15, 15)');
      }
      // A stored theme is ignored and kept as it was by the toolbar's own writes (it fades out with the client's config writes).
      await popup.click('#popup-sponsor');
      await popup.waitForFunction(() => chrome.storage.local.get('ytcConfigV1').then(o => o.ytcConfigV1.p.sbo === false));
      assert.equal((await storedConfig()).theme, 'lumen', 'a toolbar write preserves the stored field');
      await popup.click('#popup-sponsor');
      await popup.waitForFunction(() => chrome.storage.local.get('ytcConfigV1').then(o => o.ytcConfigV1.p.sbo === undefined));
      assert.deepEqual(await page.locator('#ytc-player').boundingBox(), before, 'same player geometry');
      assert.ok(await page.evaluate(() => window.paletteVideo === document.getElementById('ytc-video') && window.paletteVideo.paused), 'same media element and state');

      const colors = await page.evaluate(() => ({ bg: getComputedStyle(document.body).backgroundColor, logo: [getComputedStyle(document.querySelector('.logo rect')).fill, getComputedStyle(document.querySelector('.logo path')).fill], progress: getComputedStyle(document.querySelector('.ppos')).backgroundColor, focus: getComputedStyle(document.documentElement).getPropertyValue('--ac').trim() }));
      assert.deepEqual(colors, { bg: 'rgb(15, 15, 15)', logo: ['rgb(255, 0, 51)', 'rgb(255, 255, 255)'], progress: 'rgb(255, 0, 51)', focus: '#f1f1f1' });
      const tokens = await page.evaluate(() => { const s = getComputedStyle(document.documentElement); return Object.fromEntries(['bg', 'pn', 'rs', 'cb', 'hv', 'tx', 'mu', 'fa', 'ac', 'link', 'er'].map(k => [k, s.getPropertyValue('--' + k).trim()])); });
      const luminance = hex => { let h = hex.slice(1); if (h.length === 3) h = [...h].map(c => c + c).join(''); const c = [0, 2, 4].map(i => parseInt(h.slice(i, i + 2), 16) / 255).map(x => x <= .04045 ? x / 12.92 : ((x + .055) / 1.055) ** 2.4); return c[0] * .2126 + c[1] * .7152 + c[2] * .0722; };
      const contrast = (a, b) => { const x = luminance(a), y = luminance(b); return (Math.max(x, y) + .05) / (Math.min(x, y) + .05); };
      for (const text of ['tx', 'mu', 'fa', 'link', 'er']) for (const surface of ['bg', 'pn', 'rs']) assert.ok(contrast(tokens[text], tokens[surface]) >= 4.5, `${text} on ${surface}: AA text contrast`);
      for (const surface of ['cb', 'hv']) assert.ok(contrast(tokens.mu, tokens[surface]) >= 4.5, `secondary text on ${surface}: AA contrast`);
      assert.ok(contrast(tokens.bg, tokens.ac) >= 4.5, 'selected controls have AA contrast');

      // Names inside the product are YouTube's (owner decision, 7 Nov 2026): the window title, the top bar's logo, its
      // wordmark and its accessible name; Redline is only the extension's name (manifest, toolbar popup).
      assert.match(await page.title(), / - YouTube$/);
      assert.match(await other.title(), /^palette - YouTube$/);
      assert.deepEqual(await page.evaluate(() => { const l = document.getElementById('ytc-logo'); return [l.getAttribute('aria-label'), l.textContent, l.querySelector('svg').getAttribute('viewBox'), l.querySelectorAll('svg rect, svg path').length]; }), ['YouTube home', 'YouTube', '0 0 28 20', 2]);
      for (const p of [page, other]) assert.doesNotMatch(await visibleNames(p), /redline/i, 'no Redline inside the product');
      for (const p of [page, other, popup]) assert.doesNotMatch(await visibleNames(p), /lumen/i);
      if (process.env.YTC_SHOTS) {
        await page.mouse.move(700, 400);
        await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'redline-watch.png') });
        await other.screenshot({ path: path.join(process.env.YTC_SHOTS, 'redline-search.png') });
        await other.screenshot({ path: path.join(process.env.YTC_SHOTS, 'redline-topbar.png'), clip: { x: 0, y: 0, width: 1400, height: 64 } });
        await popup.screenshot({ path: path.join(process.env.YTC_SHOTS, 'redline-toolbar.png'), clip: { x: 0, y: 0, width: 360, height: 300 } });
        const b = await other.locator('#ytc-logo').boundingBox();
        await other.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
        await other.waitForTimeout(400);
        await other.screenshot({ path: path.join(process.env.YTC_SHOTS, 'redline-logo-hover.png'), clip: { x: 0, y: 0, width: 260, height: 64 } });
        await other.mouse.move(700, 500);
      }
      await popup.close(); popup = null;

      // A new tab paints its first frame graphite with nothing read first, and forgets the old palette flag.
      await page.evaluate(() => localStorage.setItem('ytc-palette-youtube', '1'));
      fresh = await ctx.newPage();
      // (Read in the first animation frame after the client's document is written: the style that frame paints.)
      await fresh.addInitScript(() => { new MutationObserver((_, o) => { if (document.getElementById('ytc')) { o.disconnect(); requestAnimationFrame(() => { window.firstBg = [getComputedStyle(document.documentElement).backgroundColor, getComputedStyle(document.body).backgroundColor]; window.firstAttr = document.documentElement.getAttribute('data-ytc-palette'); }); } }).observe(document, { childList: true, subtree: true }); });
      await fresh.goto('https://www.youtube.com/results?search_query=palette+fresh', { waitUntil: 'commit' });
      await fresh.waitForFunction(() => window.firstBg);
      assert.deepEqual(await fresh.evaluate(() => [window.firstBg, window.firstAttr]), [['rgb(15, 15, 15)', 'rgb(15, 15, 15)'], null], 'the first frame of a new tab');
      await fresh.waitForSelector('#ytc-list .c');
      assert.equal(await fresh.evaluate(() => localStorage.getItem('ytc-palette-youtube')), null, 'the old palette flag is removed');
      // Stored "youtube" (what 0.13.3+ wrote) is just as harmless.
      await sw.evaluate(cfg => chrome.storage.local.set({ ytcConfigV1: { ...(cfg || {}), theme: 'youtube' } }), saved);
      await fresh.reload({ waitUntil: 'commit' });
      await fresh.waitForSelector('#ytc-list .c');
      assert.deepEqual(await fresh.evaluate(() => [getComputedStyle(document.body).backgroundColor, document.documentElement.hasAttribute('data-ytc-palette')]), ['rgb(15, 15, 15)', false]);

      native = await ctx.newPage();
      await native.goto('https://www.youtube.com/watch?v=ntvfixture1&yt-native=1', { waitUntil: 'commit' });
      await native.waitForFunction(() => document.documentElement.dataset.ytcNative && document.querySelector('.ytp-play-progress'));
      assert.deepEqual(await native.evaluate(() => [getComputedStyle(document.documentElement).backgroundColor, getComputedStyle(document.querySelector('.ytp-play-progress')).backgroundColor, window.__ytAppRan]), ['rgb(15, 15, 15)', 'rgb(255, 0, 51)', true], 'YouTube\'s own page keeps its renderer and takes the palette');
      const logo = await native.evaluate(() => { const a = document.querySelector('ytd-topbar-logo-renderer a#logo'); return a && [getComputedStyle(a, '::after').content, getComputedStyle(a, '::before').content]; });
      if (logo) assert.deepEqual(logo, ['none', 'none'], 'YouTube\'s own header logo, unmodified');
      assert.doesNotMatch(await visibleNames(native), /lumen|redline/i);
    } finally {
      await popup?.close(); await other?.close(); await native?.close(); await fresh?.close();
      await sw.evaluate(cfg => cfg === undefined ? chrome.storage.local.remove('ytcConfigV1') : chrome.storage.local.set({ ytcConfigV1: cfg }), saved);
    }
  });

  await step('toolbar: failed saves roll back, retain unrelated preferences, and allow immediate retry', async () => {
    const saved = await storedConfig();
    const popup = await openToolbar();
    try {
      const was = await popup.getAttribute('#popup-sponsor', 'aria-checked');
      const flipped = was === 'true' ? 'false' : 'true';
      for (const sync of [false, true]) {
        await popup.evaluate(sync => {
          window.realStore = chrome.storage.local.set;
          chrome.storage.local.set = () => { if (sync) throw new Error('test save failure'); return Promise.reject(new Error('test save failure')); };
        }, sync);
        await popup.click('#popup-sponsor');
        await popup.waitForFunction(() => document.getElementById('popup-status').textContent.includes('Couldn’t save'));
        assert.equal(await popup.getAttribute('#popup-sponsor', 'aria-checked'), was, 'failed switch reverts');
        assert.deepEqual(await storedConfig(), saved, 'failed write leaves storage intact');
        await popup.evaluate(() => { chrome.storage.local.set = window.realStore; });
      }
      await popup.click('#popup-sponsor');
      await popup.waitForFunction(() => document.getElementById('popup-status').textContent === '');
      await popup.waitForFunction(off => chrome.storage.local.get('ytcConfigV1').then(o => (o.ytcConfigV1.p.sbo === false) === off), flipped === 'false');
      assert.equal(await popup.getAttribute('#popup-sponsor', 'aria-checked'), flipped, 'retry saves');
      const cfg = await storedConfig();
      for (const k of Object.keys(saved || {})) if (k !== 'p') assert.deepEqual(cfg[k], saved[k], `unrelated ${k} kept`);
    } finally {
      await popup.close();
      await sw.evaluate(cfg => cfg === undefined ? chrome.storage.local.remove('ytcConfigV1') : chrome.storage.local.set({ ytcConfigV1: cfg }), saved);
    }
  });

  await step('toolbar: rapid edits coalesce and a fresh read preserves concurrent player and category changes', async () => {
    const saved = await storedConfig();
    const popup = await openToolbar();
    try {
      assert.equal(await popup.getAttribute('#popup-sponsor', 'aria-checked'), 'true');
      await popup.evaluate(() => {
        window.realRead = chrome.storage.local.get.bind(chrome.storage.local);
        chrome.storage.local.get = key => new Promise(resolve => { window.finishRead = () => { chrome.storage.local.get = window.realRead; window.realRead(key).then(resolve); }; });
        for (const id of ['popup-sponsor', 'popup-sponsor', 'popup-sponsor', 'popup-cat-intro', 'popup-cat-intro', 'popup-cat-intro']) document.getElementById(id).click();
      });
      assert.equal(await popup.getAttribute('#popup-sponsor', 'aria-checked'), 'false', 'rapid input paints immediately, before storage answers');
      await sw.evaluate(() => chrome.storage.local.get('ytcConfigV1').then(o => {
        const cfg = o.ytcConfigV1 || {}; cfg.p = { ...cfg.p, vol: .37, cc: false, sb: 'sponsor,selfpromo,interaction,outro' };
        return chrome.storage.local.set({ ytcConfigV1: cfg });
      }));
      await popup.evaluate(() => window.finishRead());
      await popup.waitForFunction(() => chrome.storage.local.get('ytcConfigV1').then(o => o.ytcConfigV1.p.sbo === false && o.ytcConfigV1.p.sb === 'sponsor,selfpromo,interaction,intro,outro'));
      const cfg = await storedConfig();
      assert.equal(cfg.p.vol, .37, 'concurrent volume is preserved');
      assert.equal(cfg.p.cc, false, 'concurrent caption choice is preserved');
      assert.equal(await popup.getAttribute('#popup-cat-intro', 'aria-pressed'), 'true', 'last rapid category edit wins');
    } finally {
      await popup.close();
      await sw.evaluate(cfg => cfg === undefined ? chrome.storage.local.remove('ytcConfigV1') : chrome.storage.local.set({ ytcConfigV1: cfg }), saved);
    }
  });

  // ---- SponsorBlock (player/sponsor.rs, player/own/sb.rs)
  const sbCfg = async () => (await sw.evaluate(() => chrome.storage.local.get('ytcConfigV1'))).ytcConfigV1?.p || {};
  // Every played position (each animation frame) from now on, to prove no frame inside a skipped range played.
  const sbRec = () => page.evaluate(() => { const v = document.getElementById('ytc-video'); window.__sbT = []; const f = () => { window.__sbT.push(v.currentTime); window.__sbRaf = requestAnimationFrame(f); }; cancelAnimationFrame(window.__sbRaf); f(); });
  const sbSeen = () => page.evaluate(() => window.__sbT);
  await step('SponsorBlock: the lookup sends only the sha256 prefix (no cookies/referrer) in parallel with the player; the first frame does not wait; ranges fade in on the bar', async () => {
    SB.log.length = 0; SB.delay = 1500;
    await page.goto(`https://www.youtube.com/watch?v=${SB_ID}`, { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    const ffAt = await page.evaluate(() => performance.timeOrigin + +document.documentElement.dataset.ytcFirstFrame);
    // Caught on the frame the ranges arrive: they are fading in (an opacity transition from @starting-style).
    const fade = await (await page.waitForFunction(() => { const e = document.querySelector('#ytc-segs .psb'); return e && { run: e.getAnimations().map(a => a.transitionProperty).join() }; }, null, { polling: 'raf', timeout: 6000 })).jsonValue();
    SB.delay = 0;
    assert.equal(SB.log.length, 1, 'one lookup');
    const r = SB.log[0], u = new URL(r.url);
    assert.equal(u.pathname, '/api/skipSegments/' + sha4(SB_ID), 'only the 4-hex prefix of sha256(id)');
    assert.equal(sha4(SB_ID), '671e');
    assert.ok(!r.url.includes(SB_ID) && !r.url.includes('sponsorB'), 'the video id never leaves: ' + r.url);
    assert.ok(!r.headers.cookie && !r.headers.referer, 'no cookies, no referrer ' + JSON.stringify(r.headers));
    assert.deepEqual(JSON.parse(u.searchParams.get('categories')), ['sponsor', 'selfpromo', 'interaction', 'intro', 'outro', 'preview', 'music_offtopic', 'filler', 'poi_highlight'], 'every category asked (the answer does not reveal the choice)');
    assert.ok(r.sent < ffAt && ffAt < r.done, `sent with the player (${ffAt - r.sent} ms before the first frame), first frame ${r.done - ffAt} ms before the answer`);
    console.log(`     SponsorBlock lookup sent ${(ffAt - r.sent).toFixed(0)} ms before the first frame; the first frame did not wait for its 1.5 s answer`);
    assert.ok(fade.run.includes('opacity'), 'the ranges fade in ' + JSON.stringify(fade));
    // Four ranges (decoys from the same bucket are not ours), the highlight tick; sponsor at 8/40 of the bar.
    assert.equal(await page.locator('#ytc-segs .psb').count(), 4);
    assert.equal(await page.locator('#ytc-segs .psbp').count(), 1);
    const g = await page.evaluate(() => { const b = document.getElementById('ytc-segs').getBoundingClientRect(); return [...document.querySelectorAll('#ytc-segs .psb,#ytc-segs .psbp')].map(e => { const r = e.getBoundingClientRect(); return { c: e.className, l: (r.left - b.left) / b.width, r: (r.right - b.left) / b.width }; }); });
    const sp = g.find(x => x.c === 'psb k0');
    assert.ok(Math.abs(sp.l - 8 / 40) < 0.003 && Math.abs(sp.r - 14 / 40) < 0.003, 'sponsor range 8–14 s ' + JSON.stringify(g));
    assert.ok(Math.abs((g.find(x => x.c === 'psbp k8').l + g.find(x => x.c === 'psbp k8').r) / 2 - 34 / 40) < 0.003, 'highlight tick at 34 s');
    // The highlight is offered as a jump.
    await waitT(() => document.getElementById('ytc-sbt').classList.contains('on'));
    assert.equal(await page.textContent('#ytc-sbtl'), 'Highlight at 0:34');
    assert.equal(await page.textContent('#ytc-sbtb'), 'Jump');
    if (process.env.YTC_SHOTS) { await page.mouse.move(700, 400); await page.waitForTimeout(400); const b = await page.locator('#ytc-player').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'sponsor-bar.png'), clip: { x: b.x, y: b.y + b.height - 120, width: b.width, height: 120 } }); }
    await page.click('#ytc-sbtb');
    await waitT(() => Math.abs(document.getElementById('ytc-video').currentTime - 34) < 0.6);
    await page.evaluate(() => document.getElementById('ytc-video').pause());
    assert.ok(!(await page.evaluate(() => document.getElementById('ytc-sbt').classList.contains('on'))), 'the toast leaves after Jump');
  });

  await step('SponsorBlock: playing into a sponsor skips it at its start (touching interaction too) with "Skipped sponsor · Undo"; Undo goes back and plays it', async () => {
    await page.evaluate(() => { const v = document.getElementById('ytc-video'); v.currentTime = 5; v.playbackRate = 2; v.play(); });
    await page.waitForTimeout(150);
    await sbRec();
    await waitT(() => document.getElementById('ytc-sbt').classList.contains('on') && document.getElementById('ytc-sbtl').textContent.startsWith('Skipped'), null, 8000);
    await page.waitForTimeout(300);
    const seen = await sbSeen();
    const inside = seen.filter(t => t > 8.15 && t < 15.4);
    assert.deepEqual(inside, [], 'no frame inside the skipped 8–15.5 s');
    assert.ok(Math.max(...seen.filter(t => t < 8.5)) >= 7.5 && seen.some(t => t >= 15.5), 'played up to the sponsor, landed at 15.5 ' + JSON.stringify(seen.filter(t => t > 7 && t < 16.5).map(t => +t.toFixed(2))));
    console.log(`     SponsorBlock skip at ${Math.max(...seen.filter(t => t < 8.5)).toFixed(3)} s (segment starts at 8, tolerance 0.1; 2× speed)`);
    assert.equal(await page.textContent('#ytc-sbtl'), 'Skipped sponsor');
    assert.equal(await page.textContent('#ytc-sbtb'), 'Undo');
    assert.ok((await page.getAttribute('#ytc-sbt', 'class')).includes('k0'), 'sponsor color dot');
    if (process.env.YTC_SHOTS) { const b = await page.locator('#ytc-player').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'sponsor-toast.png'), clip: { x: b.x, y: b.y + b.height - 130, width: 420, height: 130 } }); }
    await page.click('#ytc-sbtb');
    await waitT(() => { const t = document.getElementById('ytc-video').currentTime; return t > 7.9 && t < 9; });
    assert.ok(!(await page.evaluate(() => document.getElementById('ytc-sbt').classList.contains('on'))), 'the toast leaves on Undo');
    await sbRec();
    await waitT(() => document.getElementById('ytc-video').currentTime > 15.8, null, 8000);
    const again = await sbSeen();
    const gaps = again.slice(1).map((t, i) => t - again[i]);
    assert.ok(Math.max(...gaps) < 0.6 && again.some(t => t > 11 && t < 13), 'after Undo the sponsor plays through (no jump) ' + Math.max(...gaps));
    assert.ok(!(await page.evaluate(() => document.getElementById('ytc-sbt').classList.contains('on'))), 'and is not skipped again');
  });

  await step('SponsorBlock: a manual seek into a segment only shows it; the tooltip names the category', async () => {
    const box = await page.locator('.pseek').boundingBox();
    await page.mouse.move(box.x + box.width * 27 / 40, box.y + box.height / 2, { steps: 4 });
    await waitT(() => document.getElementById('ytc-tipk').textContent === 'Self-promo');
    assert.equal(await page.getAttribute('#ytc-tipk', 'class'), 'ptk k1');
    if (process.env.YTC_SHOTS) { await page.waitForTimeout(600); const b = await page.locator('#ytc-player').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'sponsor-tooltip.png'), clip: { x: b.x, y: b.y + b.height - 120, width: b.width, height: 120 } }); }
    await page.mouse.click(box.x + box.width * 27 / 40, box.y + box.height / 2);
    await sbRec();
    await page.waitForTimeout(900);
    const seen = await sbSeen();
    await page.evaluate(() => document.getElementById('ytc-video').pause());
    assert.ok(seen.length > 10 && seen.every(t => t > 26.5 && t < 30), 'stayed inside self-promo 26–30 s ' + JSON.stringify([seen[0], seen.at(-1)]));
    assert.ok(seen.at(-1) - seen[0] > 0.8, 'and played');
    assert.ok(!(await page.evaluate(() => document.getElementById('ytc-sbt').classList.contains('on'))), 'no skip toast');
    await page.mouse.move(700, 300);
  });

  await step('SponsorSkip toolbar: categories and counts are remembered; off cancels skipping and all new lookups', async () => {
    assert.equal(await page.locator('#ytc-sbb').count(), 0, 'settings moved out of the video controls');
    let popup = await openToolbar();
    try {
      await popup.locator('summary').click();
      await popup.waitForFunction(() => document.getElementById('popup-cat-sponsor-count').textContent === '1');
      const rows = await popup.$$eval('.popup-categories button', bs => bs.map(b => [b.querySelector('span').textContent, b.getAttribute('aria-pressed') === 'true', b.querySelector('small').textContent]));
      assert.deepEqual(rows, [['Sponsor', true, '1'], ['Self-promo', true, '1'], ['Interaction', true, '1'], ['Intro', false, '1'], ['Outro', false, ''], ['Preview', false, ''], ['Non-music', false, ''], ['Filler', false, '']]);
      assert.equal(await popup.getAttribute('#popup-sponsor', 'aria-checked'), 'true');
      await popup.click('#popup-cat-intro');
      await popup.waitForFunction(() => chrome.storage.local.get('ytcConfigV1').then(o => o.ytcConfigV1.p.sb === 'sponsor,selfpromo,interaction,intro'));
      assert.equal(await popup.getAttribute('#popup-cat-intro', 'aria-pressed'), 'true');
      await popup.click('#popup-cat-intro');
      await popup.waitForFunction(() => chrome.storage.local.get('ytcConfigV1').then(o => o.ytcConfigV1.p.sb === undefined));
      await popup.click('#popup-sponsor');
      await waitT(() => document.getElementById('ytc-player').classList.contains('sboff') && !document.querySelector('#ytc-segs .psb'));
      await popup.waitForFunction(() => chrome.storage.local.get('ytcConfigV1').then(o => o.ytcConfigV1.p.sbo === false));
      assert.equal(await popup.getAttribute('#popup-sponsor', 'aria-checked'), 'false');
      if (process.env.YTC_SHOTS) await popup.screenshot({ path: path.join(process.env.YTC_SHOTS, 'sponsorskip-toolbar.png'), fullPage: true });
      await popup.close(); popup = null;
      await page.evaluate(() => { const v = document.getElementById('ytc-video'); v.currentTime = 7; v.play(); });
      await sbRec();
      await waitT(() => document.getElementById('ytc-video').currentTime > 10, null, 8000);
      await page.evaluate(() => document.getElementById('ytc-video').pause());
      const seen = await sbSeen();
      assert.ok(seen.some(t => t > 8.5 && t < 10), 'off: the sponsor plays');
      assert.ok(!(await page.evaluate(() => document.getElementById('ytc-sbt').classList.contains('on'))), 'off: no toast');
      const n = SB.log.length;
      await page.goto(`https://www.youtube.com/watch?v=${SB_ID2}`, { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
      await page.waitForTimeout(500);
      assert.equal(SB.log.length, n, 'off: no request');
      popup = await openToolbar();
      assert.equal(await popup.getAttribute('#popup-sponsor', 'aria-checked'), 'false', 'off survives reopening');
      await popup.click('#popup-sponsor');
      for (let i = 0; i < 30 && SB.log.length === n; i++) await page.waitForTimeout(100);
      assert.equal(SB.log.length, n + 1, 'on: looked up');
      assert.equal(new URL(SB.log.at(-1).url).pathname, '/api/skipSegments/' + sha4(SB_ID2));
      await popup.waitForFunction(() => chrome.storage.local.get('ytcConfigV1').then(o => o.ytcConfigV1.p.sbo === undefined));
      assert.equal((await sbCfg()).sbo, undefined);
      assert.equal(await page.locator('#ytc-segs .psb').count(), 0);
    } finally { await popup?.close(); }
  });

  await step('SponsorSkip toolbar: Undo reaches the active player without replacing it', async () => {
    await page.goto(`https://www.youtube.com/watch?v=${SB_ID}`, { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame && document.querySelector('#ytc-segs .psb'), null, 8000);
    await page.evaluate(() => { const v = document.getElementById('ytc-video'); window.toolbarVideo = v; v.currentTime = 7.7; v.playbackRate = 2; v.play(); });
    await waitT(() => document.getElementById('ytc-video').currentTime >= 15.5, null, 8000);
    await page.evaluate(() => document.getElementById('ytc-video').pause());
    const popup = await openToolbar();
    try {
      await popup.waitForSelector('#popup-undo[data-visible=true]');
      await popup.click('#popup-undo');
      await waitT(() => document.getElementById('ytc-video').currentTime < 9);
      assert.ok(await page.evaluate(() => window.toolbarVideo === document.getElementById('ytc-video')), 'same video');
      await popup.waitForSelector('#popup-undo[data-visible=false]', { state: 'attached' });
    } finally { await popup.close(); }
  });

  await step('SponsorSkip toolbar: off aborts a pending lookup; re-enabling gets a full timeout of its own', async () => {
    const id = 'sbTimeout01', prefix = sha4(id), delay = SB.delay;
    const saved = await storedConfig();
    let popup;
    try {
      SB.videos[id] = [['sponsor', 8, 14]]; SB.delay = 8000;
      const n = SB.log.length;
      await page.goto(`https://www.youtube.com/watch?v=${id}`, { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
      const first = SB.log.slice(n).find(r => r.prefix === prefix);
      assert.ok(first, 'first lookup sent');
      await page.waitForTimeout(Math.max(0, first.sent + 2100 - Date.now()));
      popup = await openToolbar();
      await popup.click('#popup-sponsor');
      await waitT(() => document.getElementById('ytc-player').classList.contains('sboff'));
      assert.equal(await page.locator('#ytc-segs .psb').count(), 0);
      SB.delay = 4500;
      await popup.click('#popup-sponsor');
      await waitT(() => !document.getElementById('ytc-player').classList.contains('sboff'));
      await page.waitForSelector('#ytc-segs .psb', { timeout: 6000, state: 'attached' });
      const requests = SB.log.slice(n).filter(r => r.prefix === prefix);
      assert.equal(requests.length, 2, 'new lookup after abort');
      assert.ok(requests[1].done - first.sent > 6000, 'fresh answer survives the old request’s timeout');
      assert.ok(requests[1].done - requests[1].sent >= 4400, 'fresh request had its own time budget');
    } finally {
      await popup?.close(); SB.delay = delay; delete SB.videos[id];
      await sw.evaluate(cfg => cfg === undefined ? chrome.storage.local.remove('ytcConfigV1') : chrome.storage.local.set({ ytcConfigV1: cfg }), saved);
    }
  });

  await step('own player: googlevideo 403 → one player refresh → plays', async () => {
    await page.goto('https://www.youtube.com/watch?v=refresh4033', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    for (let i = 0; i < 50 && !log.gv.some(g => g.v === 'refresh4033' && g.gen === 2 && g.a > 0); i++) await page.waitForTimeout(100);
    assert.equal(log.api.filter(a => a.ep === 'player' && a.body.videoId === 'refresh4033' && a.body.context.client.clientName === 'VISIONOS').length, 2);
    assert.ok(log.gv.some(g => g.v === 'refresh4033' && g.gen === 2 && g.a > 0));
    assert.ok(!page.url().includes('yt-native'));
  });

  // ---- Failures stay in the player: a panel in the
  // player box with the message and the reason, Try again (a new player, fresh VISIONOS) and Open on YouTube.
  const visCalls = (v, from = 0) => log.api.slice(from).filter(a => a.ep === 'player' && a.body.videoId === v && a.body.context.client.clientName === 'VISIONOS');
  const panel = async () => ({ url: new URL(page.url()).search, m: await page.textContent('#ytc-player.failed .pfm'), r: await page.textContent('#ytc-player.failed .pfr') });
  // The panel's buttons: label centred in its box (±0.5 px), both on one row, centred in the player (±1 px).
  const panelGeometry = () => page.evaluate(() => {
    const box = document.getElementById('ytc-player').getBoundingClientRect(), bs = [...document.querySelectorAll('#ytc-player .pfb .pfbt')];
    const off = bs.map(b => { const r = b.getBoundingClientRect(), g = document.createRange(); g.selectNodeContents(b); const t = g.getBoundingClientRect();
      return { b: b.textContent, dx: +((t.left + t.width / 2) - (r.left + r.width / 2)).toFixed(1), dy: +((t.top + t.height / 2) - (r.top + r.height / 2)).toFixed(1), top: r.top }; });
    const a = bs[0].getBoundingClientRect(), z = bs.at(-1).getBoundingClientRect(), p = document.querySelector('#ytc-player .pfail').getBoundingClientRect();
    return { off: off.filter(o => Math.abs(o.dx) > .5 || Math.abs(o.dy) > .5), row: Math.abs(off[0].top - off.at(-1).top), rowDx: +(((a.left + z.right) / 2) - (box.left + box.width / 2)).toFixed(1), panelDx: +(((p.left + p.right) / 2) - (box.left + box.width / 2)).toFixed(1), panelDy: +(((p.top + p.bottom) / 2) - (box.top + box.height / 2)).toFixed(1) };
  });
  await step('own player: forced failure (VISIONOS UNPLAYABLE) → the panel in the player (the page stays); reason in debug overlay', async () => {
    const docs = log.docs.length;
    await page.goto('https://www.youtube.com/watch?v=unplayable1&t=7', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-player.failed .pfail');
    await page.waitForTimeout(600);
    assert.deepEqual(await panel(), { url: '?v=unplayable1&t=7', m: "YouTube won't play this video here", r: 'VISIONOS UNPLAYABLE: Forced failure (e2e)' });
    assert.equal(log.docs.length, docs + 1, 'no navigation after the failure');
    assert.equal(await page.locator('#ytc-video').count(), 0, 'media released');
    assert.equal(await page.$eval('.pfr', e => getComputedStyle(e).userSelect), 'text', 'the reason is selectable');
    assert.deepEqual(await page.locator('#ytc-player .pfbt').allTextContents(), ['Try again', 'Open on YouTube']);
    for (const w of [1400, 2560]) {
      await page.setViewportSize({ width: w, height: w === 2560 ? 1440 : 900 });
      await page.waitForTimeout(300);
      const g = await panelGeometry();
      assert.deepEqual(g.off, [], `button labels centred at ${w}px`);
      assert.ok(g.row < .5 && Math.abs(g.rowDx) <= 1 && Math.abs(g.panelDx) <= 1 && Math.abs(g.panelDy) <= 1, `panel and buttons centred at ${w}px: ${JSON.stringify(g)}`);
      if (process.env.YTC_SHOTS) { const b = await page.locator('#ytc-player').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `player-failed-${w}.png`), clip: b }); }
    }
    await page.setViewportSize({ width: 1400, height: 900 });
    await page.keyboard.press('Backquote');
    await page.waitForSelector('#ytc-debug.on');
    const dbg = await page.locator('#ytc-debug').textContent();
    assert.match(dbg, /player failed: unplayable1 at 7s \(panel shown\)/);
    assert.match(dbg, /player failed \d+ min ago · unplayable1 at 7s · VISIONOS UNPLAYABLE: Forced failure \(e2e\)/);
    await page.keyboard.press('Backquote');
  });

  await step('debug center: the forced failure arrived as a full incident report (kind player-failed)', async () => {
    for (let i = 0; i < 50 && !incidents.some(x => x.kind === 'player-failed' && /Forced failure/.test(x.reason)); i++) await page.waitForTimeout(100);
    const r = incidents.find(x => x.kind === 'player-failed' && /Forced failure/.test(x.reason));
    assert.ok(r, 'incident delivered: ' + JSON.stringify(incidents.map(x => x.kind)));
    assert.equal(r.player.video, 'unplayable1');
    assert.ok(Array.isArray(r.player.requests) && r.player.requests.some(x => x.includes('player VISIONOS')), JSON.stringify(r.player.requests));
    assert.ok(!JSON.stringify(r).includes('SAPISID') && !JSON.stringify(r).includes('sig='), 'no secrets in the report');
  });

  await step('failure panel: Try again (pointer and Enter) re-mounts the player with a fresh VISIONOS call; Open on YouTube goes native at the time', async () => {
    const docs = log.docs.length, inc0 = incidents.length, n0 = visCalls('unplayable1').length;
    await page.click('#ytc-pfretry');
    for (let i = 0; i < 50 && visCalls('unplayable1').length < n0 + 1; i++) await page.waitForTimeout(100);
    await page.waitForSelector('#ytc-player.failed .pfail');
    assert.equal(visCalls('unplayable1').length, n0 + 1, 'one fresh VISIONOS call');
    await page.focus('#ytc-pfretry');
    await page.keyboard.press('Enter');
    for (let i = 0; i < 50 && visCalls('unplayable1').length < n0 + 2; i++) await page.waitForTimeout(100);
    await page.waitForSelector('#ytc-player.failed .pfail');
    assert.equal(visCalls('unplayable1').length, n0 + 2, 'Enter on the focused button retries too');
    assert.equal(log.docs.length, docs, 'Try again stays in the page');
    for (let i = 0; i < 30 && incidents.slice(inc0).filter(x => x.kind === 'player-failed').length < 2; i++) await page.waitForTimeout(100);
    assert.equal(incidents.slice(inc0).filter(x => x.kind === 'player-failed').length, 2, 'each attempt reports its failure');
    nativeOk.push('https://www.youtube.com/watch?v=unplayable1&t=7s&yt-native=1');
    await page.locator('#ytc-player .pfbt', { hasText: 'Open on YouTube' }).click();
    await page.waitForURL(/yt-native=1/, { timeout: 8000 });
    assert.equal(new URL(page.url()).search, '?v=unplayable1&t=7s&yt-native=1');
    await page.waitForFunction(() => window.__nativeBundle === 1, null, { timeout: 5000 });
  });

  await step('failure panel: members-only and age-restricted show their message, without an incident', async () => {
    for (const [v, m] of [['members0001', 'Members-only video'], ['agerestrict', 'Age-restricted — sign-in needed']]) {
      const inc0 = incidents.length;
      await page.goto(`https://www.youtube.com/watch?v=${v}`, { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-player.failed .pfail');
      assert.equal(await page.textContent('.pfm'), m);
      assert.equal(new URL(page.url()).search, `?v=${v}`);
      await page.waitForTimeout(300);
      assert.deepEqual(problems(inc0), [], 'expected, not an incident');
    }
  });

  // ---- Bot check recovery (2 Nov 2026): the other routes in parallel, the winner kept for the session.
  // SPA to the first related video: returns its id once its media is fetched.
  const relatedPlay = async () => {
    const card = page.locator('#ytc-list .c').filter({ has: page.locator('a.th[href^="/watch"]') }).first();
    const v = new URL(await card.locator('a.th').getAttribute('href'), 'https://www.youtube.com').searchParams.get('v');
    const docs = log.docs.length;
    await card.locator('a.th').hover();
    await page.waitForTimeout(150);
    await card.locator('a.t').click();
    for (let i = 0; i < 80 && !log.gv.some(g => g.v === v && g.a > 0); i++) await page.waitForTimeout(100);
    assert.ok(log.gv.some(g => g.v === v && g.a > 0), 'the next video plays from googlevideo');
    assert.equal(log.docs.length, docs, 'SPA');
    assert.ok(!page.url().includes('yt-native'));
    return v;
  };
  await step('own player: bot check → fresh visitor retry plays; later videos and prefetches keep that visitor', async () => {
    FX.bot = 'visitor';
    const mark = log.api.length;
    try {
      await page.goto('https://www.youtube.com/watch?v=botfresh001', { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
      assert.ok(!page.url().includes('yt-native'));
      const vis = visCalls('botfresh001', mark);
      assert.equal(vis.length, 2, 'the bot-checked call and one retry');
      assert.notEqual(vis[0].body.context.client.visitorData, FRESH_VD);
      assert.equal(vis[1].body.context.client.visitorData, FRESH_VD);
      assert.equal(vis[1].headers['x-goog-visitor-id'], FRESH_VD);
      assert.ok(vis.every(a => !a.headers.authorization && !a.headers.cookie), 'anonymous');
      const ids = log.api.slice(mark).filter(a => a.ep === 'visitor_id');
      assert.equal(ids.length, 1, 'one visitor_id');
      assert.ok(!ids[0].headers.cookie && !ids[0].headers['x-goog-visitor-id'] && !ids[0].body.context.client.visitorData, 'a new anonymous visitor');
      // Later videos in this session: the fresh visitor, no new visitor_id, no bot check.
      const v = await relatedPlay();
      assert.ok(visCalls(v, mark).some(a => a.body.context.client.visitorData === FRESH_VD), 'the next video goes by the fresh visitor');
      // Calls sent after the recovery's answer (+ a few ms to apply it); earlier prefetches may still go anonymous,
      // and one the page sent just before the answer reached it can land here up to ~150 ms later under load.
      const late = log.api.slice(mark).filter(a => a.ep === 'player' && a.body.context.client.clientName === 'VISIONOS' && a.body.videoId !== 'botfresh001' && a.t > vis[1].done + 30);
      const settled = late.filter(a => a.t > vis[1].done + 150 || a.body.context.client.visitorData === FRESH_VD);
      assert.ok(settled.length && settled.every(a => a.body.context.client.visitorData === FRESH_VD), 'every later VISIONOS call (prefetches too) uses it: ' + late.map(a => a.body.videoId + (a.body.context.client.visitorData === FRESH_VD ? '' : ' (old)')));
      assert.equal(log.api.slice(mark).filter(a => a.ep === 'visitor_id').length, 1, 'no second visitor_id');
      await page.keyboard.press('Backquote');
      await page.waitForSelector('#ytc-debug.on');
      assert.match(await page.locator('#ytc-debug').textContent(), /player own \(VISIONOS, fresh visitor\)/);
      await page.keyboard.press('Backquote');
    } finally { FX.bot = null; }
  });

  await step('own player: bot check on anonymous and fresh visitor → the account session plays (signed in), and stays the route', async () => {
    const had = (await ctx.cookies('https://www.youtube.com')).find(c => c.name === 'SAPISID');
    await ctx.addCookies([{ name: 'SAPISID', value: 'e2e-sapisid', domain: '.youtube.com', path: '/', secure: true }]);
    FX.bot = 'account';
    const mark = log.api.length;
    try {
      await page.goto('https://www.youtube.com/watch?v=botacct0001', { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
      assert.ok(!page.url().includes('yt-native'));
      await page.waitForTimeout(200);
      const vis = visCalls('botacct0001', mark);
      // The fresh visitor's VISIONOS call only goes out if visitor_id answers before the account call.
      assert.ok(vis.length === 2 || vis.length === 3, `anonymous, then the account session (and maybe a fresh visitor): ${vis.length}`);
      assert.equal(log.api.slice(mark).filter(a => a.ep === 'visitor_id').length, 1, 'visitor_id in parallel with the account call');
      assert.ok(!vis[0].headers.authorization, 'first call anonymous');
      const acct = vis.filter(a => a.headers.authorization);
      assert.equal(acct.length, 1);
      assert.match(acct[0].headers.authorization, /^SAPISIDHASH \d+_[0-9a-f]{40}/);
      assert.equal(acct[0].headers['x-goog-authuser'], '0');
      assert.equal(acct[0].body.context.client.visitorData, vis[0].body.context.client.visitorData, 'the account call keeps our own visitor');
      assert.ok(vis.slice(1).every(a => a.headers.authorization || a.body.context.client.visitorData === FRESH_VD), 'only the two recovery routes');
      // A later answer from the other attempt is ignored: still playing.
      await page.waitForTimeout(300);
      assert.ok(!page.url().includes('yt-native'));
      const v = await relatedPlay();
      const late = log.api.slice(mark).filter(a => a.ep === 'player' && a.body.context.client.clientName === 'VISIONOS' && a.body.videoId !== 'botacct0001' && a.t > acct[0].done + 30);
      assert.ok(visCalls(v, mark).some(a => a.headers.authorization), 'the next video (prefetched or on click) goes by the account session');
      assert.ok(late.length && late.every(a => a.headers.authorization), 'every later VISIONOS call (prefetches too) is signed: ' + late.map(a => a.body.videoId));
      assert.equal(log.api.slice(mark).filter(a => a.ep === 'visitor_id').length, 1, 'no new recovery');
    } finally {
      FX.bot = null;
      if (had) await ctx.addCookies([had]); else await ctx.clearCookies({ name: 'SAPISID' });
    }
  });

  await step('own player: bot check on every route → the panel (the page stays); the incident lists every step tried; the next video tries the remembered route only; Try again recovers fully', async () => {
    const had = (await ctx.cookies('https://www.youtube.com')).find(c => c.name === 'SAPISID');
    await ctx.addCookies([{ name: 'SAPISID', value: 'e2e-sapisid', domain: '.youtube.com', path: '/', secure: true }]);
    FX.bot = 'all';
    const mark = log.api.length, inc0 = incidents.length;
    try {
      await page.goto('https://www.youtube.com/watch?v=botall00001&t=5', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 8000 });
      const p = await panel();
      assert.equal(p.url, '?v=botall00001&t=5', 'the page stays');
      assert.equal(p.m, "YouTube asks to sign in to confirm you're not a bot");
      assert.match(p.r, /^VISIONOS LOGIN_REQUIRED: Sign in to confirm you’re not a bot · tried: anonymous \(bot check\), /);
      assert.equal(visCalls('botall00001', mark).length, 3, 'anonymous + account + fresh visitor');
      assert.equal(log.api.slice(mark).filter(a => a.ep === 'visitor_id').length, 1);
      let r;
      for (let i = 0; i < 50 && !(r = incidents.slice(inc0).find(x => x.kind === 'player-failed')); i++) await page.waitForTimeout(100);
      assert.ok(r, 'incident delivered');
      assert.match(r.reason, /^VISIONOS LOGIN_REQUIRED: Sign in to confirm you’re not a bot · tried: anonymous \(bot check\), /);
      for (const s of ['account session (bot check)', 'fresh visitor (bot check)']) assert.ok(r.reason.includes(s), r.reason);
      assert.ok(r.player.requests.some(x => x.includes('visitor_id')) && r.player.requests.some(x => x.includes('player VISIONOS (account session)')), JSON.stringify(r.player.requests));
      const str = JSON.stringify(r);
      assert.ok(!str.includes('e2e-sapisid') && !str.includes('SAPISIDHASH') && !str.includes(FRESH_VD), 'no cookie, auth or visitor in the report');
      // Was "VISIONOS bot check earlier: opening natively": now the next video (SPA, same session) tries the
      // remembered route once and, blocked again, shows the panel without the parallel recovery.
      const m2 = log.api.length;
      await page.evaluate(() => { history.pushState({}, '', '/watch?v=botall00002'); dispatchEvent(new PopStateEvent('popstate')); });
      await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 8000 });
      assert.equal(visCalls('botall00002', m2).length, 1, 'the remembered route only');
      assert.equal(log.api.slice(m2).filter(a => a.ep === 'visitor_id').length, 0, 'no parallel recovery');
      assert.match(await page.textContent('.pfr'), /every route was bot-checked shortly before/);
      assert.equal(new URL(page.url()).search, '?v=botall00002');
      // Try again: the full recovery (anonymous + account + fresh visitor), still blocked → the panel.
      const m3 = log.api.length;
      await page.click('#ytc-pfretry');
      for (let i = 0; i < 50 && visCalls('botall00002', m3).length < 3; i++) await page.waitForTimeout(100);
      assert.equal(visCalls('botall00002', m3).length, 3, 'Try again lifts the block: every route again');
      await page.waitForSelector('#ytc-player.failed .pfail');
      // The bot check is gone: Try again plays.
      FX.bot = null;
      await page.click('#ytc-pfretry');
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
      await page.waitForSelector('#ytc-video');
      assert.equal(new URL(page.url()).search, '?v=botall00002');
    } finally {
      FX.bot = null;
      if (had) await ctx.addCookies([had]); else await ctx.clearCookies({ name: 'SAPISID' });
    }
  });

  await step('own player: 403 again after the refresh → the panel at the current time; Try again resumes there with a fresh VISIONOS call (not the cached one)', async () => {
    failures.gone = false;
    await page.goto('https://www.youtube.com/watch?v=gone403gone', { waitUntil: 'commit' });
    await waitT(() => document.getElementById('ytc-video')?.currentTime >= 4, null, 9000);
    failures.gone = true;
    const docs = log.docs.length, inc0 = incidents.length;
    await page.click('#ytc-qual');
    await page.locator('#ytc-menu button', { hasText: '144p' }).click();
    await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 8000 });
    const p = await panel();
    assert.equal(p.url, '?v=gone403gone', 'the page stays');
    assert.equal(p.m, "This video isn't loading");
    assert.match(p.r, /^googlevideo HTTP 403 after a player refresh/);
    for (let i = 0; i < 30 && !incidents.slice(inc0).some(x => x.kind === 'player-failed'); i++) await page.waitForTimeout(100);
    assert.ok(incidents.slice(inc0).some(x => x.kind === 'player-failed' && /HTTP 403/.test(x.reason)), 'incident sent');
    await page.keyboard.press('Backquote');
    const at = +(await page.locator('#ytc-debug').textContent()).match(/player failed: gone403gone at (\d+)s/)[1];
    await page.keyboard.press('Backquote');
    assert.ok(at >= 4, `failed at ${at}s`);
    failures.gone = false;
    const n0 = visCalls('gone403gone').length;
    await page.click('#ytc-pfretry');
    await waitT(t => { const v = document.getElementById('ytc-video'); return v && v.readyState >= 2 && v.currentTime >= t; }, at, 9000);
    assert.equal(visCalls('gone403gone').length, n0 + 1, 'a fresh VISIONOS call');
    assert.equal(await page.locator('#ytc-player.failed').count(), 0);
    assert.equal(log.docs.length, docs, 'no navigation');
    await page.goto('https://www.youtube.com/watch?v=aqz-KE-bpKQ', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await page.click('#ytc-qual');
    await page.locator('#ytc-menu button', { hasText: 'Auto' }).click();
  });

  await step('own player: decodingInfo ruling out the format being loaded never strands it (incident tz8jz7IiQK0)', async () => {
    await page.evaluate(() => localStorage.setItem('ytc-test-caps', JSON.stringify({ 243: { s: false, e: false } })));
    try {
      await page.goto('https://www.youtube.com/watch?v=capschange1', { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
      await waitT(() => document.getElementById('ytc-video')?.videoHeight === 240, null, 9000);
      assert.ok(!page.url().includes('yt-native'), 'no native fallback');
    } finally {
      await page.evaluate(() => localStorage.removeItem('ytc-test-caps'));
    }
  });

  await step('own player: decodingInfo before any media starts from the smooth format, no chunks of the ruled-out one (incident v8avTs34rDU)', async () => {
    await page.evaluate(() => localStorage.setItem('ytc-test-caps', JSON.stringify({ 243: { s: false, e: false } })));
    try {
      await page.goto('https://www.youtube.com/watch?v=capsrestart', { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
      await waitT(() => document.getElementById('ytc-video')?.videoHeight === 240, null, 9000);
      await page.waitForTimeout(800);
      assert.ok(!page.url().includes('yt-native'), 'no native fallback');
      assert.deepEqual(log.gv.filter(g => g.v === 'capsrestart' && g.itag === '243' && g.a > 0), [], 'no 243 chunks after its index');
    } finally {
      await page.evaluate(() => localStorage.removeItem('ytc-test-caps'));
    }
  });

  await step('own player: a MediaSource that opens without its sourceopen event still plays (incident T2TuUC2fd4A, background tab)', async () => {
    await page.evaluate(() => localStorage.setItem('ytc-test-nosourceopen', '1'));
    try {
      await page.goto('https://www.youtube.com/watch?v=nosrcopen01', { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
      assert.ok(!page.url().includes('yt-native'), 'no native fallback');
    } finally {
      await page.evaluate(() => localStorage.removeItem('ytc-test-nosourceopen'));
    }
  });

  await step('own player: googlevideo silent for 2.5 s → fresh URLs once, then plays (no fallback)', async () => {
    await page.goto('https://www.youtube.com/watch?v=gvhangfirst', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 12000);
    assert.ok(!page.url().includes('yt-native'), 'no native fallback');
    const gens = [...new Set(log.gv.filter(g => g.v === 'gvhangfirst').map(g => g.gen))];
    assert.deepEqual(gens, [1, 2], 'asked with the first URLs, then the refreshed ones: ' + gens);
  });

  // ---- Stalls are not failures: "Still loading — retrying…" in the player, retries in place.
  const stuckShot = async name => {
    if (!process.env.YTC_SHOTS) return;
    const b = await page.locator('#ytc-player').boundingBox();
    await page.screenshot({ path: path.join(process.env.YTC_SHOTS, name + '.png'), clip: { x: b.x + b.width - 460, y: b.y, width: 460, height: 64 } });
    await page.screenshot({ path: path.join(process.env.YTC_SHOTS, name + '-player.png'), clip: b });
  };
  const incident = async (from, kind) => { let r; for (let i = 0; i < 50 && !(r = incidents.slice(from).find(x => x.kind === kind)); i++) await page.waitForTimeout(100); return r; };
  await step('own player: no first frame at 4 s → "Still loading — retrying…" in the player + a player-stuck incident; fresh URLs at 6 s; media arriving later recovers it (player-recovered)', async () => {
    const inc0 = incidents.length, docs = log.docs.length;
    STALL.hold.set('stallstart1', 'all');
    try {
      await page.goto('https://www.youtube.com/watch?v=stallstart1', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-player.stuck', { timeout: 9000 });
      await page.waitForTimeout(300);
      assert.ok(await page.locator('#ytc-pstuck').isVisible(), 'status visible');
      assert.equal(await page.textContent('#ytc-pstuck .pst'), 'Still loading — retrying…');
      assert.deepEqual(await page.locator('#ytc-pstuck .pfbt').allTextContents(), ['Try again', 'Open on YouTube']);
      // Non-blocking: the middle of the player is still the video's own surface.
      assert.equal(await page.evaluate(() => { const r = document.getElementById('ytc-player').getBoundingClientRect(); return document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2).className; }), 'pov');
      const geo = await page.evaluate(() => [...document.querySelectorAll('#ytc-pstuck .pfbt')].map(b => { const r = b.getBoundingClientRect(), g = document.createRange(); g.selectNodeContents(b); const t = g.getBoundingClientRect(); return [+((t.left + t.width / 2) - (r.left + r.width / 2)).toFixed(1), +((t.top + t.height / 2) - (r.top + r.height / 2)).toFixed(1)]; }));
      assert.ok(geo.every(([dx, dy]) => Math.abs(dx) <= .5 && Math.abs(dy) <= .5), 'status button labels centred: ' + JSON.stringify(geo));
      await stuckShot('player-stuck-1400');
      const r = await incident(inc0, 'player-stuck');
      assert.ok(r, 'player-stuck incident');
      assert.equal(r.reason, 'no first frame 4 s after the streams were chosen: retrying in place');
      assert.ok(r.player && r.player.stuck && r.player.video === 'stallstart1', 'with the full player report');
      for (let i = 0; i < 60 && !log.gv.some(g => g.v === 'stallstart1' && g.gen === 3); i++) await page.waitForTimeout(100);
      assert.deepEqual([...new Set(log.gv.filter(g => g.v === 'stallstart1').map(g => g.gen))], [1, 2, 3], 'refresh at 2.5 s, fresh URLs at 6 s');
      assert.equal(await page.locator('#ytc-player.stuck').count(), 1, 'still stuck, still the player');
      unstall('stallstart1');
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
      await waitT(() => !document.getElementById('ytc-player').classList.contains('stuck'), null, 2000);
      const ok = await incident(inc0, 'player-recovered');
      assert.ok(ok, 'player-recovered incident');
      assert.match(ok.reason, /^no first frame 4 s after the streams were chosen: recovered after \d+\.\d s \(1 retry with fresh URLs\)$/);
      assert.deepEqual(problems(inc0).map(x => x.split(':')[0]), ['player-stuck', 'player-recovered'], 'one episode, one outcome');
      assert.equal(log.docs.length, docs + 1, 'no navigation');
      assert.equal(new URL(page.url()).search, '?v=stallstart1');
      await waitT(() => { const v = document.getElementById('ytc-video'); return v && !v.paused && v.currentTime > 0.5; }, null, 6000);
    } finally { unstall('stallstart1'); }
  });

  await step('own player: index in but chunks stuck (data, no picture) → at 6 s a fresh MediaSource + fresh URLs; plays when they answer (2560 px)', async () => {
    const inc0 = incidents.length;
    STALL.hold.set('stalldata01', 'chunks');
    await page.setViewportSize({ width: 2560, height: 1440 });
    try {
      await page.goto('https://www.youtube.com/watch?v=stalldata01&t=10', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-player.stuck', { timeout: 9000 });
      await page.waitForTimeout(300);
      await stuckShot('player-stuck-2560');
      // gen 2: the throttle ladder (no headers on the URL's host, nor on its mirror → fresh URLs at ~1.9 s);
      // gen 3: the stuck retry at 6 s, which also rebuilds the MediaSource.
      for (let i = 0; i < 80 && !log.gv.some(g => g.v === 'stalldata01' && g.gen >= 3 && g.a > 0); i++) await page.waitForTimeout(100);
      assert.ok(log.gv.some(g => g.v === 'stalldata01' && g.gen >= 3 && g.a > 0), 'chunks asked again with fresh URLs');
      unstall('stalldata01');
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
      await waitT(() => { const v = document.getElementById('ytc-video'); return v && v.currentTime >= 10 && !document.getElementById('ytc-player').classList.contains('stuck'); }, null, 6000);
      const ok = await incident(inc0, 'player-recovered');
      assert.ok(ok, 'player-recovered incident');
      assert.match(ok.reason, /recovered after .* \(1 retry with fresh URLs, MediaSource rebuilt\)$/);
      assert.ok(ok.player.notes.some(n => n.includes('data but no picture: a fresh MediaSource')), ok.player.notes.join('\n'));
      assert.deepEqual(problems(inc0).map(x => x.split(':')[0]), ['player-stuck', 'player-recovered']);
    } finally { unstall('stalldata01'); await page.setViewportSize({ width: 1400, height: 900 }); }
  });

  await step('own player: the stuck status\' Try again re-mounts at once with a fresh VISIONOS call', async () => {
    STALL.hold.set('stalltry001', 'all');
    try {
      await page.goto('https://www.youtube.com/watch?v=stalltry001', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-player.stuck', { timeout: 9000 });
      const n0 = visCalls('stalltry001').length, docs = log.docs.length;
      await page.locator('#ytc-pstuck .pfbt', { hasText: 'Try again' }).click();
      unstall('stalltry001');
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
      assert.equal(visCalls('stalltry001').length, n0 + 1, 'a fresh VISIONOS call');
      assert.equal(log.docs.length, docs, 'same page');
      assert.equal(await page.locator('#ytc-player.stuck').count(), 0);
    } finally { unstall('stalltry001'); }
  });

  await step('own player: a prefetched (cached) player response whose URLs hang → fresh URLs after 0.8 s, same mirror', async () => {
    await page.goto('https://www.youtube.com/results?search_query=cache+hang', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    await page.evaluate(() => { const a = document.querySelector('#ytc-list a.t[href^="/watch"]'); a.setAttribute('href', '/watch?v=gvhangcach1'); });
    await page.hover('#ytc-list a.t[href="/watch?v=gvhangcach1"]');
    await page.waitForTimeout(700); // hover prefetch (VISIONOS cached)
    // The prefetch warms the video server's connection before the click (slow DNS/handshake, live 2 Nov 2026).
    assert.ok(await page.evaluate(() => !!document.querySelector('link[rel=preconnect][href="https://rr1---sn-fixture.googlevideo.com"][crossorigin]')), 'googlevideo preconnected on hover');
    const tc = Date.now();
    await page.click('#ytc-list a.t[href="/watch?v=gvhangcach1"]');
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 12000);
    assert.ok(!page.url().includes('yt-native'), 'stays in our player');
    const g = log.gv.filter(x => x.v === 'gvhangcach1');
    assert.deepEqual([...new Set(g.map(x => x.gen))], [1, 2], 'cached URLs, then fresh ones');
    assert.ok(g.every(x => x.host === 'rr1---sn-fixture.googlevideo.com'), 'no mirror switch for a stale cache');
    // From the click: the hover media prefetch may already have asked the cached URLs before it.
    const t1 = Math.max(tc, Math.min(...g.filter(x => x.gen === 1).map(x => x.t))), t2 = Math.min(...g.filter(x => x.gen === 2).map(x => x.t));
    assert.ok(t2 - t1 < 1600, `fresh URLs soon after the cached ones hung: ${t2 - t1} ms`);
  });

  await step('own player: a stuck googlevideo node → the next mirror from the URL (like YouTube), then plays', async () => {
    await page.goto('https://www.youtube.com/watch?v=gvnodehang1', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 15000);
    assert.ok(!page.url().includes('yt-native'), 'stays in our player');
    const hosts = [...new Set(log.gv.filter(g => g.v === 'gvnodehang1').map(g => g.host))];
    assert.ok(hosts.includes('rr1---sn-fixture2.googlevideo.com'), 'asked the mirror: ' + hosts);
  });

  await step('own player: a slow googlevideo node (first answers 4 s late) → the same ranges on the mirror after 0.7 s → plays at once, no refresh', async () => {
    await page.goto('https://www.youtube.com/watch?v=gvslowfirst', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
    const ff = await page.evaluate(() => +document.documentElement.dataset.ytcFirstFrame);
    const g = log.gv.filter(x => x.v === 'gvslowfirst');
    assert.ok(!page.url().includes('yt-native'), 'stays in our player');
    assert.ok(g.some(x => x.host === 'rr1---sn-fixture2.googlevideo.com'), 'asked the mirror: ' + [...new Set(g.map(x => x.host))]);
    assert.deepEqual([...new Set(g.map(x => x.gen))], [1], 'no player refresh needed');
    assert.ok(ff < 2500, `first frame ${ff.toFixed(0)} ms after navigation (the slow node alone: > 4000)`);
    // Later requests go to the host that answered.
    await page.waitForTimeout(1500);
    const later = log.gv.filter(x => x.v === 'gvslowfirst' && x.t > Date.now() - 1200);
    assert.ok(later.every(x => x.host === 'rr1---sn-fixture2.googlevideo.com'), 'later requests on the mirror: ' + later.map(x => x.host));
  });

  await step('own player: a hung VISIONOS request → asked again after 0.4 s → plays (no 8 s timeout)', async () => {
    const n0 = visCalls('vishang0001').length;
    await page.goto('https://www.youtube.com/watch?v=vishang0001', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 6000);
    const ff = await page.evaluate(() => +document.documentElement.dataset.ytcFirstFrame);
    const calls = visCalls('vishang0001').slice(n0);
    assert.equal(calls.length, 2, 'two VISIONOS requests');
    assert.ok(calls[1].t - calls[0].t >= 350 && calls[1].t - calls[0].t < 1000, `the second after ~0.4 s: ${calls[1].t - calls[0].t} ms`);
    assert.ok(ff < 2000, `first frame ${ff.toFixed(0)} ms after navigation`);
    assert.equal(await page.locator('#ytc-player .pfail').count(), 0, 'no failure panel');
  });

  await step('own player: resting on a video link prefetches its first media (index + first segment); the click uses it, with no second request', async () => {
    await page.goto('https://www.youtube.com/results?search_query=media+prefetch', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    await page.evaluate(() => { const a = document.querySelector('#ytc-list a.t[href^="/watch"]'); a.setAttribute('href', '/watch?v=mediapre001'); });
    await page.hover('#ytc-list a.t[href="/watch?v=mediapre001"]');
    for (let i = 0; i < 40 && log.gv.filter(g => g.v === 'mediapre001').length < 2; i++) await page.waitForTimeout(50);
    const pre = log.gv.filter(g => g.v === 'mediapre001');
    assert.equal(pre.length, 2, 'video and audio first requests while hovering: ' + JSON.stringify(pre));
    assert.ok(pre.every(g => g.a === 0 && g.b > 0), 'index + first segment ranges');
    await page.waitForTimeout(300);
    await page.click('#ytc-list a.t[href="/watch?v=mediapre001"]');
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    const after = log.gv.filter(g => g.v === 'mediapre001').slice(2);
    assert.ok(!after.some(g => g.a === 0), 'the prefetched first requests were used, not asked again: ' + JSON.stringify(after));
  });

  // How a start ends: the picture (and where it started), or the failure panel's reason.
  const startOutcome = async () => {
    for (let k = 0; k < 150; k++) {
      const r = await page.evaluate(() => { const v = document.getElementById('ytc-video');
        return { f: document.querySelector('#ytc-player.failed .pfr')?.textContent, ok: !!document.documentElement.dataset.ytcFirstFrame && v?.readyState >= 2, t: v?.currentTime }; });
      if (r.f) return { panel: r.f };
      if (r.ok) return { t: +r.t.toFixed(1) };
      await page.waitForTimeout(100);
    }
    return { timeout: true };
  };
  const playerNotes = async () => { await page.keyboard.press('Backquote'); await page.waitForSelector('#ytc-debug.on'); const d = await page.locator('#ytc-debug').textContent(); await page.keyboard.press('Backquote'); return d; };
  // Owner report (7 Nov 2026, Brave): "The browser couldn't play this video", media error 4
  // DEMUXER_ERROR_COULD_NOT_OPEN: MediaSource endOfStream before demuxer initialization completes.
  // A start at (or past) the end found nothing to fetch on either track, so the end of stream came
  // before any init segment. Back to a video that played to its end starts where the tab left it.
  await step('own player: a start at or past the end of the media (t= at/past the end, a saved resume point past the end, Back to an ended video) plays from the beginning, never the failure panel', async () => {
    const seen = {};
    for (const [v, t] of [['endexact001', SECONDS], ['endpast0001', SECONDS + 25]]) {
      await page.goto(`https://www.youtube.com/watch?v=${v}&t=${t}`, { waitUntil: 'commit' });
      seen[v] = await startOutcome();
    }
    // A resume record whose position is past this media's end (kept from a longer upload, imported):
    // stored once the last player's page is gone (its own records are written as it leaves), read by
    // a fresh page, then the video opened in the page.
    await page.goto('https://www.youtube.com/results?search_query=resume+past+the+end', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    await sw.evaluate(async now => { const r = (await chrome.storage.local.get('yourTubeProgressV1')).yourTubeProgressV1 || { version: 1, entries: {} };
      r.entries.endresume01 = { position: 55, duration: 100, completed: false, updatedAt: now }; await chrome.storage.local.set({ yourTubeProgressV1: r }); }, Date.now());
    await page.goto('https://www.youtube.com/results?search_query=resume+past+the+end+2', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    // The page reads the stored records at boot: on a busy machine that can come after the first try, which
    // then starts at 0 without using the record; try again until the record was used.
    let notes = '';
    for (let i = 0; i < 6; i++) {
      await page.waitForTimeout(300);
      await page.evaluate(() => { history.pushState(null, '', '/watch?v=endresume01'); dispatchEvent(new PopStateEvent('popstate')); });
      seen.endresume01 = await startOutcome();
      notes = seen.endresume01.panel ? '' : await playerNotes();
      if (seen.endresume01.panel || /start 55\.00 s is at the end/.test(notes)) break;
      await page.evaluate(() => { history.pushState(null, '', '/results?search_query=resume+past+the+end+2'); dispatchEvent(new PopStateEvent('popstate')); });
      await page.waitForSelector('#ytc-list .c');
    }
    // Back to a video that played to its end: the tab remembers where it stopped (the end).
    await page.goto('https://www.youtube.com/watch?v=endback0001', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await nearEnd();
    await waitT(() => document.getElementById('ytc-video').ended, null, 15000);
    await page.evaluate(() => { history.pushState(null, '', '/results?search_query=after+the+end'); dispatchEvent(new PopStateEvent('popstate')); });
    await page.waitForSelector('#ytc-list .c');
    await page.evaluate(() => delete document.documentElement.dataset.ytcFirstFrame);
    await page.goBack({ waitUntil: 'commit' });
    seen.endback0001 = await startOutcome();
    for (const [v, o] of Object.entries(seen)) assert.ok(o.t !== undefined && o.t < 5, `${v}: plays from the beginning, no panel: ${JSON.stringify(seen)}`);
    assert.match(notes, /start 55\.00 s is at the end \(40\.\d+ s\): from the beginning/, 'the saved resume point was used (and moved to the start)');
    await waitT(() => document.getElementById('ytc-video').currentTime > 1.5, null, 8000);
  });

  // Defence in depth: a media error before the first frame is retried in place (a fresh
  // MediaSource, then fresh URLs + indexes, then another format); the panel only after those.
  await step('own player: a media error before the first frame is retried by itself (fresh MediaSource, then fresh URLs) and plays, never the panel; when every retry fails, the panel names the automatic retries', async () => {
    // No VISIONOS hedge on a busy machine: its identical second request would be another player response.
    await page.evaluate(() => localStorage.setItem('ytc-test-nohedge', '1'));
    try {
    const inc0 = incidents.length, n0 = visCalls('mediaerr001').length;
    await page.goto('https://www.youtube.com/watch?v=mediaerr001', { waitUntil: 'commit' });
    const o = await startOutcome();
    assert.ok(o.t !== undefined, 'plays by itself, no panel: ' + JSON.stringify(o));
    await waitT(() => document.getElementById('ytc-video').currentTime > 1.5, null, 8000);
    assert.equal(await page.locator('#ytc-player.failed').count(), 0);
    assert.equal(visCalls('mediaerr001').length - n0, 2, 'the second retry asked for fresh URLs (one more VISIONOS call)');
    for (let i = 0; i < 50 && !incidents.slice(inc0).some(x => x.kind === 'player-recovered'); i++) await page.waitForTimeout(100);
    const rec = incidents.slice(inc0).filter(x => x.kind === 'player-recovered');
    assert.equal(rec.length, 1, JSON.stringify(incidents.slice(inc0).map(x => [x.kind, x.reason])));
    assert.match(rec[0].reason, /^(SourceBuffer error|appendBuffer|media error).* before the first frame: recovered after [\d.]+ s \(2 automatic retries\)$/);
    assert.equal(incidents.slice(inc0).filter(x => x.kind === 'player-failed').length, 0, 'no failure reported');
    const inc1 = incidents.length, n1 = visCalls('mediaerr002').length;
    await page.goto('https://www.youtube.com/watch?v=mediaerr002', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
    const p = await panel();
    assert.equal(p.m, "The browser couldn't play this video");
    assert.match(p.r, /\(after 3 automatic retries \(fresh MediaSource, fresh URLs, another format\)\)$/);
    assert.equal(visCalls('mediaerr002').length - n1, 2, 'one fresh-URL retry');
    const vids = new Set(log.gv.filter(g => g.v === 'mediaerr002' && media.formats.find(f => String(f.itag) === g.itag)?.mimeType.startsWith('video/')).map(g => g.itag));
    assert.ok(vids.size >= 2, 'the last retry tried another format: ' + [...vids]);
    for (let i = 0; i < 50 && !incidents.slice(inc1).some(x => x.kind === 'player-failed'); i++) await page.waitForTimeout(100);
    assert.equal(incidents.slice(inc1).filter(x => x.kind === 'player-failed').length, 1);
    } finally { await page.evaluate(() => localStorage.removeItem('ytc-test-nohedge')).catch(() => {}); }
  });

  // Owner report (7 Nov 2026, Brave): "The browser couldn't play this video — SourceBuffer error (video/mp4;
  // codecs="av01.0.08M.08")" on a video opened in a tab and come back to an hour later; Try again played it.
  // Any media error at any time recovers by itself where it was (a fresh MediaSource, fresh URLs, another format
  // or codec family), the panel only after three automatic recoveries within two minutes.
  const vel = () => page.evaluate(() => { const v = document.getElementById('ytc-video'); return { t: v ? v.currentTime : 0, p: v ? v.paused : null, rs: v ? v.readyState : 0, f: !!document.querySelector('#ytc-player.failed'), stuck: !!document.querySelector('#ytc-player.stuck') }; });
  // No VISIONOS hedge (its second request would be another player response) and, with `held`, no throttle
  // verdicts on requests the fixture holds on purpose.
  const noHedge = async (fn, held) => { const keys = ['ytc-test-nohedge', ...(held ? ['ytc-test-nothrottle'] : [])];
    await page.evaluate(k => k.forEach(x => localStorage.setItem(x, '1')), keys);
    try { return await fn(); } finally { await page.evaluate(k => k.forEach(x => localStorage.removeItem(x)), keys).catch(() => {}); } };
  await step('own player: a media error mid-playback recovers by itself at the same time, playing or paused (fresh MediaSource, then fresh URLs), never the panel', () => noHedge(async () => {
    for (const [v, paused] of [['midplayerr1', false], ['midplayerr3', true]]) {
      MIDERR.go = false;
      const inc0 = incidents.length, n0 = visCalls(v).length;
      await page.goto(`https://www.youtube.com/watch?v=${v}`, { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame && document.getElementById('ytc-video').currentTime > 2, null, 15000);
      if (paused) await page.evaluate(() => document.getElementById('ytc-video').pause());
      const t0 = (await vel()).t;
      // The next segment's answer (held until now) is one the browser refuses: an append error at t0.
      midRelease();
      const seen = [];
      for (let i = 0; i < 150; i++) {
        // Paused: the tab "shown" again and again while the recovery is on its way is no second fault.
        if (paused && i < 15) await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
        const r = await vel();
        seen.push(r);
        if (r.f) break;
        if (incidents.slice(inc0).some(x => x.kind === 'player-recovered') && (paused ? r.rs >= 2 : r.t > t0 + 2)) break;
        await page.waitForTimeout(100);
      }
      const kinds = incidents.slice(inc0).map(x => [x.kind, x.reason]);
      assert.ok(!seen.some(r => r.f), `${v}: no panel: ` + JSON.stringify(kinds));
      const rec = incidents.slice(inc0).filter(x => x.kind === 'player-recovered');
      assert.equal(rec.length, 1, `${v}: one recovered episode: ` + JSON.stringify(kinds));
      assert.match(rec[0].reason, /^(SourceBuffer error|appendBuffer|media error|MediaSource).* at 0:0\d: recovered after [\d.]+ s \(2 automatic retries\)$/);
      assert.equal(visCalls(v).length - n0, 2, `${v}: the second recovery asked for fresh URLs`);
      assert.ok(seen.every(r => r.t >= t0 - 0.6), `${v}: never back to the start (from ${t0}): ` + seen.map(r => r.t.toFixed(1)).join(' '));
      if (paused) {
        assert.ok(seen.every(r => r.p), `${v}: stayed paused`);
        assert.ok(Math.abs(seen.at(-1).t - t0) < 0.6, `${v}: at the same spot: ${seen.at(-1).t} vs ${t0}`);
        await page.evaluate(() => document.getElementById('ytc-video').play());
      }
      await waitT(t => document.getElementById('ytc-video').currentTime > t, t0 + 3, 10000);
      assert.equal(incidents.slice(inc0).filter(x => x.kind === 'player-failed').length, 0);
    }
    MIDERR.go = false;
  }, true));

  await step('own player: an AV1-only media error steps down to another codec family and plays; a codec that fails twice is avoided for the rest of the tab session', () => noHedge(async () => {
    const av1 = g => ['396', '395', '394'].includes(g.itag);
    const inc0 = incidents.length;
    await page.goto('https://www.youtube.com/watch?v=av1fault001', { waitUntil: 'commit' });
    const o = await startOutcome();
    assert.ok(o.t !== undefined, 'plays by itself, no panel: ' + JSON.stringify(o) + ' ' + JSON.stringify(incidents.slice(inc0).map(x => [x.kind, x.reason])));
    const gv1 = log.gv.filter(g => g.v === 'av1fault001');
    assert.ok(gv1.some(av1), 'it started on AV1: ' + [...new Set(gv1.map(g => g.itag))]);
    assert.ok(gv1.some(g => ['243', '242', '278'].includes(g.itag)), 'then VP9');
    await waitT(() => document.getElementById('ytc-video').currentTime > 1.5, null, 8000);
    assert.equal(await page.locator('#ytc-player.failed').count(), 0);
    for (let i = 0; i < 50 && !incidents.slice(inc0).some(x => x.kind === 'player-recovered'); i++) await page.waitForTimeout(100);
    const rec = incidents.slice(inc0).filter(x => x.kind === 'player-recovered');
    assert.equal(rec.length, 1, JSON.stringify(incidents.slice(inc0).map(x => [x.kind, x.reason])));
    assert.match(rec[0].reason, /av01.* before the first frame: recovered after [\d.]+ s \(2 automatic retries\)$/);
    assert.match(await playerNotes(), /AV1 given up after a codec fault/);
    // The same tab: AV1 fails once more (another video), then later videos start on VP9 at once.
    const open = async v => { await page.evaluate(v => { delete document.documentElement.dataset.ytcFirstFrame; history.pushState(null, '', '/watch?v=' + v); dispatchEvent(new PopStateEvent('popstate')); }, v); return startOutcome(); };
    const o2 = await open('av1fault002');
    assert.ok(o2.t !== undefined, 'the second AV1 video recovers too: ' + JSON.stringify(o2));
    assert.ok(log.gv.some(g => g.v === 'av1fault002' && av1(g)), 'still tried AV1 after one fault');
    const o3 = await open('av1fault003');
    assert.ok(o3.t !== undefined, JSON.stringify(o3));
    const gv3 = log.gv.filter(g => g.v === 'av1fault003');
    assert.ok(gv3.length && !gv3.some(av1), 'AV1 avoided after two faults: ' + [...new Set(gv3.map(g => g.itag))]);
    assert.equal(incidents.slice(inc0).filter(x => x.kind === 'player-failed').length, 0);
  }));

  await step('own player: when every automatic recovery of a mid-playback media error fails, the panel names them (never a loop)', () => noHedge(async () => {
    MIDERR.go = false;
    const inc0 = incidents.length, n0 = visCalls('midplayerr2').length;
    await page.goto('https://www.youtube.com/watch?v=midplayerr2', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame && document.getElementById('ytc-video').currentTime > 2, null, 15000);
    midRelease();
    await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
    const p = await panel();
    assert.equal(p.m, "The browser couldn't play this video");
    assert.match(p.r, / at 0:0\d \(after 3 automatic retries \(fresh MediaSource, fresh URLs, another format\)\)$/);
    assert.equal(visCalls('midplayerr2').length - n0, 2, 'one fresh-URL recovery');
    for (let i = 0; i < 50 && !incidents.slice(inc0).some(x => x.kind === 'player-failed'); i++) await page.waitForTimeout(100);
    assert.equal(incidents.slice(inc0).filter(x => x.kind === 'player-failed').length, 1);
    assert.equal(incidents.slice(inc0).filter(x => x.kind === 'player-recovered').length, 0);
    MIDERR.go = false;
  }, true));

  // A forgotten tab: URLs near their expiry (their own lifetime, `expiresInSeconds`: 20 min). The first URLs'
  // later segments never answer (stale), so only fresh URLs asked before the play needs data keep it playing
  // without a stall. (Condition waits only: no timing bounds.)
  const visWait = async (v, n0, n) => { for (let i = 0; i < 50 && visCalls(v).length - n0 < n; i++) await page.waitForTimeout(100); return visCalls(v).length - n0; };
  await step('own player: a video left paused with URLs near their expiry refreshes them on play or when the tab is shown, before it needs data, never a stall or the panel', () => noHedge(async () => {
    for (const [v, how] of [['staleurl001', 'play'], ['staleurl002', 'show']]) {
      const inc0 = incidents.length, n0 = visCalls(v).length;
      await page.goto(`https://www.youtube.com/watch?v=${v}`, { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame && document.getElementById('ytc-video').currentTime > 1.5, null, 15000);
      await page.evaluate(() => document.getElementById('ytc-video').pause());
      // Paused a while: the requests for the next segments got no answer (asked again on the wake).
      await page.waitForTimeout(3500);
      assert.equal(visCalls(v).length - n0, 1, `${v}: one player response so far`);
      if (how === 'show') {
        // The tab comes back (the bridge's visibilitychange handler; a headless page is never really hidden).
        await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
        assert.equal(await visWait(v, n0, 2), 2, `${v}: fresh URLs when the tab is shown, before any play`);
      }
      const t0 = (await vel()).t;
      await page.evaluate(() => document.getElementById('ytc-video').play());
      assert.equal(await visWait(v, n0, 2), 2, `${v}: fresh URLs ${how === 'play' ? 'on play' : 'once'}`);
      const seen = [];
      for (let i = 0; i < 120; i++) { const r = await vel(); seen.push(r); if (r.f || r.t > t0 + 6) break; await page.waitForTimeout(100); }
      const last = seen.at(-1);
      assert.ok(!last.f && last.t > t0 + 6, `${v}: plays on past the stale URLs' end: ${JSON.stringify(last)}`);
      assert.ok(!seen.some(r => r.stuck), `${v}: no "Still loading"`);
      assert.ok(log.gv.some(g => g.v === v && g.gen === 2 && g.a > 0), `${v}: the media came from the fresh URLs`);
      assert.equal(visCalls(v).length - n0, 2, `${v}: one refresh`);
      assert.equal(incidents.slice(inc0).filter(x => x.kind === 'player-stuck' || x.kind === 'player-failed').length, 0, JSON.stringify(incidents.slice(inc0).map(x => [x.kind, x.reason])));
    }
  }, true));

  // Healthy playback never loops on refreshes: URLs that stay short-lived (every answer's expiresInSeconds is
  // 20 min: a refresh that does not lengthen them) get exactly one proactive refresh, then none, and playing
  // is never held for it.
  await step('own player: URLs that stay near their expiry get one proactive refresh, then none (no loop); playback goes on', () => noHedge(async () => {
    const v = 'expirenoext', inc0 = incidents.length, n0 = visCalls(v).length;
    await page.goto(`https://www.youtube.com/watch?v=${v}`, { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame && document.getElementById('ytc-video').currentTime > 1, null, 15000);
    for (let k = 0; k < 4; k++) {
      await page.evaluate(() => document.getElementById('ytc-video').pause());
      await page.waitForTimeout(200);
      await page.evaluate(() => document.getElementById('ytc-video').play());
      if (k === 0) assert.equal(await visWait(v, n0, 2), 2, 'one proactive refresh');
      await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
      await page.waitForTimeout(400);
    }
    assert.equal(visCalls(v).length - n0, 2, 'no further refresh after one that did not lengthen the URLs');
    const t1 = (await vel()).t;
    await waitT(t => document.getElementById('ytc-video').currentTime > t + 2, t1, 10000);
    assert.match(await playerNotes(), /no more proactive refreshes/);
    assert.equal(incidents.slice(inc0).filter(x => ['player-stuck', 'player-failed', 'player-recovered'].includes(x.kind)).length, 0);
  }));

  // A background refresh that never answers ends by its own timeout (8 s): the URLs stay, no more proactive
  // refreshes, and the player is not left "refreshing" (a 403 later would wait for it for good).
  await step('own player: a background URL refresh that never answers times out: URLs kept, no more proactive refreshes, playback goes on', () => noHedge(async () => {
    const v = 'expirehang1', inc0 = incidents.length, n0 = visCalls(v).length;
    await page.goto(`https://www.youtube.com/watch?v=${v}`, { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame && document.getElementById('ytc-video').currentTime > 1, null, 15000);
    await page.evaluate(() => document.getElementById('ytc-video').pause());
    await page.evaluate(() => document.getElementById('ytc-video').play());
    assert.equal(await visWait(v, n0, 2), 2, 'one background refresh (it hangs)');
    let notes = '';
    for (let i = 0; i < 40 && !/background URL refresh: no answer/.test(notes); i++) { await page.waitForTimeout(500); notes = await playerNotes(); }
    assert.match(notes, /background URL refresh: no answer in 8 s/);
    for (let k = 0; k < 3; k++) { await page.evaluate(() => { const e = document.getElementById('ytc-video'); e.pause(); e.play(); document.dispatchEvent(new Event('visibilitychange')); }); await page.waitForTimeout(300); }
    assert.equal(visCalls(v).length - n0, 2, 'no further refresh');
    await waitT(() => { const e = document.getElementById('ytc-video'); return !e.paused && e.currentTime > 4; }, null, 15000);
    assert.equal(incidents.slice(inc0).filter(x => ['player-stuck', 'player-failed'].includes(x.kind)).length, 0);
  }));

  await step('own player: media intent dwell overlaps a slow VISIONOS response instead of starting again after it', async () => {
    const ids = ['mediarest01', 'mediarest02', 'mediarest03'];
    const answered = new Map(), waits = [];
    const hold = async route => {
      const b = route.request().postDataJSON();
      if (ids.includes(b?.videoId) && b?.context?.client?.clientName === 'VISIONOS') {
        await new Promise(resolve => setTimeout(resolve, 300));
        answered.set(b.videoId, Date.now());
      }
      await route.fallback();
    };
    await page.route('**/youtubei/v1/player*', hold);
    try {
      for (const id of ids) {
        await page.goto('https://www.youtube.com/results?search_query=media+rest', { waitUntil: 'commit' });
        await page.waitForSelector('#ytc-list .c a.t');
        await page.mouse.move(2, 2);
        await page.evaluate(v => document.querySelector('#ytc-list .c a.t').setAttribute('href', '/watch?v=' + v), id);
        await page.hover('#ytc-list .c a.t[href="/watch?v=' + id + '"]');
        for (let i = 0; i < 120 && log.gv.filter(g => g.v === id).length < 2; i++) await page.waitForTimeout(10);
        const heads = log.gv.filter(g => g.v === id);
        assert.equal(heads.length, 2, 'both tracks start while the pointer rests');
        waits.push(heads[0].t - answered.get(id));
        await page.mouse.move(2, 2);
      }
      // Three independent documents tolerate an isolated scheduling pause. The old
      // response-then-dwell path waits at least 150 ms in every one.
      assert.ok(median(waits) < 120, 'no second 150 ms dwell after the response: ' + waits.join(', '));
      console.log('     response → first media request, ms: ' + waits.join(', '));
    } finally { await page.unroute('**/youtubei/v1/player*', hold); }
  });

  // The warm first frame (own/warm.rs): the zoom shows the video's own first frame, never a thumbnail scaled up.
  // Watches the warm marks (decoders alive, frames made) and what enters the player box while it opens.
  const warmWatch = pg => pg.evaluate(() => {
    const d = document.documentElement, w = window.__warm = { live: [], states: [], added: [] };
    new MutationObserver(() => { w.live.push(+d.dataset.ytcWarmLive || 0); w.states.push(d.dataset.ytcWarm || ''); }).observe(d, { attributes: true, attributeFilter: ['data-ytc-warm', 'data-ytc-warm-live'] });
    new MutationObserver(ms => { for (const m of ms) for (const n of m.addedNodes) if (n.nodeType === 1 && n.closest('#ytc-player')) {
      const v = document.getElementById('ytc-video'), f = n.matches('canvas.pwf') ? n : null, x = f && f.getContext('2d').getImageData(0, 0, f.width, f.height).data;
      let lit = 0; if (x) for (let i = 0; i < x.length; i += 4) if (x[i] + x[i + 1] + x[i + 2] > 30) lit++;
      w.added.push({ tag: n.tagName, cls: n.className, w: f && f.width, h: f && f.height, lit: x ? lit / (x.length / 4) : null, afterVideo: n.previousElementSibling === v,
        poster: v && v.getAttribute('poster'), imgs: document.querySelectorAll('#ytc-player img').length, zooming: !!document.getElementById('ytc-player')?.getAnimations().some(a => a.id === 'flip'),
        frameShown: !!d.dataset.ytcFirstFrame && +d.dataset.ytcFirstFrame > 0 && v && v.readyState >= 2 });
    } }).observe(document.body, { childList: true, subtree: true });
  });
  const warmState = pg => pg.evaluate(() => ({ s: document.documentElement.dataset.ytcWarm || '', now: +document.documentElement.dataset.ytcWarmLive || 0, made: +document.documentElement.dataset.ytcWarmMade || 0, ...window.__warm }));
  await step('open: resting on a card decodes its first frame from the prefetch (no request, decoder released at once); the click zooms that frame, sharp (no thumbnail poster), and the player\'s own frame takes over; one warm frame at a time, dropped when intent moves', async () => {
    await page.goto('https://www.youtube.com/results?search_query=warm+first+frame', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c[data-v] a.th');
    await page.mouse.move(2, 2);
    const ids = ['warmfrm0001', 'warmfrm0002'];
    await page.evaluate(ids => { const cs = document.querySelectorAll('#ytc-list .c[data-v]'); ids.forEach((v, i) => cs[i].querySelectorAll('a[href^="/watch"]').forEach(a => a.setAttribute('href', '/watch?v=' + v))); }, ids);
    await warmWatch(page);
    const g0 = log.gv.length, mine = v => log.gv.slice(g0).filter(g => g.v === v);
    const thumb = i => page.locator('#ytc-list .c[data-v] a.th').nth(i);
    await thumb(0).hover();
    await page.waitForFunction(v => document.documentElement.dataset.ytcWarm === 'ready:' + v, ids[0], { timeout: 6000 });
    let w = await warmState(page);
    assert.equal(mine(ids[0]).length, 2, 'only the media prefetch\'s two requests (the warm decode fetches nothing): ' + JSON.stringify(mine(ids[0])));
    assert.equal(w.now, 0, 'the warm decoder is released once its frame is drawn');
    // Intent moves to another card: the first warm frame goes, the second is decoded; never two decoders.
    await thumb(1).hover();
    await page.waitForFunction(v => document.documentElement.dataset.ytcWarm === 'ready:' + v, ids[1], { timeout: 6000 });
    w = await warmState(page);
    assert.ok(Math.max(...w.live) <= 1, 'one warm decoder at a time: ' + w.live);
    const i1 = w.states.findIndex(x => x.endsWith(ids[1]));
    assert.ok(i1 > 0 && !w.states.slice(i1).some(x => x.endsWith(ids[0])), 'the first card\'s frame dropped when intent moved: ' + w.states);
    // Back on the first card: decoded again from its kept prefetch (no request), then a pointer click opens it.
    await thumb(0).hover();
    await page.waitForFunction(v => document.documentElement.dataset.ytcWarm === 'ready:' + v, ids[0], { timeout: 6000 });
    const made = (await warmState(page)).made;
    const tc = Date.now();
    const b = await thumb(0).boundingBox();
    await page.mouse.click(b.x + b.width / 2, b.y + b.height / 2);
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await page.waitForFunction(() => !document.querySelector('#ytc-player .pwf'), null, { timeout: 3000 });
    w = await warmState(page);
    const f = w.added.find(a => a.tag === 'CANVAS');
    assert.ok(f, 'the warm frame entered the player box: ' + JSON.stringify(w.added));
    assert.ok(f.cls === 'pwf' && f.afterVideo && f.zooming && !f.frameShown, 'over the <video>, while the box zooms, before the player\'s own first frame: ' + JSON.stringify(f));
    assert.ok(f.w >= 640 && f.h >= 360 && f.lit > 0.5, 'the video\'s own first frame at its resolution (640×360 fixture): ' + JSON.stringify(f));
    assert.ok(w.added.every(a => !a.poster && !a.imgs), 'no thumbnail poster or image in the zooming box: ' + JSON.stringify(w.added));
    assert.equal(w.s, 'used:' + ids[0]);
    assert.equal(w.made, made, 'nothing decoded again at the click');
    assert.ok(!mine(ids[0]).some(g => g.t >= tc && g.a === 0), 'the prefetched first requests were used: ' + JSON.stringify(mine(ids[0]).map(g => [g.itag, g.a, g.b])));
    assert.equal(await page.getAttribute('#ytc-video', 'poster'), null, 'no poster');
  });

  await step('own player: a media prefetch still loading is cancelled when the pointer leaves the link (no stray requests)', async () => {
    STALL.hold.set('mediapre002', 'all');
    const aborted = [];
    const onFail = r => { if (r.url().includes('v=mediapre002')) aborted.push(r.failure()?.errorText || ''); };
    page.on('requestfailed', onFail);
    try {
      await page.goto('https://www.youtube.com/results?search_query=media+prefetch+leave', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      await page.evaluate(() => { const a = document.querySelector('#ytc-list a.t[href^="/watch"]'); a.setAttribute('href', '/watch?v=mediapre002'); });
      await page.hover('#ytc-list a.t[href="/watch?v=mediapre002"]');
      for (let i = 0; i < 40 && log.gv.filter(g => g.v === 'mediapre002').length < 2; i++) await page.waitForTimeout(50);
      assert.equal(log.gv.filter(g => g.v === 'mediapre002').length, 2, 'prefetch started');
      await page.hover('#ytc-top');
      for (let i = 0; i < 40 && aborted.length < 2; i++) await page.waitForTimeout(50);
      assert.equal(aborted.length, 2, 'both requests cancelled: ' + aborted);
      await page.waitForTimeout(800);
      assert.equal(log.gv.filter(g => g.v === 'mediapre002').length, 2, 'nothing asked again');
    } finally { page.off('requestfailed', onFail); STALL.parked.length = 0; STALL.hold.delete('mediapre002'); }
  });

  await step('own player: a dropped connection → "No connection — continuing when it is back", then it plays by itself', async () => {
    NET.down = true; NET.probes = 0;
    try {
      await page.goto('https://www.youtube.com/watch?v=netdown0001', { waitUntil: 'commit' });
      await page.waitForFunction(() => /No connection/.test(document.querySelector('#ytc-player .pfm')?.textContent || ''), null, { timeout: 30000 });
      assert.ok(!page.url().includes('yt-native'), 'stays on our page');
      assert.ok(NET.probes >= 1, 'the network was probed');
      NET.down = false;
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 15000);
      assert.equal(await page.locator('#ytc-player .pfail').count(), 0, 'the panel is gone: it continued by itself');
    } finally { NET.down = false; }
  });

  await step('own player: a momentary 503 on the starting format is asked again (no fallback)', async () => {
    await page.goto('https://www.youtube.com/watch?v=flaky503vid', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
    assert.ok(!page.url().includes('yt-native'), 'no native fallback');
    assert.equal((await vstate()).h, 360, 'kept the intended format');
    assert.equal(log.gv.filter(g => g.v === 'flaky503vid' && g.itag === '243' && g.a === 0).length, 2, 'index asked twice');
  });

  await step('own player: a rung whose chunks keep failing steps down instead of falling back', async () => {
    await page.goto('https://www.youtube.com/watch?v=stepdown001', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
    await waitT(() => document.getElementById('ytc-video')?.videoHeight === 240, null, 15000);
    assert.ok(!page.url().includes('yt-native'), 'no native fallback');
  });

  await step('own live: plays a live stream (HLS timeline, fMP4 segments by sq) near the live edge', async () => {
    const before = incidents.length, gv0 = log.gv.length, lv0 = log.live.length;
    LIVE.streams.delete('livestream1');
    await page.goto('https://www.youtube.com/watch?v=livestream1', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 10000);
    await waitT(() => { const v = document.getElementById('ytc-video'); return v && !v.paused && v.readyState >= 3; }, null, 6000);
    const st = LIVE.streams.get('livestream1'), head = liveHead(st);
    const segs = log.gv.slice(gv0).filter(g => g.live);
    const first = segs[0].sq;
    // Start ≥ 2 target durations (4 s → 8 s) behind the end of the newest segment: 1003 of 1000…1005.
    assert.ok(first >= LIVE.base + 3 && first <= LIVE.base + 4, `started at sq ${first} (head ${head})`);
    assert.deepEqual([...new Set(segs.filter(g => g.sq === first).map(g => g.itag))].sort(), ['134', '140'], 'video + audio of the same sq');
    const lv = log.live.slice(lv0);
    assert.equal(lv.filter(x => x.p.includes('/hls_variant/')).length, 1, 'master fetched once');
    assert.ok(lv.some(x => x.p.includes('/itag/230/')), 'follows the 360p variant playlist');
    const s = await vstate();
    assert.ok(Math.abs(s.t - first * LIVE.fx.SEG) < LIVE.fx.SEG + 1, `media time ${s.t} ≈ sq ${first} × ${LIVE.fx.SEG}`);
    assert.equal(s.h, 360);
    const cls = await page.getAttribute('#ytc-player', 'class');
    assert.match(cls, /\blive\b/); assert.match(cls, /\bedge\b/);
    assert.ok(await page.locator('.plive').isVisible(), 'LIVE button');
    if (process.env.YTC_SHOTS) { await page.mouse.move(700, 400); await page.waitForTimeout(100); const b = await page.locator('#ytc-player').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'player-live.png'), clip: { x: b.x, y: b.y + b.height - 70, width: b.width, height: 70 } }); }
    assert.equal(await page.textContent('#ytc-time'), '');
    assert.ok(!page.url().includes('yt-native'));
    assert.deepEqual(problems(before), []);
  });

  await step('own live: the playlist refreshes every target duration and new segments play (latency stays small)', async () => {
    const lv0 = log.live.length, gv0 = log.gv.length, t0 = (await vstate()).t;
    await page.waitForTimeout(7000);
    const polls = log.live.slice(lv0).filter(x => x.p.includes('/hls_playlist/'));
    assert.ok(polls.length >= 1 && polls.length <= 4, `${polls.length} playlist refreshes in 7 s (target 4 s)`);
    const st = LIVE.streams.get('livestream1'), head = liveHead(st);
    const fetched = log.gv.slice(gv0).filter(g => g.live && g.itag === '134').map(g => g.sq);
    assert.ok(fetched.length >= 1 && Math.max(...fetched) >= head - 1, `new segments fetched: ${fetched} (head ${head})`);
    const s = await vstate();
    assert.ok(s.t - t0 > 5, `played on: ${t0} → ${s.t}`);
    const edgeT = (head + 1) * LIVE.fx.SEG;
    assert.ok(edgeT - s.t < 15, `${(edgeT - s.t).toFixed(1)} s behind the newest segment`);
    // Headless Chromium decodes in software: under machine load it drops a frame or two.
    assert.ok(s.dropped <= Math.max(2, s.frames * 0.02), `dropped ${s.dropped} of ${s.frames} frames`);
    await page.keyboard.press('Backquote');
    const dbg = await page.locator('#ytc-debug').textContent();
    assert.match(dbg, /live · window sq \d+–\d+ .* at the edge · latency \d+\.\d s/);
    assert.match(dbg, /live latency \d+\.\d s behind the broadcast/);
    await page.keyboard.press('Backquote');
  });

  await step('own live: DVR seek back leaves the edge (−m:ss), LIVE returns to it', async () => {
    const before = (await vstate()).t;
    await page.keyboard.press('j');
    await waitT(() => !/\bedge\b/.test(document.getElementById('ytc-player').className), null, 3000);
    await waitT(() => /^−0:\d\d$/.test(document.getElementById('ytc-time').textContent), null, 3000);
    await waitT(b => { const v = document.getElementById('ytc-video'); return v.currentTime < b - 5 && v.readyState >= 3 && !v.paused; }, before, 8000);
    await page.locator('.plive').click();
    await waitT(() => /\bedge\b/.test(document.getElementById('ytc-player').className), null, 3000);
    await waitT(b => document.getElementById('ytc-video').currentTime > b, before, 5000);
  });

  await step('own live: a stream that ends plays to its last segment, then ended (Out::Ended)', async () => {
    const before = incidents.length;
    LIVE.streams.delete('liveended01');
    await page.goto('https://www.youtube.com/watch?v=liveended01', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 20000);
    await page.evaluate(() => { document.getElementById('ytc-video').playbackRate = 2; });
    await waitT(() => document.getElementById('ytc-video').ended && document.documentElement.dataset.ytcEnded, null, 25000);
    const s = await vstate();
    assert.ok(Math.abs(s.t - (LIVE.base + 7) * LIVE.fx.SEG) < 0.5, `ended at ${s.t}`);
    assert.deepEqual(problems(before), []);
    assert.ok(!page.url().includes('yt-native'));
  });

  await step('own live: upcoming streams/premieres show YouTube\'s words in the panel (the page stays), without an incident', async () => {
    const before = incidents.length;
    await page.goto('https://www.youtube.com/watch?v=liveupcomin', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 8000 });
    await page.waitForTimeout(500);
    assert.deepEqual(await panel(), { url: '?v=liveupcomin', m: 'Premieres in 2 hours', r: 'upcoming live stream or premiere: Premieres in 2 hours' });
    assert.deepEqual(problems(before), []);
  });

  await step('own player: a better quality chosen while paused redraws the paused frame (no resume needed)', async () => {
    await page.goto('https://www.youtube.com/watch?v=aqz-KE-bpKQ', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
    await page.click('#ytc-qual');
    await page.locator('#ytc-menu button[data-arg="q:144"]').click();
    await waitT(() => document.getElementById('ytc-video').videoHeight === 144, null, 9000);
    await page.evaluate(() => document.getElementById('ytc-video').pause());
    await page.waitForTimeout(300);
    const t0 = await page.evaluate(() => document.getElementById('ytc-video').currentTime);
    await page.click('#ytc-qual');
    await page.locator('#ytc-menu button[data-arg="q:0"]').click();
    await waitT(() => document.getElementById('ytc-video').videoHeight === 360, null, 9000);
    const st = await vstate();
    assert.ok(st.paused, 'still paused');
    assert.ok(Math.abs(st.t - t0) < 0.3, `same position (${t0} → ${st.t})`);
  });

  await step('own player: an unreachable starting format is replaced before playback', async () => {
    await page.goto('https://www.youtube.com/watch?v=deadstart01', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
    await page.waitForTimeout(800);
    assert.ok(!page.url().includes('yt-native'), 'no native fallback');
    const st = await vstate();
    assert.ok(!st.paused && st.h === 240, JSON.stringify(st));
  });

  await step('own player: an unreachable quality switch keeps playing the current one', async () => {
    // Remember 144p first; switching up to 360p then meets a format googlevideo
    // will not serve, so playback must stay on 144p.
    await page.goto('https://www.youtube.com/watch?v=aqz-KE-bpKQ', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await page.click('#ytc-qual');
    await page.locator('#ytc-menu button[data-arg="q:144"]').click();
    await page.waitForTimeout(300);
    await page.goto('https://www.youtube.com/watch?v=deadupgrade', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await page.click('#ytc-qual');
    await page.waitForSelector('#ytc-player.menu #ytc-menu button');
    await page.locator('#ytc-menu button[data-arg="q:360"]').click();
    for (let i = 0; i < 40 && !log.gv.some(g => g.v === 'deadupgrade' && g.itag === '243'); i++) await page.waitForTimeout(100);
    await page.waitForTimeout(1500);
    assert.ok(log.gv.some(g => g.v === 'deadupgrade' && g.itag === '243'), 'the switch was attempted');
    assert.ok(!page.url().includes('yt-native'), 'no native fallback');
    const st = await vstate();
    assert.ok(!st.paused && st.h === 144, JSON.stringify(st));
    await page.click('#ytc-qual');
    await page.locator('#ytc-menu button[data-arg="q:0"]').click();
  });

  // Media is appended while it downloads (player/frag.rs, own/stream.rs): the test switch hands every
  // googlevideo body to the player in 997-byte pieces, so EBML elements, blocks and the index split anywhere.
  await step('own player: media appended while it downloads, in 997-byte pieces (elements split anywhere): first frame, playback, a seek outside the buffer and the end are right', async () => {
    await page.evaluate(() => localStorage.setItem('ytc-test-chunk', '997'));
    const err0 = errors.length, inc0 = incidents.length;
    try {
      await page.goto('https://www.youtube.com/watch?v=piecemeal01&t=27', { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
      await waitT(() => { const v = document.getElementById('ytc-video'); return v.currentTime > 28.5 && v.readyState >= 3; }, null, 8000);
      assert.equal((await vstate()).h, 360);
      // Outside the buffer: back to 6 s (segment 5–10), then on to the end of the 40 s video.
      const box = await page.locator('.pseek').boundingBox();
      await page.mouse.click(box.x + box.width * 0.15, box.y + box.height / 2);
      await waitT(() => { const v = document.getElementById('ytc-video'); return v.currentTime > 6.3 && v.currentTime < 12 && v.readyState >= 3 && !v.paused; }, null, 8000);
      const ranges = await page.evaluate(() => { const v = document.getElementById('ytc-video'), r = v.buffered, o = []; for (let i = 0; i < r.length; i++) o.push([r.start(i), r.end(i)]); return o; });
      assert.ok(ranges.some(([a, b]) => a <= 6.5 && b >= 9.5), 'the seek target segment is buffered whole: ' + JSON.stringify(ranges));
      await page.evaluate(() => { const v = document.getElementById('ytc-video'); v.currentTime = 37; });
      await waitT(() => document.getElementById('ytc-video').ended, null, 9000);
      assert.deepEqual(problems(inc0), [], 'no incident');
      assert.deepEqual(errors.slice(err0), [], 'no page error');
    } finally { await page.evaluate(() => localStorage.removeItem('ytc-test-chunk')); }
  });

  // The instant switch (own/stream.rs): a chosen quality shows at once (the new segment under the playhead,
  // then a seek to where the playhead is), not at the next segment boundary (5 s segments here).
  await step('own player: a chosen quality shows within a fraction of a second, not at the next segment; audio untouched, playback goes on', async () => {
    await page.goto('https://www.youtube.com/watch?v=instantq001', { waitUntil: 'commit' });
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
    await page.evaluate(() => { const v = document.getElementById('ytc-video'); window.__w = []; for (const e of ['waiting', 'playing']) v.addEventListener(e, () => window.__w.push([e, performance.now()])); });
    for (const [q, h] of [['144', 144], ['0', 360]]) {
      // Well inside a segment: the next boundary is ≥ 3 s away.
      await waitT(() => { const t = document.getElementById('ytc-video').currentTime; return t % 5 > 0.6 && t % 5 < 1.8; }, null, 8000);
      const audio0 = log.gv.filter(g => g.v === 'instantq001' && g.itag === '249' && g.a > 0).map(g => g.a);
      await page.click('#ytc-qual');
      await page.waitForSelector('#ytc-player.menu #ytc-menu button');
      const [t0, ct0] = await page.evaluate(() => [performance.now(), document.getElementById('ytc-video').currentTime]);
      await page.locator(`#ytc-menu button[data-arg="q:${q}"]`).click();
      await waitT(h => document.getElementById('ytc-video').videoHeight === h, h, 6000);
      const [t1, ct1] = await page.evaluate(() => [performance.now(), document.getElementById('ytc-video').currentTime]);
      console.log(`     quality ${q === '0' ? 'Auto (360p)' : q + 'p'} chosen at ${ct0.toFixed(2)} s → on screen in ${(t1 - t0).toFixed(0)} ms (next segment at ${(Math.ceil(ct0 / 5) * 5).toFixed(0)} s)`);
      assert.ok(t1 - t0 < 1200 && ct1 < Math.ceil(ct0 / 5) * 5, `shown before the next segment: ${(t1 - t0).toFixed(0)} ms, at ${ct1.toFixed(2)} s`);
      await page.waitForTimeout(1200);
      const st = await vstate();
      assert.ok(!st.paused && st.t > ct1 + 0.8 && st.rs >= 3, 'playback goes on ' + JSON.stringify(st));
      const audio1 = log.gv.filter(g => g.v === 'instantq001' && g.itag === '249' && g.a > 0).map(g => g.a);
      assert.ok(audio1.slice(audio0.length).every(a => !audio0.includes(a)), 'no audio fetched again: ' + JSON.stringify([audio0, audio1]));
      await page.mouse.move(5, 5);
    }
    const w = await page.evaluate(() => window.__w);
    let frozen = 0; for (let i = 0; i < w.length; i++) if (w[i][0] === 'waiting') { const p = w.slice(i).find(x => x[0] === 'playing'); if (p) frozen += p[1] - w[i][1]; }
    console.log(`     time waiting during the two switches: ${frozen.toFixed(0)} ms`);
    assert.ok(frozen < 600, 'at most a brief freeze: ' + frozen.toFixed(0) + ' ms');
  });

  // "Data but no picture" (own/stream.rs): the test switch makes the first video SourceBuffer take data but
  // never show anything; at 1.5 s the format is given up and the start continues on the next one.
  await step('own player: media buffered but no picture → another format within ~1.5 s (no 4 s stuck status, no 6 s retry)', async () => {
    const inc0 = incidents.length;
    await page.evaluate(() => localStorage.setItem('ytc-test-nopicture', '1'));
    // 243's later chunks come late, so its end of stream does not come before the 1.5 s.
    SLOWSEG.set('nopicture01', { ms: 4000, itag: '243' });
    try {
      const t0 = Date.now();
      await page.goto('https://www.youtube.com/watch?v=nopicture01', { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
      const ms = Date.now() - t0;
      console.log(`     no picture → another format → first frame after ${ms} ms`);
      assert.ok(ms < 3800, 'before the 4 s stuck status: ' + ms);
      await page.waitForTimeout(500);
      assert.equal(await page.locator('#ytc-player.stuck').count(), 0, 'no stuck status');
      assert.equal((await vstate()).h, 240, 'the next format plays');
      assert.deepEqual(problems(inc0), [], 'no player-stuck incident');
      await page.keyboard.press('Backquote');
      const dbg = await page.locator('#ytc-debug').textContent();
      await page.keyboard.press('Backquote');
      assert.match(dbg, /itag 243: media covers .* but no picture: 242/);
    } finally { SLOWSEG.delete('nopicture01'); await page.evaluate(() => localStorage.removeItem('ytc-test-nopicture')); }
  });

  await step('own player: a seek drops the requests in flight at once; resting on the seek bar fetches that spot ahead and the click takes it over', async () => {
    const v = 'seekcancel1', aborted = [];
    const onFail = r => { const u = new URL(r.url()); if (u.hostname.endsWith('googlevideo.com') && u.searchParams.get('v') === v) aborted.push({ itag: u.searchParams.get('itag'), range: u.searchParams.get('range'), t: Date.now() }); };
    page.on('requestfailed', onFail);
    // Every segment request takes 1 s (below the 1.5 s that counts as throttled while playing).
    SLOWSEG.set(v, { ms: 1000 });
    try {
      await page.goto(`https://www.youtube.com/watch?v=${v}`, { waitUntil: 'commit' });
      await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 9000);
      // The rest of the first segment is still on its way: seek to 31 s.
      const box = await page.locator('.pseek').boundingBox();
      const n0 = log.gv.length, ts = Date.now();
      await page.mouse.click(box.x + box.width * (31 / 40), box.y + box.height / 2);
      for (let i = 0; i < 30 && !aborted.some(a => a.t >= ts); i++) await page.waitForTimeout(50);
      assert.ok(aborted.some(a => a.t >= ts), 'the old request was dropped: ' + JSON.stringify(aborted));
      const next = log.gv.slice(n0).find(g => g.v === v && g.itag === '243' && g.a > 0);
      assert.ok(next && next.t - ts < 400, 'the target segment asked at once: ' + JSON.stringify(next));
      await waitT(() => { const x = document.getElementById('ytc-video'); return x.currentTime > 31 && x.readyState >= 3; }, null, 8000);
      // Rest on 13 s (not buffered) for 0.5 s: its segments are fetched ahead; the click there takes them over.
      await page.mouse.move(box.x + box.width * (13 / 40), box.y + box.height / 2, { steps: 3 });
      const n1 = log.gv.length;
      await page.waitForTimeout(500);
      const pre = log.gv.slice(n1).filter(g => g.v === v && g.a > 0);
      assert.ok(pre.some(g => g.itag === '243') && pre.some(g => g.itag === '249'), 'video and audio fetched ahead: ' + JSON.stringify(pre));
      const n2 = log.gv.length, tc = Date.now();
      await page.mouse.down(); await page.mouse.up();
      await waitT(() => { const x = document.getElementById('ytc-video'); return x.currentTime > 13 && x.currentTime < 20 && x.readyState >= 3; }, null, 8000);
      const ms = Date.now() - tc;
      const again = log.gv.slice(n2).filter(g => g.v === v && g.a > 0 && pre.some(p => p.itag === g.itag && p.a === g.a));
      console.log(`     seek to a spot fetched ahead (still loading, 1 s per request): playing after ${ms} ms`);
      assert.deepEqual(again, [], 'nothing asked twice');
      assert.ok(ms < 900, 'sooner than a fresh 1 s request: ' + ms);
      await page.keyboard.press('Backquote');
      const dbg = await page.locator('#ytc-debug').textContent();
      await page.keyboard.press('Backquote');
      assert.match(dbg, /video segment 2 fetched ahead/);
    } finally { SLOWSEG.delete(v); page.off('requestfailed', onFail); await page.mouse.move(5, 5); }
  });

  await step('layout: six-link sidebar (no Shorts) while browsing, none on watch pages; results render without layout shift', async () => {
    await page.goto('https://www.youtube.com/results?search_query=layout', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    await page.waitForTimeout(150);
    const cls = await page.evaluate(() => new Promise(r => { let s = 0; new PerformanceObserver(l => { for (const e of l.getEntries()) if (!e.hadRecentInput) s += e.value; }).observe({ type: 'layout-shift', buffered: true }); setTimeout(() => r(s), 100); }));
    console.log(`     cumulative layout shift (search, cold): ${cls.toFixed(4)}`);
    assert.ok(cls < 0.05, 'CLS ' + cls);
    assert.deepEqual(await page.locator('#ytc-side a').allTextContents(), ['Home', 'History', 'Watch later', 'Subscriptions', 'Playlists', 'Liked videos']);
    assert.ok(await page.locator('#ytc-side').isVisible());
    const search = await page.locator('#ytc-search').boundingBox();
    assert.ok(search.width <= 721 && Math.abs(search.x + search.width / 2 - 700) < 2, 'search centered, ≤720px: ' + JSON.stringify(search));
    const first = await page.locator('#ytc-list .c').first().boundingBox();
    assert.ok(first.x >= 224, 'content clears the sidebar');
    await page.click('#ytc-list a.t[href^="/watch"] >> nth=0');
    await page.waitForSelector('#ytc-info h1.wt');
    assert.equal(await page.getAttribute('#ytc', 'data-nav'), 'watch');
    assert.ok(!(await page.locator('#ytc-side').isVisible()), 'no sidebar on watch');
    await morphDone();
    assert.ok((await page.locator('#ytc-player').boundingBox()).x < 40, 'player uses the released width');
    for (const id of ['ytc-sub', 'ytc-acts', 'ytc-comments']) assert.equal(await page.locator('#' + id).count(), 1, id);
    await morphDone();
    const pl = await page.locator('#ytc-player').boundingBox();
    assert.ok(pl.height >= 0.55 * 900 && Math.abs(pl.width / pl.height - 16 / 9) < 0.02, 'the player dominates: ' + JSON.stringify(pl));
    // The description is fully open at its natural height: no clamp and no Show more.
    assert.deepEqual(await page.evaluate(() => { const d = document.getElementById('ytc-desc'), cs = getComputedStyle(d); return [d.classList.contains('clamp'), !!d.querySelector('button.lnk'), cs.maxHeight, cs.overflow, d.scrollHeight <= d.clientHeight + 1]; }), [false, false, 'none', 'visible', true], 'description fully open');
  });

  await step('keyboard-first: arrows walk the cards (rows by layout), Enter opens, g+letter jumps, ? shows the sheet', async () => {
    await page.goto('https://www.youtube.com/results?search_query=keys', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    const idx = () => page.evaluate(() => [...document.querySelectorAll('#ytc-list a.t')].indexOf(document.activeElement));
    await page.keyboard.press('ArrowRight');
    assert.equal(await idx(), 0, 'first card in view');
    await page.keyboard.press('ArrowRight');
    assert.equal(await idx(), 1);
    const cols = await page.evaluate(() => getComputedStyle(document.getElementById('ytc-list')).gridTemplateColumns.split(' ').length);
    await page.keyboard.press('ArrowDown');
    assert.equal(await idx(), 1 + cols, `one row down (${cols} columns)`);
    await page.keyboard.press('ArrowUp');
    assert.equal(await idx(), 1);
    const href = await page.evaluate(() => document.activeElement.getAttribute('href'));
    const docs = log.docs.length;
    await page.keyboard.press('Enter');
    await page.waitForFunction(h => location.pathname + location.search === h, href);
    assert.equal(log.docs.length, docs, 'SPA');
    await page.waitForSelector('#ytc-info h1.wt');
    await page.keyboard.press('Shift+?');
    await page.waitForSelector('#ytc-keys.on');
    assert.ok((await page.locator('#ytc-keys').textContent()).includes('Watch later'));
    assert.ok(!(await page.locator('#ytc-keys').textContent()).includes('0–9'), 'no 0–9 row (blocked keys)');
    await page.keyboard.press('Escape');
    await page.waitForSelector('#ytc-keys:not(.on)', { state: 'attached' });
    await page.keyboard.press('g');
    await page.keyboard.press('h');
    await page.waitForFunction(() => location.pathname === '/' && document.getElementById('ytc').dataset.nav === 'home');
  });

  // ---- motion (the motion law): the watch-open morph and its exceptions
  // Animations on the watch page's player/info/related/comments (WAAPI and CSS), as [target, kind, first keyframe].
  // (Comments appended after the first frame settle in on any navigation: `withComments` only for restores.)
  const mainAnims = (withComments = false) => page.evaluate(wc => document.getAnimations().filter(a => {
    const t = a.effect && a.effect.target; return t && (t.closest('#ytc-player') === t || ['ytc-info', 'ytc-desc', 'ytc-list', 'ytc-pl'].includes(t.id) || (wc && t.classList.contains('ycm')) || t.classList.contains('upc'));
  }).map(a => [a.effect.target.id || a.effect.target.className, a.id || a.constructor.name, JSON.stringify(a.effect.getKeyframes()[0] || {})]), withComments);
  // The thumbnail of card `i` on a results page, and a pointer click on it (real mouse events).
  const thumbAt = i => page.evaluate(i => { const r = document.querySelectorAll('#ytc-list .c[data-v] a.th')[i].getBoundingClientRect(); return { x: r.left, y: r.top, w: r.width, h: r.height }; }, i);
  await step('motion: a pointer click morphs the player from the clicked thumbnail (spring, transform only); final and interactive on frame one', async () => {
    await page.goto('https://www.youtube.com/results?search_query=morph', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c[data-v]');
    await page.mouse.move(2, 2);
    const th = await thumbAt(1);
    await page.mouse.click(th.x + th.w / 2, th.y + th.h / 2);
    // Same task as the swap: the page is complete when the animation exists.
    const m = await page.evaluate(async () => {
      const p = document.getElementById('ytc-player'), a = p.getAnimations().find(x => x.id === 'flip');
      if (!a) return null;
      const k = a.effect.getKeyframes(), t = a.effect.getTiming(), now = p.getBoundingClientRect();
      const hit = document.elementFromPoint(now.left + now.width / 2, now.top + now.height / 2);
      const out = { from: k[0].transform, to: k[k.length - 1].transform, origin: k[0].transformOrigin, easing: t.easing, ms: t.duration, props: Object.keys(k[0]).filter(x => !['offset', 'easing', 'composite', 'computedOffset'].includes(x)),
        state: a.playState, title: !!document.querySelector('#ytc-info h1.wt'), hitPlayer: !!(hit && hit.closest('#ytc-player')), flipClass: p.dataset.flip === '1', z: getComputedStyle(p).zIndex };
      a.finish();
      const r = p.getBoundingClientRect(); out.final = { x: r.left, y: r.top, w: r.width, h: r.height };
      await a.finished; await new Promise(res => requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(res))));
      out.flipAfter = 'flip' in p.dataset;
      return out;
    });
    assert.ok(m, 'a flip animation on #ytc-player');
    assert.equal(m.state, 'running');
    assert.deepEqual(m.props.sort(), ['transform', 'transformOrigin'], 'compositor-only: ' + m.props);
    assert.equal(m.to, 'none');
    assert.ok(/^linear\(0[ ,]/.test(m.easing) && m.easing.split(',').length === 25 && m.ms === 458, 'soft spring from motion.rs: ' + m.easing.slice(0, 30) + ' ' + m.ms);
    const [, tx, ty, sx, sy] = /translate\(([-\d.e]+)px, ?([-\d.e]+)px\) scale\(([\d.e-]+), ?([\d.e-]+)\)/.exec(m.from).map(Number);
    const start = { x: m.final.x + tx, y: m.final.y + ty, w: m.final.w * sx, h: m.final.h * sy };
    for (const k of ['x', 'y', 'w', 'h']) assert.ok(Math.abs(start[k] - th[k]) < 1, `morph starts on the thumbnail (${k}): ${JSON.stringify(start)} vs ${JSON.stringify(th)}`);
    assert.ok(m.title && m.hitPlayer, 'page complete and the player takes clicks where it is painted on frame one');
    assert.ok(m.flipClass && m.z === '3' && !m.flipAfter, 'above its neighbours while moving, class removed at the end: ' + JSON.stringify([m.flipClass, m.z, m.flipAfter]));
    assert.ok(m.final.x < 40 && m.final.w > 900, 'final player box: ' + JSON.stringify(m.final));
    // The title skeleton has the final geometry; the description skeleton has the views/date-only panel's, so
    // when next lands nothing moves up and resource lines only extend the panel downwards.
    await page.waitForSelector('#ytc-desc:not(.sk)');
    const g = await page.evaluate(() => {
      const info = document.getElementById('ytc-info'), el = (tag, cls, kid) => { const e = document.createElement(tag); e.className = cls; if (kid) e.append(kid); return e; };
      const add = e => { info.append(e); const r = e.offsetHeight; e.remove(); return r; };
      return { title: document.querySelector('#ytc-info h1.wt').offsetHeight, titleSk: add(el('h1', 'wt', el('i', 'sk l w60'))), desc: document.getElementById('ytc-desc').offsetHeight, descSk: add(el('div', 'desc sk')), metaOnly: add(el('section', 'desc', el('div', 'ws', el('span', '', '1 view')))) };
    });
    assert.ok(Math.abs(g.title - g.titleSk) < 0.5 && Math.abs(g.metaOnly - g.descSk) < 0.5 && g.desc >= g.descSk, 'skeleton geometry: ' + JSON.stringify(g));
    // A click on the player right away reaches it (no input lock during motion).
    await page.mouse.click(m.final.x + m.final.w / 2, m.final.y + m.final.h / 3);
    assert.equal(errors.length, 0, errors.join('\n'));
    // Theater: a pointer click morphs the player into its new box (--spring, 351 ms); the t key is instant.
    await page.mouse.move(m.final.x + m.final.w / 2, m.final.y + m.final.h / 2);
    await page.locator('#ytc-player button[data-act="p-theater"]').click();
    const th1 = await page.evaluate(() => { const a = document.getElementById('ytc-player').getAnimations().find(x => x.id === 'flip'); const r = a && a.effect.getTiming().duration; if (a) a.finish(); return [document.getElementById('ytc-watch').className, r]; });
    assert.deepEqual(th1, ['watch theater', 351], 'theater by pointer morphs');
    await page.mouse.move(2, 2);
    await page.keyboard.press('t');
    const th2 = await page.evaluate(() => [document.getElementById('ytc-watch').className, document.getElementById('ytc-player').getAnimations().some(x => x.id === 'flip')]);
    assert.deepEqual(th2, ['watch', false], 't is instant');
    if (process.env.YTC_SHOTS) {
      // Frames of the morph (paused at fixed times) for review.
      await page.goto('https://www.youtube.com/results?search_query=morphshots', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.waitForTimeout(800);
      await page.mouse.move(2, 2);
      const t2 = await thumbAt(1);
      await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'morph-0-before.png') });
      await page.mouse.click(t2.x + t2.w / 2, t2.y + t2.h / 2);
      for (const ms of [0, 40, 100, 200, 458]) {
        await page.evaluate(ms => { for (const a of document.getAnimations()) { a.pause(); a.currentTime = Math.min(ms, a.effect.getTiming().duration); } }, ms);
        await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `morph-${ms}.png`) });
      }
      await page.evaluate(() => { for (const a of document.getAnimations()) a.finish(); });
    }
  });

  await step('motion: keyboard Enter, back/forward and restored pages are instant; reduced motion fades the player instead', async () => {
    // Keyboard: Enter on a focused card opens the watch page without any animation.
    await page.goto('https://www.youtube.com/results?search_query=morphkeys', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c[data-v]');
    await page.mouse.move(2, 2);
    await page.evaluate(() => document.querySelectorAll('#ytc-list a.t[href^="/watch"]')[2].focus());
    await page.waitForTimeout(400); // focus is intent: its next prefetch lands, so Enter paints the whole page at once
    await page.evaluate(() => { window.__still = []; new MutationObserver(() => window.__still.push(document.getElementById('ytc').dataset.still)).observe(document.getElementById('ytc'), { attributes: true, attributeFilter: ['data-still'] }); });
    await page.keyboard.press('Enter');
    await page.waitForSelector('#ytc-info h1.wt');
    assert.deepEqual(await mainAnims(), [], 'no motion after keyboard Enter');
    await page.waitForFunction(() => window.__still.length >= 2);
    assert.deepEqual(await page.evaluate(() => window.__still), ['1', '0'], 'CSS entrances off for the first frame only');
    // Comments appended later (pointer-independent) do settle in.
    await page.locator('#ytc-cm').scrollIntoViewIfNeeded();
    await page.waitForSelector('#ytc-cm-list > .ycm');
    // Back to results, forward to the watch page (restored from memory): instant.
    await page.evaluate(() => history.back());
    await page.waitForFunction(() => location.pathname === '/results' && document.querySelector('#ytc-list .c[data-v]'));
    assert.deepEqual(await mainAnims(true), [], 'no motion on back');
    await page.evaluate(() => history.forward());
    await page.waitForFunction(() => location.pathname === '/watch' && document.querySelector('#ytc-cm-list > .ycm'));
    assert.deepEqual(await mainAnims(true), [], 'no motion on forward (restored page with its comments)');
    // Reduced motion: the pointer click fades the player in, no transform.
    await page.emulateMedia({ reducedMotion: 'reduce' });
    try {
      await page.goto('https://www.youtube.com/results?search_query=morphrm', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      const th = await thumbAt(0);
      await page.mouse.click(th.x + th.w / 2, th.y + th.h / 2);
      const k = await page.evaluate(() => document.getElementById('ytc-player').getAnimations().map(a => a.effect.getKeyframes().map(f => Object.keys(f).filter(x => !['offset', 'easing', 'composite', 'computedOffset'].includes(x)).join() + ':' + (f.opacity ?? '')).join(' ')));
      assert.deepEqual(k, ['opacity:0 opacity:1'], 'reduced motion: one opacity fade, no transform');
    } finally { await page.emulateMedia({ reducedMotion: null }); }
  });

  await step('motion: the morph costs the click no frame (pointer click vs keyboard-style click, same cached page, median of 11)', async () => {
    // click event → the next frame begins (rAF) → that frame has painted (rAF + task), median of 7 each.
    const once = async kind => {
      await page.goto('https://www.youtube.com/results?search_query=morphtime', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.mouse.move(2, 2);
      await page.waitForTimeout(150);
      // (The bubble listener on window runs after the client's document listener: the click task's own work.)
      await page.evaluate(() => { window.__t = null; let t0 = 0, task = 0; addEventListener('click', () => { task = performance.now() - t0; }, { once: true }); addEventListener('click', () => { t0 = performance.now(); requestAnimationFrame(() => { const f = performance.now(); setTimeout(() => { window.__t = { task, frame: f - t0, painted: performance.now() - t0 }; }); }); }, { capture: true, once: true }); });
      if (kind === 'pointer') { const th = await thumbAt(1); await page.mouse.click(th.x + th.w / 2, th.y + th.h / 2); }
      else await page.evaluate(() => document.querySelectorAll('#ytc-list .c[data-v] a.th')[1].dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, detail: 0 })));
      await page.waitForFunction(() => window.__t);
      const r = await page.evaluate(() => ({ ...window.__t, morph: document.getElementById('ytc-player').getAnimations().some(a => a.id === 'flip') }));
      if (!warm) assert.equal(r.morph, kind === 'pointer', kind + ' ' + JSON.stringify(r));
      return r;
    };
    let warm = true;
    await once('pointer'); // warm: next + VISIONOS cached for this card
    warm = false;
    const rows = { pointer: [], key: [] };
    for (let i = 0; i < 11; i++) { rows.pointer.push(await once('pointer')); rows.key.push(await once('key')); }
    const md = (k, f) => median(rows[k].map(r => r[f])).toFixed(1);
    console.log(`     median of 11: click task (swap + morph setup) ${md('pointer', 'task')} ms vs ${md('key', 'task')} ms without; click → next frame starts ${md('pointer', 'frame')} vs ${md('key', 'frame')} ms; → that frame painted ${md('pointer', 'painted')} vs ${md('key', 'painted')} ms`);
  });

  await step('search: YouTube\'s recommendation shelves (People also watched, For you, People also search for) are dropped; a channel\'s linked shelf and playlist results stay; same in continuations', async () => {
    const mark = log.api.length;
    await page.goto('https://www.youtube.com/results?search_query=shelves', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    const has = id => page.locator(`#ytc-list .c[data-v="${id}"]`).count();
    for (const id of [...SHELF_IDS.rec.slice(0, 2), 'AH30avEEC9A', '1AGWEnrPeQY']) assert.equal(await has(id), 0, `recommended ${id} not shown`);
    for (const id of SHELF_IDS.keep) assert.equal(await has(id), 1, `channel shelf video ${id} kept`);
    const titles = await page.locator('#ytc-list .c .t').allTextContents();
    assert.ok(titles.includes('Roblox Animation Tutorials'), 'playlist result kept');
    assert.ok(!(await page.locator('#ytc-list').textContent()).includes('People also search for'), 'no refinement row');
    // Order: the kept shelf sits where YouTube put it, between real results.
    const ids = await page.$$eval('#ytc-list .c[data-v]', cs => cs.map(c => c.dataset.v));
    assert.ok(ids.indexOf(SHELF_IDS.keep[0]) >= 0 && ids.indexOf(SHELF_IDS.keep[1]) === ids.indexOf(SHELF_IDS.keep[0]) + 1 && ids.indexOf('TlgW8jhLsc0') === ids.indexOf(SHELF_IDS.keep[1]) + 1, ids.join(','));
    await page.evaluate(() => scrollTo(0, document.body.scrollHeight));
    await page.waitForFunction(() => document.querySelectorAll('#ytc-list .c').length > 20, null, { timeout: 5000 });
    assert.ok(log.api.slice(mark).some(a => a.ep === 'search' && a.body.continuation === SHELF_CONT), 'continuation asked');
    assert.equal(await has(SHELF_IDS.rec[2]), 0, 'People also watched in the continuation dropped');
    // The debug overlay counts them.
    await page.keyboard.press('Backquote');
    await page.waitForSelector('#ytc-debug.on');
    const dbg = await page.locator('#ytc-debug').textContent();
    assert.ok(/recommendation shelves [4-9]/.test(dbg), dbg.slice(0, 400));
    await page.keyboard.press('Backquote');
    await shot('search-no-rec-shelves');
    await page.evaluate(() => scrollTo(0, 0));
  });

  await step('debug overlay toggles with ` and shows parse counts', async () => {
    await page.goto('https://www.youtube.com/results?search_query=roblox', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    await page.keyboard.press('Backquote');
    await page.waitForSelector('#ytc-debug.on');
    const txt = await page.locator('#ytc-debug').textContent();
    assert.match(txt, /shorts hidden [1-9]/);
    assert.match(txt, /client WEB 2\.\d{8}/);
    await page.keyboard.press('Backquote');
    await page.waitForSelector('#ytc-debug:not(.on)', { state: 'attached' });
  });

  await step('config: sw.js_data refresh stored in chrome.storage (small config only)', async () => {
    for (let i = 0; i < 40 && !log.api.some(a => a.ep === 'sw.js_data'); i++) await page.waitForTimeout(100);
    assert.ok(log.api.some(a => a.ep === 'sw.js_data'), 'sw.js_data fetched');
    await page.waitForTimeout(200);
    await page.goto('https://www.youtube.com/results?search_query=config', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    const s = log.api.filter(a => a.ep === 'search').pop();
    assert.equal(s.headers['x-youtube-client-version'], '2.20261029.11.00', 'version from sw.js_data');
    assert.ok(s.headers['x-goog-visitor-id']?.startsWith('Cgt'), 'visitor id header');
    assert.equal(s.body.context.client.visitorData, s.headers['x-goog-visitor-id']);
  });

  await step('native mode (?yt-native=1, the panel\'s Open on YouTube) loads YouTube with its bundles; no escape link in the header', async () => {
    assert.equal(await page.locator('#ytc-native').count(), 0, 'no "Open on YouTube" link');
    await page.goto('https://www.youtube.com/results?search_query=roblox&yt-native=1', { waitUntil: 'commit' });
    await page.waitForFunction(() => window.__nativeBundle === 1, null, { timeout: 5000 });
    assert.equal(await page.evaluate(() => window.__ytAppRan), true);
    assert.equal(await page.locator('#ytc-top').count(), 0);
    assert.ok(log.assets.some(u => u.includes('/s/desktop/abc123/jsbin/')), 'native bundle allowed');
    const r = await page.evaluate(async () => { try { await fetch('/youtubei/v1/log_event?alt=json'); return 'ok'; } catch { return 'blocked'; } });
    assert.equal(r, 'blocked', 'telemetry still blocked in native mode');
  });

  await step('excluded native paths are left alone (/account)', async () => {
    await page.goto('https://www.youtube.com/account', { waitUntil: 'commit' });
    await page.waitForFunction(() => window.__nativeBundle === 1, null, { timeout: 5000 });
    assert.equal(await page.locator('#ytc-top').count(), 0);
  });

  await step('pages the client does not render open YouTube\'s own page directly', async () => {
    await page.goto('https://www.youtube.com/hashtag/roblox', { waitUntil: 'commit' });
    await page.waitForURL(u => u.search.includes('yt-native=1'), { timeout: 5000 });
    assert.equal(new URL(page.url()).pathname, '/hashtag/roblox');
  });

  // ---- Native-page cleanup (2 Nov 2026): YouTube's own pages keep the client's setup. The page is a
  // fixture of YouTube's DOM assembled from the old extension's verified test fixtures.
  const NATIVE_FX = 'https://www.youtube.com/watch?v=ntvfixture1&yt-native=1';
  const cfgBefore = await sw.evaluate(() => chrome.storage.local.get(['ytcConfigV1', 'shortsInFeeds']));
  const setStore = o => sw.evaluate(o => chrome.storage.local.set(o), o);
  const getCfg = () => sw.evaluate(() => chrome.storage.local.get('ytcConfigV1').then(r => r.ytcConfigV1 || {}));
  const editCfg = p => sw.evaluate(p => chrome.storage.local.get('ytcConfigV1').then(r => { const c = r.ytcConfigV1 || {}; c.p = { ...(c.p || {}), ...p }; return chrome.storage.local.set({ ytcConfigV1: c }); }), p);
  const openNative = async () => {
    await page.goto(NATIVE_FX, { waitUntil: 'commit' });
    await page.waitForSelector('html[data-ytc-native] #movie_player', { timeout: 8000 });
    await page.waitForTimeout(150);
  };
  // Which of these ids are rendered (checkVisibility: display/visibility of the element and its ancestors).
  const shown = ids => page.evaluate(ids => Object.fromEntries(ids.map(id => { const el = document.getElementById(id); return [id, !!el && el.checkVisibility()]; })), ids);
  const expectShown = async (want, ids) => {
    const got = await shown(ids);
    const wrong = ids.filter(id => got[id] !== want);
    assert.deepEqual(wrong, [], (want ? 'hidden but should show: ' : 'shown but should be hidden: ') + wrong.join(', '));
  };
  await step('native page cleanup: header, sidebar, Shorts, Playables and promotions as in the old extension (YouTube DOM fixture)', async () => {
    await setStore({ shortsInFeeds: false });
    await openNative();
    assert.equal(await page.evaluate(() => window.__ytAppRan), true, 'YouTube\'s own page runs');
    await expectShown(false, ['voice-search-button', 'country-code', 'create']);
    await expectShown(true, ['logo', 'logo-icon', 'search', 'notifications', 'account']);
    assert.deepEqual(await page.evaluate(() => ['::before', '::after'].map(p => getComputedStyle(document.getElementById('logo'), p).content)), ['none', 'none'], 'YouTube\'s own header logo, unmodified');
    // Sidebar: the six browsing links; Liked videos out of the collapsed part.
    await expectShown(true, ['home', 'subscriptions-nav', 'history', 'playlists', 'watch-later', 'liked-videos', 'mini-home', 'mini-subscriptions']);
    await expectShown(false, ['shorts-nav', 'mini-shorts', 'your-channel', 'your-videos', 'downloads', 'podcasts', 'courses', 'expander-item', 'subscriptions', 'subscribed-channel', 'explore', 'more-youtube', 'report-history', 'footer']);
    // Shorts and Playables out of feeds; ordinary and channel cards stay.
    await expectShown(false, ['short-video', 'absolute-short', 'short-shelf', 'playable-card']);
    await expectShown(true, ['normal-video', 'channel-link']);
    // Promotions: banners hidden, the offer dialog closed by its own dismiss button (never the trial),
    // the account notice mealbar left alone.
    await expectShown(false, ['membership-section', 'premium-section']);
    await page.waitForFunction(() => window.__dismissed === 1, null, { timeout: 3000 });
    await page.waitForTimeout(1700);
    assert.deepEqual(await page.evaluate(() => [window.__dismissed, window.__trial, window.__notice]), [1, 0, 0]);
    await expectShown(true, ['notice', 'notice-action']);
    assert.equal(await page.evaluate(() => getComputedStyle(document.documentElement).backgroundColor), 'rgb(15, 15, 15)', 'client palette');
  });

  await step('native page cleanup: watch page (Join, Thanks, overflow, Auto-dubbed, Summary/Transcript/Video details, related chips) and player overlays', async () => {
    await expectShown(false, ['join-host', 'thanks-host', 'overflow', 'dubbed', 'super-title', 'summary', 'games', 'gaming', 'native-transcript', 'repeat-channel', 'video-details', 'related-filters']);
    await expectShown(true, ['like', 'dislike', 'share', 'save', 'subscribe', 'owner-renderer', 'full-transcript', 'attributed-description', 'quality-badge']);
    const player = await page.evaluate(() => {
      const hid = s => !document.querySelector(s).checkVisibility();
      return { overlays: ['.ytp-fullscreen-grid', '.ytp-more-videos-button', '.ytp-fullscreen-quick-actions', '.ytp-fullscreen-metadata', '.iv-branding', '.ytp-gradient-top', '.ytp-gradient-bottom', '#cinematics'].filter(s => !hid(s)),
        bg: getComputedStyle(document.querySelector('.html5-video-container')).backgroundImage + ' ' + getComputedStyle(document.getElementById('movie_player')).backgroundColor,
        progress: getComputedStyle(document.querySelector('.ytp-play-progress')).backgroundColor, controls: !hid('.ytp-play-button') && !hid('.ytp-subtitles-button') };
    });
    assert.deepEqual(player.overlays, [], 'fullscreen grid/quick actions/metadata, watermark, gradients and cinematics hidden');
    assert.match(player.bg, /none rgb\(0, 0, 0\)/, 'solid black around the picture');
    assert.equal(player.progress, 'rgb(255, 0, 51)', 'red progress');
    assert.ok(player.controls, 'playback controls stay');
  });

  await step('native page: captions in the client\'s style and place; drag, wheel and the size range write ytcConfigV1.p.cs', async () => {
    await editCfg({ cs: { v: 2, x: 0.1, y: 0.2, s: 1 }, cc: true });
    await openNative();
    const box = () => page.evaluate(() => {
      const f = document.querySelector('.ytp-caption-window-container').getBoundingClientRect(), c = document.getElementById('caption-window-1').getBoundingClientRect();
      const seg = getComputedStyle(document.querySelector('#caption-window-1 .ytp-caption-segment'));
      return { x: (c.left - f.left) / f.width, y: (c.top - f.top) / f.height, cx: c.left + Math.min(c.width / 2, 40), cy: c.top + c.height / 2, color: seg.color, bg: seg.backgroundColor, font: parseFloat(seg.fontSize), family: seg.fontFamily, notice: document.getElementById('caption-window-_notice').checkVisibility() };
    });
    let b = await box();
    assert.ok(Math.abs(b.x - 0.1) < 0.002 && Math.abs(b.y - 0.2) < 0.002, 'shared position: ' + JSON.stringify(b));
    assert.equal(b.color, 'rgb(0, 255, 255)');
    assert.equal(b.bg, 'rgba(0, 0, 0, 0)');
    assert.match(b.family, /^Arial/);
    // The client's size formula: clamp(16px, 5cqh, 72px) × .75 × s → 540 px player: 27 × .75 = 20.25 px.
    assert.ok(Math.abs(b.font - 20.25) < 0.5, 'size ' + b.font);
    assert.equal(b.notice, false, 'the "click gear for settings" notice is hidden');
    // Drag: YouTube never sees the press (no native drag, no pause), the new place is saved for the client.
    await page.mouse.move(b.cx, b.cy);
    await page.mouse.down();
    await page.mouse.move(b.cx + 48, b.cy + 27, { steps: 4 });
    await page.mouse.move(b.cx + 96, b.cy + 54, { steps: 4 });
    await page.mouse.up();
    b = await box();
    assert.ok(Math.abs(b.x - 0.2) < 0.003 && Math.abs(b.y - 0.3) < 0.003, 'moved: ' + JSON.stringify(b));
    assert.deepEqual(await page.evaluate(() => [window.__nativeCaptionDrag, window.__playerClicks]), [0, 0], 'no native drag or play toggle');
    for (let i = 0; i < 30 && Math.abs(((await getCfg()).p?.cs?.x ?? 0) - 0.2) > 0.003; i++) await page.waitForTimeout(100);
    let cs = (await getCfg()).p.cs;
    assert.ok(cs.v === 2 && Math.abs(cs.x - 0.2) < 0.003 && Math.abs(cs.y - 0.3) < 0.003 && cs.s === 1, JSON.stringify(cs));
    // Wheel over the cue: two size steps up, the page does not scroll; saved once the wheel rests.
    const y0 = await page.evaluate(() => scrollY);
    await page.mouse.move(b.cx, b.cy);
    await page.mouse.wheel(0, -100);
    await page.waitForTimeout(100);
    assert.equal(await page.evaluate(() => scrollY), y0, 'page did not scroll');
    for (let i = 0; i < 30 && (await getCfg()).p.cs.s !== 1.1; i++) await page.waitForTimeout(100);
    assert.equal((await getCfg()).p.cs.s, 1.1);
    // The size range beside the cue shows on hover, mirrors the size and sets it.
    await page.mouse.move(b.cx, b.cy);
    await page.waitForSelector('#movie_player > .ytc-ccsz input', { state: 'visible', timeout: 3000 });
    assert.equal(parseFloat(await page.locator('.ytc-ccsz input').inputValue()), 1.1);
    await page.evaluate(() => { const r = document.querySelector('.ytc-ccsz input'); r.value = '1.5'; r.dispatchEvent(new Event('input', { bubbles: true })); r.dispatchEvent(new Event('change', { bubbles: true })); });
    for (let i = 0; i < 30 && (await getCfg()).p.cs.s !== 1.5; i++) await page.waitForTimeout(100);
    assert.equal((await getCfg()).p.cs.s, 1.5);
    assert.ok(Math.abs((await box()).font - 20.25 * 1.5) < 0.6);
    // A reload keeps it (and the client player reads the same p.cs).
    await openNative();
    b = await box();
    assert.ok(Math.abs(b.x - 0.2) < 0.003 && Math.abs(b.font - 20.25 * 1.5) < 0.6, JSON.stringify(b));
  });

  await step('native page: 0–9, +/− and V never reach YouTube; typing in a field and other keys do', async () => {
    await page.evaluate(() => { document.activeElement?.blur(); window.__keys = []; });
    await page.mouse.click(1300, 860);
    for (const k of ['5', '0', '9', 'Equal', 'Minus', 'Shift+Equal', 'v', 'Shift+V', 'k', 'ArrowLeft', 'j']) await page.keyboard.press(k);
    assert.deepEqual(await page.evaluate(() => window.__keys.filter(k => k !== 'Shift')), ['k', 'ArrowLeft', 'j']);
    await page.click('#search');
    await page.keyboard.type('a5');
    assert.equal(await page.inputValue('#search'), 'a5');
    assert.deepEqual((await page.evaluate(() => window.__keys)).slice(-2), ['a', '5'], 'typing reaches the page');
  });

  await step('native page: CC follows the client\'s remembered choice (p.cc); a deliberate toggle is remembered', async () => {
    await editCfg({ cc: false });
    await openNative();
    await page.waitForFunction(() => document.querySelector('.ytp-subtitles-button').getAttribute('aria-pressed') === 'false', null, { timeout: 4000 });
    await page.waitForTimeout(1500);
    assert.equal(await page.getAttribute('.ytp-subtitles-button', 'aria-pressed'), 'false', 'switched off once, not toggled back');
    await page.click('.ytp-subtitles-button');
    for (let i = 0; i < 30 && (await getCfg()).p.cc !== true; i++) await page.waitForTimeout(100);
    assert.equal((await getCfg()).p.cc, true, 'turning CC on here is remembered for the client');
    await page.waitForTimeout(1500);
    assert.equal(await page.getAttribute('.ytp-subtitles-button', 'aria-pressed'), 'true');
  });

  await step('native page: the client\'s Shorts switch (shortsInFeeds) keeps Shorts in feeds; the sidebar entry stays out', async () => {
    await setStore({ shortsInFeeds: true });
    await openNative();
    await page.waitForFunction(() => document.documentElement.dataset.ytcShorts === 'on');
    await expectShown(true, ['short-video', 'absolute-short', 'short-shelf']);
    await expectShown(false, ['shorts-nav', 'playable-card']);
    await setStore({ shortsInFeeds: false });
  });

  await step('native page: a related video returns to the client; client pages carry no native cleanup', async () => {
    await openNative();
    await page.click('#related-link');
    await page.waitForURL(u => u.searchParams.get('v') === '-Bxc4gJluC0', { timeout: 8000, waitUntil: 'commit' }).catch(e => { throw new Error(e.message + ' at ' + page.url()); });
    await page.waitForSelector('#ytc-top');
    assert.equal(await page.evaluate(() => document.documentElement.hasAttribute('data-ytc-native')), false);
    await sw.evaluate(o => chrome.storage.local.remove(['ytcConfigV1', 'shortsInFeeds']).then(() => chrome.storage.local.set(Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)))), cfgBefore);
  });

  // ---- actions: comments, chapters, like/dislike, subscribe, save, share, notifications, account
  const calls = ep => log.api.filter(a => a.ep === ep);
  // YTC_SHOTS=dir saves screenshots of the action surfaces (for review).
  await step('comments: lazy load, header, replies, paging to an End marker, timestamps seek, Top/Newest', async () => {
    // The previous step leaves this same video open, whose lazy comment request could land after the log
    // is cleared and count twice: leave that document first.
    await page.goto('about:blank');
    await page.waitForTimeout(100);
    log.api.length = 0;
    await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-info h1.wt');
    await page.waitForSelector('#ytc-cm .yc-h');
    assert.equal(await page.locator('#ytc-cm .yc-sort button').count(), 2);
    await page.locator('#ytc-cm').scrollIntoViewIfNeeded();
    await page.waitForFunction(() => document.querySelectorAll('#ytc-cm-list > .ycm').length >= 20);
    assert.equal(calls('next').filter(a => a.body.continuation === T_FIRST).length, 1, 'first page token from the next response: ' + JSON.stringify(calls('next').filter(a => a.body.continuation === T_FIRST).map(a => a.headers.referer)));
    assert.equal(await page.locator('#ytc-cm .yc-h h2').textContent(), '585 Comments');
    assert.equal(await page.locator('#ytc-cm-list > .ycm').first().locator('.yca').textContent(), '@EdutainmentGames');
    assert.equal((await page.locator('#ytc-cm-list > .ycm').first().locator('.ycl').textContent()).trim(), '760');
    assert.equal(await page.evaluate(() => window.__xss), undefined);
    assert.ok((await page.locator('#ytc-cm-list > .ycm').nth(1).locator('.yct').textContent()).includes('<img src=x'), 'hostile comment shown as text');
    // Replies on demand, collapsible.
    await page.locator('#ytc-rb-0').click();
    await page.waitForFunction(() => document.querySelectorAll('#ytc-rp-0 .ycm').length === 10);
    assert.equal(calls('next').filter(a => a.body.continuation === T_REP).length, 1);
    assert.equal(await page.locator('#ytc-rp-0 button.ycrb').textContent(), 'Show more replies');
    await shot('comments');
    await page.locator('#ytc-rb-0').click();
    assert.ok(await page.locator('#ytc-rp-0.hide').count(), 'replies hidden again');
    assert.equal(calls('next').filter(a => a.body.continuation === T_REP).length, 1, 'no refetch');
    // Paging until the last page → End (only then).
    await page.evaluate(() => scrollTo(0, document.body.scrollHeight));
    await page.waitForSelector('#ytc-cm-more.yc-end');
    assert.equal(await page.locator('#ytc-cm-more').textContent(), 'End');
    assert.equal(await page.locator('#ytc-cm-list > .ycm').count(), 40);
    assert.ok(calls('next').some(a => a.body.continuation === T_PAGE2));
    // Timestamp in a comment seeks our player.
    await waitT(() => document.documentElement.dataset.ytcFirstFrame);
    await page.locator('#ytc-cm-list > .ycm').nth(1).locator('a[data-act="seek"]').click();
    await waitT(() => Math.abs(document.getElementById('ytc-video').currentTime - 12) < 2);
    // Newest: list reloads; no End during the transition.
    await page.locator('#ytc-cm').scrollIntoViewIfNeeded();
    const pill = page.evaluate(() => new Promise(res => {
      const b = document.getElementById('ytc-cs1');
      const mo = new MutationObserver(() => { if (b.className !== 'on') return; mo.disconnect(); requestAnimationFrame(() => requestAnimationFrame(() => { const m = document.querySelector('#ytc-csort > i.ycsk'); res({ ok: !!m, run: m ? m.getAnimations().map(a => a.transitionProperty) : [] }); })); });
      mo.observe(b, { attributes: true });
      setTimeout(() => res(null), 3000);
    }));
    await page.locator('#ytc-cm .yc-sort button', { hasText: 'Newest' }).click();
    assert.equal(await page.locator('#ytc-cm-more.yc-end').count(), 0, 'no End while the new sort loads');
    const pr = await pill;
    assert.ok(pr && pr.ok && pr.run.includes('clip-path'), 'the Top/Newest pill slides ' + JSON.stringify(pr));
    await page.waitForTimeout(450);
    const pillOn = await page.evaluate(() => { const s = document.getElementById('ytc-csort'), m = s.querySelector(':scope > i.ycsk'), b = document.getElementById('ytc-cs1'), a = s.getBoundingClientRect(), r = b.getBoundingClientRect(), v = (c => { const v = /inset\(([^)]*?)(?: round [^)]*)?\)/.exec(c)[1].trim().split(/\s+/).map(parseFloat); return [v[0], v[1] ?? v[0], v[2] ?? v[0], v[3] ?? v[1] ?? v[0]]; })(getComputedStyle(m).clipPath); return { dl: v[3] - (r.left - a.left), dw: (a.width - v[1] - v[3]) - r.width, dt: v[0] - (r.top - a.top), fg: getComputedStyle(b).color }; });
    assert.ok(Math.abs(pillOn.dl) < 0.6 && Math.abs(pillOn.dw) < 0.6 && Math.abs(pillOn.dt) < 0.6, 'the pill lands exactly on Newest ' + JSON.stringify(pillOn));
    await page.waitForFunction(() => document.querySelector('#ytc-cm-list > .ycm .yca')?.textContent === '@newest-e2e');
    assert.ok(calls('next').some(a => a.body.continuation === T_NEW));
    assert.equal(await page.getAttribute('#ytc-cm .yc-sort button.on', 'aria-pressed'), 'true');
  });

  if (process.env.YTC_SHOTS) await step('shots: watch page body mid-motion (Top/Newest pill, link underline, reply thread unrolling), slowed 10×', async () => {
    const dir = process.env.YTC_SHOTS;
    await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-cm .yc-sort');
    await page.locator('#ytc-cm').scrollIntoViewIfNeeded();
    await page.waitForSelector('#ytc-cm-list > .ycm');
    await page.waitForTimeout(400);
    await cdp.send('Animation.enable'); await cdp.send('Animation.setPlaybackRate', { playbackRate: 0.1 });
    const hb = await page.locator('#ytc-cm .yc-h').boundingBox();
    await page.locator('#ytc-cm .yc-sort button', { hasText: 'Newest' }).click();
    await page.waitForTimeout(900);
    await page.screenshot({ path: path.join(dir, 'comments-pill-mid.png'), clip: { x: hb.x, y: hb.y - 4, width: 360, height: hb.height + 8 } });
    await page.waitForTimeout(3000);
    await page.screenshot({ path: path.join(dir, 'comments-pill-end.png'), clip: { x: hb.x, y: hb.y - 4, width: 360, height: hb.height + 8 } });
    await page.locator('#ytc-rb-0').click();
    await page.waitForFunction(() => document.querySelectorAll('#ytc-rp-0 .ycm').length > 0);
    await page.locator('#ytc-rb-0').click();
    await page.locator('#ytc-rb-0').click();
    await page.waitForTimeout(1500);
    const rb = await page.locator('#ytc-cm-list > .ycm').first().boundingBox();
    await page.screenshot({ path: path.join(dir, 'comments-replies-unroll-mid.png'), clip: { x: rb.x, y: rb.y, width: 700, height: 420 } });
    await cdp.send('Animation.setPlaybackRate', { playbackRate: 1 });
    await page.evaluate(() => scrollTo(0, 0));
    const a = page.locator('#ytc-desc a').first();
    if (await a.count()) {
      await a.scrollIntoViewIfNeeded();
      await page.waitForTimeout(200);
      await cdp.send('Animation.setPlaybackRate', { playbackRate: 0.1 });
      await a.hover();
      await page.waitForTimeout(1100);
      const db = await page.locator('#ytc-desc').boundingBox();
      await page.screenshot({ path: path.join(dir, 'desc-underline-mid.png'), clip: { x: db.x, y: db.y, width: Math.min(db.width, 760), height: Math.min(db.height, 200) } });
      await cdp.send('Animation.setPlaybackRate', { playbackRate: 1 });
      await page.waitForTimeout(500);
      await page.screenshot({ path: path.join(dir, 'desc-underline-end.png'), clip: { x: db.x, y: db.y, width: Math.min(db.width, 760), height: Math.min(db.height, 200) } });
    }
    await page.mouse.move(5, 5);
  });

  // The chapter panel 160 px below the top bar (room for the list to unfold below it).
  const chapTop = () => page.evaluate(() => { const r = document.getElementById('ytc-chd').getBoundingClientRect(); scrollBy(0, r.top - 160); });
  const chapGeo = () => page.evaluate(() => {
    const q = x => document.querySelector(x), r = x => q(x).getBoundingClientRect(), d = r('#ytc-chd'), c = q('#ytc-chd .ychc'), cr = c.getBoundingClientRect();
    const cl = getComputedStyle(c).clipPath, m = /^inset\((.*?)(?: round [^)]*)?\)$/.exec(cl), parts = m ? m[1].match(/calc\([^)]*\)|\S+/g) : ['0px'];
    const v = parts.map(x => { let t = 0, sg = 1; for (const k of x.replace(/^calc\(|\)$/g, '').split(/\s+/)) { if (k === '-') sg = -1; else if (k === '+') sg = 1; else { t += sg * parseFloat(k) * (k.endsWith('%') ? cr.height / 100 : 1); sg = 1; } } return t; });
    const box = e => { const b = e.getBoundingClientRect(); return { l: b.left, t: b.top, r: b.right, w: b.width, h: b.height, cx: b.left + b.width / 2, cy: b.top + b.height / 2 }; };
    const x = box(q('#ytc-chh .ychx'));
    return { clip: cl, cls: q('#ytc-chd').className, top: d.top, h: d.height, l: d.left, w: d.width, cm: r('#ytc-cm').top, vis: cr.height - (v[2] ?? v[0]) - (v[0] || 0), full: cr.height,
      run: c.getAnimations().length, olOp: getComputedStyle(q('#ytc-chd ol')).opacity, desc: { l: r('#ytc-desc').left, w: r('#ytc-desc').width },
      frame: box(q('#ytc-chh .sbf')), text: { c: x.cy }, chev: box(q('#ytc-chh .ychv')), chevSvg: box(q('#ytc-chh .ychv svg')), row0: box(q('#ytc-chi0 .sbf')) };
  });

  await step('chapters from the player bar are listed and seek our player', async () => {
    await page.goto('https://www.youtube.com/watch?v=kCc8FmEb1nY', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-chap .ych li');
    assert.equal(await page.locator('#ytc-chap li').count(), 30);
    await waitT(() => document.documentElement.dataset.ytcFirstFrame);
    // Compact at rest; the list unfolds under the pointer and a row click still seeks.
    await chapTop();
    assert.equal(await page.getAttribute('#ytc-chd', 'class'), 'ych', 'compact at rest');
    await page.hover('#ytc-chh');
    await waitT(() => document.getElementById('ytc-chd').classList.contains('open'));
    await page.waitForTimeout(500);
    await shot('chapters');
    await page.locator('#ytc-chap li').nth(1).locator('a').click();
    await waitT(() => Math.abs(document.getElementById('ytc-video').currentTime - 20) < 2);
    await page.mouse.move(700, 880);
    await waitT(() => document.getElementById('ytc-chd').className === 'ych');
  });

  await step('chapter panel: compact by default and aligned, hover unfolds it over the comments without moving them, pointer-out shrinks faster, a pass-through never opens it, keyboard focus is instant, Escape collapses', async () => {
    await page.mouse.move(700, 880);
    await page.evaluate(() => document.getElementById('ytc-video').pause());
    await chapTop();
    await page.waitForTimeout(300);
    let g = await chapGeo();
    assert.equal(g.cls, 'ych', 'compact: ' + g.cls);
    assert.ok(Math.abs(g.h - 72) < 0.5 && Math.abs(g.vis - 72) < 0.5, `one 72 px row reserved and shown (${g.h}, ${g.vis}, ${g.clip}, ${g.full})`);
    assert.equal(g.olOp, '0', 'the list is folded away');
    // Aligned with the description above (same edges, frame at its text's 17 px inset) and the comments 20 px below.
    assert.ok(Math.abs(g.l - g.desc.l) < 0.5 && Math.abs(g.w - g.desc.w) < 0.5, `same edges as the description (${g.l}/${g.w} vs ${g.desc.l}/${g.desc.w})`);
    assert.ok(Math.abs(g.frame.l - g.l - 17) < 0.5 && Math.abs(g.frame.t - g.top - 9) < 0.5 && g.frame.w === 96 && g.frame.h === 54, 'compact frame 96×54 at 17/9 px ' + JSON.stringify(g.frame));
    assert.ok(Math.abs(g.text.c - (g.top + 36)) <= 1, 'text block centred in the row ' + JSON.stringify(g.text));
    assert.ok(Math.abs(g.chev.cx - g.chevSvg.cx) <= 1 && Math.abs(g.chev.cy - g.chevSvg.cy) <= 1 && Math.abs(g.chev.cy - (g.top + 36)) <= 1, 'chevron centred in its circle and the row ' + JSON.stringify([g.chev, g.chevSvg]));
    assert.ok(Math.abs(g.l + g.w - g.chev.r - 15) < 0.5, 'chevron 15 px from the right edge');
    assert.ok(Math.abs(g.cm - (g.top + 72 + 20)) <= 1, `comments 20 px below (${g.cm - g.top - 72})`);
    // Every chapter shows its own storyboard cell (sheet L2 at 1×, frames every 1 s): the frame taken at its start.
    await waitT(() => !!document.getElementById('ytc-chf2').getAttribute('style'));
    const fr = await page.evaluate(() => [0, 1, 2, 3].map(i => document.getElementById('ytc-chf' + i)).concat(document.getElementById('ytc-chhf')).map(e => {
      const st = e.getAttribute('style'), m = /url\("([^"]+)"\) ([\d.]+)% ([\d.]+)%\/(\d+)% (\d+)%/.exec(st), b = e.parentElement.getBoundingClientRect(), r = e.getBoundingClientRect();
      const u = new URL(m[1]), page = +(/\/M(\d+)\.jpg$/.exec(u.pathname) || [0, 0])[1], cols = +m[4] / 100, rows = +m[5] / 100;
      const col = cols > 1 ? Math.round(+m[2] * (cols - 1) / 100) : 0, row = rows > 1 ? Math.round(+m[3] * (rows - 1) / 100) : 0;
      return { path: u.pathname, sigh: u.searchParams.get('sigh'), n: page * 25 + row * 5 + col, cell: `${page}:${col}:${row}`, fill: [r.left - b.left, r.top - b.top, r.width - b.width, r.height - b.height].every(x => Math.abs(x) < 0.01), st };
    }));
    assert.deepEqual(fr.slice(0, 3).map(f => f.n), [0, 20, 30], 'chapter frames at 0 / 20 / 30 s: ' + JSON.stringify(fr.slice(0, 3)));
    assert.equal(new Set(fr.slice(0, 3).map(f => f.cell)).size, 3, 'three different sprite cells');
    assert.equal(fr[3].n, SECONDS - 1, 'a chapter past the end shows the last frame');
    assert.ok(fr.every(f => /^\/sb\/kCc8FmEb1nY\/storyboard3_L2\/M\d\.jpg$/.test(f.path) && f.sigh === 'rs$E2E2' && f.fill), 'L2 sheets (96 px boxes at 1×), the cell fills its 16:9 box: ' + JSON.stringify(fr));
    assert.equal(fr[4].st, fr[1].st, 'the compact row shows the current chapter\'s frame');
    assert.equal(await page.textContent('#ytc-chht'), await page.textContent('#ytc-chi1 span'));
    assert.equal(await page.textContent('#ytc-chhn'), 'Chapter 2 of 30'); assert.equal(await page.textContent('#ytc-chhc'), '0:20');
    const sheets = [...new Set(log.sb.filter(x => x.url.includes('/kCc8FmEb1nY/')).map(x => new URL(x.url).pathname))].sort();
    assert.deepEqual(sheets, ['/sb/kCc8FmEb1nY/storyboard3_L2/M0.jpg', '/sb/kCc8FmEb1nY/storyboard3_L2/M1.jpg'], 'only the sheets the frames use');
    if (process.env.YTC_SHOTS) {
      const b = await page.locator('#ytc-chd').boundingBox();
      await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'chapters-compact-1400.png'), clip: { x: b.x - 16, y: b.y - 90, width: b.width + 32, height: 260 } });
    }
    // Hover: unfolds over the comments; nothing below moves on any frame, the page does not scroll.
    const sampler = () => page.evaluate(() => { window.__chs = []; const go = () => { if (!window.__chs) return; try { const c = document.querySelector('#ytc-chd .ychc'), r = c.getBoundingClientRect(), H = r.height, cl = getComputedStyle(c).clipPath;
      const m = /^inset\((.*?)(?: round [^)]*)?\)$/.exec(cl), parts = m ? m[1].match(/calc\([^)]*\)|\S+/g) : ['0px'], v = parts.map(x => { let t = 0, sg = 1; for (const k of x.replace(/^calc\(|\)$/g, '').split(/\s+/)) { if (k === '-') sg = -1; else if (k === '+') sg = 1; else { t += sg * parseFloat(k) * (k.endsWith('%') ? H / 100 : 1); sg = 1; } } return t; }), bot = v[2] ?? v[0];
      window.__chs.push({ cm: document.getElementById('ytc-cm').getBoundingClientRect().top, y: scrollY, vis: H - bot - (v[0] || 0) }); } catch (e) { window.__chs.push({ err: String(e) }); } requestAnimationFrame(go); }; requestAnimationFrame(go); });
    const sampled = () => page.evaluate(() => { const s = window.__chs; window.__chs = null; return s; });
    await sampler();
    const hb = await page.locator('#ytc-chh').boundingBox();
    await page.mouse.move(hb.x + 200, hb.y + 30);
    await waitT(() => document.getElementById('ytc-chd').className === 'ych open hv');
    await page.waitForTimeout(650);
    let ss = await sampled();
    g = await chapGeo();
    assert.ok(ss.length > 10 && ss.every(x => x.cm === ss[0].cm && x.y === ss[0].y) && Math.abs(ss[0].cm - g.cm) < 0.01, 'comments and scroll still on every frame of the unfold: ' + JSON.stringify([ss[0], ss.at(-1), ss.length, g]));
    assert.ok(ss.some(x => x.vis > 80 && x.vis < g.vis - 8), 'it unfolded over several frames (not a jump)');
    assert.ok(g.vis > 360 && Math.abs(g.vis - g.full) < 0.5 && g.olOp === '1' && g.run === 0, 'fully open, list shown: ' + JSON.stringify(g));
    assert.ok(Math.abs(g.row0.l - g.frame.l) < 0.5, 'rows\' frames line up with the compact row\'s');
    assert.equal(await page.getAttribute('#ytc-chh', 'aria-expanded'), 'true');
    assert.equal(await page.evaluate(() => getComputedStyle(document.querySelector('#ytc-chd .ychc')).transitionDuration), '0.351s', 'opens on the house spring');
    await shot('chapters-open');
    // Pointer out (below the open panel): folds back, faster; the comments never move.
    await sampler();
    await page.mouse.move(hb.x + 200, g.top + g.vis + 40);
    await waitT(() => document.getElementById('ytc-chd').className === 'ych');
    assert.equal(await page.evaluate(() => getComputedStyle(document.querySelector('#ytc-chd .ychc')).transitionDuration), '0.18s', 'closes faster than it opens');
    await page.waitForTimeout(350);
    ss = await sampled();
    assert.ok(ss.every(x => x.cm === ss[0].cm && x.y === ss[0].y), 'comments still while it folds');
    g = await chapGeo();
    assert.ok(Math.abs(g.vis - 72) < 0.5 && g.run === 0, 'back to the compact row');
    // A quick pass across the row never opens it (60 ms intent).
    await sampler();
    await page.mouse.move(hb.x + 200, hb.y - 20); await page.mouse.move(hb.x + 200, hb.y + 30); await page.mouse.move(hb.x + 200, hb.y + 100);
    await page.waitForTimeout(300);
    ss = await sampled();
    assert.ok(ss.every(x => x.vis < 72.5), 'a pass-through does not open it: ' + Math.max(...ss.map(x => x.vis)));
    assert.equal(await page.getAttribute('#ytc-chd', 'class'), 'ych');
    // Mid-unfold, slowed 10×, at 1400 and 2560.
    if (process.env.YTC_SHOTS) {
      await cdp.send('Animation.enable');
      for (const [w, h] of [[1400, 900], [2560, 1440]]) {
        await page.setViewportSize({ width: w, height: h });
        await chapTop();
        await page.waitForTimeout(150);
        const b = await page.locator('#ytc-chd').boundingBox();
        await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `chapters-compact-${w}.png`), clip: { x: b.x - 16, y: b.y - 90, width: b.width + 32, height: 260 } });
        await cdp.send('Animation.setPlaybackRate', { playbackRate: 0.1 });
        await page.mouse.move(b.x + 200, b.y + 30);
        await page.waitForTimeout(1500); // ≈ 90 ms into the unfold (after the 60 ms intent)
        await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `chapters-mid-expand-${w}.png`), clip: { x: b.x - 16, y: b.y - 90, width: b.width + 32, height: 560 } });
        await cdp.send('Animation.setPlaybackRate', { playbackRate: 1 });
        await page.waitForTimeout(500);
        await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `chapters-open-${w}.png`), clip: { x: b.x - 16, y: b.y - 90, width: b.width + 32, height: 560 } });
        await page.mouse.move(b.x + 200, b.y + 560);
        await waitT(() => document.getElementById('ytc-chd').className === 'ych');
        await page.waitForTimeout(300);
      }
      await page.setViewportSize({ width: 1400, height: 900 });
      await chapTop();
    }
    // Keyboard: focus moving in opens it with no motion; Escape folds it at once onto the compact row; Tab opens it again.
    await page.evaluate(() => document.getElementById('ytc-chh').focus());
    assert.equal(await page.getAttribute('#ytc-chd', 'class'), 'ych', 'programmatic focus after a pointer input: stays compact');
    const instant = () => page.evaluate(() => { const c = document.querySelector('#ytc-chd .ychc'); return { cls: document.getElementById('ytc-chd').className, run: c.getAnimations().length + document.querySelector('#ytc-chd ol').getAnimations().length, clip: getComputedStyle(c).clipPath, act: document.activeElement.id || document.activeElement.closest('li')?.id }; });
    await page.keyboard.press('Tab');
    let k = await instant();
    assert.ok(k.cls === 'ych open now' && k.run === 0 && k.clip === 'inset(0px round 12px)' && k.act === 'ytc-chi0', 'Tab into the list: open at once ' + JSON.stringify(k));
    await page.keyboard.press('Escape');
    k = await instant();
    assert.ok(k.cls === 'ych now' && k.run === 0 && /^inset\(0px 0px calc\(100% - 72px\)( 0px)? round 12px\)$/.test(k.clip) && k.act === 'ytc-chh', 'Escape: compact at once, focus on the row ' + JSON.stringify(k));
    assert.equal(await page.getAttribute('#ytc-chh', 'aria-expanded'), 'false');
    // The pointer coming in opens it again even with focus still on the row; leaving folds it.
    await page.mouse.move(hb.x + 200, hb.y + 30);
    await waitT(() => document.getElementById('ytc-chd').className === 'ych open hv');
    await page.mouse.move(hb.x + 200, hb.y - 60);
    await waitT(() => document.getElementById('ytc-chd').className === 'ych');
    await page.keyboard.press('Escape');
    await page.keyboard.press('Shift+Tab'); await page.keyboard.press('Tab');
    assert.equal(await page.evaluate(() => document.activeElement.id), 'ytc-chh');
    await page.keyboard.press('Tab');
    k = await instant();
    assert.ok(k.cls === 'ych open now' && k.run === 0 && k.act === 'ytc-chi0', 'Tab on: open again ' + JSON.stringify(k));
    await page.keyboard.press('Shift+Tab'); await page.keyboard.press('Shift+Tab');
    k = await instant();
    assert.ok(k.cls === 'ych now' && k.run === 0 && k.act !== 'ytc-chh', 'focus leaving: compact at once ' + JSON.stringify(k));
    // A click on the open compact row folds it (it stays folded until the pointer leaves); back in, it opens again.
    await page.click('#ytc-chh');
    await waitT(() => document.getElementById('ytc-chd').className === 'ych');
    await page.waitForTimeout(150);
    assert.equal(await page.getAttribute('#ytc-chd', 'class'), 'ych', 'stays folded under the pointer');
    await page.mouse.move(hb.x + 200, hb.y - 60);
    await page.mouse.move(hb.x + 200, hb.y + 30);
    await waitT(() => document.getElementById('ytc-chd').className === 'ych open hv');
    await page.mouse.move(hb.x + 200, hb.y - 60);
    await waitT(() => document.getElementById('ytc-chd').className === 'ych');
    await page.waitForTimeout(500);
    // The compact row follows playback: the new chapter's text slides in (playback, not a key).
    await page.waitForTimeout(500); // past the last key's no-motion window
    const slid = page.evaluate(() => new Promise(res => { const t = document.getElementById('ytc-chht'); const mo = new MutationObserver(() => { mo.disconnect(); res(document.getElementById('ytc-chhx').getAnimations().length); }); mo.observe(t, { childList: true, characterData: true, subtree: true }); setTimeout(() => res(-1), 5000); }));
    await page.evaluate(() => { document.getElementById('ytc-video').currentTime = 30.5; });
    assert.equal(await slid, 1, 'the compact row\'s text slides to the new chapter');
    await waitT(() => document.getElementById('ytc-chhn').textContent === 'Chapter 3 of 30' && document.getElementById('ytc-chhc').textContent === '0:30');
    assert.equal(await page.textContent('#ytc-chht'), await page.textContent('#ytc-chi2 span'));
    assert.equal(await page.getAttribute('#ytc-chhf', 'style'), await page.getAttribute('#ytc-chf2', 'style'), 'its frame follows');
    await page.evaluate(() => { document.getElementById('ytc-video').currentTime = 20.5; });
    await waitT(() => document.getElementById('ytc-chhn').textContent === 'Chapter 2 of 30');
    await page.evaluate(() => scrollTo(0, 0));
  });

  // Seek-bar geometry: segments at --s..--e of #ytc-segs (whole pixels), a 2 px gap after each but the last.
  const segGeo = () => page.evaluate(() => {
    const b = document.getElementById('ytc-segs').getBoundingClientRect();
    return { l: b.left, w: b.width, segs: [...document.querySelectorAll('#ytc-segs .pbar')].map(e => { const r = e.getBoundingClientRect(); return { l: r.left - b.left, r: r.right - b.left, s: +getComputedStyle(e).getPropertyValue('--s'), e: +getComputedStyle(e).getPropertyValue('--e') }; }) };
  });
  const checkSegs = (g, fr) => {
    assert.equal(g.segs.length, fr.length - 1, 'segment count');
    g.segs.forEach((sg, i) => {
      const gap = i < g.segs.length - 1 ? 2 : 0;
      assert.ok(Math.abs(sg.l - Math.round(fr[i] * g.w)) <= 1, `segment ${i} starts at ${fr[i]} (${sg.l} of ${g.w})`);
      assert.ok(Math.abs(sg.r - (Math.round(fr[i + 1] * g.w) - gap)) <= 1, `segment ${i} ends at ${fr[i + 1]} − gap (${sg.r})`);
      assert.ok(Math.abs(sg.l - Math.round(sg.l)) < 0.01 || Math.abs((g.l + sg.l) - Math.round(g.l + sg.l)) < 0.01, `segment ${i} on a whole pixel (${g.l + sg.l})`);
      if (i > 0) assert.ok(Math.abs(sg.l - g.segs[i - 1].r - 2) < 0.6, `2 px gap before segment ${i} (${sg.l - g.segs[i - 1].r})`);
    });
  };
  const hoverSeek = async f => {
    const r = await page.evaluate(() => { const b = document.getElementById('ytc-segs').getBoundingClientRect(), s = document.getElementById('ytc-seek').getBoundingClientRect(); return { l: b.left, w: b.width, y: s.top + s.height / 2 }; });
    await page.mouse.move(r.l + f * r.w, r.y);
    return r.l + f * r.w;
  };
  const barShot = async name => { if (!process.env.YTC_SHOTS) return; const b = await page.locator('#ytc-player').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, name + '.png'), clip: { x: b.x, y: b.y + b.height - 230, width: b.width, height: 230 } }); };
  const tipBox = () => page.evaluate(() => { const t = document.querySelector('.ptip'), r = t.getBoundingClientRect(), s = document.getElementById('ytc-seek').getBoundingClientRect(); return { l: r.left, r: r.right, c: r.left + r.width / 2, sl: s.left, sr: s.right, op: getComputedStyle(t).opacity, chap: document.getElementById('ytc-tipc').textContent, t: document.getElementById('ytc-tipt').textContent }; });

  await step('chapters split the seek bar: segments, fills, hover tooltip + grown segment, label, list mark, Ctrl/⌘+arrows', async () => {
    await page.evaluate(() => scrollTo(0, 0));
    await waitT(() => document.querySelectorAll('#ytc-segs .pbar').length === 3);
    const titles = await page.$$eval('#ytc-chap li span', xs => xs.map(x => x.textContent));
    assert.equal(await page.getAttribute('#ytc-chb', 'data-n'), '3', 'label shown');
    checkSegs(await segGeo(), [0, 0.5, 0.75, 1]);
    // Paused at 25 s: segment 0 full, segment 1 filled to exactly 25/40 of the whole bar, segment 2 empty.
    await page.evaluate(() => { const v = document.getElementById('ytc-video'); v.pause(); v.currentTime = 25; });
    await waitT(() => Math.abs(parseFloat(document.getElementById('ytc-seek').style.getPropertyValue('--p')) - 0.625) < 0.002);
    await page.waitForTimeout(300);
    const fills = await page.evaluate(() => { const b = document.getElementById('ytc-segs').getBoundingClientRect(); return [...document.querySelectorAll('#ytc-segs .ppos')].map(e => { const r = e.getBoundingClientRect(); return { l: r.left - b.left, r: r.right - b.left, w: b.width }; }); });
    assert.ok(Math.abs(fills[1].r - 0.625 * fills[1].w) < 1, `fill edge at 25 s (${fills[1].r} vs ${0.625 * fills[1].w})`);
    assert.ok(fills[0].r - fills[0].l > 0.49 * fills[0].w && fills[2].r - fills[2].l < 0.5, 'segment 0 full, segment 2 empty');
    assert.equal(await page.textContent('#ytc-chl'), titles[1], 'label = chapter at 25 s');
    assert.equal(await page.getAttribute('#ytc-chi1', 'class'), 'on', 'list marks the current chapter');
    assert.equal(await page.locator('#ytc-chap li.on').count(), 1);
    // Hover: tooltip with the chapter title + time, centered on the pointer; the hovered segment grows.
    const x = await hoverSeek(0.6);
    await waitT(t => document.getElementById('ytc-tipc').textContent === t && document.getElementById('ytc-tipt').textContent === '0:24', titles[1]);
    assert.equal(await page.getAttribute('#ytc-sg1', 'class'), 'pbar on', 'hovered segment grown');
    assert.match(await page.$eval('#ytc-sg1', e => getComputedStyle(e).transform), /^matrix\(1, 0, 0, 1\.6, 0, 0\)$/, 'grown taller than the rest');
    assert.equal(await page.$eval('#ytc-sg0', e => getComputedStyle(e).transform), 'none');
    assert.equal(await page.locator('#ytc-segs .pbar.on').count(), 1);
    let tb = await tipBox();
    assert.equal(tb.op, '1', 'tooltip visible on hover');
    assert.ok(Math.abs(tb.c - x) < 1.5, `tooltip centered on the pointer (${tb.c} vs ${x})`);
    // The storyboard frame at the pointer's time, above the title: 160×90, centred, the right cell (24 s → sheet M0, cell 24).
    const tipFrame = () => page.evaluate(() => {
      const f = document.querySelector('.ptip .ptf'), i = document.getElementById('ytc-tipf'), b = f.getBoundingClientRect(), t = document.getElementById('ytc-tip').getBoundingClientRect(), st = i.getAttribute('style') || '';
      const m = /url\("([^"]+)"\) ([\d.]+)% ([\d.]+)%\/(\d+)% (\d+)%/.exec(st), r = i.getBoundingClientRect();
      if (!m) return { shown: getComputedStyle(f).display };
      const u = new URL(m[1]), page = +(/\/M(\d+)\.jpg$/.exec(u.pathname) || [0, 0])[1], cols = +m[4] / 100, rows = +m[5] / 100;
      const col = cols > 1 ? Math.round(+m[2] * (cols - 1) / 100) : 0, row = rows > 1 ? Math.round(+m[3] * (rows - 1) / 100) : 0;
      return { shown: getComputedStyle(f).display, w: b.width, h: b.height, dc: (b.left + b.width / 2) - (t.left + t.width / 2), above: b.bottom <= document.getElementById('ytc-tipc').getBoundingClientRect().top, n: page * 25 + row * 5 + col, dn: +i.dataset.n, path: u.pathname, fill: Math.abs(r.width - b.width) < 0.01 && Math.abs(r.height - b.height) < 0.01 };
    });
    let tf = await tipFrame();
    assert.ok(tf.shown === 'block' && tf.w === 160 && tf.h === 90 && Math.abs(tf.dc) < 0.6 && tf.above && tf.fill, 'seek preview frame ' + JSON.stringify(tf));
    assert.ok(tf.n === 24 && tf.dn === 24 && tf.path === '/sb/kCc8FmEb1nY/storyboard3_L2/M0.jpg', 'the frame at 0:24 ' + JSON.stringify(tf));
    await barShot('chapters-hover-1400');
    // Near the right end: last chapter, the tooltip stays inside the bar.
    await hoverSeek(0.9);
    await waitT(() => document.getElementById('ytc-tipf').dataset.n === '36');
    tf = await tipFrame();
    assert.ok(tf.n === 36 && tf.path === '/sb/kCc8FmEb1nY/storyboard3_L2/M1.jpg', 'tracks the pointer: 0:36 → the cropped last sheet ' + JSON.stringify(tf));
    await hoverSeek(0.995);
    await waitT(t => document.getElementById('ytc-tipc').textContent === t && document.getElementById('ytc-tipt').textContent === '0:39', titles[2]);
    assert.equal(await page.evaluate(() => document.getElementById('ytc-tipf').dataset.n), '39');
    tb = await tipBox();
    assert.ok(tb.r <= tb.sr + 0.5 && tb.l >= tb.sl - 0.5, `tooltip clamped inside the bar (${tb.l}..${tb.r} in ${tb.sl}..${tb.sr})`);
    assert.equal(await page.getAttribute('#ytc-sg2', 'class'), 'pbar on');
    await hoverSeek(0.002);
    tb = await tipBox();
    assert.ok(tb.l >= tb.sl - 0.5, 'clamped at the left end');
    await page.mouse.move(700, 880);
    await waitT(() => !document.querySelector('#ytc-segs .pbar.on'));
    // Sweeping into another chapter morphs the tooltip's width (its surface clips from the old title's box).
    await hoverSeek(0.3);
    await waitT(t => document.getElementById('ytc-tipc').textContent === t, titles[0]);
    await page.waitForTimeout(400);
    const tipMorph = page.evaluate(() => new Promise(res => {
      const c = document.getElementById('ytc-tipc'), tip = document.getElementById('ytc-tip');
      const mo = new MutationObserver(() => { mo.disconnect(); requestAnimationFrame(() => res({ surf: document.getAnimations().filter(a => a.effect && a.effect.target === tip && a.effect.pseudoElement === '::before').length, clip: getComputedStyle(tip, '::before').clipPath })); });
      mo.observe(c, { childList: true, characterData: true, subtree: true });
      setTimeout(() => res(null), 2000);
    }));
    await hoverSeek(0.6);
    const tm = await tipMorph;
    assert.ok(tm && tm.surf === 1 && /^inset/.test(tm.clip), 'the tooltip morphs between chapter titles ' + JSON.stringify(tm));
    await page.waitForTimeout(400);
    tb = await tipBox();
    assert.ok(Math.abs(tb.c - (await hoverSeek(0.6))) < 1.5, 'still centred on the pointer after the morph');
    await page.mouse.move(700, 880);
    // Playing across a boundary: the label and the list mark follow (no re-render: same elements); the list's
    // highlight is one mark that springs from row 1 to row 2 and lands exactly on it.
    const li = await page.$('#ytc-chi2');
    await chapTop();
    { const hb = await page.locator('#ytc-chh').boundingBox(); await page.mouse.move(hb.x + 200, hb.y + 30); }
    await waitT(() => document.getElementById('ytc-chd').classList.contains('open'));
    await page.waitForTimeout(450);
    const chMark = page.evaluate(() => new Promise(res => {
      const li2 = document.getElementById('ytc-chi2');
      const mo = new MutationObserver(() => { if (li2.className !== 'on') return; mo.disconnect(); requestAnimationFrame(() => requestAnimationFrame(() => { const m = document.querySelector('#ytc-chol > i.ychk'); res({ ok: !!m, run: m ? m.getAnimations().map(a => a.transitionProperty) : [], mk: document.getElementById('ytc-chol').dataset.mk }); })); });
      mo.observe(li2, { attributes: true });
      setTimeout(() => res(null), 7000);
    }));
    await page.evaluate(() => { const v = document.getElementById('ytc-video'); v.currentTime = 29.2; v.play(); });
    await waitT(t => document.getElementById('ytc-chl').textContent === t && document.getElementById('ytc-chi2').className === 'on' && !document.getElementById('ytc-chi1').className, titles[2], 6000);
    assert.ok(await li.evaluate(e => e.isConnected), 'list item kept (class toggle, no re-render)');
    const cm = await chMark;
    assert.ok(cm && cm.ok && cm.mk === '1' && cm.run.includes('clip-path'), 'the chapter highlight slides to the new row ' + JSON.stringify(cm));
    await page.waitForTimeout(550);
    const onRow = () => page.evaluate(() => { const ol = document.getElementById('ytc-chol'), m = ol.querySelector(':scope > i.ychk'), a = ol.querySelector('li.on a'), s = ol.getBoundingClientRect(), r = a.getBoundingClientRect(), v = (c => { const v = /inset\(([^)]*?)(?: round [^)]*)?\)/.exec(c)[1].trim().split(/\s+/).map(parseFloat); return [v[0], v[1] ?? v[0], v[2] ?? v[0], v[3] ?? v[1] ?? v[0]]; })(getComputedStyle(m).clipPath); return { dt: v[0] - (r.top - s.top + ol.scrollTop - ol.clientTop), dl: v[3] - (r.left - s.left - ol.clientLeft), dh: (parseFloat(m.style.height) - v[0] - v[2]) - r.height, bg: getComputedStyle(a).backgroundColor, run: m.getAnimations().length }; });
    let orow = await onRow();
    assert.ok(Math.abs(orow.dt) < 0.6 && Math.abs(orow.dl) < 0.6 && Math.abs(orow.dh) < 0.6 && orow.run === 0 && orow.bg === 'rgba(0, 0, 0, 0)', 'the highlight lands exactly on the current row ' + JSON.stringify(orow));
    if (process.env.YTC_SHOTS) {
      await cdp.send('Animation.enable'); await cdp.send('Animation.setPlaybackRate', { playbackRate: 0.1 });
      await page.evaluate(() => { document.getElementById('ytc-video').currentTime = 20.5; });
      await page.waitForTimeout(1300);
      const cb = await page.locator('#ytc-chap').boundingBox();
      await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'chapters-mark-mid.png'), clip: { x: cb.x, y: cb.y, width: cb.width, height: 300 } });
      await cdp.send('Animation.setPlaybackRate', { playbackRate: 1 });
      await page.waitForTimeout(600);
      await page.evaluate(() => { const v = document.getElementById('ytc-video'); v.currentTime = 29.2; v.play(); });
      await waitT(() => document.getElementById('ytc-chi2').className === 'on', null, 6000);
      await page.waitForTimeout(600);
    }
    await page.mouse.move(700, 880);
    await page.evaluate(() => scrollTo(0, 0));
    // Ctrl/⌘+← / →: previous / next chapter (just into chapter 2 → chapter 1's start).
    await page.keyboard.press('Control+ArrowLeft');
    await waitT(() => Math.abs(document.getElementById('ytc-video').currentTime - 20) < 1);
    await waitT(() => document.getElementById('ytc-chi1').className === 'on');
    assert.equal(await page.evaluate(() => new Promise(res => requestAnimationFrame(() => requestAnimationFrame(() => res(document.querySelector('#ytc-chol > i.ychk').getAnimations().length))))), 0, 'a keyboard chapter jump moves the highlight at once');
    assert.equal(await page.textContent('#ytc-toast'), titles[1], 'toast names the chapter');
    await page.keyboard.press('Meta+ArrowRight');
    await waitT(() => Math.abs(document.getElementById('ytc-video').currentTime - 30) < 1);
    await page.waitForTimeout(400);
    await page.keyboard.press('Control+ArrowRight');
    await page.waitForTimeout(200);
    assert.ok(Math.abs(await page.evaluate(() => document.getElementById('ytc-video').currentTime) - 30) < 2, 'last chapter: Ctrl+→ stays');
    await page.evaluate(() => document.getElementById('ytc-video').pause());
    // The label opens the chapter panel (pinned, scrolled into view); a press elsewhere folds it.
    assert.equal(await page.getAttribute('#ytc-chd', 'class'), 'ych');
    await page.click('#ytc-chb');
    await waitT(() => { const d = document.getElementById('ytc-chd'), r = d.getBoundingClientRect(); return d.className === 'ych open' && r.top >= 0 && r.top < innerHeight; });
    await page.waitForTimeout(450);
    await shot('chapters-list');
    await page.mouse.move(700, 120);
    await page.mouse.down(); await page.mouse.up();
    await waitT(() => document.getElementById('ytc-chd').className === 'ych');
    // 2560 wide: the same fractions on whole pixels.
    await page.setViewportSize({ width: 2560, height: 1440 });
    await page.evaluate(() => scrollTo(0, 0));
    await page.waitForTimeout(200);
    checkSegs(await segGeo(), [0, 0.5, 0.75, 1]);
    await hoverSeek(0.3);
    await waitT(t => document.getElementById('ytc-tipc').textContent === t, titles[0]);
    await barShot('chapters-hover-2560');
    await page.setViewportSize({ width: 1400, height: 900 });
    await page.mouse.move(700, 880);
  });

  await step('description timecodes become chapters when next has none; invalid ones do not', async () => {
    await page.goto('https://www.youtube.com/watch?v=descchaps01', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-chap .ych li');
    assert.deepEqual(await page.$$eval('#ytc-chap li span', xs => xs.map(x => x.textContent)), ['Intro', 'Setup', 'Build https://example.com']);
    await waitT(() => document.querySelectorAll('#ytc-segs .pbar').length === 3);
    checkSegs(await segGeo(), [0, 12 / 40, 26 / 40, 1]);
    await waitT(() => document.getElementById('ytc-chl').textContent !== '');
    await hoverSeek(0.5);
    await waitT(() => document.getElementById('ytc-tipc').textContent === 'Setup' && document.getElementById('ytc-tipt').textContent === '0:20');
    await barShot('chapters-desc');
    await page.mouse.move(700, 880);
    await page.goto('https://www.youtube.com/watch?v=descnochap1', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-cm .yc-h');
    await waitT(() => / \/ 0:40$/.test(document.getElementById('ytc-time').textContent));
    await page.waitForTimeout(300);
    assert.equal(await page.locator('#ytc-chap li').count(), 0, 'no chapter list');
    assert.equal(await page.locator('#ytc-segs .pbar').count(), 1, 'one bar');
    assert.equal(await page.getAttribute('#ytc-chb', 'data-n'), '0', 'no chapter label');
    assert.equal(await page.evaluate(() => getComputedStyle(document.getElementById('ytc-chb')).display), 'none');
    // Without chapters the tooltip shows the time alone; the single bar spans the whole track.
    const g = await segGeo();
    assert.ok(Math.abs(g.segs[0].l) < 0.5 && Math.abs(g.segs[0].r - g.w) < 1, 'single bar full width');
    await hoverSeek(0.25);
    await waitT(() => document.getElementById('ytc-tipt').textContent === '0:10' && document.getElementById('ytc-tipc').textContent === '');
    assert.equal((await tipBox()).op, '1');
    // No storyboard in this WEB response: the tooltip is the time alone, no frame box.
    assert.deepEqual(await page.evaluate(() => ({ sb: document.getElementById('ytc-player').classList.contains('sb'), f: getComputedStyle(document.querySelector('.ptip .ptf')).display, h: document.getElementById('ytc-tip').getBoundingClientRect().height })), { sb: false, f: 'none', h: 24 }, 'no storyboard: unchanged tooltip');
    assert.equal(await page.locator('#ytc-segs .pbar.on').count(), 0, 'no grown segment without chapters');
    await barShot('nochapters-hover');
    await page.mouse.move(700, 880);
  });

  await step('description: only resource lines (bare links labelled), no hashtags/timestamps/prose, views/date on top, fully open, 12px below the channel row, Block beside links', async () => {
    const shots = async tag => {
      if (!process.env.YTC_SHOTS) return;
      for (const [w, h] of [[1400, 900], [2560, 1440]]) {
        await page.setViewportSize({ width: w, height: h });
        await page.waitForTimeout(150);
        await page.evaluate(() => window.scrollTo(0, 0));
        const r = await page.locator('#ytc-info').boundingBox();
        await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `desc-${tag}-${w}.png`), fullPage: true, clip: { x: 0, y: Math.max(0, r.y - 60), width: w, height: r.height + 120 }, animations: 'disabled' });
      }
      await page.setViewportSize({ width: 1400, height: 900 });
      await page.evaluate(() => window.scrollTo(0, 0));
    };
    for (const [id, tag] of [['descRULES01', 'rules'], ['kCc8FmEb1nY', 'real']]) {
      await page.goto('https://www.youtube.com/watch?v=' + id, { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-desc:not(.sk)');
      await shots(tag);
    }
    await page.goto('https://www.youtube.com/watch?v=descRULES01', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-desc:not(.sk)');
    const d = await page.evaluate(() => {
      const desc = document.getElementById('ytc-desc'), cs = getComputedStyle(desc);
      const rows = [...desc.querySelectorAll('.dr')].map(r => { const c = r.cloneNode(true); c.querySelectorAll('.fx-blk').forEach(b => b.remove()); return c.textContent.replace(/\s+/g, ' ').trim(); });
      const ws = desc.firstElementChild, spans = [...ws.children];
      const wo = document.querySelector('#ytc-info .wo').getBoundingClientRect(), box = desc.getBoundingClientRect();
      return {
        rows, hrefs: [...desc.querySelectorAll('.dr a')].map(a => a.getAttribute('href')), first: ws.className, meta: spans.map(s => s.textContent),
        metaGap: spans.length > 1 ? spans[1].getBoundingClientRect().left - spans[0].getBoundingClientRect().right : null,
        metaFont: [...new Set(spans.map(s => getComputedStyle(s).fontSize + '/' + getComputedStyle(s).fontWeight + '/' + getComputedStyle(s).color))],
        text: desc.textContent, seek: desc.querySelectorAll('a[data-act="seek"]').length, more: desc.querySelectorAll('button.lnk,[data-act="desc"]').length,
        clamp: desc.classList.contains('clamp'), open: desc.scrollHeight <= desc.clientHeight + 1 && cs.overflow === 'visible' && cs.maxHeight === 'none',
        gap: box.top - wo.bottom, width: box.width, blk: [...desc.querySelectorAll('.fx-blk')].map(b => b.textContent),
        tags: document.querySelectorAll('#ytc-info a[href^="/hashtag/"]').length,
      };
    });
    assert.deepEqual(d.rows, ['Join my Discord: Discord', 'Project files: Download', 'Related tutorial'], 'only resource lines, bare link labelled: ' + JSON.stringify(d.rows));
    assert.deepEqual(d.hrefs, ['https://discord.example/join', 'https://files.example/dl', '/watch?v=aqz-KE-bpKQ'], 'real destinations');
    assert.ok(!/#|0:30|Intro prose|Thanks|opening/.test(d.text), 'no hashtags, timestamps or prose: ' + d.text);
    assert.equal(d.seek, 0, 'no timestamp links'); assert.equal(d.tags, 0, 'no hashtag links on the watch info');
    assert.equal(d.first, 'ws', 'views/date row first'); assert.equal(d.meta.length, 2); assert.ok(/views/.test(d.meta[0]) && d.meta[1].length > 0, JSON.stringify(d.meta));
    assert.ok(d.metaGap >= 8, 'views and date visibly separated: ' + d.metaGap); assert.equal(d.metaFont.length, 1, 'one typography: ' + d.metaFont);
    assert.ok(!d.more && !d.clamp && d.open, 'fully open, no Show more');
    assert.ok(Math.abs(d.gap - 12) <= 1, '12px below the channel/actions row: ' + d.gap);
    // Spans the column like the title row and the chapter list (no ragged right edge between panels).
    assert.ok(Math.abs(d.width - await page.evaluate(() => document.getElementById('ytc-info').getBoundingClientRect().width)) < 1, 'description spans the column: ' + d.width);
    // Hidden Block buttons take no room: the text after a link continues right after it.
    const gap = await page.evaluate(() => { const a = [...document.querySelectorAll('#ytc-desc a')].find(x => x.nextElementSibling?.classList.contains('fx-blk') && x.nextElementSibling.nextSibling?.textContent?.trim()); if (!a) return null; const r = document.createRange(); r.setStart(a.nextElementSibling.nextSibling, 0); r.setEnd(a.nextElementSibling.nextSibling, 1); const t = r.getBoundingClientRect(), l = a.getBoundingClientRect(); return Math.round(t.left - l.right); });
    assert.ok(gap === null || gap < 8, 'no hole after a link: ' + gap);
    assert.deepEqual(d.blk, ['Block discord.example', 'Block files.example'], 'Block beside external links');
    await page.hover('#ytc-desc .dr a[href="https://files.example/dl"]');
    await page.waitForFunction(() => getComputedStyle(document.querySelectorAll('#ytc-desc .fx-blk')[1]).opacity === '1');
    // A description without any resource keeps just views/date.
    await page.goto('https://www.youtube.com/watch?v=descNONE001', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-desc:not(.sk)');
    const none = await page.evaluate(() => { const d = document.getElementById('ytc-desc'); return { kids: [...d.children].map(c => c.className), visible: d.getBoundingClientRect().height > 0, text: d.textContent }; });
    assert.deepEqual(none.kids, ['ws'], 'views/date only: ' + JSON.stringify(none));
    assert.ok(none.visible && /views/.test(none.text) && !/MOON|today/.test(none.text));
  });

  await step('back to a watch page keeps its comments (memory) and the actions work', async () => {
    await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-cm .yc-h');
    await page.locator('#ytc-cm').scrollIntoViewIfNeeded();
    await page.locator('#ytc-cm .yc-sort button', { hasText: 'Newest' }).click();
    await page.waitForFunction(() => document.querySelector('#ytc-cm-list > .ycm .yca')?.textContent === '@newest-e2e');
    await page.evaluate(() => document.querySelector('#ytc-list a.t[href^="/watch"]').click());
    await page.waitForFunction(() => new URL(location.href).searchParams.get('v') !== '-Bxc4gJluC0');
    // (Every fixture video shares one comments token: let this page's comments load first.)
    await page.waitForSelector('#ytc-cm .yc-h');
    await page.locator('#ytc-cm').scrollIntoViewIfNeeded();
    await page.waitForSelector('#ytc-cm-list > .ycm');
    const before = calls('next').filter(a => a.body.continuation === T_FIRST || a.body.continuation === T_NEW).length;
    await page.evaluate(() => history.back());
    await page.waitForFunction(() => new URL(location.href).searchParams.get('v') === '-Bxc4gJluC0' && document.querySelector('#ytc-cm-list > .ycm'));
    assert.equal(await page.locator('#ytc-cm-list > .ycm .yca').first().textContent(), '@newest-e2e', 'restored list');
    assert.equal(calls('next').filter(a => a.body.continuation === T_FIRST || a.body.continuation === T_NEW).length, before, 'no refetch');
    await page.locator('#ytc-cm .yc-sort button', { hasText: 'Top' }).click();
    await page.waitForFunction(() => document.querySelector('#ytc-cm-list > .ycm .yca')?.textContent === '@EdutainmentGames');
  });

  await step('signed out: Like asks to sign in, sends nothing; no bell/avatar', async () => {
    log.api.length = 0;
    await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-like');
    assert.equal(await page.locator('#ytc-like-n').textContent(), '7.4K');
    await page.click('#ytc-like');
    assert.match(await page.locator('#ytc-act-msg').textContent(), /Sign in to rate videos/);
    assert.equal(await page.getAttribute('#ytc-like', 'aria-pressed'), 'false');
    assert.equal(calls('like/like').length, 0);
    assert.equal(await page.locator('#ytc-acct button').count(), 0);
  });

  await ctx.addCookies([{ name: 'SAPISID', value: 'e2e-sapisid', domain: '.youtube.com', path: '/', secure: true }]);
  ACTS.signedIn = true;
  await step('like/dislike: optimistic, exact requests, rollback with reason on failure', async () => {
    log.api.length = 0;
    await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-like');
    ACTS.delay = 500;
    await page.click('#ytc-like');
    assert.equal(await page.getAttribute('#ytc-like', 'aria-pressed'), 'true', 'pressed before the response');
    await page.waitForFunction(() => document.getElementById('ytc-like').className === 'ya-b on');
    const l = calls('like/like')[0];
    assert.equal(l.body.target.videoId, '-Bxc4gJluC0');
    assert.ok(l.body.params?.startsWith('Cg0KCy1CeGM0'), 'likeParams from the page');
    assert.ok(l.headers.authorization?.startsWith('SAPISIDHASH '), 'signed request');
    ACTS.fail.add('like/dislike');
    await page.click('#ytc-dis');
    assert.equal(await page.getAttribute('#ytc-dis', 'aria-pressed'), 'true');
    assert.equal(await page.getAttribute('#ytc-like', 'aria-pressed'), 'false');
    await page.waitForFunction(() => document.getElementById('ytc-like').getAttribute('aria-pressed') === 'true', null, { timeout: 3000 });
    assert.equal(await page.getAttribute('#ytc-dis', 'aria-pressed'), 'false', 'rolled back to the confirmed Like');
    assert.match(await page.locator('#ytc-act-msg').textContent(), /Rating not saved\. YouTube is unavailable \(HTTP 500\): Backend Error/);
    assert.equal(calls('like/dislike')[0].body.target.videoId, '-Bxc4gJluC0');
    for (let i = 0; i < 50 && !incidents.some(x => x.kind === 'action-like'); i++) await page.waitForTimeout(100);
    const inc = incidents.find(x => x.kind === 'action-like');
    assert.ok(inc && /rating -Bxc4gJluC0: YouTube is unavailable \(HTTP 500\)/.test(inc.reason), 'debug-center incident: ' + JSON.stringify(inc));
    assert.ok(!JSON.stringify(inc).includes('e2e-sapisid'), 'no cookie in the report');
    ACTS.fail.clear();
    await page.click('#ytc-like');
    await page.waitForFunction(() => document.getElementById('ytc-like').className === 'ya-b');
    assert.equal(calls('like/removelike').length, 1);
    ACTS.delay = 0;
  });

  await step('hedged reads: a stalled search / next is sent once more at its deadline, identically; the page renders from the copy, the stalled request is aborted; a slow Like is never sent twice', async () => {
    const failed = [], from = incidents.length;
    const onFail = r => { if (r.url().includes('/youtubei/v1/')) failed.push({ url: r.url(), err: r.failure()?.errorText || '', post: r.postData() || '' }); };
    page.on('requestfailed', onFail);
    try {
      log.api.length = 0;
      HEDGE.stall = { ep: 'search', q: 'hedge stall', n: 1 };
      const t0 = Date.now();
      await page.goto('https://www.youtube.com/results?search_query=hedge+stall', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]', { timeout: 5000 });
      const ms = Date.now() - t0;
      const sent = calls('search').filter(a => a.body.query === 'hedge stall');
      assert.equal(sent.length, 2, 'the stalled search went out once more (not more)');
      assert.equal(JSON.stringify(sent[1].body), JSON.stringify(sent[0].body), 'identical body (same context, visitor, query)');
      assert.equal(sent[1].headers.authorization, sent[0].headers.authorization, 'same session');
      const gap = sent[1].t - sent[0].t;
      assert.ok(gap >= 1000 && gap < 1700, `copy at the search deadline (1.2 s): ${gap} ms`);
      assert.ok(ms < 2500, `rendered from the copy: ${ms} ms`);
      for (let i = 0; i < 30 && !failed.some(f => f.post.includes('hedge stall')); i++) await page.waitForTimeout(100);
      assert.ok(failed.some(f => f.post.includes('hedge stall') && /ABORTED/.test(f.err)), 'the stalled request was aborted: ' + JSON.stringify(failed.map(f => f.err)));
      // A watch page whose `next` stalls: the copy fills it.
      log.api.length = 0;
      HEDGE.stall = { ep: 'next', v: '-Bxc4gJluC0', n: 1 };
      await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-like', { timeout: 5000 });
      const nx = calls('next').filter(a => a.body.videoId === '-Bxc4gJluC0' && !a.body.continuation && !a.url.includes('fields=contents%2F'));
      assert.equal(nx.length, 2, 'next hedged once');
      assert.equal(JSON.stringify(nx[1].body), JSON.stringify(nx[0].body));
      // Home's first page belongs to the Home path (src/app/homeready.rs), which sends its own single hedge:
      // the generic hedger adds nothing on top (no ready response in this check).
      log.api.length = 0;
      await readyDel();
      await page.evaluate(() => { localStorage.setItem('ytc-test-noready', '1'); });
      HEDGE.stall = { ep: 'browse', home: true, n: 1 };
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForTimeout(3200);
      assert.equal(calls('browse').filter(a => a.body.browseId === 'FEwhat_to_watch' && !a.body.continuation).length, 2, 'Home browse: the first request + the Home path\'s one hedge, nothing more');
      await page.evaluate(() => localStorage.removeItem('ytc-test-noready'));
      HEDGE.stall = null;
      await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-like');
      // Actions are never hedged: a Like slower than every deadline goes out once.
      log.api.length = 0;
      ACTS.delay = 2600;
      await page.click('#ytc-like');
      await page.waitForTimeout(3200);
      assert.equal(calls('like/like').length + calls('like/removelike').length, 1, 'one rating request');
      ACTS.delay = 0;
      if (DEV_BUILD) {
        await page.goto('https://www.youtube.com/results?search_query=after+hedge', { waitUntil: 'commit' });
        for (let i = 0; i < 50 && !telBatches(from).some(b => b.events.some(e => e.m === 'req.next' && e.hedge_fired)); i++) await page.waitForTimeout(100);
        const ev = telBatches(from).flatMap(b => b.events);
        const nev = ev.find(e => e.m === 'req.next' && e.hedge_fired);
        assert.ok(nev && nev.hedge_won === true && nev.path === 'hedge' && nev.status === 200 && nev.v >= 1000, 'req.next records the hedge: ' + JSON.stringify(nev));
        const nav = ev.find(e => e.m === 'nav.watch' && e.reqs?.some(r => r.hedge_won));
        assert.ok(nav, 'the navigation breakdown names the hedged request');
      }
    } finally {
      HEDGE.stall = null; ACTS.delay = 0; page.off('requestfailed', onFail);
      // The next steps start on this watch page.
      await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-sub button');
    }
  });

  await step('subscribe/unsubscribe: channelIds, deliberate unsubscribe, rollback on failure', async () => {
    log.api.length = 0;
    await page.click('#ytc-sub button');
    assert.equal((await page.locator('#ytc-sub button').textContent()), 'Subscribed');
    await page.waitForFunction(() => !document.querySelector('#ytc-sub .busy'));
    assert.deepEqual(calls('subscription/subscribe')[0].body.channelIds, ['UCaZO-66UcqyVT9btiqdDryA']);
    await page.click('#ytc-sub button');
    assert.equal(await page.locator('#ytc-sub button').textContent(), 'Unsubscribe?');
    assert.equal(calls('subscription/unsubscribe').length, 0, 'first click only asks');
    await page.click('#ytc-sub button');
    await page.waitForFunction(() => document.querySelector('#ytc-sub button').textContent === 'Subscribe' && !document.querySelector('#ytc-sub .busy'));
    assert.deepEqual(calls('subscription/unsubscribe')[0].body.channelIds, ['UCaZO-66UcqyVT9btiqdDryA']);
    ACTS.fail.add('subscription/subscribe');
    await page.click('#ytc-sub button');
    await page.waitForFunction(() => document.querySelector('#ytc-sub button').textContent === 'Subscribe' && !document.querySelector('#ytc-sub .busy'));
    assert.match(await page.locator('#ytc-act-msg').textContent(), /Subscription not saved/);
    ACTS.fail.clear();
  });

  await step('save: hover opens after intent, hover selects nothing, toggles/create/rollback, closes on leave', async () => {
    // Signed in, the membership is asked once quietly after the page settled: the bookmark is filled
    // (the accent) because the video is in Watch later, without opening the chooser.
    await page.waitForFunction(() => document.getElementById('ytc-save-b')?.classList.contains('saved'), null, { timeout: 4000 });
    assert.equal(await page.locator('#ytc-save-l').textContent(), 'Saved');
    assert.equal(await page.evaluate(() => getComputedStyle(document.querySelector('#ytc-save-b .ya-bm svg.f')).fill), 'rgb(241, 241, 241)', 'accent fill');
    log.api.length = 0;
    await page.mouse.move(5, 5);
    await page.hover('#ytc-save button[data-act="a-save"]');
    await page.waitForSelector('#ytc-save-p .ya-opt');
    assert.equal(calls('playlist/get_add_to_playlist').length, 0, 'membership from the quiet check, no new request');
    assert.equal(await page.getAttribute('#ytc-pl-0', 'aria-checked'), 'true', 'WL contains the video');
    assert.equal(await page.getAttribute('#ytc-pl-1', 'aria-checked'), 'false');
    assert.ok((await page.locator('#ytc-pl-1').textContent()).includes('<img src=x'), 'title escaped');
    await page.hover('#ytc-pl-1'); await page.hover('#ytc-pl-0'); await page.waitForTimeout(150);
    assert.equal(calls('browse/edit_playlist').length, 0, 'hover selects nothing');
    await page.click('#ytc-pl-1');
    assert.equal(await page.getAttribute('#ytc-pl-1', 'aria-checked'), 'true');
    assert.ok((await page.evaluate(() => document.querySelector('#ytc-pl-1 .ya-ck svg').getAnimations().map(a => a.transitionProperty))).includes('stroke-dashoffset'), 'the check mark draws itself in');
    await page.waitForFunction(() => !document.querySelector('#ytc-save-p .busy'));
    assert.deepEqual(calls('browse/edit_playlist')[0].body.actions, [{ addedVideoId: '-Bxc4gJluC0', action: 'ACTION_ADD_VIDEO' }]);
    assert.equal(calls('browse/edit_playlist')[0].body.playlistId, 'PLe2e');
    ACTS.fail.add('browse/edit_playlist'); ACTS.delay = 400;
    await page.click('#ytc-pl-0');
    assert.equal(await page.getAttribute('#ytc-pl-0', 'aria-checked'), 'false', 'optimistic remove');
    await page.waitForFunction(() => document.getElementById('ytc-pl-0').getAttribute('aria-checked') === 'true');
    assert.match(await page.locator('#ytc-save-m').textContent(), /Removing not saved/);
    for (let i = 0; i < 50 && !incidents.some(x => x.kind === 'action-save'); i++) await page.waitForTimeout(100);
    assert.ok(incidents.some(x => x.kind === 'action-save' && /edit_playlist remove -Bxc4gJluC0/.test(x.reason)), 'save incident');
    assert.deepEqual(calls('browse/edit_playlist')[1].body.actions, [{ action: 'ACTION_REMOVE_VIDEO_BY_VIDEO_ID', removedVideoId: '-Bxc4gJluC0' }]);
    ACTS.fail.clear(); ACTS.delay = 0;
    // Optimistic bookmark: removing the video from every playlist empties it at once (before the
    // responses), adding one back fills it again.
    ACTS.delay = 400;
    for (const i of [0, 1]) await page.click(`#ytc-pl-${i}`);
    assert.ok(!(await page.locator('#ytc-save-b').getAttribute('class')).includes('saved'), 'emptied at once');
    assert.equal(await page.locator('#ytc-save-l').textContent(), 'Save');
    await shot('save-empty');
    await page.click('#ytc-pl-1');
    assert.ok((await page.locator('#ytc-save-b').getAttribute('class')).includes('saved'), 'filled at once');
    assert.ok((await page.evaluate(() => document.querySelector('#ytc-save-b .ya-bm svg.f').getAnimations().map(a => a.transitionProperty))).includes('clip-path'), 'the fill sweeps in (pointer)');
    await page.waitForFunction(() => !document.querySelector('#ytc-save-p .busy'));
    ACTS.delay = 0;
    await page.click('#ytc-pl-0');
    await page.waitForFunction(() => !document.querySelector('#ytc-save-p .busy'));
    await page.fill('#ytc-a-newpl input', 'My list');
    await page.press('#ytc-a-newpl input', 'Enter');
    await page.waitForFunction(() => document.querySelectorAll('#ytc-save-p .ya-opt').length === 3);
    await shot('save');
    assert.deepEqual(calls('playlist/create')[0].body.videoIds, ['-Bxc4gJluC0']);
    assert.equal(calls('playlist/create')[0].body.title, 'My list');
    assert.equal(calls('playlist/create')[0].body.privacyStatus, 'PRIVATE');
    assert.equal(await page.getAttribute('#ytc-pl-0', 'aria-checked'), 'true');
    await page.mouse.move(5, 400);
    await page.waitForSelector('#ytc-save-p', { state: 'hidden', timeout: 1500 });
    // Click opens (keyboard path), Escape closes.
    await page.focus('#ytc-save button[data-act="a-save"]');
    await page.keyboard.press('Enter');
    await page.waitForSelector('#ytc-save-p');
    await page.keyboard.press('Escape');
    await page.waitForSelector('#ytc-save-p', { state: 'hidden' });
    assert.equal(calls('playlist/get_add_to_playlist').length, 0, 'membership kept in memory');
    assert.ok((await page.locator('#ytc-save-b').getAttribute('class')).includes('saved'));
    await shot('save-saved');
  });

  await step('share: youtu.be link, optional start time', async () => {
    await page.evaluate(() => { const v = document.getElementById('ytc-video'); v.pause(); v.currentTime = 15; });
    await page.waitForTimeout(100);
    await page.click('#ytc-share button[data-act="a-share"]');
    assert.equal(await page.inputValue('#ytc-share-u'), 'https://youtu.be/-Bxc4gJluC0');
    await page.click('#ytc-share-t');
    assert.match(await page.inputValue('#ytc-share-u'), /^https:\/\/youtu\.be\/-Bxc4gJluC0\?t=1[45]$/);
    await shot('share');
    await page.click('#ytc-copy');
    assert.match(await page.getAttribute('#ytc-copy', 'class'), /\bdone\b/, 'Copy morphs into its confirmation');
    assert.match(await page.locator('#ytc-copied').textContent(), /Copied|⌘C/);
    assert.match(await page.getAttribute('#ytc-copy', 'aria-label'), /copied|⌘C/);
    await page.mouse.click(5, 400);
    await page.waitForSelector('#ytc-share-p', { state: 'hidden' });
  });

  await step('notifications: count from topbar, list, More, mark read on click; account menu', async () => {
    log.api.length = 0;
    await page.waitForSelector('#ytc-acct .yac-n');
    assert.equal(await page.locator('#ytc-acct .yac-n').textContent(), '3');
    assert.equal(await page.getAttribute('#ytc-acct .yac-av img', 'src'), 'https://yt3.ggpht.com/e2e-avatar=s88');
    await page.click('#ytc-acct button[data-act="a-nt"]');
    await page.waitForSelector('#ytc-nt-p .yac-i');
    assert.equal(calls('notification/get_notification_menu')[0].body.notificationsMenuRequestType, 'NOTIFICATIONS_MENU_REQUEST_TYPE_INBOX');
    assert.equal(await page.locator('#ytc-acct .yac-n:not(.z)').count(), 0, 'badge cleared on open');
    assert.equal(await page.locator('#ytc-nt-p .yac-i').count(), 2);
    assert.equal(await page.evaluate(() => window.__xss), undefined);
    await page.click('#ytc-nt-p button[data-act="a-ntmore"]');
    await page.waitForFunction(() => document.querySelectorAll('#ytc-nt-p .yac-i').length === 3);
    assert.equal(calls('notification/get_notification_menu')[1].body.ctoken, 'CTOK');
    await shot('notifications');
    const docs = log.docs.length;
    await page.click('#ytc-nt-p .yac-i >> nth=1');
    await page.waitForSelector('#ytc-nt-p', { state: 'hidden' });
    assert.equal(calls('notification/record_interactions')[0].body.serializedRecordNotificationInteractionsRequest, 'SER2');
    assert.equal(log.docs.length, docs, 'in-app navigation');
    await page.click('#ytc-acct button[data-act="a-ac"]');
    await page.waitForFunction(() => document.querySelector('#ytc-ac-p')?.textContent.includes('E2E Viewer'));
    assert.equal(calls('account/account_menu').length, 1);
    assert.equal(await page.getAttribute('#ytc-ac-p a[data-arg="/logout"]', 'href'), '/logout');
    // Trimmed account menu: exactly these four entries, in this order.
    assert.deepEqual(await page.$$eval('#ytc-ac-p .yac-a', as => as.map(a => a.textContent.trim())), ['Google Account', 'Switch account', 'Settings', 'Sign out']);
    await shot('account-menu');
    await page.mouse.click(600, 600);
    await page.waitForSelector('#ytc-ac-p', { state: 'hidden' });
  });
  // Routing (above) disables Chromium's HTTP cache, so the cached-thumbnail promise of the
  // fade_imgs primitive is checked here: bridge.js alone in a plain browser, a local server
  // with YouTube's image cache headers.
  await step('fade_imgs: a late image fades in; a cached one (srcset, same document) is never hidden, not even for one frame', async () => {
    const http = require('node:http');
    const srv = http.createServer((req, res) => {
      if (req.url === '/') { res.writeHead(200, { 'content-type': 'text/html' }); return res.end('<!doctype html><style>img.ld{opacity:0}img.ldf{transition:opacity 220ms}</style><div id="box"></div><script src="/bridge.js"></script>'); }
      if (req.url === '/bridge.js') { res.writeHead(200, { 'content-type': 'text/javascript' }); return res.end(fs.readFileSync(path.join(__dirname, '..', 'browser', 'bridge.js'))); }
      setTimeout(() => { res.writeHead(200, { 'content-type': 'image/png', 'cache-control': 'public, max-age=7200' }); res.end(PNG); }, 250);
    }).listen(0, '127.0.0.1');
    await new Promise(r => srv.once('listening', r));
    const browser = await chromium.launch({ headless: true, ...chromiumOptions() });
    try {
      const p = await browser.newPage();
      await p.goto(`http://127.0.0.1:${srv.address().port}/`);
      const html = [1, 2, 3, 4].map(i => `<img src="/i${i}.png" srcset="/i${i}.png 320w, /j${i}.png 720w" sizes="300px" width="160" height="90">`).join('');
      // Fresh: hidden while loading (from the first frame), then faded in.
      const first = await p.evaluate(h => new Promise(r => { const box = document.getElementById('box'); box.innerHTML = h; YTC.fade_imgs('#box img'); requestAnimationFrame(() => r([...box.querySelectorAll('img')].map(x => x.className))); }), html);
      assert.deepEqual(first, ['ld', 'ld', 'ld', 'ld']);
      await p.waitForFunction(() => [...document.querySelectorAll('#box img')].every(x => x.className === 'ldf'), null, { timeout: 3000 });
      // The same images again: complete once their source is chosen, so never hidden.
      const again = await p.evaluate(h => new Promise(r => { const box = document.getElementById('box'); box.innerHTML = h; YTC.fade_imgs('#box img'); requestAnimationFrame(() => r([...box.querySelectorAll('img')].map(x => [x.className, x.complete]))); }), html);
      assert.deepEqual(again, [['', true], ['', true], ['', true], ['', true]]);
    } finally {
      await browser.close();
      srv.close();
    }
  });

  await step('motion: popovers grow from their trigger and work on the frame they open (keyboard: no motion); hover-out and release restore; late thumbnails fade in', async () => {
    // Animations slowed 20× so the checks land mid-transition deterministically.
    const slow = rate => cdp.send('Animation.setPlaybackRate', { playbackRate: rate });
    await cdp.send('Animation.enable');
    // The panel never moves (rows land where they stay); its surface (::before) morphs out of the trigger.
    const pop = id => page.evaluate(id => {
      const p = document.getElementById(id), cs = getComputedStyle(p), up = cs.getPropertyValue('--up').trim();
      return { cls: p.className, vis: cs.visibility, pe: cs.pointerEvents, op: +cs.opacity, tf: cs.transform, clip: getComputedStyle(p, '::before').clipPath, open: `inset(${up} 0px 0px round 12px)`, focus: document.activeElement && document.activeElement.id };
    }, id);
    const hits = (sel, inside) => page.evaluate(([sel, inside]) => {
      const r = document.querySelector(sel).getBoundingClientRect(), h = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      return !!h && !!h.closest(inside);
    }, [sel, inside]);
    try {
      // Share (mouse): visible, hit-testable and focused at once, while the entrance still runs.
      await page.mouse.move(5, 400);
      await slow(0.05);
      await page.click('#ytc-share-b');
      let s = await pop('ytc-share-p');
      assert.equal(s.cls, 'ya-pop open in');
      assert.deepEqual([s.vis, s.pe, s.focus, s.tf], ['visible', 'auto', 'ytc-share-u', 'none']);
      assert.ok(s.op < 1 && s.clip !== s.open, `entrance running (opacity ${s.op}, surface ${s.clip})`);
      assert.ok(await hits('#ytc-copy', '#ytc-copy'), 'Copy is clickable on the opening frame');
      if (process.env.YTC_SHOTS) { const b = await page.locator('#ytc-acts').boundingBox(); for (const [ms, n] of [[300, 1], [600, 2], [900, 3], [2000, 4]]) { await page.waitForTimeout(ms); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `motion-share-opening-${n}.png`), clip: { x: b.x + b.width - 520, y: b.y - 10, width: 540, height: 170 } }); } }
      await slow(1);
      await page.waitForTimeout(300);
      // It grows out of the trigger: the surface starts as the Share button's own box and radius.
      s = await pop('ytc-share-p');
      assert.deepEqual([s.tf, s.clip], ['none', s.open], 'settled: the full 12 px panel');
      const geo = await page.evaluate(() => {
        const p = document.getElementById('ytc-share-p'), b = document.getElementById('ytc-share-b'), v = k => parseFloat(getComputedStyle(p).getPropertyValue(k));
        return { p: p.getBoundingClientRect().toJSON(), b: b.getBoundingClientRect().toJSON(), bw: v('--bw'), bh: v('--bh'), up: v('--up'), bx: v('--bx'), br: v('--br') };
      });
      const from = { left: geo.p.right - geo.bx - geo.bw, top: geo.p.top - geo.up, right: geo.p.right - geo.bx, bottom: geo.p.top - geo.up + geo.bh };
      for (const k of ['left', 'top', 'right', 'bottom']) assert.ok(Math.abs(from[k] - geo.b[k]) <= 0.5, `morph starts at the button (${k}): ${JSON.stringify(from)} vs ${JSON.stringify(geo.b)}`);
      assert.equal(geo.br, geo.b.height / 2, 'with the button\'s radius');
      assert.ok(Math.abs(geo.p.right - geo.b.right) <= 1 && Math.abs(geo.p.top - geo.b.bottom - 6) <= 1, 'panel 6 px below, right edges aligned');
      // Outside click: no longer clickable at once; the exit (faster) finishes, then it is hidden.
      await slow(0.05);
      await page.mouse.click(5, 400);
      s = await pop('ytc-share-p');
      assert.deepEqual([s.cls, s.pe, s.vis, s.tf], ['ya-pop', 'none', 'visible', 'none']);
      assert.ok(!(await hits('#ytc-copy', '#ytc-share-p')), 'a closing panel takes no clicks');
      if (process.env.YTC_SHOTS) { await page.waitForTimeout(500); const b = await page.locator('#ytc-acts').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'motion-share-closing.png'), clip: { x: b.x + b.width - 520, y: b.y - 10, width: 540, height: 150 } }); }
      await slow(1);
      await page.waitForSelector('#ytc-share-p', { state: 'hidden', timeout: 1000 });
      // Keyboard: the Save chooser opens and closes without motion, even with animations slowed.
      await slow(0.05);
      await page.focus('#ytc-save-b');
      await page.keyboard.press('Enter');
      s = await pop('ytc-save-p');
      assert.ok(/\bopen\b/.test(s.cls) && /\bnow\b/.test(s.cls) && !/\bin\b/.test(s.cls), s.cls);
      assert.deepEqual([s.vis, s.op, s.tf, s.clip], ['visible', 1, 'none', s.open]);
      await page.keyboard.press('Escape');
      s = await pop('ytc-save-p');
      assert.deepEqual([s.vis, s.focus], ['hidden', 'ytc-save-b']);
      // Bell (mouse): a row is clickable on the opening frame; it grows from the bell.
      await page.click('#ytc-nt-b');
      s = await pop('ytc-nt-p');
      assert.deepEqual([s.cls, s.vis, s.pe, s.tf], ['ya-pop yac-p open in', 'visible', 'auto', 'none']);
      assert.ok(s.op < 1 && s.clip !== s.open, 'bell panel entrance running');
      if (process.env.YTC_SHOTS) for (const [ms, n] of [[300, 1], [600, 2], [900, 3], [2000, 4]]) { await page.waitForTimeout(ms); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `motion-bell-opening-${n}.png`), clip: { x: 900, y: 0, width: 500, height: 360 } }); }
      await page.waitForSelector('#ytc-nt-p .yac-i');
      assert.ok(await hits('#ytc-nt-p .yac-i', '#ytc-nt-p'), 'rows take clicks while it opens');
      await page.keyboard.press('Escape');
      assert.equal((await pop('ytc-nt-p')).vis, 'hidden', 'Escape closes at once');
      await slow(1);

      // Cards: hover ring at once, eased off; the press gives (.985) and the release restores.
      await page.goto('https://www.youtube.com/results?search_query=roblox+studio+animation', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c a.th img');
      const th = page.locator('#ytc-list .c a.th').nth(1);
      const ring = () => th.evaluate(el => getComputedStyle(el).outlineColor);
      await th.hover();
      assert.equal(await ring(), 'rgb(63, 63, 63)', 'ring on at once');
      await slow(0.05);
      await page.mouse.move(5, 600);
      await page.waitForTimeout(300);
      const mid = await ring();
      assert.ok(mid !== 'rgb(63, 63, 63)' && mid !== 'rgba(0, 0, 0, 0)', `ring eases off (${mid})`);
      await slow(1);
      await page.waitForFunction(() => getComputedStyle(document.querySelectorAll('#ytc-list .c a.th')[1]).outlineColor === 'rgba(0, 0, 0, 0)', null, { timeout: 1000 });
      const b = await th.boundingBox();
      await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
      await page.mouse.down();
      await page.waitForTimeout(200);
      assert.equal(await th.evaluate(el => getComputedStyle(el).transform), 'matrix(0.985, 0, 0, 0.985, 0, 0)');
      const url = page.url();
      await page.mouse.move(5, 600);
      await page.mouse.up(); // released outside: no click
      await page.waitForTimeout(250);
      assert.equal(await th.evaluate(el => getComputedStyle(el).transform), 'none', 'release restores');
      assert.equal(page.url(), url);
      // Filters panel: hover opens after the intent, leaving closes it back to its resting scale.
      await page.hover('#ytc-hfx .fx-fb');
      await page.waitForTimeout(450); // 40 ms intent + the UI spring's settle (--d-spring 351 ms)
      assert.deepEqual(await page.$eval('#ytc-hfx .fx-p', p => [getComputedStyle(p).visibility, getComputedStyle(p).transform]), ['visible', 'none']);
      await page.mouse.move(5, 600);
      await page.waitForTimeout(450);
      // Closed, it rests raised onto its button (37 px + the 6 px gap), clipped to a button-sized box: it morphs out of the button.
      assert.deepEqual(await page.$eval('#ytc-hfx .fx-p', p => [getComputedStyle(p).visibility, getComputedStyle(p).transform, /^inset\(0px 0px calc\(-37px \+ 100%\) calc\(-112px \+ 100%\) round 10px\)$|^inset\(0px 0px calc\(100% - 37px\) calc\(100% - 112px\) round 10px\)$/.test(getComputedStyle(p).clipPath) || getComputedStyle(p).clipPath]), ['hidden', 'matrix(1, 0, 0, 1, 0, -43)', true]);

      // Skeleton → results: every placeholder sits where its content lands (thumbnails, title and
      // metadata lines), so nothing below moves when the cards replace it.
      IMG.search = 700;
      await page.goto('https://www.youtube.com/results?search_query=roblox+skeleton', { waitUntil: 'commit' });
      await page.waitForSelector('.c.sk');
      const boxes = () => page.$$eval('.grid > .c', cs => cs.slice(0, 8).map(c => {
        const r = el => { const b = el.getBoundingClientRect(); return [Math.round(b.top), Math.round(b.left), Math.round(b.height)]; };
        return { th: r(c.querySelector('.th')), t: r(c.querySelector('.t')), m: [...c.querySelectorAll('.m')].map(r) };
      }));
      const sk = await boxes();
      IMG.search = 0;
      await page.waitForSelector('#ytc-list .c:not(.sk)');
      const real = await boxes();
      for (let i = 0; i < 8; i++) {
        assert.deepEqual(real[i].th, sk[i].th, `card ${i} thumbnail`);
        if (real[i].t[2] === 48) assert.deepEqual(real[i].t, sk[i].t, `card ${i} title lines`);
      }
      if (real[0].t[2] === 48 && real[0].m.length === 2) assert.deepEqual(real[0].m, sk[0].m, 'metadata lines');
      // Thumbnails: late ones are hidden until they land, then fade in; nothing is left hidden.
      IMG.delay = 300;
      await page.goto('https://www.youtube.com/results?search_query=roblox+fade', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c a.th img');
      assert.ok(await page.locator('#ytc-list img.ld').count() > 0, 'loading thumbnails wait hidden');
      if (process.env.YTC_SHOTS) { await slow(0.1); await page.waitForSelector('#ytc-list img.ldf', { timeout: 3000 }); await page.waitForTimeout(150); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'motion-thumbs-fading.png'), clip: { x: 0, y: 60, width: 1400, height: 420 } }); await slow(1); }
      // (Lazy ones below the fold load, and fade, when scrolled near.)
      await page.waitForFunction(() => !document.querySelector('#ytc-list img.ld:not([loading=lazy])'), null, { timeout: 4000 });
      assert.ok(await page.locator('#ytc-list img.ldf').count() > 0, 'late thumbnails fade');
      await page.waitForTimeout(400);
      assert.deepEqual([...new Set(await page.$$eval('#ytc-list .c a.th img', xs => xs.filter(x => x.complete).map(x => getComputedStyle(x).opacity)))], ['1']);
    } finally {
      IMG.delay = 0; IMG.search = 0;
      await slow(1).catch(() => {});
    }
  });

  // ---- chrome motion (header, search, sidebar, bell): pointer morphs, keyboard/back instant, typing untouched
  const slowAnims = rate => cdp.send('Animation.setPlaybackRate', { playbackRate: rate });
  const running = sel => page.evaluate(sel => document.getAnimations().filter(a => a.playState === 'running' && a.effect && a.effect.target && a.effect.target.closest && a.effect.target.closest(sel))
    .map(a => (a.transitionProperty || a.animationName) + (a.effect.pseudoElement || '')), sel);
  const shotAt = async (name, clip, steps) => { if (!process.env.YTC_SHOTS) return; for (const [ms, n] of steps) { await page.waitForTimeout(ms); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, `${name}-${n}.png`), clip }); } };
  await step('motion: search field widens on pointer focus (spring), the first keystroke paints on its keydown frame, the dropdown unfolds with rows clickable at once; × clears; keyboard focus and Enter are instant', async () => {
    await cdp.send('Animation.enable');
    try {
      await page.goto('https://www.youtube.com/results?search_query=roblox', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      // Recent searches for the dropdown: on the "roblox morph" page, "roblox morph lua" matches.
      for (const q of ['roblox morph lua', 'roblox morph']) { await page.fill('#ytc-q', q); await page.press('#ytc-q', 'Enter'); await page.waitForFunction(q => new URLSearchParams(location.search).get('search_query') === q, q); }
      await page.waitForSelector('#ytc-list .c');
      await page.mouse.click(5, 700);
      const box = () => page.evaluate(() => { const r = document.getElementById('ytc-q').getBoundingClientRect(); return [r.left, r.top, r.width, r.height]; });
      const q0 = await box();
      await page.evaluate(() => { window.__f = 0; const t = () => { window.__f++; requestAnimationFrame(t); }; requestAnimationFrame(t);
        const q = document.getElementById('ytc-q'); q.addEventListener('keydown', () => { window.__kd = window.__f; }, { once: true }); q.addEventListener('input', () => { window.__in = window.__f; window.__val = q.value; }, { once: true }); });
      await slowAnims(0.05);
      await page.mouse.click(q0[0] + 200, q0[1] + q0[3] / 2);
      const st = await page.evaluate(() => ({ cls: document.getElementById('ytc-search').className, sug: document.getElementById('ytc-sug').className, focus: document.activeElement.id }));
      assert.deepEqual(st, { cls: '', sug: 'sug on', focus: 'ytc-q' });
      const a1 = await running('#ytc-search');
      assert.ok(['clip-path::before', 'clip-path::after', 'translate'].every(x => a1.includes(x)), 'field morph running: ' + a1);
      assert.ok(a1.includes('ytcglance'), 'the magnifier glances: ' + a1);
      assert.ok(a1.filter(x => x === 'opacity').length >= 2 && a1.includes('clip-path::before'), 'dropdown unfolding, rows settling: ' + a1);
      assert.ok(await page.evaluate(() => { const r = document.querySelector('#ytc-sug .sg a').getBoundingClientRect(); const h = document.elementFromPoint(r.left + 40, r.top + r.height / 2); return !!h && !!h.closest('#ytc-sug .sg'); }), 'a row takes clicks on the opening frame');
      await shotAt('motion-search-focus', { x: 300, y: 0, width: 800, height: 380 }, [[100, 1], [500, 2], [1500, 3]]);
      // First keystroke: in the field on its keydown frame; the text box never moves; nothing animates the input.
      await page.keyboard.type(' ');
      const k = await page.evaluate(() => ({ kd: window.__kd, in: window.__in, val: window.__val, sug: document.getElementById('ytc-sug').className, qa: document.getElementById('ytc-q').getAnimations().length }));
      assert.ok(k.kd !== undefined && k.in === k.kd, `input painted on the keydown frame (${k.kd} → ${k.in})`);
      assert.ok(k.val.endsWith(' '), k.val);
      assert.deepEqual(await box(), q0, 'the text box did not move');
      assert.equal(k.qa, 0, 'nothing animates the input');
      assert.equal(k.sug, 'sug on now', 'typing changes the dropdown with no motion');
      assert.deepEqual((await running('#ytc-sug')).filter(x => !x.startsWith('background')), [], 'typing stopped the unfold');
      await slowAnims(1);
      await page.waitForTimeout(500);
      // Settled: the pill spans the whole form, the button flush at its right end, the dropdown under the field.
      const g = await page.evaluate(() => { const f = document.getElementById('ytc-search').getBoundingClientRect(), b = document.querySelector('#ytc-search .qs').getBoundingClientRect(), u = document.getElementById('ytc-sug').getBoundingClientRect(); return { clip: getComputedStyle(document.querySelector('#ytc-search .qf'), '::before').clipPath, br: b.right - f.right, ul: u.left - f.left, ur: b.left - u.right }; });
      assert.ok(/^inset\(0px( 0px)* round 21px\)$/.test(g.clip) && g.br === 0 && g.ul === 0 && g.ur === 0, JSON.stringify(g));
      // × empties the field, keeps it focused, recent searches come back; the × spins out.
      await page.click('#ytc-search .qx');
      assert.equal(await page.inputValue('#ytc-q'), '');
      assert.equal(await page.evaluate(() => document.activeElement.id), 'ytc-q');
      await page.waitForSelector('#ytc-sug li.hs');
      await page.waitForFunction(() => getComputedStyle(document.querySelector('#ytc-search .qx')).visibility === 'hidden', null, { timeout: 1000 });
      // Pointer blur folds the field back (eased); keyboard `/` focus is instant.
      await slowAnims(0.05);
      await page.mouse.click(5, 700);
      assert.equal(await page.getAttribute('#ytc-search', 'class'), '');
      assert.ok((await running('#ytc-search')).includes('clip-path::before'), 'blur folds the field back');
      await slowAnims(1);
      await page.waitForTimeout(500);
      await slowAnims(0.05);
      await page.keyboard.press('/');
      assert.equal(await page.evaluate(() => document.activeElement.id), 'ytc-q');
      assert.equal(await page.getAttribute('#ytc-search', 'class'), 'now');
      assert.deepEqual(await running('#ytc-search'), [], 'keyboard focus: no motion');
      await page.keyboard.type('rob');
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => /rob/.test(location.search));
      assert.deepEqual(await running('#ytc-search'), [], 'Enter: no motion');
      assert.equal(await page.locator('#ytc-sug li').count(), 0);
    } finally { await slowAnims(1).catch(() => {}); }
  });

  await step('motion: the sidebar pill slides between pages on pointer navigation (both edges on the spring, the leading one first), and moves at once for g+letter and back', async () => {
    HOME.rich = true;
    const pill = () => page.evaluate(() => { const e = document.getElementById('ytc-sdi'), cs = getComputedStyle(e); return { st: cs.getPropertyValue('--st'), sb: cs.getPropertyValue('--sb'), op: cs.opacity, dir: document.getElementById('ytc-side').dataset.dir || '', anims: e.getAnimations().map(a => a.transitionProperty).sort() }; });
    try {
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      let p = await pill();
      assert.deepEqual([p.st, p.sb, p.op, p.anims], ['0px', '220px', '1', []], 'first paint: on Home, no motion');
      await slowAnims(0.05);
      await page.click('#ytc-side a[data-nav=history]');
      p = await pill();
      assert.deepEqual([p.dir, p.anims], ['d', ['--sb', '--st']]);
      await page.waitForTimeout(1500);
      p = await pill();
      const [st, sb] = [parseFloat(p.st), parseFloat(p.sb)];
      assert.ok(st > 0 && st < 44 && sb > 176 && sb < 220 && 220 - sb > st, `mid-slide, the leading edge ahead: top ${st}, bottom ${sb}`);
      await shotAt('motion-sidebar-slide', { x: 0, y: 56, width: 260, height: 320 }, [[0, 1], [1500, 2], [3000, 3]]);
      await slowAnims(1);
      await page.waitForFunction(() => !document.getElementById('ytc-sdi').getAnimations().length, null, { timeout: 2000 });
      const r = await page.evaluate(() => { const el = document.getElementById('ytc-sdi'), e = el.getBoundingClientRect(), a = document.querySelector('#ytc-side a[data-nav=history]').getBoundingClientRect(), cs = getComputedStyle(el); return { top: e.top + parseFloat(cs.getPropertyValue('--st')) - a.top, bottom: e.bottom - parseFloat(cs.getPropertyValue('--sb')) - a.bottom, left: e.left - a.left, right: e.right - a.right }; });
      assert.deepEqual(r, { top: 0, bottom: 0, left: 0, right: 0 }, 'settled exactly on the History link');
      assert.equal(await page.$eval('#ytc-side a[data-nav=history]', a => getComputedStyle(a).color), 'rgb(241, 241, 241)');
      // Keyboard g+w, then back: instant (no transition even with animations slowed 20×).
      await slowAnims(0.05);
      await page.keyboard.press('g'); await page.keyboard.press('w');
      await page.waitForFunction(() => document.getElementById('ytc').dataset.nav === 'wl');
      p = await pill();
      assert.deepEqual([p.st, p.sb, p.anims], ['88px', '132px', []], 'g+w: instant');
      await page.evaluate(() => history.back());
      await page.waitForFunction(() => document.getElementById('ytc').dataset.nav === 'history');
      p = await pill();
      assert.deepEqual([p.st, p.anims], ['44px', []], 'back: instant');
      // A page without a link: the pill goes where it is (Enter: at once); the next pointer navigation slides from there.
      await slowAnims(1);
      await page.fill('#ytc-q', 'pill'); await page.press('#ytc-q', 'Enter');
      await page.waitForFunction(() => document.getElementById('ytc').dataset.nav === 'search');
      p = await pill();
      assert.deepEqual([p.st, p.op, p.anims], ['44px', '0', []], 'Enter: the pill leaves at once, where it was');
      await page.click('#ytc-side a[data-nav=playlists]');
      p = await pill();
      assert.deepEqual([p.dir, p.anims], ['d', ['--sb', '--st', 'opacity']]);
    } finally { HOME.rich = false; await slowAnims(1).catch(() => {}); }
  });

  await step('motion: the bell badge grows in, pulses on a new count and the bell rings once when it rises (not on first paint); it shrinks away as the list opens', async () => {
    const tb = SIGNED_TOPBAR.desktopTopbarRenderer.topbarButtons[0].notificationTopbarButtonRenderer;
    const bell = () => page.evaluate(() => ({ n: document.getElementById('ytc-nt-n').className, text: document.getElementById('ytc-nt-n').textContent, bell: document.getElementById('ytc-nt-b').className, label: document.getElementById('ytc-nt-b').getAttribute('aria-label'),
      anims: [...document.getElementById('ytc-nt-n').getAnimations(), ...document.querySelector('#ytc-nt-b svg').getAnimations()].map(a => a.animationName || a.transitionProperty).sort() }));
    HOME.rich = true;
    try {
      tb.notificationCount = 3;
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-nt-n:not(.z)');
      let b = await bell();
      assert.deepEqual([b.n, b.text, b.bell, b.anims], ['yac-n', '3', 'yac-b', []], 'first paint: still');
      tb.notificationCount = 5;
      await slowAnims(0.05);
      await page.click('#ytc-side a[data-nav=wl]');
      await page.waitForFunction(() => document.getElementById('ytc-nt-n').textContent === '5');
      b = await bell();
      assert.ok(/^yac-n p[12]$/.test(b.n) && /^yac-b r[12]$/.test(b.bell) && b.label === 'Notifications, 5 new', JSON.stringify(b));
      assert.ok(b.anims.some(x => /^ytcpulse/.test(x)) && b.anims.some(x => /^ytcring/.test(x)), 'pulse + ring: ' + b.anims);
      await shotAt('motion-bell-ring', { x: 1200, y: 0, width: 200, height: 64 }, [[200, 1], [2400, 2], [5000, 3]]);
      await slowAnims(1);
      tb.notificationCount = 0; // opening the list marks them seen on YouTube's side too
      await page.click('#ytc-nt-b');
      b = await bell();
      assert.deepEqual([b.n, b.text, b.label], ['yac-n z', '5', 'Notifications'], 'opening shrinks the badge away');
      await page.keyboard.press('Escape');
      tb.notificationCount = 2;
      await slowAnims(0.05);
      await page.click('#ytc-side a[data-nav=liked]');
      await page.waitForFunction(() => document.getElementById('ytc-nt-n').textContent === '2');
      b = await bell();
      const op = await page.$eval('#ytc-nt-n', e => [getComputedStyle(e).opacity, getComputedStyle(e).transform]);
      // Growing = the transform is mid-way from its small start (the faster opacity fade may already be done).
      const sc = /^matrix\(([\d.]+)/.exec(op[1]);
      assert.ok(b.n === 'yac-n' && /^yac-b r[12]$/.test(b.bell) && b.anims.includes('transform') && sc && +sc[1] < 1, 'grows back in: ' + JSON.stringify(b) + JSON.stringify(op));
    } finally { tb.notificationCount = 3; HOME.rich = false; await slowAnims(1).catch(() => {}); }
  });

  await step('shots: watch actions mid-motion: Copy → Copied, a checkbox drawing, the status message unfolding (YTC_SHOTS only)', async () => {
    if (!process.env.YTC_SHOTS) return;
    try {
      await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-share-b');
      await page.click('#ytc-share-b'); await page.waitForTimeout(500);
      const b = await page.locator('#ytc-acts').boundingBox(), clip = { x: b.x + b.width - 520, y: b.y - 10, width: 540, height: 170 };
      await slowAnims(0.25); // 4× slower (the revert timer runs in real time, at 1.8 s)
      await page.click('#ytc-copy');
      await shotAt('motion-copy', clip, [[120, 1], [250, 2], [600, 3]]);
      await slowAnims(1);
      await page.mouse.click(5, 400); await page.waitForTimeout(300);
      await page.click('#ytc-save-b'); await page.waitForSelector('#ytc-save-p .ya-opt'); await page.waitForTimeout(500);
      await slowAnims(0.05);
      await page.click('#ytc-pl-1');
      await shotAt('motion-check', { x: b.x + b.width - 320, y: b.y + 30, width: 330, height: 160 }, [[600, 1], [2400, 2], [5000, 3]]);
      await slowAnims(1);
      await page.mouse.click(5, 400); await page.waitForTimeout(300);
      ACTS.fail.add('like/like');
      await slowAnims(0.05);
      await page.click('#ytc-like');
      await page.waitForFunction(() => document.getElementById('ytc-act-msg').textContent);
      await shotAt('motion-msg', clip, [[100, 1], [1500, 2], [4000, 3]]);
    } finally { ACTS.fail.clear(); await slowAnims(1).catch(() => {}); }
  });

  await step('shots: header, sidebar and popovers at 1400 and 2560 (YTC_SHOTS only)', async () => {
    if (!process.env.YTC_SHOTS) return;
    const dir = process.env.YTC_SHOTS;
    HOME.rich = true;
    try {
      for (const [w, h] of [[1400, 900], [2560, 1440]]) {
        await page.setViewportSize({ width: w, height: h });
        await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
        await page.waitForSelector('#ytc-list .c');
        await page.mouse.move(w / 2, h - 5);
        await page.waitForTimeout(400);
        await page.screenshot({ path: path.join(dir, `chrome-${w}-rest.png`), clip: { x: 0, y: 0, width: w, height: 420 } });
        await page.hover('#ytc-side a[data-nav=history]'); await page.waitForTimeout(500);
        await page.screenshot({ path: path.join(dir, `chrome-${w}-side-hover.png`), clip: { x: 0, y: 56, width: 260, height: 320 } });
        await page.click('#ytc-q'); await page.waitForTimeout(600);
        await page.screenshot({ path: path.join(dir, `chrome-${w}-search-focus.png`), clip: { x: 0, y: 0, width: w, height: 420 } });
        await page.mouse.click(5, h - 5); await page.waitForTimeout(300);
        await page.click('#ytc-nt-b'); await page.waitForSelector('#ytc-nt-p .yac-i'); await page.waitForTimeout(600);
        await page.screenshot({ path: path.join(dir, `chrome-${w}-bell.png`), clip: { x: w - 560, y: 0, width: 560, height: 420 } });
        await page.click('#ytc-ac-b'); await page.waitForFunction(() => document.querySelector('#ytc-ac-p')?.textContent.includes('E2E Viewer')); await page.waitForTimeout(600);
        await page.screenshot({ path: path.join(dir, `chrome-${w}-account.png`), clip: { x: w - 560, y: 0, width: 560, height: 420 } });
        await page.mouse.click(5, h - 5); await page.waitForTimeout(300);
      }
    } finally { HOME.rich = false; await page.setViewportSize({ width: 1400, height: 900 }); }
  });
  ACTS.signedIn = false;
  await ctx.clearCookies();

  // ---- Home timing table (simulated 150 ms InnerTube latency for Home + continuations)
  if (!ONLY) {
    HOME.rich = true; HOME.latency = 150;
    const rows = [];
    for (let i = 0; i < 5; i++) {
      await page.goto('https://www.youtube.com/results?search_query=reset', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      await page.evaluate(() => sessionStorage.clear());
      await readyDel();
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForFunction(() => document.querySelectorAll('#ytc-list .c').length >= 30, null, { timeout: 10000 });
      const r = await page.evaluate(() => { const d = document.documentElement.dataset; return { script: +d.ytcScript, wasm: +d.ytcWasm, shell: +d.ytcShell, first: +d.ytcReady }; });
      await page.waitForTimeout(1500); // the user looks at the first screen
      r.preload = await page.evaluate(() => +document.documentElement.dataset.ytcPreloaded);
      const t0 = Date.now();
      await page.waitForFunction(() => { scrollBy(0, 1200); return document.querySelectorAll('#ytc-list .c').length >= 100; }, null, { timeout: 15000, polling: 50 });
      r.scroll100 = Date.now() - t0;
      await page.evaluate(() => scrollTo(0, 0));
      await page.waitForTimeout(200);
      await page.reload({ waitUntil: 'commit' });
      await page.waitForFunction(() => document.querySelectorAll('#ytc-list .c').length >= 30 && document.documentElement.dataset.ytcReady, null, { timeout: 10000 });
      await page.waitForFunction(() => { const d = document.documentElement.dataset; return d.ytcFresh || !d.ytcRepaint; }, null, { timeout: 10000 });
      Object.assign(r, await page.evaluate(() => { const d = document.documentElement.dataset; const fcp = performance.getEntriesByName('first-contentful-paint')[0]; return { reload: +d.ytcReady, rs: +d.ytcReady - +d.ytcScript, rp: fcp ? fcp.startTime - +d.ytcScript : NaN, fresh: +(d.ytcFresh || d.ytcReady), fs: +(d.ytcFresh || d.ytcReady) - +d.ytcScript }; }));
      rows.push(r);
    }
    HOME.rich = false; HOME.latency = 0;
    const hm = k => { const v = median(rows.map(r => r[k])); return Number.isNaN(v) ? 'n/a' : v.toFixed(0); };
    console.log(`\nHome timings (${EXT}), median of 5, ms; InnerTube latency simulated at 150 ms:`);
    console.log(`  content.js start ${hm('script')} · wasm ready ${hm('wasm')} · shell ${hm('shell')} · first results ${hm('first')} · 100 videos preloaded ${hm('preload')} · scroll to 100 cards ${hm('scroll100')} · reload: cards painted ${hm('reload')} (after content.js start ${hm('rs')}, first paint ${hm('rp')}) · reload: fresh feed ${hm('fresh')} (after content.js start ${hm('fs')})`);
  }
  // ---- speed table (targets A and B): slow signed-in-like Home (+1500 ms first page,
  // 150 ms continuations) and slow `next` (300 ms). Runs on any build (YTC_EXT=<old client> YTC_TIMING_ONLY=1).
  if (!ONLY || ONLY.test('speed table')) {
    const rafWait = (fn, arg) => page.evaluate(([src, arg]) => new Promise(res => {
      const f = new Function('return ' + src)()(); const t0 = performance.now(); let first = null;
      f.start(arg);
      const tick = () => { const now = performance.now() - t0; const r = f.check(arg); if (r === 1 && first === null) first = now; if (r === 2) return res({ first: first ?? now, done: now }); if (now > 9000) return res({ first, done: NaN }); requestAnimationFrame(tick); };
      requestAnimationFrame(tick);
    }), [fn.toString(), arg]);
    const homeRows = [], idleRows = [], focusRows = [];
    HOME.rich = true; HOME.stamp = true; HOME.delay = 1500; HOME.latency = 150; NEXT.latency = 300;
    for (let i = 0; i < 3; i++) {
      // A: Home seen, then other pages for > the background age, then Home again.
      await page.goto('https://www.youtube.com/results?search_query=speed' + i, { waitUntil: 'commit' });
      await page.evaluate(() => { sessionStorage.clear(); sessionStorage.setItem('ytc:bgage', '4000'); });
      await readyDel();
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForFunction(() => /Fresh #/.test(document.querySelector('#ytc-list .c .t')?.textContent || ''), null, { timeout: 10000 });
      const k0 = HOME.served;
      await page.mouse.move(700, 400);
      await page.fill('#ytc-q', 'a' + i); await page.press('#ytc-q', 'Enter');
      await page.waitForSelector('#ytc-list .c[data-v]');
      for (let w = 0; w < 9; w++) { await page.mouse.move(600 + w, 500); await page.waitForTimeout(500); }
      await page.fill('#ytc-q', 'b' + i); await page.press('#ytc-q', 'Enter');
      await page.waitForSelector('#ytc-list .c[data-v]');
      for (let w = 0; w < 6; w++) { await page.mouse.move(650 + w, 500); await page.waitForTimeout(400); }
      homeRows.push(await rafWait(() => ({ start: () => document.querySelector('a.logo').click(),
        check: k0 => { if (location.pathname !== '/') return 0; const t = document.querySelector('#ytc-list .c .t')?.textContent || ''; const m = /Fresh #(\d+)/.exec(t); return m && +m[1] > k0 ? 2 : t ? 1 : 0; } }), k0));
      // B: a results page left idle for 1.5 s, then a click on a card in view (no hover/press: element.click()).
      await page.goto('https://www.youtube.com/results?search_query=idle' + i, { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.waitForTimeout(1500);
      idleRows.push(await rafWait(() => ({ start: () => document.querySelectorAll('#ytc-list a.t[href^="/watch"]')[3].click(),
        check: () => location.pathname === '/watch' && document.querySelector('#ytc-list .c[data-v]') ? 2 : 0 }), null));
      // B: keyboard: focus a card below the first screen, Enter 150 ms later.
      await page.goto('https://www.youtube.com/results?search_query=focus' + i, { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.evaluate(() => { const as = document.querySelectorAll('#ytc-list a.t[href^="/watch"]'), a = as[Math.min(12, as.length - 1)]; a.focus({ preventScroll: true }); });
      await page.waitForTimeout(150);
      focusRows.push(await rafWait(() => ({ start: () => document.activeElement.click(), check: () => location.pathname === '/watch' && document.querySelector('#ytc-list .c[data-v]') ? 2 : 0 }), null));
    }
    HOME.rich = false; HOME.stamp = false; HOME.delay = 0; HOME.latency = 0; NEXT.latency = 0;
    await page.evaluate(() => sessionStorage.removeItem('ytc:bgage'));
    const md = (rows, k) => { const v = median(rows.map(r => r[k])); return Number.isNaN(v) ? 'n/a' : v.toFixed(0); };
    console.log(`\nSpeed table (${EXT}), median of 3, ms; Home first page +1500 ms, next +300 ms:`);
    console.log(`  A Home click → cards visible ${md(homeRows, 'first')} · → fresh Home (newer than the last visit) ${md(homeRows, 'done')}`);
    console.log(`  B results idle 1.5 s, click a card in view → related visible ${md(idleRows, 'done')} · focus a card, Enter 150 ms later → related visible ${md(focusRows, 'done')}`);
  }
  if (TIMING_ONLY) { await ctx.close(); fs.rmSync(profile, { recursive: true, force: true }); process.exit(0); }

  // ---- timings
  if (!ONLY) {
  const cold = [], spa = [];
  for (let i = 0; i < 7; i++) {
    await page.goto(`https://www.youtube.com/results?search_query=timing${i}`, { waitUntil: 'commit' });
    await page.waitForFunction(() => document.documentElement.dataset.ytcReady);
    const t = await page.evaluate(() => {
      const nav = performance.getEntriesByType('navigation')[0];
      const fcp = performance.getEntriesByName('first-contentful-paint')[0];
      const d = document.documentElement.dataset;
      return { responseStart: nav.responseStart, script: +d.ytcScript, wasm: +d.ytcWasm, shell: +d.ytcShell, fcp: fcp ? fcp.startTime : NaN, ready: +d.ytcReady };
    });
    cold.push(t);
    // SPA navigation: click a related/result video → watch rendered.
    const t0 = await page.evaluate(() => { const a = document.querySelectorAll('#ytc-list a.t[href^="/watch"]')[3]; const t = performance.now(); a.click(); return t; });
    await page.waitForFunction(t => +document.documentElement.dataset.ytcNavReady > t, t0);
    spa.push(await page.evaluate(t => +document.documentElement.dataset.ytcNavReady - t, t0));
    await page.waitForFunction(t => +document.documentElement.dataset.ytcFirstFrame > t, t0, { timeout: 8000 });
    ff.spa.push(await page.evaluate(t => +document.documentElement.dataset.ytcFirstFrame - t, t0));
  }
  await page.keyboard.press('Backquote');
  const dbg = await page.locator('#ytc-debug').textContent();
  console.log('\nLast SPA navigation, Rust marks:\n' + dbg.split('timing')[1]);
  const m = k => median(cold.map(c => c[k]))?.toFixed(1) ?? 'n/a';
  console.log('\nTimings (ms since navigation start, median of 7, fixtures served locally by Playwright):');
  console.log(`  document response start ${m('responseStart')}  content.js start ${m('script')}  wasm ready ${m('wasm')}  shell in DOM ${m('shell')}  first contentful paint ${m('fcp')}  results rendered ${m('ready')}`);
  console.log(`  SPA click → watch page rendered (not prefetched): median ${median(spa).toFixed(1)} ms`);
  console.log(`  own player first frame (fixture media, local): direct load ${ff.direct?.toFixed(0)} ms after navigation start · SPA click → first frame median ${median(ff.spa).toFixed(0)} ms`);
  }

  // ---- features: filters, topics, Watch later, blocked websites, resume/progress
  const leveldb = () => { const dir = path.join(profile, 'Default', 'Local Extension Settings'); return fs.readdirSync(dir).flatMap(x => fs.readdirSync(path.join(dir, x)).map(f => path.join(dir, x, f))).filter(f => fs.statSync(f).isFile()).map(f => fs.readFileSync(f).toString('latin1')).join(''); };
  const cardIds = () => page.$$eval('#ytc-list .c[data-v]', cs => cs.map(c => c.dataset.v));
  const durSecs = t => t.split(':').reduce((a, x) => a * 60 + +x, 0);

  await step('filters: Search minimum length hides known short videos at once (no request), remembered across reload', async () => {
    await page.mouse.move(5, 600); // not resting on a card (hover prefetch)
    await page.goto('https://www.youtube.com/results?search_query=roblox+studio+animation', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    await page.waitForSelector('#ytc-hfx .fx-fb');
    const all = await page.$$eval('#ytc-list .c', cs => cs.map(c => (c.querySelector('.th b:not(.n):not(.lv)') || {}).textContent || ''));
    // Hover opens the panel (CSS intent delay), no Live/Order/Playlists on Search.
    await page.hover('#ytc-hfx .fx-fb');
    await page.waitForFunction(() => getComputedStyle(document.querySelector('#ytc-hfx .fx-p')).visibility === 'visible');
    assert.equal(await page.locator('#ytc-hfx [data-act=fx-live], #ytc-hfx [data-act=fx-order], #ytc-hfx [data-act=fx-pl]').count(), 0);
    const n = log.api.length;
    await page.click('#ytc-hfx [data-act=fx-dur][data-arg="20"]');
    await page.waitForFunction(() => document.querySelector('#ytc-fx-dur-20')?.getAttribute('aria-pressed') === 'true');
    const kept = await page.$$eval('#ytc-list .c', cs => cs.map(c => (c.querySelector('.th b:not(.n):not(.lv)') || {}).textContent || ''));
    const short = all.filter(d => /^\d+:\d\d$/.test(d) && durSecs(d) < 1200).length;
    assert.ok(short > 0, 'fixture has short videos');
    assert.equal(kept.length, all.length - short, `kept ${kept.length} of ${all.length}`);
    assert.ok(kept.every(d => !/^\d+(:\d\d){1,2}$/.test(d) || durSecs(d) >= 1200), kept.join());
    // A display filter never re-queries YouTube (filling the freed screen space with the next page and hover/idle prefetch are normal).
    assert.deepEqual(log.api.slice(n).filter(a => a.ep === 'search' && !a.body.continuation), [], 'no new search for a display filter');
    assert.equal((await page.locator('#ytc-fx-note').textContent()) || '', '', 'filters work silently (no hidden-count note)');
    // Slider: custom value, presets follow.
    await page.$eval('#ytc-hfx input[type=range]', el => { el.value = '35'; el.dispatchEvent(new Event('input', { bubbles: true })); });
    assert.equal(await page.locator('#ytc-fx-min').textContent(), '35+ min');
    await page.click('#ytc-hfx [data-act=fx-dur][data-arg="20"]');
    await page.waitForTimeout(600); // saved after 400 ms
    await page.reload({ waitUntil: 'commit' });
    await page.waitForSelector('#ytc-hfx .fx-fb small');
    assert.equal(await page.locator('#ytc-hfx .fx-fb small').textContent(), '20+ min');
    const after = await page.$$eval('#ytc-list .c', cs => cs.map(c => (c.querySelector('.th b:not(.n):not(.lv)') || {}).textContent || ''));
    assert.ok(after.length > 0 && after.every(d => !/^\d+(:\d\d){1,2}$/.test(d) || durSecs(d) >= 1200), 'filtered right after reload: ' + after.join());
    assert.ok(leveldb().includes('yourTubeMinimumHomeDurationV1'));
    // "Any" in the (hover-open) panel resets it.
    await page.hover('#ytc-hfx .fx-fb');
    await page.click('#ytc-hfx [data-act=fx-dur][data-arg="0"]');
    // Every video is back (more may have loaded meanwhile to fill the space the filter freed).
    await page.waitForFunction(n => document.querySelectorAll('#ytc-list .c').length >= n, all.length);
  });

  await step('search filters: YouTube\'s own groups (Duration merged), hover prefetches + opens nothing, click re-queries with YouTube\'s params, chip removes, keyboard', async () => {
    const searches = () => log.api.filter(a => a.ep === 'search' && !a.body.continuation);
    await page.goto('https://www.youtube.com/results?search_query=roblox+studio+animation', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-hfx #ytc-fx-sf-Thisyear', { state: 'attached' });
    await page.hover('#ytc-hfx .fx-fb');
    await page.waitForFunction(() => getComputedStyle(document.querySelector('#ytc-hfx .fx-p')).visibility === 'visible');
    const groups = await page.$$eval('#ytc-hfx .fx-p > .fx-row[role=group]', rs => rs.map(r => r.getAttribute('aria-label')));
    assert.deepEqual(groups.filter(g => !/^Minimum/.test(g)).slice(0, 5), ['Type', 'Duration', 'Upload date', 'Features', 'Prioritize']);
    assert.equal(groups.indexOf(groups.find(g => /^Minimum/.test(g))), 2, 'shared minimum length sits inside the Duration section');
    assert.equal(await page.locator('#ytc-hfx [data-act=fx-dur]').count(), 5, 'one length row');
    assert.equal(await page.locator('#ytc-hfx .fx-p a.fx-ch', { hasText: /^Shorts$/ }).count(), 0, 'no Shorts type');
    assert.equal(await page.locator('#ytc-hfx .fx-row[aria-label=Features] .fx-ch').count(), 11);
    // Hover an option: prefetch after the intent delay, nothing applied.
    const n = searches().length, url = page.url();
    await page.hover('#ytc-fx-sf-Thisyear');
    for (let i = 0; i < 20 && searches().length === n; i++) await page.waitForTimeout(25);
    assert.equal(searches().length, n + 1, 'hover prefetch');
    assert.equal(searches()[n].body.params, 'EgIIBQ%3D%3D', "YouTube's params verbatim");
    assert.equal(page.url(), url, 'hover applies nothing');
    await page.click('#ytc-fx-sf-Thisyear');
    await page.waitForSelector('#ytc-filters .fx-act #ytc-fx-sx-Thisyear', { state: 'attached' });
    assert.match(page.url(), /&sp=EgIIBQ%253D%253D$/);
    assert.equal(searches().length, n + 1, 'click reused the hover prefetch');
    assert.equal(await page.locator('#ytc-fx-sf-Thisyear').getAttribute('aria-current'), 'true');
    if (process.env.YTC_SHOTS) { await page.hover('#ytc-hfx .fx-fb'); await page.waitForTimeout(300); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'search-filters.png') }); }
    assert.equal(await page.locator('#ytc-fx-sf-Channels').count(), 0, 'disabled Channels is not a link');
    assert.ok(await page.locator('#ytc-list .c').count() > 5);
    // One click on the chip removes it.
    await page.mouse.move(5, 600);
    await page.click('#ytc-fx-sx-Thisyear');
    await page.waitForFunction(() => !document.querySelector('#ytc-filters .fx-act') && !location.search.includes('sp='));
    assert.equal(searches().at(-1).body.params, undefined);
    // Keyboard: Tab into the panel, Enter re-queries, focus returns to the same option.
    await page.mouse.move(5, 600);
    await page.focus('#ytc-hfx .fx-fb');
    for (let i = 0; i < 30; i++) { await page.keyboard.press('Tab'); if (await page.evaluate(() => document.activeElement?.id) === 'ytc-fx-sf-Thisweek') break; }
    await page.keyboard.press('Enter');
    await page.waitForFunction(() => location.search.includes('sp=EgIIAw') && document.activeElement?.id === 'ytc-fx-sf-Thisweek');
    assert.equal(searches().at(-1).body.params, 'EgIIAw%3D%3D');
    assert.equal(await page.evaluate(() => getComputedStyle(document.querySelector('#ytc-hfx .fx-p')).visibility), 'visible', 'panel stays open for the next choice');
    await page.keyboard.press('Escape');
    await page.evaluate(() => document.activeElement?.blur());
  });

  await step('filters: Home popularity order (all loaded cards), Relevance restores; Music/Live labelled cards hidden by default; Playlists only', async () => {
    HOME.rich = true; FX.homeLabels = true;
    // The ready Home fill (Home + HOMECONT1, tested on its own) stays out of this zero-request window.
    await page.evaluate(() => { localStorage.setItem('ytc-test-noready', '1'); sessionStorage.clear(); });
    try {
    await readyDel(true);
    await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    await page.waitForFunction(() => +document.documentElement.dataset.ytcItems >= 100, null, { timeout: 8000 });
    let ids = await cardIds();
    assert.ok(!ids.includes(homeLabeled.music), 'Music-labelled video hidden (Music off by default)');
    assert.ok(!ids.includes(homeLabeled.live), 'live video hidden (Live off by default)');
    // The Home Filters switches sit on one line, inside the panel.
    const sw = await page.evaluate(() => { const p = document.querySelector('#ytc-hfx .fx-p').getBoundingClientRect(); const xs = [...document.querySelectorAll('#ytc-hfx .fx-sw')].map(e => e.getBoundingClientRect()); return { n: xs.length, rows: new Set(xs.map(r => Math.round(r.top))).size, inside: xs.every(r => r.left >= p.left && r.right <= p.right) }; });
    assert.ok(sw.n >= 4 && sw.rows === 1 && sw.inside, 'switches on one line: ' + JSON.stringify(sw));
    if (process.env.YTC_SHOTS) { await page.hover('#ytc-hfx .fx-fb'); await page.waitForTimeout(300); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'home-filters.png') }); await page.mouse.move(5, 600); }
    const native = ids.slice(0, 8);
    const n = log.api.length;
    await page.hover('#ytc-hfx .fx-fb');
    await page.click('#ytc-hfx [data-act=fx-order][data-arg=popular]');
    await page.waitForFunction(() => document.querySelector('#ytc-hfx [data-act=fx-order][data-arg=popular]')?.getAttribute('aria-pressed') === 'true');
    const views = await page.$$eval('#ytc-list .c .m', ms => ms.map(m => m.textContent).filter(t => /views/.test(t)).map(t => { const m = t.match(/([\d.,]+)\s*([KMB]?) views/); return m ? parseFloat(m[1].replace(/,/g, '')) * ({ K: 1e3, M: 1e6, B: 1e9 }[m[2]] || 1) : -1; }));
    assert.ok(views.length > 10);
    for (let i = 1; i < views.length; i++) assert.ok(views[i] <= views[i - 1], `sorted by views at ${i}: ${views.slice(0, i + 1)}`);
    // A re-sort brings other cards into view, and cards in view get their speculative
    // `next`/VISIONOS prefetch in idle time (by design); the order itself fetches no feed.
    { const feeds = log.api.slice(n).filter(a => a.ep === 'browse' || a.ep === 'search').map(a => a.ep + ' ' + (a.body?.continuation || a.body?.browseId || '') + (a.doc ? ' doc' : '')); assert.deepEqual(feeds, [], 'no feed request for ordering: ' + JSON.stringify(feeds)); }
    await page.click('#ytc-hfx [data-act=fx-order][data-arg=recommended]');
    await page.waitForFunction(first => document.querySelector('#ytc-list .c')?.dataset.v === first, native[0]);
    assert.deepEqual((await cardIds()).slice(0, 8), native, 'Relevance restores the native order');
    // Music on → the labelled video returns.
    await page.click('#ytc-hfx [data-act=fx-music]');
    await page.waitForFunction(id => !!document.querySelector(`#ytc-list .c[data-v="${id}"]`), homeLabeled.music);
    await page.click('#ytc-hfx [data-act=fx-music]');
    // Playlists only: no playlists loaded → empty with a Show all reset.
    await page.click('#ytc-hfx [data-act=fx-pl]');
    await page.waitForFunction(() => document.querySelectorAll('#ytc-list .c').length === 0);
    assert.match(await page.locator('#ytc-fx-note').textContent(), /No videos match your filters/);
    await page.mouse.move(5, 600);
    await page.click('#ytc-fx-note [data-act=fx-pl]');
    await page.waitForFunction(() => document.querySelectorAll('#ytc-list .c').length > 20);
    await page.mouse.move(5, 600);
    } finally {
      FX.homeLabels = false;
      await page.evaluate(() => localStorage.removeItem('ytc-test-noready')).catch(() => {});
    }
  });

  await step('filters: Shorts switch (off by default) shows/hides Home Shorts at once, no request; Shorts badge, normal watch link, kept at any minimum length; remembered', async () => {
    const wasShorts = FX.homeShorts;
    const noReadyBefore = await page.evaluate(() => localStorage.getItem('ytc-test-noready'));
    HOME.rich = true; FX.homeShorts = true;
    try {
      // Ready Home refills independently after the shown batch (Home + HOMECONT1). Keep that
      // separately tested prefetch out of this display-only switch's zero-request window.
      await page.evaluate(() => { localStorage.setItem('ytc-test-noready', '1'); sessionStorage.clear(); });
      await readyDel(true);
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      await page.waitForFunction(() => +document.documentElement.dataset.ytcItems >= 100, null, { timeout: 8000 });
      const shortIds = () => cardIds().then(ids => ids.filter(id => homeShorts.ids.includes(id)));
      const items = await page.evaluate(() => +document.documentElement.dataset.ytcItems);
      assert.deepEqual(await shortIds(), [], 'Shorts hidden by default');
      assert.equal(await page.locator('#ytc-list .th b', { hasText: /^Shorts$/ }).count(), 0);
      await page.hover('#ytc-hfx .fx-fb');
      await page.waitForFunction(() => getComputedStyle(document.querySelector('#ytc-fx-f .fx-p')).visibility === 'visible');
      assert.equal(await page.locator('#ytc-fx-shorts').getAttribute('aria-checked'), 'false');
      const n = log.api.length;
      await page.click('#ytc-hfx [data-act=fx-shorts]');
      await page.waitForFunction(id => !!document.querySelector(`#ytc-list .c[data-v="${id}"]`), homeShorts.ids[0]);
      assert.equal(await page.locator('#ytc-fx-shorts').getAttribute('aria-checked'), 'true');
      assert.deepEqual(await shortIds(), homeShorts.ids, 'all Shorts of the shelf, in native order');
      const ids = await cardIds();
      assert.equal(ids.indexOf(homeShorts.ids[0]), 2, 'in the shelf\'s place, after the first two videos');
      const card = page.locator(`#ytc-list .c[data-v="${homeShorts.ids[0]}"]`);
      assert.equal(await card.locator('.th b').textContent(), 'Shorts', 'Shorts badge in place of the duration');
      assert.equal(await card.locator('a.t').textContent(), homeShorts.title);
      assert.match(await card.locator('.m').last().textContent(), /views$/);
      assert.equal(await card.locator('a.th').getAttribute('href'), `/watch?v=${homeShorts.ids[0]}`, 'opens our watch page');
      assert.deepEqual(log.api.slice(n).filter(a => a.ep === 'browse' || a.ep === 'search').map(a => ({ ep: a.ep, browseId: a.body?.browseId, continuation: a.body?.continuation, doc: a.doc })), [], 'no feed request for the switch');
      // A minimum length hides short videos but keeps the Shorts (they were asked for).
      await page.click('#ytc-hfx [data-act=fx-dur][data-arg="60"]');
      await page.waitForFunction(() => document.querySelector('#ytc-fx-dur-60')?.getAttribute('aria-pressed') === 'true');
      assert.deepEqual(await shortIds(), homeShorts.ids, 'Shorts kept at 60+ min');
      await page.click('#ytc-hfx [data-act=fx-dur][data-arg="0"]');
      // Remembered across a reload (chrome.storage, the main extension's key).
      await page.waitForTimeout(600);
      await page.reload({ waitUntil: 'commit' });
      await page.waitForFunction(id => !!document.querySelector(`#ytc-list .c[data-v="${id}"]`), homeShorts.ids[0]);
      assert.ok(leveldb().includes('shortsInFeeds'));
      // Off again: hidden at once, and Home counts as before (hidden Shorts are not loaded cards).
      await page.hover('#ytc-hfx .fx-fb');
      await page.click('#ytc-hfx [data-act=fx-shorts]');
      await page.waitForFunction(id => !document.querySelector(`#ytc-list .c[data-v="${id}"]`), homeShorts.ids[0]);
      assert.equal(await page.locator('#ytc-fx-shorts').getAttribute('aria-checked'), 'false');
      assert.deepEqual(await shortIds(), []);
      await page.waitForTimeout(600);
      await page.evaluate(() => sessionStorage.clear());
      await readyDel();
      await page.reload({ waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      await page.waitForFunction(() => +document.documentElement.dataset.ytcItems >= 100, null, { timeout: 8000 });
      assert.deepEqual(await shortIds(), [], 'off is remembered');
      assert.equal(await page.evaluate(() => +document.documentElement.dataset.ytcItems), items, 'same loaded count as with Shorts off before');
    } finally {
      FX.homeShorts = wasShorts;
      try {
        // An assertion can interrupt the test with Shorts or the 60-minute filter still on.
        // Reset through the real controls so both this page's state and its saved preferences agree.
        const reset = await page.evaluate(() => {
          let changed = false;
          const shorts = document.getElementById('ytc-fx-shorts');
          if (shorts?.getAttribute('aria-checked') === 'true') { shorts.click(); changed = true; }
          const any = document.getElementById('ytc-fx-dur-0');
          if (any && any.getAttribute('aria-pressed') !== 'true') { any.click(); changed = true; }
          return changed;
        });
        if (reset) await until(async () => {
          const p = await sw.evaluate(() => chrome.storage.local.get(['shortsInFeeds', 'yourTubeMinimumHomeDurationV1']));
          return p.shortsInFeeds !== true && !(p.yourTubeMinimumHomeDurationV1 > 0);
        }, 3000, 'Shorts test preferences reset');
        await page.mouse.move(5, 600);
      } finally {
        await page.evaluate(previous => {
          if (previous === null) localStorage.removeItem('ytc-test-noready');
          else localStorage.setItem('ytc-test-noready', previous);
        }, noReadyBefore);
      }
    }
  });

  await step('topic feeds: + Topic adds a topic; first cards from one anonymous fresh search (YouTube\'s This month + Videos params) in < 1 s; All restores Home; × removes it', async () => {
    await page.waitForSelector('#ytc-filters .fx-plus');
    const homeFirstIds = (await cardIds()).slice(0, 4);
    const n = log.api.length;
    await page.hover('#ytc-filters .fx-plus');
    await page.fill('#fx-topic-form input', 'Roblox animation');
    const t0 = Date.now();
    await page.press('#fx-topic-form input', 'Enter');
    await page.waitForFunction(() => document.querySelector('#ytc-filters [data-act=fx-topic][aria-pressed=true]')?.textContent === 'Roblox animation');
    await page.waitForFunction(() => document.querySelectorAll('#ytc-list .c').length >= 3, null, { timeout: 8000 });
    const firstMs = Date.now() - t0;
    console.log(`     topic: first cards ${firstMs} ms after Enter (mocked InnerTube)`);
    assert.ok(firstMs < 1000, `first topic cards in ${firstMs} ms`);
    assert.equal(await page.locator('#ytc-fx-note').textContent(), '', 'no "Finding…" loading text');
    const reqs = log.api.slice(n);
    const searches = reqs.filter(a => a.ep === 'search');
    const before = leveldb();
    assert.equal(searches[0].body.query, 'Roblox animation tutorial');
    assert.equal(searches[0].body.params, 'EgQIBBAB', 'This month + Videos first');
    assert.equal(searches[0].body.context.client.hl, 'en');
    assert.equal(searches[0].headers.authorization, undefined);
    assert.equal(searches[0].headers.cookie, undefined, 'anonymous (credentials omitted)');
    assert.equal(reqs.filter(a => a.ep === 'next' && a.url.includes('fields=contents%2F')).length, 0, 'no seed → related chain');
    assert.ok(reqs.filter(a => ['search', 'next'].includes(a.ep)).length <= 30, 'request bound');
    const shown = await cardIds();
    const titles = await page.locator('#ytc-list .c .t').allTextContents();
    assert.ok(titles.every(t => /roblox|moon animator|eclipse animator/i.test(t)), titles.join(' | '));
    // Stored as {version:1, topics:[…]}; the cards are not.
    await page.waitForTimeout(100);
    assert.ok(leveldb().includes('yourTubeRecommendationTopicsV1'));
    // (Fixture ids overlap with videos played earlier, whose resume records are stored.)
    const fresh = shown.find(id => !before.includes(id));
    assert.ok(!fresh || !leveldb().includes(fresh), 'topic cards not stored');
    // All → the native Home feed again, without a new Home request.
    const b = apiCount('browse');
    await page.click('#ytc-filters [data-act=fx-topic][data-arg="-1"]');
    await page.waitForFunction(first => document.querySelector('#ytc-list .c')?.dataset.v === first, homeFirstIds[0]);
    assert.equal(apiCount('browse'), b);
    // The × on the chip removes the topic (persisted).
    await page.hover('#ytc-filters [data-act=fx-topic][data-arg="0"]');
    await page.click('#ytc-fx-tx-0');
    await page.waitForFunction(() => !document.querySelector('#ytc-filters [data-act=fx-topic][data-arg="0"]'));
    await page.waitForTimeout(100);
    assert.deepEqual((await sw.evaluate(() => chrome.storage.local.get('yourTubeRecommendationTopicsV1'))).yourTubeRecommendationTopicsV1.topics, []);
    await page.mouse.move(5, 600);
    HOME.rich = false;
  });

  await step('no sideways scrolling: nothing is wider than the window (signed-in Home with chips, popovers closed)', async () => {
    const had = (await ctx.cookies('https://www.youtube.com')).find(c => c.name === 'SAPISID'), wasIn = ACTS.signedIn;
    await ctx.addCookies([{ name: 'SAPISID', value: 'e2e-sapisid', domain: '.youtube.com', path: '/', secure: true }]);
    ACTS.signedIn = true;
    try {
    HOME.rich = true; FX.homeChips = true;
    await page.evaluate(() => sessionStorage.clear());
    await readyDel(true);
    await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-filters [data-act=fx-nchip]');
    await page.waitForTimeout(300);
    // Classic scrollbars take layout width (macOS "Show scroll bars: Always"); headless hides them.
    await page.addStyleTag({ content: '::-webkit-scrollbar{width:15px;height:15px;background:#222}' });
    for (const width of [1400, 900]) {
      await page.setViewportSize({ width, height: 900 });
      await page.waitForTimeout(100);
      const r = await page.evaluate(() => {
        const vw = document.documentElement.clientWidth, sw = document.scrollingElement.scrollWidth;
        const wide = [...document.querySelectorAll('body *')].filter(e => { const b = e.getBoundingClientRect(); return b.width && b.right > vw + 0.5 && !e.closest('.fx-tc'); })
          .slice(0, 6).map(e => `${e.tagName.toLowerCase()}#${e.id}.${String(e.className).replace(/ /g, '.')} right=${Math.round(e.getBoundingClientRect().right)}`);
        scrollTo(500, scrollY);
        return { vw, iw: innerWidth, sw, x: scrollX, wide, acct: document.querySelectorAll('#ytc-acct button').length };
      });
      assert.ok(r.sw <= r.vw && r.x === 0, `page scrolls sideways at ${width}px: ` + JSON.stringify(r));
    }
    // Sidebar rhythm: its first row sits on the chip row (same top, same height); the logo's tile is centred over the icons.
    const al = await page.evaluate(() => { const r = e => e.getBoundingClientRect(); const side = r(document.querySelector('#ytc-side a')), chip = r(document.querySelector('#ytc-filters .fx-tc > .fx-ch')), logo = r(document.querySelector('#ytc .logo svg')), icon = r(document.querySelector('#ytc-side a svg'));
      return { top: +(side.top - chip.top).toFixed(1), h: +(side.height - chip.height).toFixed(1), logoX: +((logo.left + logo.width / 2) - (icon.left + icon.width / 2)).toFixed(1) }; });
    assert.deepEqual(al, { top: 0, h: 0, logoX: 0 }, 'sidebar aligned with the chips and the logo: ' + JSON.stringify(al));
    // Every icon-only button: its icon sits on the button's centre (±1 px), e.g. the search magnifier.
    const off = await page.evaluate(() => [...document.querySelectorAll('button')].filter(b => b.offsetParent && b.children.length === 1 && b.firstElementChild.tagName === 'svg' && !b.textContent.trim())
      .map(b => { const a = b.getBoundingClientRect(), i = b.firstElementChild.getBoundingClientRect(); return { id: b.id || b.className || b.getAttribute('aria-label'), dx: +((i.left + i.width / 2) - (a.left + a.width / 2)).toFixed(1), dy: +((i.top + i.height / 2) - (a.top + a.height / 2)).toFixed(1) }; })
      .filter(o => Math.abs(o.dx) > 1 || Math.abs(o.dy) > 1));
    assert.deepEqual(off, [], 'icons centred in their buttons');
    // The card's Watch later bookmark (and its round hover wash) sits inside the card, which clips its paint.
    const wl = await page.evaluate(() => { const c = document.querySelector('#ytc-list .c:has(.wl)'); if (!c) return null; const a = c.getBoundingClientRect(), b = c.querySelector('.wl').getBoundingClientRect(); return { out: Math.max(0, b.right - a.right, a.left - b.left, a.top - b.top) }; });
    assert.ok(wl && wl.out < 0.5, 'bookmark inside its card: ' + JSON.stringify(wl));
    } finally {
      await page.setViewportSize({ width: 1400, height: 900 });
      ACTS.signedIn = wasIn;
      if (had) await ctx.addCookies([had]); else await ctx.clearCookies({ name: 'SAPISID' });
    }
  });

  await step('YouTube Home chips: beside the own topics, switch by the chip\'s token, All restores without a request, reorder/hide remembered', async () => {
    HOME.rich = true; FX.homeChips = true;
    await page.evaluate(() => sessionStorage.clear());
    await readyDel(true);
    await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-filters [data-act=fx-nchip]');
    const bar = () => page.$$eval('#ytc-filters .fx-tc > .fx-ch', bs => bs.map(b => b.textContent));
    assert.deepEqual((await bar()).slice(0, 3), ['All', 'Gaming', 'Podcasts'], 'YouTube chips right after All, no duplicate All');
    // One sideways-scrolling row (never a second line), wheel scrolls it, the page never rubber-bands.
    // (The fixture has three chips: the row is squeezed to make it overflow.)
    await page.evaluate(() => { document.getElementById('ytc-fx-tc').style.maxWidth = '140px'; });
    const row = await page.evaluate(() => { const t = document.getElementById('ytc-fx-tc'), ed = document.getElementById('ytc-fx-ed');
      return { over: t.scrollWidth > t.clientWidth, tops: new Set([...t.children].map(c => c.getBoundingClientRect().top)).size, edTop: Math.round(ed.getBoundingClientRect().top - t.children[0].getBoundingClientRect().top), osb: getComputedStyle(document.documentElement).overscrollBehaviorY }; });
    assert.deepEqual(row, { over: true, tops: 1, edTop: 0, osb: 'none' }, 'chips in one row');
    await page.mouse.move((await page.locator('#ytc-fx-tc').boundingBox()).x + 60, (await page.locator('#ytc-fx-tc').boundingBox()).y + 20);
    await page.mouse.wheel(0, 300);
    await page.waitForFunction(() => document.getElementById('ytc-fx-tc').scrollLeft > 0);
    assert.equal(await page.evaluate(() => scrollY), 0, 'the wheel scrolled the chips, not the page');
    await page.mouse.move(5, 600);
    await page.evaluate(() => { document.getElementById('ytc-fx-tc').style.maxWidth = ''; });
    const allIds = (await cardIds()).slice(0, 4);
    const conts = () => log.api.filter(a => a.ep === 'browse' && /^CHIP/.test(a.body.continuation || '')).map(a => a.body.continuation);
    await page.click('#ytc-fx-nchip-gaming');
    await page.waitForFunction(() => document.querySelector('#ytc-list .c')?.dataset.v?.includes('H7z'));
    assert.deepEqual(conts(), ['CHIPGAMING'], "the chip's own continuation token");
    if (process.env.YTC_SHOTS) { await page.hover('#ytc-filters .fx-plus'); await page.waitForTimeout(300); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'home-chips.png') }); await page.mouse.move(5, 600); }
    assert.equal(await page.locator('#ytc-fx-nchip-gaming').getAttribute('aria-pressed'), 'true');
    assert.equal(await page.locator('#ytc-filters [data-act=fx-topic][data-arg="-1"]').getAttribute('aria-pressed'), 'false');
    await page.click('#ytc-fx-nchip-podcasts');
    await page.waitForFunction(() => document.querySelector('#ytc-list .c')?.dataset.v?.includes('H8z'));
    assert.equal(conts()[1], 'CHIPPOD2', "token from the current chip response's bar");
    const b = apiCount('browse');
    await page.click('#ytc-filters [data-act=fx-topic][data-arg="-1"]');
    await page.waitForFunction(first => document.querySelector('#ytc-list .c')?.dataset.v === first, allIds[0]);
    assert.equal(apiCount('browse'), b, 'All from memory');
    // Editor: move Podcasts earlier, hide Gaming; remembered across a reload.
    await page.hover('#ytc-filters .fx-plus');
    await page.waitForTimeout(300);
    const frames = () => page.evaluate(() => new Promise(r => { const out = []; const f = () => { const p = document.querySelector('#ytc-fx-ed .fx-p'), cs = getComputedStyle(p); out.push(cs.visibility[0] + (+cs.opacity).toFixed(2)); if (out.length < 12) requestAnimationFrame(f); else r(out.join(' ')); }; f(); }));
    // The editor re-renders on each change and must neither blink nor replay its entrance; hiding a
    // chip shifts it under the still pointer, so it stays pinned until the pointer leaves it.
    await page.click('#ytc-fx-nup-podcasts');
    assert.equal(await frames(), Array(12).fill('v1.00').join(' '), 'editor steady after ↑');
    await page.click('#ytc-fx-nhide-gaming');
    assert.equal(await frames(), Array(12).fill('v1.00').join(' '), 'editor steady after Hide');
    // (The rows reflow on each change (FLIP), so the click above waited for them to settle; pointer moves are
    // reported at most every 150 ms: leave a real pause before the one jump away.)
    await page.waitForTimeout(160);
    await page.mouse.move(5, 600);
    await page.waitForFunction(() => getComputedStyle(document.querySelector('#ytc-fx-ed .fx-p')).visibility === 'hidden', null, { timeout: 1000 });
    assert.deepEqual((await bar()).slice(0, 2), ['All', 'Podcasts']);
    await page.waitForTimeout(100);
    assert.ok(leveldb().includes('yourTubeNativeTopicsV1'));
    assert.ok(!leveldb().includes('CHIPGAMING') && !leveldb().includes('CHIPPOD'), 'chip tokens never stored');
    await page.reload({ waitUntil: 'commit' });
    await page.waitForSelector('#ytc-filters [data-act=fx-nchip]');
    await page.waitForFunction(() => document.querySelectorAll('#ytc-filters [data-act=fx-nchip]').length === 1);
    assert.deepEqual((await bar()).slice(0, 2), ['All', 'Podcasts']);
    // Keyboard: Show Gaming again from the editor; focus stays on the control.
    await page.focus('#ytc-fx-ed-btn');
    for (let i = 0; i < 20; i++) { await page.keyboard.press('Tab'); if (await page.evaluate(() => document.activeElement?.id) === 'ytc-fx-nhide-gaming') break; }
    await page.keyboard.press('Enter');
    await page.waitForFunction(() => document.querySelectorAll('#ytc-filters [data-act=fx-nchip]').length === 2 && document.activeElement?.id === 'ytc-fx-nhide-gaming');
    assert.deepEqual((await bar()).slice(0, 3), ['All', 'Podcasts', 'Gaming']);
    await page.evaluate(() => document.activeElement?.blur());
    // Rename (inline field in the editor): Enter saves and the chip shows the name at once (no request),
    // Escape cancels, empty restores YouTube's label; the name is stored, the feed still the chip's own.
    const ren = '#ytc-fx-ed li[data-k="gaming"] input';
    const nb = apiCount('browse');
    await page.hover('#ytc-filters .fx-plus');
    await page.waitForTimeout(300);
    await page.click(ren);
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.type('My games');
    await page.keyboard.press('Enter');
    await page.waitForFunction(() => document.getElementById('ytc-fx-nchip-gaming')?.textContent === 'My games');
    assert.deepEqual((await bar()).slice(0, 3), ['All', 'Podcasts', 'My games']);
    assert.equal(await page.inputValue(ren), 'My games');
    assert.equal(await page.evaluate(() => document.activeElement?.closest('li')?.dataset.k), 'gaming', 'focus stays on the row');
    await shot('chip-rename');
    const stored = async () => (await sw.evaluate(() => chrome.storage.local.get('yourTubeNativeTopicsV1'))).yourTubeNativeTopicsV1.topics;
    assert.ok((await stored()).some(t => t.key === 'gaming' && t.name === 'My games'), 'name stored');
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.type('Junk');
    await page.keyboard.press('Escape');
    assert.equal(await page.inputValue(ren), 'My games', 'Escape cancels');
    assert.equal(await page.locator('#ytc-fx-nchip-gaming').textContent(), 'My games');
    await page.keyboard.press('ControlOrMeta+a');
    await page.keyboard.press('Backspace');
    await page.keyboard.press('Enter');
    await page.waitForFunction(() => document.getElementById('ytc-fx-nchip-gaming')?.textContent === 'Gaming');
    assert.ok((await stored()).every(t => t.name !== 'My games'), 'empty restores the original');
    assert.equal(apiCount('browse'), nb, 'renaming asks nothing');
    await page.evaluate(() => document.activeElement?.blur());
    await page.mouse.move(5, 600);
    await sw.evaluate(() => chrome.storage.local.remove('yourTubeNativeTopicsV1'));
    HOME.rich = false; FX.homeChips = false;
  });

  await step('feed motion: chip pill slides from the old chip to the new one (pointer; keyboard instant), Filters segment pill, entrance wave only on fresh pointer loads (never back/forward/restore), a card clicks mid-wave', async () => {
    const slow = rate => cdp.send('Animation.setPlaybackRate', { playbackRate: rate });
    await cdp.send('Animation.enable');
    HOME.rich = true; FX.homeChips = true;
    const box = sel => page.$eval(sel, e => { const r = e.getBoundingClientRect(); return [r.left, r.top, r.width, r.height]; });
    // The pill copy's clip as a viewport box [x, y, w, h].
    const pill = () => page.evaluate(() => {
      const g = document.querySelector('#ytc-fx-tc > .fx-pg'); if (!g) return null;
      const m = getComputedStyle(g).clipPath.match(/inset\(([-\d.]+)px ([-\d.]+)px ([-\d.]+)px ([-\d.]+)px/); if (!m) return getComputedStyle(g).clipPath;
      const r = g.getBoundingClientRect(), [t, ri, b, l] = m.slice(1).map(Number);
      return [r.left + l, r.top + t, r.width - l - ri, r.height - t - b];
    });
    try {
      await page.evaluate(() => sessionStorage.clear());
      await readyDel(true);
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-filters [data-act=fx-nchip]');
      await page.waitForSelector('#ytc-list .c');
      await page.mouse.move(5, 600);
      const all = await box('#ytc-fx-topic--1'), gaming = await box('#ytc-fx-nchip-gaming'), pod = await box('#ytc-fx-nchip-podcasts');
      // Pointer: the pill starts on All and slides (and widens) to Gaming; the copy lies exactly on the row.
      await slow(0.02);
      await page.click('#ytc-fx-nchip-gaming');
      const p0 = await pill();
      assert.ok(Array.isArray(p0), 'pill copy present: ' + p0);
      assert.ok(Math.abs(p0[0] - all[0]) < Math.abs(p0[0] - gaming[0]) && p0[0] < gaming[0] - 2, `pill starts at All (${p0} vs All ${all}, Gaming ${gaming})`);
      const fit = await page.evaluate(() => { const a = document.querySelector('#ytc-fx-tc > .fx-pg [data-of=ytc-fx-nchip-gaming]').getBoundingClientRect(), b = document.getElementById('ytc-fx-nchip-gaming').getBoundingClientRect(); return Math.max(...['left', 'top', 'width', 'height'].map(k => Math.abs(a[k] - b[k]))); });
      assert.ok(fit < 0.5, `the copy covers the real chips to the pixel (off by ${fit})`);
      const st = await page.evaluate(() => { const r = document.getElementById('ytc-fx-nchip-gaming').getBoundingClientRect(), h = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        return { pill: document.getElementById('ytc-fx-tc').dataset.pill, pressed: document.getElementById('ytc-fx-nchip-gaming').getAttribute('aria-pressed'), hit: h && h.id }; });
      assert.deepEqual(st, { pill: '1', pressed: 'true', hit: 'ytc-fx-nchip-gaming' }, 'selection final at once; the real chip takes the clicks');
      if (process.env.YTC_SHOTS) { await slow(0.1); await page.waitForTimeout(700); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'motion-chip-pill-mid.png'), clip: { x: Math.max(0, all[0] - 20), y: all[1] - 16, width: 520, height: all[3] + 32 } }); await slow(0.02); }
      const p1 = await pill();
      // The feed answers mid-slide (the row is re-rendered): the slide carries on, it never restarts or jumps.
      await page.waitForFunction(() => document.querySelector('#ytc-list .c')?.dataset.v?.includes('H7z'));
      const p2 = await pill();
      assert.ok(Array.isArray(p2) && p2[0] >= p1[0] - 0.5 && p2[0] <= gaming[0] + 0.5, `carried over the re-rendered row (${p1} → ${p2})`);
      await slow(1);
      await page.waitForFunction(() => !document.querySelector('#ytc-fx-tc > .fx-pg') && !document.getElementById('ytc-fx-tc').dataset.pill, null, { timeout: 2000 });
      assert.equal(await page.$eval('#ytc-fx-nchip-gaming', b => getComputedStyle(b).backgroundColor), 'rgb(241, 241, 241)', 'the real chip took over');
      // Keyboard: instant (no copy, no pill state).
      await page.focus('#ytc-fx-nchip-podcasts');
      await slow(0.02);
      await page.keyboard.press('Enter');
      assert.deepEqual(await page.evaluate(() => [!!document.querySelector('#ytc-fx-tc > .fx-pg'), document.getElementById('ytc-fx-tc').dataset.pill || '', document.getElementById('ytc-fx-nchip-podcasts').getAttribute('aria-pressed'), getComputedStyle(document.getElementById('ytc-fx-nchip-podcasts')).backgroundColor]),
        [false, '', 'true', 'rgb(241, 241, 241)'], 'keyboard selection instant');
      await slow(1);
      await page.evaluate(() => document.activeElement?.blur());
      await page.waitForFunction(() => document.querySelector('#ytc-list .c')?.dataset.v?.includes('H8z'));
      // A chip's first cards enter as a wave (pointer), and the All feed from memory paints at once.
      await page.mouse.move(5, 600);
      const listAnims = () => page.evaluate(() => document.getAnimations().filter(a => a.effect?.target?.closest?.('#ytc-list') && a.playState === 'running').length);
      await page.click('#ytc-fx-topic--1');
      assert.equal(await listAnims(), 0, 'All (memory) paints at once');
      await slow(0.05);
      await page.click('#ytc-fx-nchip-gaming');
      await page.waitForFunction(() => document.querySelector('#ytc-list .c')?.dataset.v?.includes('H7z'));
      const wave = await listAnims();
      assert.ok(wave >= 5 && wave <= 15, `chip feed enters as a capped wave (${wave} cards)`);
      await slow(1);
      await page.click('#ytc-fx-topic--1');
      await page.waitForTimeout(200);

      // Filters segments: the pill slides between Any → 20+ (pointer), keyboard instant.
      await page.hover('#ytc-hfx .fx-fb');
      await page.waitForTimeout(400);
      await slow(0.02);
      await page.click('#ytc-fx-dur-20');
      const seg = await page.evaluate(() => { const row = document.getElementById('ytc-fx-durrow'), g = row.querySelector(':scope > .fx-pg'); return { pill: row.dataset.pill, copy: !!g, n: g ? g.children.length : 0 }; });
      assert.deepEqual(seg, { pill: '1', copy: true, n: 5 }, 'segment pill slides');
      await slow(1);
      await page.waitForFunction(() => !document.querySelector('#ytc-fx-durrow > .fx-pg'), null, { timeout: 2000 });
      await page.focus('#ytc-fx-dur-0');
      await page.keyboard.press('Enter');
      assert.equal(await page.evaluate(() => !!document.querySelector('#ytc-fx-durrow > .fx-pg')), false, 'keyboard segment instant');
      assert.equal(await page.getAttribute('#ytc-fx-dur-0', 'aria-pressed'), 'true');
      await page.evaluate(() => document.activeElement?.blur());
      await page.mouse.move(5, 600);
      await page.waitForTimeout(300);
      HOME.rich = false; FX.homeChips = false;

      // Entrance wave: a fresh pointer load that replaces a skeleton (a channel page); never on back/forward.
      const textAnims = () => page.evaluate(() => document.getAnimations().filter(a => a.effect?.target?.matches?.('#ytc-list .c > .d')).length);
      await page.goto('https://www.youtube.com/results?search_query=roblox+studio+animation', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c .m a');
      await page.waitForTimeout(300);
      assert.equal(await textAnims(), 0, 'no wave on the first page load');
      await slow(0.05);
      await page.click('#ytc-list .c .m a[href^="/@"]');
      await page.waitForSelector('.ch h1');
      await page.waitForSelector('#ytc-list .c:not(.sk)');
      const w = await textAnims();
      assert.ok(w >= 5 && w <= 15, `fresh pointer load: the text replaces the skeleton bars in a wave (${w})`);
      if (process.env.YTC_SHOTS) { await page.waitForTimeout(1500); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'motion-entrance-wave-mid.png'), clip: { x: 0, y: 0, width: 1400, height: 760 } }); }
      // A card clicks mid-wave: the watch page opens.
      await page.click('#ytc-list .c:nth-child(2) a.t');
      await page.waitForFunction(() => location.pathname === '/watch' && document.querySelector('#ytc-info h1.wt'));
      await slow(1);
      await page.waitForTimeout(400);
      // Back (memory restore) and forward again: no wave.
      await slow(0.05);
      await page.evaluate(() => history.back());
      await page.waitForFunction(() => document.querySelector('.ch h1') && document.querySelectorAll('#ytc-list .c').length > 5);
      assert.equal(await textAnims(), 0, 'back: restored without a wave');
      await page.evaluate(() => history.back());
      await page.waitForFunction(() => location.pathname === '/results' && document.querySelectorAll('#ytc-list .c').length > 5);
      assert.equal(await textAnims(), 0, 'back again: no wave');
      await page.evaluate(() => history.forward());
      await page.waitForFunction(() => document.querySelector('.ch h1') && document.querySelectorAll('#ytc-list .c').length > 5);
      assert.equal(await textAnims(), 0, 'forward: no wave');
      await slow(1);
    } finally {
      await slow(1);
      HOME.rich = false; FX.homeChips = false;
    }
  });

  // YTC_SHOTS only: the same states at 1400 and 2560 px (works on older builds too, for before/after), then
  // mid-animation frames with the animations slowed (new motion only; skipped where a build lacks it).
  if (process.env.YTC_SHOTS) await step('shots: feeds, chips, Filters and the bookmark at 1400 and 2560 (YTC_SHOTS)', async () => {
    const dir = process.env.YTC_SHOTS, slow = rate => cdp.send('Animation.setPlaybackRate', { playbackRate: rate });
    await cdp.send('Animation.enable');
    HOME.rich = true; FX.homeChips = true; FX.wlMock = true;
    const had = (await ctx.cookies('https://www.youtube.com')).find(c => c.name === 'SAPISID');
    await ctx.addCookies([{ name: 'SAPISID', value: 'e2eSAPISID', domain: '.youtube.com', path: '/', secure: true }]);
    const shot = (name, clip) => page.screenshot({ path: path.join(dir, name + '.png'), ...(clip ? { clip } : {}) });
    try {
      for (const w of [1400, 2560]) {
        await page.setViewportSize({ width: w, height: w === 1400 ? 900 : 1440 });
        await page.evaluate(() => sessionStorage.clear());
        await readyDel();
        await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
        await page.waitForSelector('#ytc-filters [data-act=fx-nchip]');
        await page.waitForSelector('#ytc-list .c');
        await page.click('#ytc-fx-nchip-gaming');
        await page.waitForFunction(() => document.querySelector('#ytc-list .c')?.dataset.v?.includes('H7z'));
        await page.mouse.move(5, 600);
        await page.waitForTimeout(700);
        await page.hover('#ytc-list .c:nth-child(2) a.th');
        await page.waitForTimeout(800);
        await shot(`home-hover-${w}`, { x: 0, y: 0, width: w, height: w === 1400 ? 620 : 760 });
        const card = await page.locator('#ytc-list .c:nth-child(2)').boundingBox();
        await shot(`card-hover-${w}`, { x: card.x - 12, y: card.y - 12, width: card.width + 24, height: card.height + 24 });
        await page.mouse.move(5, 600);
        await page.hover('#ytc-hfx .fx-fb');
        await page.waitForTimeout(600);
        const fp = await page.locator('#ytc-hfx .fx-p').boundingBox();
        await shot(`filters-open-${w}`, { x: fp.x - 40, y: 0, width: fp.width + 80, height: fp.y + fp.height + 50 });
        await page.mouse.move(5, 600);
        await page.waitForTimeout(400);
        await page.hover('#ytc-filters .fx-plus');
        await page.waitForTimeout(600);
        const ep = await page.locator('#ytc-fx-ed .fx-p').boundingBox();
        await shot(`topic-editor-open-${w}`, { x: Math.max(0, ep.x - 260), y: ep.y - 70, width: ep.width + 300, height: ep.height + 110 });
        await page.mouse.move(5, 600);
        await page.waitForTimeout(400);
      }
      await page.setViewportSize({ width: 1400, height: 900 });
      await page.mouse.move(5, 600);
      // Mid-animation frames (20× slower).
      const has = sel => page.evaluate(sel => !!document.querySelector(sel), sel);
      const hdr = { x: 700, y: 0, width: 700, height: 560 };
      await slow(0.05);
      await page.hover('#ytc-hfx .fx-fb');
      await page.waitForTimeout(1600);
      await shot('mid-filters-morph-a', hdr);
      await page.waitForTimeout(1600);
      await shot('mid-filters-morph-b', hdr);
      await slow(1);
      await page.waitForTimeout(300);
      if (await has('#ytc-fx-durrow')) {
        await slow(0.05);
        await page.click('#ytc-fx-dur-30');
        await page.waitForTimeout(1500);
        await shot('mid-segment-pill', hdr);
        await slow(1);
        await page.waitForTimeout(500);
        await page.click('#ytc-fx-dur-0');
      }
      await page.mouse.move(5, 600);
      await page.waitForTimeout(400);
      const card = page.locator('#ytc-list .c:nth-child(3)');
      await slow(0.05);
      await card.locator('a.th').hover();
      await page.waitForTimeout(2500);
      const cb = await card.boundingBox();
      await shot('mid-card-hover-zoom', { x: cb.x - 12, y: cb.y - 12, width: cb.width + 24, height: cb.height + 24 });
      await slow(1);
      const wl = card.locator('.wl');
      if (await wl.count()) {
        await wl.hover();
        await page.waitForTimeout(400);
        await slow(0.05);
        await wl.click();
        await page.waitForTimeout(1200);
        const b = await wl.boundingBox();
        await shot('mid-bookmark-fill-pop', { x: b.x - 30, y: b.y - 20, width: b.width + 60, height: b.height + 40 });
        await slow(1);
        await page.waitForTimeout(800);
        await shot('bookmark-saved', { x: b.x - 30, y: b.y - 20, width: b.width + 60, height: b.height + 40 });
      }
      await page.mouse.move(5, 600);
      // A YouTube chip hidden from the editor: its copy shrinks out while the row closes the gap.
      await page.hover('#ytc-filters .fx-plus');
      await page.waitForTimeout(500);
      const row = await page.locator('#ytc-filters').boundingBox();
      await slow(0.05);
      await page.click('#ytc-fx-nhide-gaming', { force: true });
      await page.waitForTimeout(1200);
      await shot('mid-chip-hide-reflow', { x: row.x, y: row.y - 10, width: 640, height: 70 });
      await slow(1);
      await page.waitForTimeout(600);
      await page.click('#ytc-fx-nhide-gaming', { force: true });
      await page.mouse.move(5, 600);
      await page.waitForTimeout(400);
      await sw.evaluate(() => chrome.storage.local.remove('yourTubeNativeTopicsV1'));
    } finally {
      await slow(1);
      await page.setViewportSize({ width: 1400, height: 900 });
      HOME.rich = false; FX.homeChips = false; FX.wlMock = false;
      if (had) await ctx.addCookies([had]); else await ctx.clearCookies({ name: 'SAPISID' });
    }
  });

  await step('sidebar hover: short passes, drag and typing never navigate; resting activates every left-side link once; click and keyboard stay immediate', async () => {
    await ctx.addCookies([{ name: 'SAPISID', value: 'e2eSAPISID', domain: '.youtube.com', path: '/', secure: true }]);
    const searchUrl = 'https://www.youtube.com/results?search_query=sidebar';
    try {
      await page.goto(searchUrl, { waitUntil: 'commit' }); await page.waitForSelector('#ytc-list .c');
      const here = page.url(), history = '#ytc-side a[data-nav=history]';
      await page.hover(history); await page.waitForTimeout(80); await page.mouse.move(700, 120); await page.waitForTimeout(300);
      assert.equal(page.url(), here, 'a short pass does not navigate');
      await page.focus('#ytc-q'); await page.hover(history); await page.waitForTimeout(350);
      assert.equal(page.url(), here, 'typing focus blocks a dwell');
      await page.locator('#ytc-q').evaluate(n => n.blur());
      await page.mouse.move(700, 120); await page.mouse.down(); await page.hover(history); await page.waitForTimeout(350); await page.mouse.up(); await page.waitForTimeout(300);
      assert.equal(page.url(), here, 'a held gesture and release do not navigate');
      for (const key of ['history', 'wl', 'subs', 'playlists', 'liked', 'home']) {
        await page.mouse.move(700, 120); await page.hover(`#ytc-side a[data-nav=${key}]`);
        await page.waitForFunction(key => document.body.dataset.nav === key, key, { timeout: 1500 });
        const url = page.url(), count = await page.evaluate(() => window.history.length);
        await page.waitForTimeout(350);
        assert.equal(page.url(), url); assert.equal(await page.evaluate(() => window.history.length), count, 'one activation per visit');
      }
      await page.mouse.move(700, 120); await page.goto(searchUrl, { waitUntil: 'commit' }); await page.waitForSelector('#ytc-list .c');
      await page.focus(history); await page.waitForTimeout(350); assert.equal(page.url(), searchUrl, 'keyboard focus does not activate');
      await page.keyboard.press('Enter'); await page.waitForFunction(() => document.body.dataset.nav === 'history');
      await page.mouse.move(700, 120); await page.click('#ytc-side a[data-nav=wl]');
      assert.equal(await page.evaluate(() => document.body.dataset.nav), 'wl', 'click does not wait for hover dwell');
    } finally { await ctx.clearCookies(); }
  });

  const startLibrary = async (newest = false) => {
    LIBRARY.on = true; LIBRARY.newest = newest; LIBRARY.ids = Array.from({ length: 60 }, (_, n) => libraryId(n));
    LIBRARY.delay = 0; LIBRARY.fail = false; LIBRARY.sortFail = false;
    await ctx.addCookies([{ name: 'SAPISID', value: 'e2eSAPISID', domain: '.youtube.com', path: '/', secure: true }]);
    await page.goto('https://www.youtube.com/playlist?list=WL', { waitUntil: 'commit' });
  };
  const endLibrary = async () => { LIBRARY.on = false; LIBRARY.delay = 0; LIBRARY.fail = false; LIBRARY.sortFail = false; await ctx.clearCookies(); };

  await step('Watch later library: heading has no owner, video-count or views metadata row', async () => {
    try {
      await startLibrary(true); await page.waitForSelector('#ytc-list .c');
      assert.equal(await page.textContent('#ytc-main h1'), 'Watch later');
      assert.equal(await page.locator('#ytc-main .sub').count(), 0, 'metadata row removed');
      assert.ok(!(await page.textContent('#ytc-main')).includes('Fixture owner'));
    } finally { await endLibrary(); }
  });

  await step('Watch later library: native newest-save sorting applies before first paint and across every continuation', async () => {
    try {
      const from = log.api.length;
      await startLibrary(); await page.waitForSelector('#ytc-list .c');
      assert.equal(await page.$eval('#ytc-list .c', c => c.dataset.v), libraryId(59), 'newest save first, regardless of upload age');
      for (let n = 0; n < 10 && await page.locator('#ytc-list .c').count() < 60; n++) { await page.evaluate(() => scrollTo(0, document.body.scrollHeight)); await page.waitForTimeout(150); }
      assert.deepEqual(await page.$$eval('#ytc-list .c', cs => cs.map(c => c.dataset.v)), Array.from({ length: 60 }, (_, n) => libraryId(59 - n)), 'global order across pages');
      const sort = log.api.slice(from).filter(a => a.ep === 'browse/edit_playlist' && a.body.actions?.[0]?.action === 'ACTION_SET_PLAYLIST_VIDEO_ORDER');
      assert.equal(sort.length, 1); assert.equal(sort[0].body.actions[0].playlistVideoOrder, 1);
      assert.match(sort[0].headers.authorization, /^SAPISIDHASH /);
      await page.reload({ waitUntil: 'commit' }); await page.waitForSelector('#ytc-list .c');
      assert.equal(await page.$eval('#ytc-list .c', c => c.dataset.v), libraryId(59));
      assert.equal(log.api.slice(from).filter(a => a.ep === 'browse/edit_playlist' && a.body.actions?.[0]?.action === 'ACTION_SET_PLAYLIST_VIDEO_ORDER').length, 1, 'already-newest does not reorder again');
    } finally { await endLibrary(); }
  });

  await step('Watch later library: unbookmark removes a card before the response; a failed removal returns it in place and retry stays removed on back and reload', async () => {
    try {
      await startLibrary(true); await page.waitForSelector('#ytc-list .c .wl');
      const id = libraryId(59), card = `#ytc-list .c[data-v="${id}"]`, button = '#ytc-wl-' + id;
      LIBRARY.delay = 700; LIBRARY.fail = true;
      await page.click(button);
      assert.equal(await page.locator(card).count(), 0, 'removed on the click before the slow server replies');
      assert.ok(LIBRARY.ids.includes(id), 'server has not answered yet');
      await page.waitForSelector(button + '.error');
      assert.equal(await page.$eval('#ytc-list .c', c => c.dataset.v), id, 'failure restores exact position');
      assert.match(await page.getAttribute(button, 'aria-label'), /Couldn’t remove.*503/);
      // A keyboard failure also keeps the next bookmark focused while the old row returns.
      await page.focus(button); await page.keyboard.press('Enter');
      assert.equal(await page.locator(card).count(), 0);
      await page.waitForSelector(button + '.error');
      assert.equal(await page.evaluate(() => document.activeElement.id), 'ytc-wl-' + libraryId(58), 'rollback preserves keyboard focus');
      LIBRARY.fail = false;
      await page.focus(button); await page.keyboard.press('Enter');
      assert.equal(await page.locator(card).count(), 0, 'keyboard removal is immediate too');
      assert.equal(await page.evaluate(() => document.activeElement.id), 'ytc-wl-' + libraryId(58), 'focus moves to the next bookmark');
      await page.waitForTimeout(850); assert.ok(!LIBRARY.ids.includes(id));
      await page.click('#ytc-side a[data-nav=history]'); await page.goBack({ waitUntil: 'commit' }); await page.waitForSelector('#ytc-list .c');
      assert.equal(await page.locator(card).count(), 0, 'back does not resurrect the removed card');
      await page.reload({ waitUntil: 'commit' }); await page.waitForSelector('#ytc-list .c');
      assert.equal(await page.locator(card).count(), 0, 'reload stays removed');
      if (process.env.YTC_SHOTS) {
        await page.mouse.move(700, 100); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'watch-later-library.png') });
      }
    } finally { await endLibrary(); }
  });

  await step('Watch later library: failed removal after navigation restores the card and retry state on Back', async () => {
    try {
      await startLibrary(true); await page.waitForSelector('#ytc-list .c .wl');
      const id = libraryId(59), button = '#ytc-wl-' + id;
      LIBRARY.delay = 700; LIBRARY.fail = true;
      await page.click(button);
      assert.equal(await page.locator(button).count(), 0);
      await page.click('#ytc-side a[data-nav=history]');
      await page.waitForTimeout(850);
      assert.ok(LIBRARY.ids.includes(id), 'failed request leaves server membership intact');
      await page.goBack({ waitUntil: 'commit' }); await page.waitForSelector('#ytc-list .c');
      assert.equal(await page.locator(button).count(), 1, 'Back restores the failed removal');
      assert.equal(await page.$eval('#ytc-list .c', c => c.dataset.v), id, 'original position restored');
      assert.match(await page.getAttribute(button, 'aria-label'), /Couldn’t remove.*503/);
    } finally { await endLibrary(); }
  });

  await step('Watch later library: re-saving a removed card through the player chooser makes it visible without a reload', async () => {
    const historyBefore = await sw.evaluate(() => chrome.storage.local.get('yourTubeHistoryV1'));
    try {
      LIBRARY.on = true; LIBRARY.newest = true; LIBRARY.ids = [libraryId(0), EVIL_ID];
      LIBRARY.delay = 0; LIBRARY.fail = false; LIBRARY.sortFail = false;
      await ctx.addCookies([{ name: 'SAPISID', value: 'e2eSAPISID', domain: '.youtube.com', path: '/', secure: true }]);
      await page.goto('https://www.youtube.com/playlist?list=WL', { waitUntil: 'commit' });
      const button = '#ytc-wl-' + EVIL_ID;
      await page.waitForSelector(button);
      const removed = page.waitForResponse(r => r.url().includes('/browse/edit_playlist') && r.request().postDataJSON()?.actions?.[0]?.removedVideoId === EVIL_ID);
      await page.click(button); await removed;
      assert.ok(!LIBRARY.ids.includes(EVIL_ID));
      await page.fill('#ytc-q', 'resave'); await page.press('#ytc-q', 'Enter');
      const card = `#ytc-list .c[data-v="${EVIL_ID}"]`;
      await page.waitForSelector(card); await page.click(card + ' a.th');
      await page.waitForSelector('#ytc-save-b'); await page.click('#ytc-save-b');
      await page.waitForSelector('#ytc-pl-0');
      assert.equal(await page.getAttribute('#ytc-pl-0', 'aria-checked'), 'false');
      await page.click('#ytc-pl-0');
      await page.waitForFunction(() => !document.querySelector('#ytc-save-p .busy'));
      assert.ok(LIBRARY.ids.includes(EVIL_ID), 'chooser confirmed the save');
      await page.goBack({ waitUntil: 'commit' }); await page.waitForSelector('#ytc-list .c');
      await page.click('#ytc-side a[data-nav=wl]'); await page.waitForSelector('#ytc-list .c');
      assert.equal(await page.locator(button).count(), 1, 're-saved card is visible in the same tab');
      assert.equal(await page.$eval('#ytc-list .c', c => c.dataset.v), EVIL_ID, 'newest save returns to the top');
    } finally {
      await endLibrary();
      await sw.evaluate(async before => { await chrome.storage.local.remove('yourTubeHistoryV1'); await chrome.storage.local.set(before); }, historyBefore);
    }
  });

  await step('Watch later library: a rejected newest-sort command still shows the list (in the order received) with a note and Try again', async () => {
    try {
      LIBRARY.on = true; LIBRARY.newest = false; LIBRARY.ids = [libraryId(0), libraryId(1)]; LIBRARY.sortFail = true;
      await ctx.addCookies([{ name: 'SAPISID', value: 'e2eSAPISID', domain: '.youtube.com', path: '/', secure: true }]);
      await page.goto('https://www.youtube.com/playlist?list=WL', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      assert.equal(await page.locator('#ytc-main .msg').count(), 0, 'never a blocked page');
      assert.deepEqual(await page.$$eval('#ytc-list .c', cs => cs.map(c => c.dataset.v)), [libraryId(0), libraryId(1)]);
      assert.match(await page.textContent('#ytc-main .sub'), /Couldn’t sort newest first/);
      LIBRARY.sortFail = false;
      await page.click('#ytc-main .sub [data-act=retry]'); await page.waitForFunction(id => document.querySelector('#ytc-list .c')?.dataset.v === id && !document.querySelector('#ytc-main .sub'), libraryId(1));
    } finally { await endLibrary(); }
  });

  await step('Watch later library in another language: the page and its cards in that language, the English menu check beside it, newest first', async () => {
    try {
      await page.goto('https://www.youtube.com/results?search_query=wl+lang', { waitUntil: 'commit' });
      await page.evaluate(() => localStorage.setItem('ytc-test-lang', 'de-DE'));
      const from = log.api.length;
      await startLibrary();
      // (The tab's saved copy of the last Watch later visit may paint first; the fresh page replaces it.)
      await page.waitForFunction(id => document.querySelector('#ytc-list .c')?.dataset.v === id, libraryId(59), { timeout: 8000 });
      const wl = log.api.slice(from).filter(a => a.ep === 'browse' && a.body.browseId === 'VLWL');
      const hl = wl.map(a => a.body.context.client.hl);
      assert.ok(hl.filter(h => h === 'en').length === 1 && hl.filter(h => h === 'de').length === 2, JSON.stringify(hl));
      const firstDe = hl.indexOf('de'), check = hl.indexOf('en');
      assert.ok(firstDe < check, 'the page goes out first, the check right beside it');
      assert.equal(log.api.slice(from).filter(a => a.ep === 'browse/edit_playlist' && a.body.actions?.[0]?.action === 'ACTION_SET_PLAYLIST_VIDEO_ORDER').length, 1);
    } finally {
      await page.evaluate(() => localStorage.removeItem('ytc-test-lang')).catch(() => {});
      await endLibrary();
    }
  });

  await step('Watch later library: removing the last card immediately shows the empty state with keyboard focus on its sidebar link', async () => {
    try {
      LIBRARY.on = true; LIBRARY.newest = true; LIBRARY.ids = [libraryId(0)]; LIBRARY.delay = 400;
      await ctx.addCookies([{ name: 'SAPISID', value: 'e2eSAPISID', domain: '.youtube.com', path: '/', secure: true }]);
      await page.goto('https://www.youtube.com/playlist?list=WL', { waitUntil: 'commit' });
      const button = '#ytc-wl-' + libraryId(0); await page.waitForSelector(button); await page.focus(button); await page.keyboard.press('Enter');
      assert.equal(await page.locator('#ytc-list .c').count(), 0);
      assert.equal(await page.textContent('#ytc-list .empty'), 'This playlist is empty.');
      assert.equal(await page.evaluate(() => document.activeElement.id), 'ytc-nav-wl');
      await page.waitForTimeout(500); assert.equal(LIBRARY.ids.length, 0);
    } finally { await endLibrary(); }
  });

  await step('Watch later (signed in, mocked): lazy saved check on hover, save/remove through browse/edit_playlist, error state with reason, keyboard', async () => {
    FX.wlMock = true;
    await ctx.addCookies([{ name: 'SAPISID', value: 'e2eSAPISID', domain: '.youtube.com', path: '/', secure: true }]);
    await page.goto('https://www.youtube.com/results?search_query=roblox+studio+animation', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c .wl');
    const v = await page.$eval('#ytc-list .c .wl', b => b.dataset.arg);
    if (process.env.YTC_SHOTS) { await page.hover(`#ytc-list .c[data-v="${v}"] .t`); await page.waitForTimeout(200); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'search-wl.png'), clip: { x: 0, y: 0, width: 1400, height: 520 } }); }
    FX.wl.add(v); // already saved on the server
    const n = log.api.length;
    await page.hover(`#ytc-wl-${v}`);
    await page.waitForFunction(id => document.getElementById('ytc-wl-' + id).classList.contains('saved'), v);
    const probe = log.api.slice(n).find(a => a.ep === 'playlist/get_add_to_playlist');
    assert.deepEqual(probe.body.videoIds, [v]);
    assert.match(probe.headers.authorization, /^SAPISIDHASH \d+_[0-9a-f]{40}/);
    await page.hover(`#ytc-wl-${v}`);
    assert.equal(log.api.slice(n).filter(a => a.ep === 'playlist/get_add_to_playlist').length, 1, 'asked once');
    // Remove, then add again with the keyboard.
    await page.click(`#ytc-wl-${v}`);
    await page.waitForFunction(id => document.getElementById('ytc-wl-' + id).className === 'ca wl', v);
    let ed = log.api.filter(a => a.ep === 'browse/edit_playlist').at(-1);
    assert.equal(ed.body.playlistId, 'WL');
    assert.deepEqual(ed.body.actions, [{ action: 'ACTION_REMOVE_VIDEO_BY_VIDEO_ID', removedVideoId: v }]);
    await page.focus(`#ytc-wl-${v}`);
    await page.keyboard.press('Enter');
    assert.ok(await page.$eval(`#ytc-wl-${v}`, b => b.classList.contains('on') || b.classList.contains('saved')), 'the bookmark fills on the click (optimistic)');
    assert.ok(!(await page.$eval(`#ytc-wl-${v}`, b => b.classList.contains('pop'))), 'a keyboard save does not pop');
    await page.waitForFunction(id => document.getElementById('ytc-wl-' + id).classList.contains('saved'), v);
    ed = log.api.filter(a => a.ep === 'browse/edit_playlist').at(-1);
    assert.deepEqual(ed.body.actions, [{ addedVideoId: v, action: 'ACTION_ADD_VIDEO' }]);
    assert.ok(ed.headers.authorization.startsWith('SAPISIDHASH '));
    assert.equal(await page.getAttribute(`#ytc-wl-${v}`, 'aria-label'), 'Saved to Watch later. Click to remove.');
    // Pointer: a remove drains the fill; a save fills it from the bottom up (clip-path) and pops once
    // (--spring-pop), the pop surviving the server's confirmation.
    await page.click(`#ytc-wl-${v}`);
    await page.waitForFunction(id => document.getElementById('ytc-wl-' + id).className === 'ca wl', v);
    await page.waitForTimeout(250);
    assert.equal(await page.$eval(`#ytc-wl-${v} svg.f`, s => getComputedStyle(s).clipPath), 'inset(100% 0px 0px)', 'drained');
    await page.click(`#ytc-wl-${v}`);
    const pop = await page.$eval(`#ytc-wl-${v}`, b => ({ cls: b.className, anims: b.getAnimations({ subtree: true }).map(a => a.animationName || a.transitionProperty).sort().join(' ') }));
    assert.ok(/\bpop\b/.test(pop.cls) && /ytcwlpop/.test(pop.anims) && /clip-path/.test(pop.anims), JSON.stringify(pop));
    await page.waitForFunction(id => document.getElementById('ytc-wl-' + id).classList.contains('saved'), v);
    assert.ok(await page.$eval(`#ytc-wl-${v}`, b => b.classList.contains('pop')), 'pop kept through the confirmation');
    await page.waitForTimeout(700);
    assert.deepEqual(await page.$eval(`#ytc-wl-${v}`, b => [getComputedStyle(b.querySelector('svg.f')).clipPath, getComputedStyle(b.querySelector('svg.o')).scale]), ['inset(0px)', 'none'], 'filled, at rest');
    // Failure keeps the reason and retries the same edit.
    FX.wlFail = true;
    await page.click(`#ytc-wl-${v}`);
    await page.waitForFunction(id => document.getElementById('ytc-wl-' + id).classList.contains('error'), v);
    assert.equal(await page.getAttribute(`#ytc-wl-${v}`, 'aria-label'), 'Couldn’t remove from Watch later: Sign in required (401). Click to retry.');
    FX.wlFail = false;
    await page.click(`#ytc-wl-${v}`);
    await page.waitForFunction(id => document.getElementById('ytc-wl-' + id).className === 'ca wl', v);
    assert.ok(!FX.wl.has(v));
    await ctx.clearCookies();
    await page.reload({ waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c');
    assert.equal(await page.locator('#ytc-list .wl').count(), 0, 'no button signed out');
    FX.wlMock = false;
  });

  await step('blocked websites: Block beside a description link; related cards whose own description links there are hidden (masked anonymous next, ≤ 2 at once); Undo', async () => {
    await page.goto('https://www.youtube.com/watch?v=blockLINK01', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-desc .fx-blk');
    assert.equal(await page.locator('#ytc-desc .fx-blk').textContent(), 'Block example.net');
    await page.waitForSelector('#ytc-list .c');
    const rel = await cardIds();
    // Cards below the first screen: the ones in view were prefetched speculatively (descriptions known).
    const linked = rel.slice(10, 13);
    FX.linked = new Set(linked);
    FX.checkDelay = 120;
    const n = log.api.length;
    await page.hover('#ytc-desc a[target=_blank]');
    await page.click('#ytc-desc .fx-blk');
    await page.waitForFunction(() => document.querySelector('#ytc-desc .fx-blk')?.textContent === 'Blocked · Undo');
    await page.waitForFunction(ids => ids.every(id => !document.querySelector(`#ytc-list .c[data-v="${id}"]`)), linked, { timeout: 10000 });
    await page.waitForFunction(n => document.querySelectorAll('#ytc-list .c').length === n, rel.length - 3, { timeout: 10000 });
    const isCheck = a => a.ep === 'next' && a.url.includes('fields=contents%2F');
    // Wait for the queue to drain (cards whose next response was already in memory need no check).
    for (let i = 0, last = -1; i < 100; i++) { const c = log.api.slice(n).filter(isCheck).length; if (c === last && !FX.checksInFlight) break; last = c; await page.waitForTimeout(300); }
    const checks = log.api.slice(n).filter(isCheck);
    assert.ok(checks.length >= 3, `checks ${checks.length}`);
    assert.equal(new Set(checks.map(c => c.body.videoId)).size, checks.length, 'each video checked once');
    assert.ok(checks.every(c => !c.headers.cookie && !c.headers.authorization));
    assert.ok(FX.checksMax <= 2, `concurrent checks ${FX.checksMax}`);
    const st = leveldb();
    assert.ok(st.includes('yourTubeBlockedWebsitesV1') && st.includes('example.net'));
    assert.ok(!st.includes('Get the plugin'), 'description text never stored');
    // Undo: the rule goes, the cards come back without new checks.
    const m = log.api.length;
    await page.click('#ytc-desc .fx-blk');
    // The linked cards come back (the taller related list may have loaded a second batch meanwhile).
    await page.waitForFunction(ids => ids.every(id => document.querySelector(`#ytc-list .c[data-v="${id}"]`)), linked);
    // Only website checks count: hover/idle prefetch of cards under the pointer is normal.
    assert.equal(log.api.slice(m).filter(isCheck).length, 0, 'verdicts re-evaluated from memory');
    FX.checkDelay = 0; FX.linked = new Set();
  });

  await step('blocked websites: appended Home batches hold back unchecked cards until their check answers; blocked cards never appear', async () => {
    const previous = { rich: HOME.rich, linked: FX.linked, checkDelay: FX.checkDelay };
    const previousRules = await sw.evaluate(() => chrome.storage.local.get('yourTubeBlockedWebsitesV1'));
    const previousMarker = await page.evaluate(() => sessionStorage.getItem('ytc-e2e-hold'));
    try {
      // Block example.net again (from the description), then open a rich Home.
      await page.goto('https://www.youtube.com/watch?v=blockLINK01', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-desc .fx-blk');
      await page.hover('#ytc-desc a[target=_blank]');
      await page.click('#ytc-desc .fx-blk');
      await page.waitForFunction(() => document.querySelector('#ytc-desc .fx-blk')?.textContent === 'Blocked · Undo');
      HOME.rich = true;
      // Every video of continuation pages 1–2 links to the blocked site.
      FX.linked = new Set([...ids11(homeConts[0]), ...ids11(homeConts[1])]);
      FX.checkDelay = 150; FX.homeMax = 0;
      // From the very first frame of the next document: was any blocked card ever displayed?
      if (!holdWatch) { holdWatch = true; await page.addInitScript(() => {
        // (about:blank, between steps, has no sessionStorage.)
        try { if (!sessionStorage.getItem('ytc-e2e-hold')) return; } catch { return; }
        const linked = JSON.parse(sessionStorage.getItem('ytc-e2e-hold')), seen = window.__hold = { flashed: [], frames: 0, inserted: [], active: true };
        const inserted = new Set();
        const card = c => { const id = c.dataset.v; if (!inserted.has(id)) { inserted.add(id); seen.inserted.push(id); } };
        const collect = node => {
          if (node.nodeType !== 1) return;
          if (node.matches('.c[data-v]')) card(node);
          node.querySelectorAll('.c[data-v]').forEach(card);
        };
        // Count insertion, including cards removed by a later verdict. The live survivor count can
        // decrease while scrolling and cannot tell whether two chunks were appended.
        const observer = window.__holdObserver = new MutationObserver(records => {
          for (const record of records) for (const node of record.addedNodes) {
            if (record.target.nodeType === 1 && record.target.closest('#ytc-list')) collect(node);
            else if (node.nodeType === 1) {
              if (node.id === 'ytc-list') collect(node);
              node.querySelectorAll('#ytc-list').forEach(collect);
            }
          }
        });
        observer.observe(document, { childList: true, subtree: true });
        document.querySelectorAll('#ytc-list').forEach(collect);
        // Keep the original paint-by-paint assertion independent of insertion accounting.
        const f = () => { if (!seen.active) return; seen.frames++; for (const id of linked) { const c = document.querySelector(`#ytc-list .c[data-v="${id}"]`); if (c && c.getClientRects().length && !seen.flashed.includes(id)) seen.flashed.push(id); } requestAnimationFrame(f); };
        requestAnimationFrame(f);
      }); }
      await page.evaluate(linked => { sessionStorage.clear(); sessionStorage.setItem('ytc-e2e-hold', JSON.stringify(linked)); }, [...FX.linked]);
      await readyDel();
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c');
      const first = await page.$$eval('#ytc-list .c:not(.fxh)', cs => cs.length);
      await page.waitForFunction(() => +document.documentElement.dataset.ytcItems >= 100, null, { timeout: 8000 });
      // Scroll fast: two more chunks are appended right away.
      const before = await page.evaluate(() => window.__hold.inserted.length);
      await page.waitForFunction(n => { scrollBy(0, 2000); return window.__hold.inserted.length >= n + 40; }, before, { timeout: 8000, polling: 50 });
      try { await page.waitForFunction(() => !document.querySelector('#ytc-list .c.fxh'), null, { timeout: 20000 }); }
      catch (e) {
        await page.keyboard.press('Backquote');
        console.log('     still held: ' + await page.$$eval('#ytc-list .c.fxh', cs => cs.map(c => c.dataset.v).join(' ')) + '\n     ' + (await page.locator('#ytc-debug').textContent()).split('\n').find(l => l.includes('websites ·')));
        throw e;
      }
      const h = await page.evaluate(() => { sessionStorage.removeItem('ytc-e2e-hold'); return window.__hold; });
      assert.ok(h.frames > 10, 'frames sampled');
      assert.deepEqual(h.flashed, [], 'no blocked card was ever displayed');
      const left = await cardIds();
      assert.ok(left.every(id => !FX.linked.has(id)), 'blocked cards removed');
      assert.ok(FX.homeMax <= 2, `concurrent checks ${FX.homeMax}`);
      await page.keyboard.press('Backquote');
      const dbg = (await page.locator('#ytc-debug').textContent()).split('\n').find(l => l.includes('websites ·')) || '';
      await page.keyboard.press('Backquote');
      assert.match(dbg, /held back \d+ \(shown \d+, blocked [1-9]\d*, timed out 0, waiting 0\)/);
      console.log(`     first paint ${first} visible cards · ${h.inserted.length - before} unique cards appended · ${left.length} surviving cards · ${dbg.trim()}`);
      // Undo the rule; restore.
      await page.goto('https://www.youtube.com/watch?v=blockLINK01', { waitUntil: 'commit' });
      await page.waitForFunction(() => document.querySelector('#ytc-desc .fx-blk')?.textContent === 'Blocked · Undo');
      await page.click('#ytc-desc .fx-blk');
      await page.waitForFunction(() => document.querySelector('#ytc-desc .fx-blk')?.textContent.startsWith('Block '));
    } finally {
      FX.checkDelay = previous.checkDelay; FX.linked = previous.linked; HOME.rich = previous.rich;
      try {
        await page.evaluate(marker => {
          if (window.__hold) window.__hold.active = false;
          window.__holdObserver?.disconnect();
          if (marker === null) sessionStorage.removeItem('ytc-e2e-hold');
          else sessionStorage.setItem('ytc-e2e-hold', marker);
        }, previousMarker);
      } finally {
        await sw.evaluate(async saved => {
          if ('yourTubeBlockedWebsitesV1' in saved) await chrome.storage.local.set(saved);
          else await chrome.storage.local.remove('yourTubeBlockedWebsitesV1');
        }, previousRules);
      }
    }
  });

  await step('resume: the player reports its position; a card shows the local progress bar; reopening resumes there; stored ≤ 2,000 records', async () => {
    await page.goto('https://www.youtube.com/results?search_query=resume+test', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c a.t[href^="/watch"]');
    const v = await page.$eval('#ytc-list .c[data-v]', c => c.dataset.v);
    await page.click(`#ytc-list .c[data-v="${v}"] a.t`);
    await page.waitForFunction(() => { const x = document.querySelector('#ytc-video'); return x && x.currentTime >= 7; }, null, { timeout: 15000 });
    await page.keyboard.press('k'); // pause → position reported
    await page.waitForFunction(() => document.querySelector('#ytc-video').paused);
    const t = await page.$eval('#ytc-video', x => x.currentTime);
    await page.goBack({ waitUntil: 'commit' });
    await page.waitForSelector(`#ytc-list .c[data-v="${v}"] .th i.pg.own`);
    const w = await page.$eval(`#ytc-list .c[data-v="${v}"] .th i.pg.own`, i => parseFloat(i.style.width));
    assert.ok(Math.abs(w - t / SECONDS * 100) < 6, `bar ${w}% for ${t}s of ${SECONDS}s`);
    await page.click(`#ytc-list .c[data-v="${v}"] a.t`);
    await page.waitForFunction(() => +document.documentElement.dataset.ytcFirstFrame > 0 && document.querySelector('#ytc-video')?.readyState >= 2, null, { timeout: 15000 });
    const start = await page.$eval('#ytc-video', x => x.currentTime);
    assert.ok(start >= t - 1, `resumed at ${start}, saved ${t}`);
    await page.keyboard.press('k');
    await page.waitForTimeout(100);
    assert.ok(leveldb().includes('yourTubeProgressV1'));
  });

  await step('watch playlist (list=): panel from next, current row, near-end prefetch, end → next item at once, click jumps; Autoplay countdown, Cancel/Play now, Off remembered', async () => {
    const PL = 'PL_pGtHsOvhtxX3WHo-Hi268L78XBziem_';
    const items = JSON.parse(fx('next_list.json')).contents.twoColumnWatchNextResults.playlist.playlist.contents.map(c => c.playlistPanelVideoRenderer.videoId);
    const nexts = () => log.api.filter(a => a.ep === 'next' && !a.body.continuation && !a.url.includes('fields=contents%2F'));
    await page.goto(`https://www.youtube.com/watch?v=${items[0]}&list=${PL}`, { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-pl .pli.cur');
    assert.ok(nexts().some(a => a.body.videoId === items[0] && a.body.playlistId === PL), 'next asks with the playlist');
    assert.match(await page.locator('#ytc-pl .plm').textContent(), /1 \/ 6$/);
    assert.equal(await page.locator('#ytc-pl .pli').count(), 6);
    assert.equal(await page.locator('#ytc-up-auto').getAttribute('aria-checked'), 'true', 'Autoplay on by default');
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    // Near the end (a position report with ≤ 45 s left): the next item's next + VISIONOS, once.
    const n0 = nexts().length;
    await nearEnd(); await keys('k');
    for (let i = 0; i < 40 && !nexts().slice(n0).some(a => a.body.videoId === items[1]); i++) await page.waitForTimeout(50);
    const pre = nexts().slice(n0).filter(a => a.body.videoId === items[1]);
    assert.equal(pre.length, 1, 'next of the next item prefetched once');
    assert.equal(pre[0].body.playlistId, PL);
    assert.ok(log.api.some(a => a.ep === 'player' && a.body.videoId === items[1] && a.body.context.client.clientName === 'VISIONOS'), 'VISIONOS prefetched');
    // End → the next playlist item at once (no countdown), from the prefetch.
    await keys('k');
    await page.waitForFunction(v => location.search.startsWith(`?v=${v}&list=`), items[1], { timeout: 10000 });
    await page.waitForFunction(() => /2 \/ 6$/.test(document.querySelector('#ytc-pl .plm')?.textContent || ''));
    assert.equal(nexts().slice(n0).filter(a => a.body.videoId === items[1]).length, 1, 'the prefetched next was used');
    assert.equal(await page.locator('#ytc-pl .pli.cur .pln').textContent(), '▶');
    if (process.env.YTC_SHOTS) await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'watch-playlist.png') });
    // A row click jumps there.
    const plMark = page.evaluate(() => new Promise(res => {
      const t0 = performance.now();
      const f = () => {
        if (performance.now() - t0 > 8000) return res(null);
        if (!/4 \/ 6$/.test(document.querySelector('#ytc-pl .plm')?.textContent || '')) return requestAnimationFrame(f);
        requestAnimationFrame(() => requestAnimationFrame(() => { const m = document.querySelector('#ytc-pll > i.plk'); res({ ok: !!m, run: m ? m.getAnimations().map(a => a.transitionProperty) : [] }); }));
      };
      requestAnimationFrame(f);
    }));
    await page.click(`#ytc-pl .pli[href*="v=${items[3]}"]`);
    await page.waitForFunction(() => /4 \/ 6$/.test(document.querySelector('#ytc-pl .plm')?.textContent || ''));
    const plm = await plMark;
    assert.ok(plm && plm.ok && plm.run.includes('clip-path'), 'the current-row mark springs from the previous row ' + JSON.stringify(plm));
    await page.waitForTimeout(600);
    const plOn = await page.evaluate(() => { const s = document.getElementById('ytc-pll'), m = s.querySelector(':scope > i.plk'), b = s.querySelector('.pli.cur'), a = s.getBoundingClientRect(), r = b.getBoundingClientRect(), v = (c => { const v = /inset\(([^)]*?)(?: round [^)]*)?\)/.exec(c)[1].trim().split(/\s+/).map(parseFloat); return [v[0], v[1] ?? v[0], v[2] ?? v[0], v[3] ?? v[1] ?? v[0]]; })(getComputedStyle(m).clipPath); return { dt: v[0] - (r.top - a.top + s.scrollTop), dh: (parseFloat(m.style.height) - v[0] - v[2]) - r.height, bg: getComputedStyle(b).backgroundColor }; });
    assert.ok(Math.abs(plOn.dt) < 0.6 && Math.abs(plOn.dh) < 0.6 && plOn.bg === 'rgba(0, 0, 0, 0)', 'the mark lands on the new current row ' + JSON.stringify(plOn));
    assert.ok(page.url().includes(`v=${items[3]}&list=${PL}`));
    // Without a playlist: Autoplay counts down (YouTube's countDownSecs), Cancel stays.
    await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-up-auto');
    assert.equal(await page.locator('#ytc-pl .pl').count(), 0, 'no panel without list=');
    // The Autoplay track ends on the same line as the related cards' bookmark circles (design check, 2 Nov 2026).
    await page.waitForSelector('#ytc-list .c', { state: 'attached' });
    // (the bookmark circle sits flush with its card's right edge, signed in; here the card edge itself)
    const edge = await page.evaluate(() => { const t = document.querySelector('#ytc-up-auto i').getBoundingClientRect(), c = document.querySelector('#ytc-list .c'), w = (c.querySelector('.wl') || c).getBoundingClientRect(); return +(t.right - w.right).toFixed(1); });
    assert.ok(Math.abs(edge) <= 0.5, 'Autoplay track and bookmark share the right edge: ' + edge);
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await nearEnd();
    await page.waitForSelector('#ytc-up .upc', { timeout: 10000 });
    assert.match(await page.locator('#ytc-up .upc').textContent(), /in 5 s/);
    if (process.env.YTC_SHOTS) await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'watch-autoplay.png') });
    if (process.env.YTC_SHOTS) { await page.waitForTimeout(2000); const b = await page.locator('#ytc-up .upc').boundingBox(); await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'upnext-playnow-filling.png'), clip: { x: b.x - 8, y: b.y - 8, width: b.width + 16, height: b.height + 16 } }); }
    await page.click('#ytc-up [data-act=up-cancel]');
    assert.equal(await page.locator('#ytc-up .upc').count(), 0);
    // Play now.
    await nearEnd(); await keys('k');
    await page.waitForSelector('#ytc-up .upc', { timeout: 10000 });
    await page.click('#ytc-up-now');
    await page.waitForFunction(() => location.search === '?v=UURYhAVph5g');
    // Autoplay Off: remembered in ytcConfigV1 (ap:false); the end then stays.
    await page.click('#ytc-up-auto');
    assert.equal(await page.locator('#ytc-up-auto').getAttribute('aria-checked'), 'false');
    await page.waitForTimeout(100);
    assert.equal((await sw.evaluate(() => chrome.storage.local.get('ytcConfigV1'))).ytcConfigV1.ap, false);
    await page.reload({ waitUntil: 'commit' });
    await page.waitForFunction(() => document.getElementById('ytc-up-auto')?.getAttribute('aria-checked') === 'false');
    await waitT(() => document.documentElement.dataset.ytcFirstFrame, null, 8000);
    await nearEnd();
    await waitT(() => document.getElementById('ytc-video').ended, null, 10000);
    await page.waitForTimeout(300);
    assert.equal(await page.locator('#ytc-up .upc').count(), 0);
    assert.equal(new URL(page.url()).search, '?v=UURYhAVph5g');
    await page.click('#ytc-up-auto');
    await page.waitForTimeout(100);
  });

  // YTC_AUDIT=dir: layout audit screenshots (Home with chips signed in, search + Filters, watch page
  // with worst-case data, popovers, player menus, playlist) at 1400×900, 2560×1440 and 900×900.
  // Not a check: skipped unless asked, so the suite's count does not change.
  if (process.env.YTC_AUDIT && !TIMING_ONLY) {
    const dir = process.env.YTC_AUDIT; fs.mkdirSync(dir, { recursive: true });
    const ap = await ctx.newPage();
    const snap = (w, n) => ap.screenshot({ path: path.join(dir, `${w}-${n}.png`), animations: 'disabled' });
    const settle = () => ap.waitForTimeout(350);
    AUDIT.worst = true; ACTS.signedIn = true;
    try {
      for (const [w, h] of [[1400, 900], [2560, 1440], [900, 900]]) {
        await ap.setViewportSize({ width: w, height: h });
        HOME.rich = true; FX.homeChips = true;
        await ap.evaluate(() => sessionStorage.clear()).catch(() => {});
        await ap.goto('https://www.youtube.com/', { waitUntil: 'commit' });
        await ap.waitForSelector('#ytc-list .c[data-v]'); await settle(); await snap(w, 'home');
        HOME.rich = false; FX.homeChips = false;
        await ap.goto('https://www.youtube.com/results?search_query=worst', { waitUntil: 'commit' });
        await ap.waitForSelector('#ytc-list .c[data-v]'); await settle(); await snap(w, 'search');
        await ap.hover('#ytc-hfx .fx-fb'); await settle(); await snap(w, 'search-filters'); await ap.mouse.move(5, h - 5);
        await ap.goto(`https://www.youtube.com/watch?v=${WORST_ID}`, { waitUntil: 'commit' });
        await ap.waitForSelector('#ytc-list .c[data-v]'); await ap.waitForSelector('#ytc-acts .ya-b'); await settle();
        await ap.evaluate(() => { const v = document.getElementById('ytc-video'); if (v) v.pause(); });
        await snap(w, 'watch');
        for (const [b, n] of [['#ytc-save-b', 'save'], ['#ytc-share-b', 'share'], ['#ytc-nt-b', 'bell'], ['#ytc-ac-b', 'account']]) {
          if (!(await ap.locator(b).count())) continue;
          await ap.click(b); await ap.waitForTimeout(700); await snap(w, n); await ap.keyboard.press('Escape'); await ap.mouse.click(5, h - 5); await ap.waitForTimeout(200);
        }
        for (const [b, n] of [['#ytc-speed', 'menu-speed'], ['#ytc-qual', 'menu-quality'], ['#ytc-ccb', 'menu-cc']]) {
          await ap.click(b); await ap.waitForTimeout(300); await snap(w, n); await ap.click(b); await ap.waitForTimeout(200);
        }
        await ap.evaluate(() => scrollTo(0, document.getElementById('ytc-comments').getBoundingClientRect().top + scrollY - 200));
        await ap.waitForSelector('#ytc-comments .ycm', { timeout: 5000 }).catch(() => {}); await settle(); await snap(w, 'comments');
        const PL = 'PL_pGtHsOvhtxX3WHo-Hi268L78XBziem_';
        const items = JSON.parse(fx('next_list.json')).contents.twoColumnWatchNextResults.playlist.playlist.contents.map(c => c.playlistPanelVideoRenderer.videoId);
        await ap.goto(`https://www.youtube.com/watch?v=${items[0]}&list=${PL}`, { waitUntil: 'commit' });
        await ap.waitForSelector('#ytc-pl .pli.cur'); await settle();
        await ap.evaluate(() => { const v = document.getElementById('ytc-video'); if (v) v.pause(); });
        await snap(w, 'watch-playlist');
        await ap.goto(`https://www.youtube.com/playlist?list=${PL}`, { waitUntil: 'commit' });
        await ap.waitForSelector('#ytc-list .c'); await settle(); await snap(w, 'playlist');
      }
    } finally { AUDIT.worst = false; ACTS.signedIn = false; HOME.rich = false; FX.homeChips = false; await ap.close(); }
    console.log(`     audit screenshots in ${dir}`);
  }


  // ---- Voice search (2 Nov 2026): hover the mic or hold Option to dictate with Deepgram (mocked here).
  const micGeo = () => page.evaluate(() => {
    const m = document.getElementById('ytc-mic'), b = m.getBoundingClientRect(), i = m.querySelector('svg').getBoundingClientRect();
    return { x: b.left, y: b.top, w: b.width, h: b.height, cx: b.right - 18, cy: b.top + 18, ix: i.left + i.width / 2, iy: i.top + i.height / 2, v: document.getElementById('ytc-search').dataset.v || '', clip: getComputedStyle(m).clipPath };
  });
  const hitMic = (x, y) => page.evaluate(([x, y]) => !!document.elementFromPoint(x, y)?.closest('#ytc-mic'), [x, y]);
  const voicePage = async q => {
    await page.goto('https://www.youtube.com/results?search_query=' + q, { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c[data-v]');
    await page.mouse.move(40, 860);
    await page.waitForTimeout(150);
  };
  const lastConn = () => dg.conns[dg.conns.length - 1];
  await step('voice: no key → resting on the mic explains in one line (nothing opened); the key is saved from the panel without entering the page', async () => {
    await voicePage('voice');
    const g = await micGeo();
    assert.ok(Math.abs(g.ix - g.cx) <= 1 && Math.abs(g.iy - g.cy) <= 1, 'mic glyph centred in its circle ' + JSON.stringify(g));
    assert.ok(!(await hitMic(g.x + 10, g.y + 18)), 'at rest the hover target is only the circle');
    await page.mouse.move(g.cx, g.cy);
    await page.waitForSelector('#ytc-vk.on', { timeout: 2000 });
    assert.equal(await page.locator('#ytc-vk-m').textContent(), 'Paste your Deepgram API key to search by voice.');
    assert.equal(dg.conns.length, 0, 'no socket without a key');
    assert.equal((await micGeo()).v, '', 'not listening');
    await page.click('#ytc-vk-in');
    await page.keyboard.type(VKEY);
    await page.keyboard.press('Enter');
    await waitT(() => /Key saved/.test(document.getElementById('ytc-vk-m')?.textContent || ''), null, 2000);
    assert.equal(await page.inputValue('#ytc-vk-in'), '', 'the field is emptied');
    assert.equal((await sw.evaluate(() => chrome.storage.local.get('ytcVoiceKeyV1'))).ytcVoiceKeyV1, VKEY);
    assert.ok(!(await page.content()).includes(VKEY), 'the key is not in the page');
    await page.mouse.click(40, 860);
    await page.waitForSelector('#ytc-vk:not(.on)', { state: 'attached' });
  });
  await step('voice: a short hover does nothing; resting 200 ms morphs the mic into a bigger listening pill (live level); interim words muted, finals normal; hover-out searches', async () => {
    await voicePage('voice');
    let g = await micGeo();
    const n0 = dg.conns.length;
    await page.mouse.move(g.cx, g.cy);
    await page.waitForTimeout(120);
    await page.mouse.move(40, 860);
    await page.waitForTimeout(300);
    assert.equal(dg.conns.length, n0, 'leaving within 200 ms opens nothing');
    assert.equal((await micGeo()).v, '');
    // Rest on it.
    const t0 = Date.now();
    await page.mouse.move(g.cx, g.cy);
    await page.waitForFunction(() => document.getElementById('ytc-search').dataset.v === 'on', null, { timeout: 2000 });
    assert.ok(Date.now() - t0 >= 180, 'starts after the dwell');
    // Mid-morph: the circle is stretching (the spring), the pointer still on it.
    const mid = await page.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(getComputedStyle(document.getElementById('ytc-mic')).clipPath)))));
    assert.notEqual(mid, 'inset(0px round 18px)', 'the pill grows on the spring, not at once: ' + mid);
    if (process.env.YTC_SHOTS) await page.screenshot({ path: path.join(process.env.YTC_SHOTS, 'voice-mid-morph.png'), clip: { x: g.x - 600, y: 0, width: 760, height: 64 } });
    await page.waitForTimeout(450);
    g = await micGeo();
    assert.equal(g.clip, 'inset(0px round 18px)', 'the listening pill');
    assert.ok(await hitMic(g.x + 10, g.y + 18), 'the pill is a bigger hover target');
    await waitT(() => !!document.getElementById('ytc-qv')?.textContent.includes('Listening'), null, 2000);
    const c = lastConn();
    assert.ok(c && c.protos[0] === 'token' && c.protos[1] === VKEY, 'Sec-WebSocket-Protocol: token, KEY');
    const u = new URL(c.url, 'http://x');
    for (const [k, v] of [['model', 'nova-3'], ['language', 'multi'], ['smart_format', 'true'], ['interim_results', 'true'], ['endpointing', '300'], ['encoding', 'linear16'], ['sample_rate', '16000'], ['channels', '1']]) assert.equal(u.searchParams.get(k), v, k);
    await waitT(() => true, null, 100);
    for (let i = 0; i < 30 && c.bins < 5; i++) await page.waitForTimeout(50);
    assert.ok(c.bins >= 5 && c.bytes % 2 === 0, `16-bit PCM chunks stream (${c.bins} chunks, ${c.bytes} bytes)`);
    // The fake microphone beeps: the level property moves.
    const b0 = c.bins, w0 = Date.now();
    const lv = await page.evaluate(() => new Promise(r => { const s = new Set(), t = setInterval(() => s.add(document.getElementById('ytc-mic').style.getPropertyValue('--lv')), 20); setTimeout(() => { clearInterval(t); r([...s]); }, 1300); }));
    assert.ok(lv.some(v => +v > 0.1), 'the waveform follows the level: ' + lv.join(' '));
    // 512 samples per chunk at 16 kHz (the rate the URL declares): ~31 chunks a second, 1024 bytes each.
    const rate = (c.bins - b0) / ((Date.now() - w0) / 1000);
    assert.ok(rate > 24 && rate < 40 && c.bytes / c.bins === 1024, `16 kHz chunks: ${rate.toFixed(1)}/s, ${c.bytes / c.bins} bytes`);
    c.send(wsMock.result('Roblox scrip'));
    await waitT(() => document.querySelector('#ytc-qv i')?.textContent === 'Roblox scrip', null, 2000);
    c.send(wsMock.result('Roblox scripting', { final: true }));
    c.send(wsMock.result('tutor'));
    await waitT(() => document.querySelector('#ytc-qv b')?.textContent === 'Roblox scripting' && document.querySelector('#ytc-qv i')?.textContent === 'tutor', null, 2000);
    const col = await page.evaluate(() => ({ b: getComputedStyle(document.querySelector('#ytc-qv b')).color, i: getComputedStyle(document.querySelector('#ytc-qv i')).color, q: document.getElementById('ytc-q').value }));
    assert.deepEqual(col, { b: 'rgb(241, 241, 241)', i: 'rgb(170, 170, 170)', q: 'Roblox scripting tutor' }, 'finals normal, interim muted');
    c.send(wsMock.result('tutorial.', { final: true, speech: true }));
    await waitT(() => document.querySelector('#ytc-qv b')?.textContent === 'Roblox scripting tutorial.', null, 2000);
    // Settled: hover-out searches at once (no flush needed); the results were warmed meanwhile.
    const before = log.api.length;
    await page.mouse.move(g.x + 10, g.y + 70);
    await page.waitForFunction(() => new URLSearchParams(location.search).get('search_query') === 'Roblox scripting tutorial', null, { timeout: 3000 });
    const d = await ds();
    console.log(`     voice hover-out → search ${(+d.ytcVoiceSearch - +d.ytcVoiceEnd).toFixed(1)} ms (settled transcript, mocked Deepgram)`);
    assert.ok(log.api.slice(before - 3).some(a => a.ep === 'search' && a.body.query === 'Roblox scripting tutorial') || log.api.some(a => a.ep === 'search' && a.body.query === 'Roblox scripting tutorial'), 'searched');
    await waitT(() => !document.getElementById('ytc-search').dataset.v, null, 1000);
    for (let i = 0; i < 20 && !c.gone; i++) await page.waitForTimeout(50);
    assert.ok(c.gone, 'the socket closes with the dictation');
    const bins = c.bins; await page.waitForTimeout(300);
    assert.equal(c.bins, bins, 'no audio after the end');
  });
  await step('voice: push-to-talk (hold Option): a tap or a chord opens nothing; held, the pill shows at once; release flushes and searches; Escape cancels', async () => {
    await voicePage('voice');
    const n0 = dg.conns.length;
    // A tap, and Option+key chords (typing, Alt+←), never open the microphone.
    await page.keyboard.down('Alt'); await page.waitForTimeout(80); await page.keyboard.up('Alt');
    await page.keyboard.down('Alt'); await page.waitForTimeout(100); await page.keyboard.press('KeyX'); await page.waitForTimeout(300); await page.keyboard.up('Alt');
    await page.waitForTimeout(200);
    assert.equal(dg.conns.length, n0, 'no socket for a tap or a chord');
    dg.onconn = c => { c.ontext = t => { if (t.includes('Finalize')) c.send(wsMock.result('Лучшие игры Roblox.', { final: true, flush: true })); }; };
    await page.keyboard.down('Alt');
    await page.waitForFunction(() => document.getElementById('ytc-search').dataset.v === 'key', null, { timeout: 2000 });
    const inst = await page.evaluate(() => new Promise(r => requestAnimationFrame(() => r(getComputedStyle(document.getElementById('ytc-mic')).clipPath))));
    assert.equal(inst, 'inset(0px round 18px)', 'keyboard: the pill is there at once');
    for (let i = 0; i < 40 && dg.conns.length === n0; i++) await page.waitForTimeout(25);
    const c = lastConn();
    c.send(wsMock.result('лучшие игры'));
    await waitT(() => document.querySelector('#ytc-qv i')?.textContent === 'лучшие игры', null, 2000);
    await page.keyboard.up('Alt');
    await page.waitForFunction(() => new URLSearchParams(location.search).get('search_query') === 'Лучшие игры Roblox', null, { timeout: 3000 });
    assert.ok(c.texts.includes('{"type":"Finalize"}') && c.texts.includes('{"type":"CloseStream"}'), 'flushed: ' + c.texts.join());
    const d = await ds();
    console.log(`     voice key release → search ${(+d.ytcVoiceSearch - +d.ytcVoiceEnd).toFixed(1)} ms (incl. the 60 ms microphone tail and the Finalize round trip, mocked)`);
    dg.onconn = null;
    // Escape (hover dictation) cancels: nothing searched, the field restored, the socket closed.
    await voicePage('keepme');
    const g = await micGeo();
    await page.mouse.move(g.cx, g.cy);
    await page.waitForFunction(() => document.getElementById('ytc-search').dataset.v === 'on', null, { timeout: 2000 });
    for (let i = 0; i < 40 && !(lastConn() && !lastConn().gone && lastConn().bins); i++) await page.waitForTimeout(25);
    const e = lastConn();
    e.send(wsMock.result('cancel me'));
    await waitT(() => document.querySelector('#ytc-qv i')?.textContent === 'cancel me', null, 2000);
    await page.keyboard.press('Escape');
    await waitT(() => !document.getElementById('ytc-search').dataset.v, null, 1000);
    assert.equal(await page.inputValue('#ytc-q'), 'keepme', 'field restored');
    for (let i = 0; i < 20 && !e.gone; i++) await page.waitForTimeout(50);
    assert.ok(e.gone, 'socket closed');
    await page.waitForTimeout(400);
    assert.equal(new URLSearchParams(new URL(page.url()).search).get('search_query'), 'keepme', 'nothing searched');
    assert.equal((await micGeo()).v, '', 'no new dwell while the pointer stays');
    await page.mouse.move(40, 860);
    // Navigation while listening stops it too.
    await page.keyboard.down('Alt');
    await page.waitForFunction(() => document.getElementById('ytc-search').dataset.v === 'key', null, { timeout: 2000 });
    const f = lastConn();
    await page.keyboard.up('Alt'); // nothing heard: the release ends quietly after the flush
    await waitT(() => !document.getElementById('ytc-search').dataset.v, null, 1000);
    for (let i = 0; i < 40 && !f.gone; i++) await page.waitForTimeout(50);
    assert.ok(f.gone, 'socket closed after an empty dictation');
    assert.equal(new URLSearchParams(new URL(page.url()).search).get('search_query'), 'keepme', 'an empty dictation searches nothing');
  });
  await step('voice: a rejected key explains itself; the key never appears in other storage, incidents, the page or the debug overlay; ? lists the key', async () => {
    await voicePage('voice');
    await sw.evaluate(() => chrome.storage.local.set({ ytcVoiceKeyV1: 'wrongKey123' }));
    const g = await micGeo();
    await page.mouse.move(g.cx, g.cy);
    await waitT(() => /rejected this key/.test(document.getElementById('ytc-vk-m')?.textContent || ''), null, 4000);
    assert.equal((await micGeo()).v, '', 'back at rest');
    assert.ok(dg.probes.some(p => p.path === '/v1/projects' && !p.ok), 'the probe told a bad key from a network error');
    await sw.evaluate(k => chrome.storage.local.set({ ytcVoiceKeyV1: k }), VKEY);
    await page.mouse.move(40, 860);
    await page.mouse.click(40, 860);
    const all = await sw.evaluate(() => chrome.storage.local.get(null));
    const leaks = Object.entries(all).filter(([k, v]) => k !== 'ytcVoiceKeyV1' && JSON.stringify(v).includes(VKEY)).map(([k]) => k);
    assert.deepEqual(leaks, [], 'the key only under its own storage key');
    assert.ok(!JSON.stringify(incidents).includes(VKEY), 'not in incidents');
    const pg = await page.evaluate(k => [document.documentElement.outerHTML, JSON.stringify(localStorage), JSON.stringify(sessionStorage)].some(x => x.includes(k)), VKEY);
    assert.ok(!pg, 'not in the page, localStorage or sessionStorage');
    await page.keyboard.press('Backquote');
    const dbg = await page.locator('#ytc-debug').textContent();
    await page.keyboard.press('Backquote');
    assert.ok(/voice · key set/.test(dbg) && !dbg.includes(VKEY), 'debug overlay: state only');
    await page.keyboard.press('?');
    assert.ok((await page.locator('#ytc-keys').textContent()).includes('Voice search'), '? sheet lists the voice key');
    await page.keyboard.press('Escape');
    // No sound: nothing is played (no media element besides the player's, which this page has none of).
    assert.equal(await page.locator('audio').count(), 0);
  });
  await step('import from the old extension: console line + Copy, strict validation (a bad paste changes nothing), merge, applied live (chips, topics, filters, history, resume bar, captions), stored; Escape closes', async () => {
    const OLD = fixture('old_extension_storage.json').toString('utf8');
    const keys = ['yourTubeHistoryV1', 'yourTubeProgressV1', 'yourTubeBlockedWebsitesV1', 'yourTubeRecommendationTopicsV1', 'yourTubeNativeTopicsV1', 'yourTubeMinimumHomeDurationV1', 'yourTubeHomeOrderV1', 'yourTubeHomePlaylistsOnlyV1', 'yourTubeShowMusicV1', 'yourTubeShowLiveV1', 'shortsInFeeds', 'ytcConfigV1'];
    const before = await sw.evaluate(k => chrome.storage.local.get(k), keys);
    HOME.rich = true; FX.homeChips = true;
    try {
      await page.evaluate(() => sessionStorage.clear());
      await readyDel(true);
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-filters [data-act=fx-nchip]');
      await page.hover('#ytc-filters .fx-plus');
      await page.waitForTimeout(300);
      await page.click('#ytc-fx-imp');
      await page.waitForSelector('#ytc-imp.on .imp-p');
      assert.equal(await page.evaluate(() => document.activeElement?.id), 'ytc-imp-in', 'focus in the paste box');
      assert.equal(await page.inputValue('#ytc-imp-cmd'), 'chrome.storage.local.get(null, d => copy(JSON.stringify(d)))');
      await page.click('#ytc-imp [data-act=fx-imp-copy]');
      assert.match(await page.locator('#ytc-imp-m').textContent(), /Copied|copy it/);
      await page.mouse.move(5, 5);
      await shot('import-dialog');
      // A broken or foreign paste is refused whole; nothing is written.
      const n0 = await sw.evaluate(() => chrome.storage.local.get(null).then(o => JSON.stringify(o).length));
      for (const bad of ['{"yourTubeHistoryV1": {"version": 1, "entries": []', '{"yourTubeHistoryV1": {"version": 1, "entries": []}, "yourTubeHomeOrderV1": "newest"}', '{"other": 1}']) {
        await page.fill('#ytc-imp-in', bad);
        await page.click('#ytc-imp [data-act=fx-imp-go]');
        await page.waitForSelector('#ytc-imp-m.er');
        await page.evaluate(() => { document.getElementById('ytc-imp-m').className = 'imp-m'; });
      }
      assert.equal(await sw.evaluate(() => chrome.storage.local.get(null).then(o => JSON.stringify(o).length)), n0, 'nothing written by a rejected paste');
      await page.fill('#ytc-imp-in', OLD);
      await page.click('#ytc-imp [data-act=fx-imp-go]');
      await page.waitForSelector('#ytc-imp-m.ok');
      const summary = await page.locator('#ytc-imp-m').textContent();
      assert.match(summary, /^Imported 87 searches, 1,204 resume positions, 2 blocked websites, 2 topics, 2 YouTube topic settings, Home filter settings and caption size, position and on\/off\. 1 record skipped \(invalid or over the limits\)\. Viewing statistics and the old topic lists are not carried over\.$/, summary);
      assert.equal(await page.inputValue('#ytc-imp-in'), '', 'the paste box is emptied');
      await shot('import-done');
      // Applied live, no reload: YouTube's chip renamed, the topics and the length filter are there.
      assert.equal(await page.locator('#ytc-fx-nchip-gaming').textContent(), 'Games');
      const chips = await page.$$eval('#ytc-filters .fx-tc .fx-ch', bs => bs.map(b => b.textContent));
      assert.ok(chips.includes('Roblox scripting') && chips.includes('Woodworking joints'), chips.join('|'));
      assert.match(await page.locator('#ytc-hfx').textContent(), /20\+ min/);
      // Escape closes the dialog at once.
      await page.focus('#ytc-imp-in');
      await page.keyboard.press('Escape');
      await page.waitForSelector('#ytc-imp:not(.on)', { state: 'attached' });
      assert.equal(await page.evaluate(() => getComputedStyle(document.getElementById('ytc-imp')).visibility), 'hidden');
      // Stored in the client's own keys; statistics and the legacy topic stores are left out.
      const st = await sw.evaluate(() => chrome.storage.local.get(null));
      assert.ok(st.yourTubeHistoryV1.entries.some(e => e.query === 'old extension search'));
      assert.ok(Object.keys(st.yourTubeProgressV1.entries).length >= 1204);
      assert.deepEqual(st.yourTubeBlockedWebsitesV1.domains.slice(-2), ['spam.example.com', 'clickbait.example.org']);
      assert.equal(st.yourTubeMinimumHomeDurationV1, 20);
      assert.equal(st.yourTubeShowMusicV1, true);
      assert.deepEqual(st.ytcConfigV1.p.cs, { v: 2, x: 0.1, y: 0.7, s: 1.25 });
      assert.equal(st.ytcConfigV1.p.cc, false);
      for (const k of ['yourTubeStatsV1', 'yourTubeDiscoveryTopicsV1', 'yourTubeHomeTopicsV1']) assert.ok(!(k in st), k + ' not stored');
      // The search box offers the imported searches; a card shows the imported resume position.
      await page.click('#ytc-q');
      await page.keyboard.type('old exten');
      await page.waitForFunction(() => document.getElementById('ytc-sug')?.textContent.includes('old extension search'));
      await page.keyboard.press('Escape');
      await page.evaluate(() => document.activeElement?.blur());
      await page.goto('https://www.youtube.com/results?search_query=imported', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v="zSK7bOoxuig"] .pg.own');
      assert.equal(await page.getAttribute('#ytc-list .c[data-v="zSK7bOoxuig"] .pg.own', 'style'), 'width:50%');
      // The player takes the caption settings: size, place, and off.
      await page.goto('https://www.youtube.com/watch?v=-Bxc4gJluC0', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-video');
      await page.waitForFunction(() => getComputedStyle(document.getElementById('ytc-player')).getPropertyValue('--cs') === '1.250');
      assert.equal(await page.evaluate(() => getComputedStyle(document.getElementById('ytc-player')).getPropertyValue('--cx')), '0.1000');
      assert.ok(!(await page.getAttribute('#ytc-player', 'class')).split(' ').includes('cc'), 'captions off');
    } finally {
      HOME.rich = false; FX.homeChips = false;
      await sw.evaluate(async ([keys, before]) => { await chrome.storage.local.remove(keys); await chrome.storage.local.set(before); }, [keys, before]);
    }
  });

  await step('perf: timings reach the debug center as kind "perf" (numbers only, no tokens/cookies/ids)', async () => {
    await page.goto('https://www.youtube.com/results?search_query=perf+check', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c[data-v]');
    const vid = new URL(await page.locator('#ytc-list a.t[href^="/watch"]').first().getAttribute('href'), 'https://www.youtube.com').searchParams.get('v');
    await page.click('#ytc-list a.t[href^="/watch"]');
    await page.waitForSelector('#ytc-list .c[data-v]');
    await page.evaluate(() => sessionStorage.removeItem('ytc:perfat'));
    await page.goto('https://www.youtube.com/results?search_query=after', { waitUntil: 'commit' }); // leaving the document reports (≥ 1 min apart)
    for (let i = 0; i < 50 && !incidents.some(x => x.kind === 'perf' && x.perf?.ms?.['nav:watch-related']); i++) await page.waitForTimeout(100);
    const r = incidents.filter(x => x.kind === 'perf' && x.perf?.ms?.['nav:watch-related']).at(-1);
    assert.ok(r, 'perf report delivered');
    assert.ok(r.perf.ms.next?.p50 >= 0 && r.perf.ms['nav:watch-related']?.n >= 1, JSON.stringify(r.perf.ms));
    assert.ok(r.perf.cache['watch-next'], 'cache hit rates');
    const str = JSON.stringify(r);
    assert.ok(!/SAPISID|continuation|token|search_query|perf\+check/.test(str) && !str.includes(vid), str.slice(0, 300));
  });

  await step('public build: no speed telemetry, nothing kept for a collector, no development permission or auto-reload', async () => {
    if (DEV_BUILD) return;
    assert.equal(telBatches(0).length, 0, 'no telemetry batch in the whole run');
    const st = await sw.evaluate(() => chrome.storage.local.get(null));
    assert.ok(!('ytcIncidentsV1' in st) && !('ytcPerfQueueV1' in st), 'nothing stored for a collector: ' + Object.keys(st).join(','));
    const m = await sw.evaluate(() => chrome.runtime.getManifest());
    assert.ok(!m.host_permissions.some(p => p.includes('127.0.0.1')) && !m.version_name, 'public manifest');
    const files = ['content.js', 'background.js', 'manifest.json'].map(f => fs.readFileSync(path.join(EXT, f), 'utf8')).join('\n');
    for (const s of ['127.0.0.1:47321', 'ytc-dev-check', 'build-id.txt', 'ytcIncidentsV1', 'ytcPerfQueueV1']) assert.ok(!files.includes(s), 'no ' + s + ' in the public build');
  });
  await step('dev build: perf telemetry: a navigation and a video start arrive as schema-2 events with plausible numbers and no secrets', async () => {
    const from = incidents.length;
    await ctx.addCookies([{ name: 'SAPISID', value: 'telSECRETsapisid0', domain: '.youtube.com', path: '/' }]);
    try {
      await page.goto('https://www.youtube.com/results?search_query=tel+check+secret', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      const vd = await page.evaluate(() => { try { return JSON.parse(sessionStorage.getItem('ytc:cfg')).vd || ''; } catch { return ''; } });
      const a = page.locator('#ytc-list a.t[href^="/watch"]').nth(2);
      const vid = new URL(await a.getAttribute('href'), 'https://www.youtube.com').searchParams.get('v');
      const t0 = await page.evaluate(() => performance.now());
      await a.click();
      await page.waitForFunction(t => +document.documentElement.dataset.ytcFirstFrame > t, t0, { timeout: 8000 });
      await page.waitForTimeout(1200);
      await page.keyboard.press('j'); // a seek (and its toast)
      await page.waitForTimeout(800);
      await page.goto('https://www.youtube.com/results?search_query=after+tel', { waitUntil: 'commit' }); // leaving sends the batch
      for (let i = 0; i < 50 && !telBatches(from).some(b => b.events.some(e => e.m === 'video.session' && e.vid === vid)); i++) await page.waitForTimeout(100);
      const batches = telBatches(from), ev = batches.flatMap(b => b.events);
      const by = m => ev.filter(e => e.m === m);
      const counts = {}; for (const e of ev) counts[e.m] = (counts[e.m] || 0) + 1;
      console.log('     telemetry events: ' + JSON.stringify(counts) + ` · overhead ${batches.reduce((s, b) => s + b.overhead_ms, 0).toFixed(1)} ms Rust + ${batches.reduce((s, b) => s + b.js_overhead_ms, 0).toFixed(1)} ms observers`);
      assert.ok(batches.length, 'batch delivered');
      for (const b of batches) {
        assert.ok(b.build && /^\d+\.\d+\.\d+ \([0-9a-f]{16}\)$/.test(b.build), 'build stamped: ' + b.build);
        assert.ok(b.events.length <= 200 && b.page && b.seq >= 1 && b.overhead_ms >= 0, JSON.stringify({ ...b, events: b.events.length }));
        for (const e of b.events) assert.ok(typeof e.m === 'string' && Number.isFinite(e.v) && e.at > 1.7e12, JSON.stringify(e));
      }
      const boot = by('nav.search').find(e => e.mode === 'boot');
      assert.ok(boot && boot.input === 'boot' && boot.v > 0 && boot.v < 5000 && boot.reqs?.some(r => r.ep === 'search' && r.status === 200 && r.bytes > 1000), 'boot navigation ' + JSON.stringify(boot));
      const nav = by('nav.watch').find(e => e.mode === 'push');
      assert.ok(nav && nav.input === 'pointer' && ['cache', 'inflight', 'network', 'prefetch'].includes(nav.src), 'watch navigation ' + JSON.stringify(nav));
      assert.ok(nav.v > 0 && nav.v < 3000 && nav.insert_ms <= nav.v && nav.handler_ms >= 0, 'watch navigation numbers ' + JSON.stringify(nav));
      const search = by('req.search')[0];
      assert.ok(search && search.status === 200 && search.bytes > 1000 && search.v >= 0 && search.via === 'page', 'req.search ' + JSON.stringify(search));
      const start = by('video.start').find(e => e.vid === vid);
      assert.ok(start && start.v > 0 && start.v < 8000 && start.input === 'navigation', 'video.start ' + JSON.stringify(start));
      assert.ok(start.gv.length > 0 && start.gv.every(r => [200, 206].includes(r.status) && r.itag > 0 && r.bytes > 0), 'googlevideo requests in the start breakdown');
      assert.ok(['cache', 'prefetch', 'fetch'].includes(start.vision.src) && (start.watch_ms == null || start.watch_ms <= start.v) && start.mse_open_ms >= 0, 'start breakdown ' + JSON.stringify(start));
      // Straight to the target rung; the slow-YouTube breakdown is there (nothing fired on local media).
      assert.ok(start.start_p === start.target_p, 'no low start rung ' + JSON.stringify(start));
      assert.deepEqual(start.slow, { vision_hedge: { fired: false, won: false }, hedge_fired: 0, hedge_won: 0, throttle_detected: 0, path: [] }, 'slow breakdown');
      const head = by('gv.head').find(e => e.vid === vid);
      assert.ok(head && /googlevideo\.com$/.test(head.host) && head.itag > 0 && head.bytes > 0 && head.v >= 0, 'gv.head ' + JSON.stringify(head));
      const seek = by('video.seek').find(e => e.vid === vid);
      assert.ok(seek && seek.v >= 0 && seek.v < 5000 && seek.input === 'keyboard', 'video.seek ' + JSON.stringify(seek));
      assert.ok(by('ui.toast').some(e => e.v >= 0 && e.v < 2000), 'toast latency');
      const session = by('video.session').find(e => e.vid === vid);
      assert.ok(session && session.started && session.seeks >= 1 && session.v > 1000, 'video.session ' + JSON.stringify(session));
      assert.ok(by('lag.cls').some(e => e.route === 'search' && e.v >= 0), 'layout shift of the page left');
      const str = JSON.stringify(batches);
      for (const secret of ['telSECRETsapisid0', 'SAPISID', FRESH_VD, vd, 'tel check', 'tel+check', 'search_query', 'videoplayback', 'sig=', 'expire=', 'authorization', 'x-goog'].filter(Boolean))
        assert.ok(!str.includes(secret), 'no ' + secret + ' in telemetry');
    } finally { await ctx.clearCookies(); }
    // Switched off (ytcTelemetryV1: false): nothing is recorded or sent.
    await sw.evaluate(() => chrome.storage.local.set({ ytcTelemetryV1: false }));
    try {
      // (The page open while it was switched off still sends what it recorded when it is left.)
      await page.goto('https://www.youtube.com/results?search_query=tel+off', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.waitForTimeout(1500);
      const off = incidents.length;
      await page.click('#ytc-list a.t[href^="/watch"]');
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.goto('https://www.youtube.com/results?search_query=tel+off+2', { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c[data-v]');
      await page.waitForTimeout(1500);
      assert.equal(telBatches(off).length, 0, 'switched off: no telemetry batch');
    } finally { await sw.evaluate(() => chrome.storage.local.remove('ytcTelemetryV1')); }
  });

  await step('dev build: interaction log: clicks, handled keys, the search box, back and a hover dwell become ui.act events (input → handler → DOM → paint, what they waited on), with no text or search terms', async () => {
    const from = incidents.length;
    await page.goto('https://www.youtube.com/results?search_query=act+log+secret', { waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c[data-v]');
    await page.waitForTimeout(400);
    const a = page.locator('#ytc-list a.th[href^="/watch"]').nth(1);
    const vid = new URL(await a.getAttribute('href'), 'https://www.youtube.com').searchParams.get('v');
    await a.click();
    await page.waitForSelector('#ytc-player');
    await page.waitForTimeout(600);
    await page.keyboard.press('k'); // pause
    await page.waitForTimeout(200);
    await page.goBack({ waitUntil: 'commit' });
    await page.waitForSelector('#ytc-list .c[data-v]');
    await page.waitForTimeout(300);
    await page.click('#ytc-q');
    await page.keyboard.type('typed words', { delay: 10 });
    await page.keyboard.press('Enter');
    await page.waitForFunction(() => location.search.includes('typed'));
    await page.waitForSelector('#ytc-list .c[data-v]');
    await page.waitForTimeout(300);
    const box = await page.locator('#ytc-logo').boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2, { steps: 2 });
    await page.waitForFunction(() => location.pathname === '/', null, { timeout: 3000 });
    await page.waitForTimeout(500);
    await page.mouse.move(700, 600);
    await page.goto('https://www.youtube.com/results?search_query=after+acts', { waitUntil: 'commit' });
    for (let i = 0; i < 50 && !telBatches(from).some(b => b.events.some(e => e.m === 'ui.act' && e.act === 'logo.home')); i++) await page.waitForTimeout(100);
    const ev = telBatches(from).flatMap(b => b.events), acts = ev.filter(e => e.m === 'ui.act');
    console.log('     interactions: ' + acts.map(e => `${e.act} ${e.input} ${e.v}${e.content_ms != null ? '/' + e.content_ms : ''} ms (${e.wait})`).join(' · '));
    for (const e of acts) {
      assert.ok(e.v >= 0 && e.v < 5000 && e.handler_ms >= 0 && e.handler_ms <= e.v + 0.1 && e.t > 0 && e.at > 1.7e12 && e.route != null && typeof e.nav === 'boolean', 'numbers ' + JSON.stringify(e));
      assert.ok(e.dom_ms == null || e.dom_ms >= e.handler_ms - 0.1, 'dom after the handler ' + JSON.stringify(e));
      assert.ok(/^[a-z0-9.\-_\/?]+$/.test(e.act), 'a code name ' + e.act);
    }
    const card = acts.find(e => e.act === 'card.open');
    assert.ok(card && card.input === 'pointer' && card.nav && card.vid === vid && card.to === 'watch' && card.kind === 'a@list' && card.route === 'search', 'card.open ' + JSON.stringify(card));
    assert.ok(['cache', 'prefetch', 'inflight', 'network', 'memory'].includes(card.wait) && card.content_ms >= card.dom_ms - 0.1, 'card.open waited on ' + JSON.stringify(card));
    assert.ok(ev.some(e => e.m === 'nav.watch' && e.act === 'card.open'), 'the navigation names its interaction');
    const k = acts.find(e => e.act === 'key.k');
    assert.ok(k && k.input === 'keyboard' && k.route === 'watch' && !k.nav && k.wait === 'none', 'key.k ' + JSON.stringify(k));
    const back = acts.find(e => e.act === 'history.pop');
    assert.ok(back && back.input === 'history' && back.nav && back.content_ms >= 0, 'history.pop ' + JSON.stringify(back));
    const sub = acts.find(e => e.act === 'search.submit');
    assert.ok(sub && sub.input === 'keyboard' && sub.nav, 'search.submit ' + JSON.stringify(sub));
    const logo = acts.find(e => e.act === 'logo.home');
    assert.ok(logo && logo.input === 'hover' && logo.nav, 'logo dwell ' + JSON.stringify(logo));
    assert.ok(!acts.some(e => /^key\.[a-z]$/.test(e.act) && e.act !== 'key.k'), 'typed letters are not interactions');
    const str = JSON.stringify(acts);
    for (const secret of ['typed', 'act log', 'act+log', 'secret', 'search_query', '/watch?']) assert.ok(!str.includes(secret), 'no ' + secret + ' in the interaction log');
    const boot = ev.find(e => e.m === 'nav.search' && e.mode === 'boot');
    assert.ok(boot && boot.boot && boot.boot.response_start > 0 && boot.boot.script >= boot.boot.response_start && boot.boot.wasm >= boot.boot.script && boot.boot.app >= boot.boot.wasm, 'boot breakdown ' + JSON.stringify(boot && boot.boot));
  });

  await step('TV mode: focus resting on a card prefetches its first media and warms googlevideo through a request (the shell\'s connection, not a <link rel=preconnect>); OK uses the prefetch', async () => {
    const tv = await ctx.newPage();
    const w0 = log.warm.length;
    try {
      await tv.addInitScript(() => { try { localStorage.setItem('ytc:platform', 'tv'); } catch {} });
      await tv.goto('https://www.youtube.com/results?search_query=tv+focus+prefetch', { waitUntil: 'commit' });
      await tv.waitForSelector('#ytc-list .c a.t');
      await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-list a.t'), null, { timeout: 5000 });
      await tv.waitForTimeout(1500); // a fresh tab: the config refresh (visitor data) lands first
      const g0 = log.gv.length;
      // Fresh ids: a card played in an earlier step resumes past its first segment (no warm first frame for it).
      await tv.evaluate(() => document.querySelectorAll('#ytc-list .c').forEach((c, i) => c.querySelectorAll('a[href^="/watch"]').forEach(a => a.setAttribute('href', '/watch?v=tvwarm' + String(i).padStart(5, '0')))));
      await tv.keyboard.press('ArrowRight');
      const href = await tv.evaluate(() => document.activeElement.getAttribute('href'));
      const v = new URL(href, 'https://www.youtube.com').searchParams.get('v');
      const mine = () => log.gv.slice(g0).filter(g => g.v === v);
      for (let i = 0; i < 40 && mine().length < 2; i++) await tv.waitForTimeout(50);
      assert.equal(mine().filter(g => g.a === 0).length, 2, 'video + audio first media while focus rests: ' + JSON.stringify(mine()));
      assert.ok(log.warm.slice(w0).includes('rr1---sn-fixture.googlevideo.com'), 'warmed with /generate_204: ' + log.warm.slice(w0));
      assert.equal(await tv.evaluate(() => document.querySelectorAll('link[rel=preconnect][href*="googlevideo"]').length), 0, 'no <link rel=preconnect> on TV');
      // The resting focus also decodes the first frame (its decoder released at once): OK shows it, not a thumbnail.
      await tv.waitForFunction(v => document.documentElement.dataset.ytcWarm === 'ready:' + v, v, { timeout: 6000 });
      await warmWatch(tv);
      assert.equal((await warmState(tv)).now, 0, 'TV: no warm decoder kept');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.documentElement.dataset.ytcFirstFrame, null, { timeout: 8000 });
      await tv.waitForFunction(() => !document.querySelector('#ytc-player .pwf'), null, { timeout: 3000 });
      const w = await warmState(tv), f = w.added.find(a => a.tag === 'CANVAS');
      assert.ok(f && f.afterVideo && !f.frameShown && f.w >= 640 && f.lit > 0.5 && w.added.every(a => !a.poster && !a.imgs), 'TV: OK shows the warm first frame, no thumbnail: ' + JSON.stringify(w.added));
      assert.equal(w.s, 'used:' + v);
      assert.ok(!mine().slice(2).some(g => g.a === 0), 'the prefetched first requests were used: ' + JSON.stringify(mine().map(g => [g.itag, g.a, g.b, g.gen])));
    } finally {
      await tv.evaluate(() => localStorage.removeItem('ytc:platform')).catch(() => {});
      await tv.close();
    }
  });

  // ---- TV mode (the TV app's platform flag and a stand-in for its shell: localStorage ytc:platform and ytc:shell,
  // the development switches for what tv/shim/shim.js provides). localStorage e2e:tv picks the shell's answers:
  // 'code' = a sign-in code whose polls fail with a long unbroken reason, 'nocode' = the code request fails,
  // 'signed' = a signed-in account. YTC_SHOTS=<dir> saves one screenshot per screen and size.
  const TVSIZES = [[960, 540], [1920, 1080]];
  const tvShots = process.env.YTC_SHOTS;
  const tvOpen = async () => {
    const tv = await ctx.newPage();
    tv.errors = [];
    tv.on('pageerror', e => tv.errors.push(String(e)));
    await tv.addInitScript(() => {
      const mode = localStorage.getItem('e2e:tv') || 'code';
      const why = 'https://oauth2.googleapis.com/device/token?' + 'x'.repeat(80);
      localStorage.setItem('ytc:platform', 'tv');
      const answers = {
        auth_token: mode === 'signed' ? JSON.stringify({ token: 'ya29.' + 'a'.repeat(60), exp: Date.now() + 3600e3 }) : '{"token":null}',
        auth_start: mode === 'nocode' ? null : JSON.stringify({ user_code: 'WMB-QZX-TPR', verification_url: 'https://www.google.com/device', interval: 1, expires_in: 1800 }),
        auth_poll: JSON.stringify({ state: 'error', error: why }),
        auth_signout: '{}',
        error: 'HTTP 503 ' + why,
        shell_info: JSON.stringify({ version: '0.1.0', version_code: 1000, bundle: { seq: 100, client: '0.9.0', source: 'builtin' }, update: { state: 'idle' }, offer: null }),
      };
      // (e2e:tvshell: answers a step puts in place of these, kept across reloads.)
      localStorage.setItem('ytc:shell', JSON.stringify(Object.assign(answers, JSON.parse(localStorage.getItem('e2e:tvshell') || '{}'))));
    });
    return tv;
  };
  const tvClose = async tv => {
    await tv.evaluate(() => { for (const k of ['e2e:tv', 'e2e:tvshell', 'ytc:platform', 'ytc:shell']) localStorage.removeItem(k); }).catch(() => {});
    await tv.close();
  };
  // A fresh page load in `mode` at `url` (sessionStorage cleared: no snapshot, no token mirror).
  const tvGo = async (tv, mode, url) => {
    await tv.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    // (The page going away writes its feed snapshot on pagehide: cleared after it, so each load starts cold.)
    await tv.evaluate(m => { localStorage.setItem('e2e:tv', m); sessionStorage.clear(); addEventListener('pagehide', () => sessionStorage.clear()); }, mode);
    await tv.goto(url, { waitUntil: 'commit' });
    await tv.waitForFunction(() => document.documentElement.dataset.ytcTv === '1' && document.querySelector('#ytc-side .rl'));
  };
  const tvShot = async (tv, name) => {
    if (!tvShots) return;
    await tv.waitForTimeout(260); // let the fixture's content settle before the visual capture
    const vp = tv.viewportSize();
    await tv.screenshot({ path: path.join(tvShots, `${name}-${vp.width}.png`) });
  };
  // Five cards per row; the fifth card focused keeps its ring inside the safe area (2.5 % on the right). Returns the
  // problems (none: []); the focus comes back to where it was.
  const tvRowEnd = tv => tv.evaluate(() => {
    const was = document.activeElement, cards = [...document.querySelectorAll('#ytc-list .c')].filter(c => c.querySelector('a.t'));
    const row = cards.filter(c => c.offsetTop === cards[0].offsetTop), bad = [];
    if (row.length !== 5) bad.push(`${row.length} cards on the first row`);
    const last = row[row.length - 1];
    last.querySelector('a.t').focus({ preventScroll: true });
    const r = last.getBoundingClientRect(), th = getComputedStyle(last.querySelector('.th'));
    const ring = parseFloat(th.outlineWidth) + parseFloat(th.outlineOffset);
    if (th.outlineColor === 'rgba(0, 0, 0, 0)' || th.outlineStyle === 'none') bad.push('no ring on the fifth card');
    if (r.right + ring > innerWidth * 0.975 + 1) bad.push(`the fifth card's ring ends at ${r.right + ring} > ${innerWidth * 0.975}`);
    if (was && was.focus) was.focus({ preventScroll: true });
    return bad;
  });
  // The pixels of a screen region (CSS px, DPR 1): relative luminance and the RGB of each, read from a screenshot.
  const tvPixels = async (tv, r) => {
    const clip = { x: Math.floor(r.x), y: Math.floor(r.y), width: Math.ceil(r.w), height: Math.ceil(r.h) };
    const png = (await tv.screenshot({ clip, animations: 'disabled' })).toString('base64');
    return tv.evaluate(async b64 => {
      const bytes = Uint8Array.from(atob(b64), c => c.charCodeAt(0));
      const img = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
      const cv = document.createElement('canvas'); cv.width = img.width; cv.height = img.height;
      const g = cv.getContext('2d'); g.drawImage(img, 0, 0);
      const d = g.getImageData(0, 0, img.width, img.height).data, lin = v => (v /= 255) <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4, lum = [];
      for (let i = 0; i < d.length; i += 4) lum.push(0.2126 * lin(d[i]) + 0.7152 * lin(d[i + 1]) + 0.0722 * lin(d[i + 2]));
      return { w: img.width, h: img.height, lum };
    }, png);
  };
  // The centre of the bright marks in a region (luminance above the region's median, weighted by how far above).
  const brightCentre = px => {
    const bg = [...px.lum].sort((a, b) => a - b)[px.lum.length >> 1];
    let sx = 0, sy = 0, sw = 0;
    px.lum.forEach((l, i) => { const wgt = Math.max(0, l - bg - 0.1); sx += wgt * (i % px.w + 0.5); sy += wgt * (Math.floor(i / px.w) + 0.5); sw += wgt; });
    return sw ? [sx / sw, sy / sw] : null;
  };
  const focusedInfo = tv => tv.evaluate(() => { const a = document.activeElement, r = a.getBoundingClientRect(); return { tag: a.tagName, id: a.id, cls: String(a.className), href: a.getAttribute('href'), nav: a.dataset.nav || '', x: r.left, y: r.top, w: r.width, h: r.height }; });
  // The 10-foot audit of what is on screen now (the TV rules, measured): 1. the focused stop exists, is shown and,
  // with its ring and its scale, lies inside the safe area (2.5 % left and right, 3.5 % top and bottom) and no
  // ancestor clips it; 2. every focus stop fixed to the screen (rail, overlay, panels) lies in the safe area; 3. every
  // text shown on screen is at least 17 px at 1080p (8.5 CSS px at 540); 4. the root font-size is 1/80 of the width.
  const tvAudit = (tv, what) => tv.evaluate(what => {
    const W = innerWidth, H = innerHeight, sx = W * 0.025, sy = H * 0.035, e = 1, bad = [];
    const px = v => Math.round(v);
    const shown = el => el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true }) && el.getClientRects().length > 0;
    const inSafe = (r, ext) => r.left - ext >= sx - e && r.top - ext >= sy - e && r.right + ext <= W - sx + e && r.bottom + ext <= H - sy + e;
    if (Math.abs(parseFloat(getComputedStyle(document.documentElement).fontSize) - W / 80) > 0.05) bad.push('root font-size is not 100vw/80');
    const a = document.activeElement;
    if (!a || a === document.body || !shown(a)) bad.push(`no focus shown (${a && a.tagName}#${a && a.id})`);
    else if (a.id === 'ytc-main') {
      // Loading: the page box holds the focus (no ring) until the content takes it; nothing scrolled for it.
      if (scrollY > 0 || !document.querySelector('#ytc-main .sk')) bad.push('the page box holds the focus outside loading');
    } else {
      const s = getComputedStyle(a), r = a.getBoundingClientRect();
      const ring = s.outlineStyle !== 'none' ? Math.max(0, parseFloat(s.outlineWidth) + parseFloat(s.outlineOffset)) : 0;
      // A card's ring is on its thumbnail: check the whole card.
      const card = a.closest('.c');
      const box = card ? card.getBoundingClientRect() : r;
      const ext = card ? parseFloat(getComputedStyle(card.querySelector('.th')).outlineWidth) * 2 : ring;
      if (!inSafe(box, ext)) bad.push(`focus ${a.tagName}.${a.className}#${a.id} ${[box.left, box.top, box.right, box.bottom].map(px)} +${px(ext)} outside the safe area of ${W}×${H}`);
      for (let p = (card || a).parentElement, q = card || a; p && p !== document.documentElement; q = p, p = p.parentElement) {
        // (Above a fixed box only the viewport clips.)
        if (getComputedStyle(q).position === 'fixed') break;
        const ps = getComputedStyle(p);
        const clips = !(ps.overflowX === 'visible' && ps.overflowY === 'visible') || ps.clipPath !== 'none' || (ps.contentVisibility === 'auto');
        if (!clips) continue;
        const pr = p.getBoundingClientRect();
        if (box.left - ext < pr.left - e || box.right + ext > pr.right + e || box.top - ext < pr.top - e || box.bottom + ext > pr.bottom + e) bad.push(`the focus ring of ${a.tagName}.${a.className} is cut by ${p.tagName.toLowerCase()}#${p.id}.${p.className}`);
      }
    }
    const SEL = "a[href]:not([tabindex='-1']),button:not([disabled]):not([tabindex='-1']),input:not([type=hidden]):not([tabindex='-1']),[tabindex='0']";
    for (const el of document.querySelectorAll(SEL)) {
      if (!shown(el) || el.closest('[inert],[aria-hidden="true"],[hidden]')) continue;
      let fixed = false;
      for (let p = el; p; p = p.parentElement) if (getComputedStyle(p).position === 'fixed') { fixed = true; break; }
      if (!fixed || el.closest('.wr #ytc-list')) continue; // (the Up next row scrolls sideways under the focus)
      // A scrollable fixed panel may contain controls beyond its visible reader.
      // Audit the painted portion here; the focused stop and its full ring are checked above.
      let r = el.getBoundingClientRect();
      for (let p = el.parentElement; p && p !== document.documentElement; p = p.parentElement) {
        const ps = getComputedStyle(p);
        if (!['auto', 'scroll', 'hidden', 'clip'].includes(ps.overflowY) && !['auto', 'scroll', 'hidden', 'clip'].includes(ps.overflowX)) continue;
        const pr = p.getBoundingClientRect();
        r = { left: Math.max(r.left, pr.left), top: Math.max(r.top, pr.top), right: Math.min(r.right, pr.right), bottom: Math.min(r.bottom, pr.bottom) };
      }
      if (r.right <= r.left || r.bottom <= r.top) continue;
      if (r.right < 0 || r.left > W || r.bottom < 0 || r.top > H) continue;
      if (!inSafe(r, 0)) bad.push(`fixed stop ${el.tagName}.${el.className}#${el.id} ${[r.left, r.top, r.right, r.bottom].map(px)} outside the safe area`);
    }
    const min = 17 * H / 1080 - 0.01;
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const p = n.parentElement;
      if (!n.data.trim() || !p || !shown(p) || p.closest('#ytc-debug,[aria-hidden="true"],.sk')) continue;
      const rg = document.createRange(); rg.selectNodeContents(n);
      const q = rg.getBoundingClientRect();
      if (!q.width || q.right < 0 || q.left > W || q.bottom < 0 || q.top > H) continue;
      const fs = parseFloat(getComputedStyle(p).fontSize);
      if (fs < min) bad.push(`text ${JSON.stringify(n.data.trim().slice(0, 24))} at ${fs.toFixed(1)} px < ${min.toFixed(1)}`);
    }
    return [...new Set(bad)].slice(0, 12).map(b => `${what}: ${b}`);
  }, what);
  // The D-pad graph of the focus stops reachable from the current focus (each stop tried in all four directions):
  // every stop reached has a way on (no dead end), and the stops of `want` (a selector of what must be reachable,
  // judged on screen at the start) are all reached.
  const tvWalk = async (tv, want, max = 60) => {
    await tv.evaluate(() => { let i = 0; for (const el of document.querySelectorAll('a[href],button,input,[tabindex]')) el.dataset.e2ek = String(i++); });
    const start = await tv.evaluate(() => document.activeElement.dataset.e2ek);
    const need = await tv.evaluate(sel => [...document.querySelectorAll(sel)].filter(el => el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true }) && el.getClientRects().length && (r => r.bottom > 0 && r.top < innerHeight)(el.getBoundingClientRect())).map(el => el.dataset.e2ek), want);
    const seen = new Set([start]), queue = [start], dead = [];
    while (queue.length && seen.size < max) {
      const k = queue.shift();
      let out = 0;
      for (const key of ['ArrowRight', 'ArrowDown', 'ArrowLeft', 'ArrowUp']) {
        // (A page that re-rendered under the walk — a fresher feed taking its place — ends that branch.)
        if (!await tv.evaluate(k => { const el = document.querySelector(`[data-e2ek="${k}"]`); if (el) el.focus({ preventScroll: true }); return !!el; }, k)) { out++; break; }
        await tv.keyboard.press(key);
        // (An element written after the walk started gets its key now.)
        const to = await tv.evaluate(() => { const a = document.activeElement; if (a && a !== document.body && !a.dataset.e2ek) a.dataset.e2ek = 'n' + (globalThis.__e2en = (globalThis.__e2en || 0) + 1); return a.dataset.e2ek; });
        if (to && to !== k) { out++; if (!seen.has(to)) { seen.add(to); queue.push(to); } }
      }
      if (!out) dead.push(k);
    }
    const gone = await tv.evaluate(ks => ks.filter(k => !document.querySelector(`[data-e2ek="${k}"]`)), need);
    const missing = need.filter(k => !seen.has(k) && !gone.includes(k));
    const name = k => tv.evaluate(k => { const el = document.querySelector(`[data-e2ek="${k}"]`); return el ? `${el.tagName}.${el.className}${el.dataset.nav ? '[' + el.dataset.nav + ']' : ''}` : k; }, k);
    return { dead: await Promise.all(dead.map(name)), missing: await Promise.all(missing.map(name)), reached: seen.size };
  };
  // The remote's Back (what the TV shell's YTC_BACK() does, through the bridge's `ytc-back` event).
  const back = tv => tv.evaluate(() => { document.dispatchEvent(new Event('ytc-back')); const r = document.documentElement.dataset.ytcBack; return { handled: r === '1', exit: r === '2' }; });

  // One layout at every resolution: the boxes of `sel` as fractions of the screen.
  const tvGeometry = (tv, sel) => tv.evaluate(sel => [...document.querySelectorAll(sel)].filter(e => e.getClientRects().length).map(e => { const r = e.getBoundingClientRect(); return [r.left / innerWidth, r.top / innerHeight, r.width / innerWidth, r.height / innerHeight]; }), sel);
  const sameGeometry = (a, b, what) => {
    assert.equal(a.length, b.length, `${what}: same boxes`);
    const d = a.map((r, i) => Math.max(...r.map((v, k) => Math.abs(v - b[i][k]) * (k % 2 ? 540 : 960))));
    const worst = Math.max(0, ...d), at = d.indexOf(worst);
    assert.ok(worst <= 1, `${what}: the 960×540 and 1920×1080 layouts differ by ${worst.toFixed(2)} px (at 960×540) at box ${at}: ${JSON.stringify([a[at], b[at]])}`);
  };
  const tvChecks = { step, tvOpen, tvClose, tvGo, tvAudit, tvShot, back, TVSIZES, tvWalk, focusedInfo, calls, log, ACTS, T_FIRST, T_TOP, T_NEW, T_PAGE2, T_REP, incidents, telBatches };
  await require('./tv-keyboard.cjs')(tvChecks);
  await require('./tv-watch.cjs')(tvChecks);
  await require('./tv-motion.cjs')(tvChecks);
  await step('TV browsing (960×540 and 1920×1080): rail, Home signed out and in, topic chips, the grid walk with its fixed row, Search screen and results, channel, playlist, a failed page; every screen inside the safe area, ≥ 17 px type, rings never clipped, no dead ends, Back rules', async () => {
    const tv = await tvOpen();
    const geo = {};
    try {
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        // Home signed out: the calm card ("Make YouTube yours"), its Sign in focused; the rail (YouTube's logo, Search … Sign in).
        await tvGo(tv, 'code', 'https://www.youtube.com/');
        await tv.waitForSelector('.tvpc .tvb');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('.tvpc .tvb'));
        assert.equal(await tv.locator('#ytc-top .logo:visible, #ytc-search:visible, .hero').count(), 0, 'no header bar, no search box on Home');
        assert.deepEqual(await tv.$$eval('#ytc-side .rl', a => a.map(e => e.dataset.nav)), ['search', 'home', 'subs', 'history', 'wl', 'playlists', 'liked', 'acct', 'about']);
        assert.equal(await tv.textContent('#ytc-side .rmk b'), 'YouTube');
        assert.equal(await tv.textContent('.tvpc .tvcd h2'), 'Make YouTube yours');
        // One palette on TV too: graphite, YouTube's red logo, and the focus frame white (the accent): a stationary ring
        // that stands out on every surface it sits on (≥ 3:1 non-text contrast; far above it in fact).
        const look = await tv.evaluate(() => {
          const f = document.activeElement, s = getComputedStyle(f), r = getComputedStyle(document.documentElement);
          return { bg: getComputedStyle(document.body).backgroundColor, mark: [...document.querySelectorAll('#ytc-side .rmk .lmk :is(rect,path)')].map(e => getComputedStyle(e).fill), ring: s.outlineColor, style: s.outlineStyle, ac: r.getPropertyValue('--ac').trim(), title: document.title, attr: document.documentElement.hasAttribute('data-ytc-palette'), text: document.body.innerText };
        });
        assert.deepEqual([look.bg, look.mark, look.ring, look.style, look.ac, look.attr], ['rgb(15, 15, 15)', ['rgb(255, 0, 51)', 'rgb(255, 255, 255)'], 'rgb(241, 241, 241)', 'solid', '#f1f1f1', false]);
        assert.ok(look.title === 'YouTube' || / - YouTube$/.test(look.title), look.title);
        assert.doesNotMatch(look.text, /lumen|redline/i);
        assert.deepEqual(await tvAudit(tv, `home signed out ${W}`), []);
        await tvShot(tv, 'home-signed-out');
        const w0 = await tvWalk(tv, '#ytc-side .rl,.tvpc .tvb');
        assert.deepEqual([w0.dead, w0.missing], [[], []], `home signed out walk ${W}`);
        // Back from the content focuses the rail (this page's item); Back on the rail at Home leaves the app.
        await tv.focus('.tvpc .tvb');
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.equal((await focusedInfo(tv)).nav, 'home');
        assert.ok(await tv.evaluate(() => document.getElementById('ytc-side').matches(':focus-within')), 'the rail opened');
        await tvShot(tv, 'rail-open');
        assert.deepEqual(await tvAudit(tv, `rail open ${W}`), []);
        const railW = await tv.evaluate(() => document.querySelector('#ytc-side .rl[data-nav=subs]').getBoundingClientRect().width);
        assert.ok(Math.abs(railW - 252 * H / 1080) <= 1, `the open rail shows the labels: items 252 px wide at 1080p (${railW})`);
        const gridX = await tv.evaluate(() => document.querySelector('.tvcd').getBoundingClientRect().left);
        assert.deepEqual(await back(tv), { handled: false, exit: true }, 'Back on the rail at Home leaves the app');
        // → from the rail returns to where the focus was in the content.
        await tv.keyboard.press('ArrowRight');
        assert.ok(await tv.evaluate(() => document.activeElement.matches('.tvpc .tvb')), '→ leaves the rail back to the content');
        assert.equal(await tv.evaluate(() => document.querySelector('.tvcd').getBoundingClientRect().left), gridX, 'the content never moved under the rail');

        // Signed in: Home's feed with YouTube's chips, five cards per row.
        HOME.rich = true; FX.homeChips = true;
        await tvGo(tv, 'signed', 'https://www.youtube.com/');
        await tv.waitForSelector('#ytc-list .c a.t');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-list a.t'), null, { timeout: 5000 });
        await tv.waitForSelector('#ytc-fx-tc .fx-ch');
        await tv.waitForTimeout(300);
        await tvShot(tv, 'home');
        const cols = await tv.evaluate(() => { const xs = [...document.querySelectorAll('#ytc-list .c')].map(c => c.offsetTop); return xs.filter(y => y === xs[0]).length; });
        assert.equal(cols, 5, 'five cards per row');
        assert.deepEqual(await tvRowEnd(tv), [], `Home's row end ${W}`);
        // The rail hugs the left edge: its tiles start at the safe edge (≤ 50 px at 1080p), their icons centred
        // (±1 px at 1080p), YouTube's logo centred over them and on the first row of the page.
        const rail = await tv.evaluate(() => {
          const r = e => e.getBoundingClientRect();
          const tiles = [...document.querySelectorAll('#ytc-side .rl')].map(t => { const a = r(t), i = r(t.querySelector('svg')); return { l: a.left, w: a.width, h: a.height, dx: (i.left + i.right) / 2 - (a.left + a.right) / 2, dy: (i.top + i.bottom) / 2 - (a.top + a.bottom) / 2, iw: i.width }; });
          const logo = r(document.querySelector('#ytc-side .rmk .lmk')), chips = r(document.querySelector('#ytc-fx-tc .fx-ch'));
          return { tiles, logoX: (logo.left + logo.right) / 2, logoY: (logo.top + logo.bottom) / 2, chipY: (chips.top + chips.bottom) / 2 };
        });
        const k = H / 1080;
        assert.ok(rail.tiles.every(t => Math.abs(t.l - 48 * k) <= 1 && Math.abs(t.w - 60 * k) <= 1 && Math.abs(t.w - t.h) < 1 && Math.abs(t.iw - 30 * k) <= 1), `rail tiles on the safe edge (48 px at 1080p), 60 px with 30 px icons, ±1 px: ${JSON.stringify(rail.tiles)}`);
        assert.ok(rail.tiles.every(t => Math.abs(t.dx) <= k && Math.abs(t.dy) <= k), `rail icons centred: ${JSON.stringify(rail.tiles)}`);
        const tileX = rail.tiles[0].l + rail.tiles[0].w / 2;
        assert.ok(Math.abs(rail.logoX - tileX) <= k && Math.abs(rail.logoY - rail.chipY) <= k, `the logo over the icons, on the chips' row: ${JSON.stringify([rail.logoX, tileX, rail.logoY, rail.chipY])}`);
        // The content starts right after the rail (130–140 px at 1080p) and the cards keep a readable scale.
        const cards = await tv.evaluate(() => { const c = document.querySelector('#ytc-list .c'), t = c.querySelector('.t'), m = c.querySelector('.m'), b = c.querySelector('.th b'); return { l: c.getBoundingClientRect().left, w: c.getBoundingClientRect().width, t: parseFloat(getComputedStyle(t).fontSize), m: parseFloat(getComputedStyle(m).fontSize), b: b ? parseFloat(getComputedStyle(b).fontSize) : 99 }; });
        assert.ok(Math.abs(cards.l - 138 * k) <= 1 && cards.t >= 22 * k && cards.t <= 24 * k && cards.m >= 19 * k && cards.m <= 20 * k && cards.b >= 17 * k, `cards: ${JSON.stringify(cards)}`);
        await tv.evaluate(() => document.activeElement.blur());
        await tv.waitForTimeout(400);
        geo[W] = await tvGeometry(tv, '#ytc-side .rl,.rmk .lmk,#ytc-list .c:nth-child(-n+8),#ytc-list .c:nth-child(-n+8) .th'); // (text-sized boxes like chips follow the font's own metrics)
        await tv.focus('#ytc-list .c a.t');
        assert.deepEqual(await tvAudit(tv, `home ${W}`), []);
        const w1 = await tvWalk(tv, '#ytc-side .rl,#ytc-fx-tc .fx-ch,#ytc-list a.t', 90);
        assert.deepEqual(w1.dead, [], `home walk: dead ends ${W}`);
        assert.deepEqual(w1.missing.filter(m => !/^A\.t/.test(m)), [], `home walk: unreachable ${W}`);
        // The grid walk: ↓ keeps the focused row where the first row sits (instant), ↑ comes back; the focus is
        // never off screen; a held key never waits.
        await tv.evaluate(() => scrollTo(0, 0));
        await tv.focus('#ytc-list .c:nth-child(2) a.t');
        // (Layout positions: the focused card's scale is still settling when a key lands.)
        const cardY = () => tv.evaluate(() => { let e = document.activeElement.closest('.c'), y = 0; for (let p = e; p; p = p.offsetParent) y += p.offsetTop; return y - scrollY; });
        const rowY = await cardY();
        const ys = [];
        for (let i = 0; i < 3; i++) { await tv.keyboard.press('ArrowDown'); ys.push(await cardY()); }
        assert.ok(ys.every(y => Math.abs(y - rowY) < 1.5), `the focused row keeps its place: ${rowY} → ${ys}`);
        assert.deepEqual(await tvAudit(tv, `home scrolled ${W}`), []);
        await tvShot(tv, 'home-scrolled');
        const t0 = Date.now();
        for (let i = 0; i < 12; i++) await tv.keyboard.press(i % 2 ? 'ArrowLeft' : 'ArrowRight', { delay: 0 });
        assert.ok(Date.now() - t0 < 12 * 120, `12 moves keep up with key repeat (${Date.now() - t0} ms)`);
        for (let i = 0; i < 3; i++) await tv.keyboard.press('ArrowUp');
        { const y = await cardY(), sy = await tv.evaluate(() => scrollY); assert.ok(Math.abs(y - rowY) < 1.5 && sy < 2, `back on the first row at the top: ${JSON.stringify([y, rowY, sy])}`); }
        await tv.keyboard.press('ArrowUp');
        assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-fx-tc .fx-ch')), '↑ from the first row reaches the chips');
        assert.deepEqual(await tvAudit(tv, `chips ${W}`), []);
        await tvShot(tv, 'home-chips');
        // A card opened and Back: the card has the focus again.
        await tv.keyboard.press('ArrowDown');
        await tv.keyboard.press('ArrowRight');
        const opened = (await focusedInfo(tv)).href;
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => location.pathname === '/watch');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.id === 'ytc-seek', null, { timeout: 8000 });
        assert.equal(await tv.evaluate(() => getComputedStyle(document.getElementById('ytc-player')).position), 'fixed', 'the video plays full screen at once');
        await tv.goBack({ waitUntil: 'commit' });
        await tv.waitForFunction(h => document.activeElement && document.activeElement.getAttribute('href') === h, opened, { timeout: 5000 });
        HOME.rich = false; FX.homeChips = false;

        // Loading: the skeleton grid in the final geometry (a slow search).
        IMG.search = 2500;
        try {
          await tvGo(tv, 'code', 'https://www.youtube.com/results?search_query=slow+tv');
          await tv.waitForSelector('.grid .c.sk');
          await tvShot(tv, 'loading');
          assert.deepEqual(await tvAudit(tv, `loading ${W}`), []);
          // The skeleton in the final geometry: a result card (title, channel, views) as tall as its skeleton.
          const skH = await tv.evaluate(() => document.querySelector('.grid .c.sk').getBoundingClientRect().height);
          await tv.waitForSelector('#ytc-list .c:not(.sk) a.t', { timeout: 8000 });
          const realH = await tv.evaluate(() => { const c = [...document.querySelectorAll('#ytc-list .c:not(.sk)')].find(c => { const t = c.querySelector('.t'); return c.querySelectorAll('.m').length === 2 && t.getBoundingClientRect().height > parseFloat(getComputedStyle(t).lineHeight) * 1.5; }); return c ? c.getBoundingClientRect().height : null; });
          assert.ok(realH !== null, 'a result card with a two-line title, its channel and views');
          assert.ok(Math.abs(skH - realH) <= 1, `skeleton ${skH} px, result card ${realH} px`);
        } finally { IMG.search = 0; }

        // Search opens its own remote keyboard; Back reveals recent searches.
        await tvGo(tv, 'code', 'https://www.youtube.com/');
        await sw.evaluate(now => chrome.storage.local.set({ yourTubeHistoryV1: { version: 1, startedAt: now - 9e5, entries: [{ query: 'lofi', at: now }, { query: 'roblox tycoon tutorial', at: now - 1000 }, { query: 'rain sounds', at: now - 2000 }] } }), Date.now());
        await tv.reload({ waitUntil: 'commit' });
        await tv.waitForSelector('#ytc-main section:not(:has(.sk))');
        await tv.waitForTimeout(300);
        await tv.focus('#ytc-side .rl[data-nav=search]');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-tvk .tvk-keys button'));
        assert.equal(await tv.getAttribute('#ytc', 'data-tvq'), '1');
        await tv.waitForTimeout(300);
        { const q = await tv.evaluate(() => document.getElementById('ytc-tvquery').getBoundingClientRect().height); assert.ok(q >= W / 80 * 2.45 && q <= W / 80 * 2.55, `the query control, as tall as a rail tile (${q} px)`); }
        assert.deepEqual(await tvAudit(tv, `search ${W}`), []);
        await tvShot(tv, 'search');
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvquery');
        await tv.keyboard.press('ArrowDown');
        assert.ok(await tv.evaluate(() => document.activeElement.closest('#ytc-sug') !== null), '↓ from the field walks the list');
        const sugTop = (await focusedInfo(tv)).y;
        await tvShot(tv, 'search-list');
        assert.deepEqual(await tvAudit(tv, `search list ${W}`), []);
        await tv.keyboard.press('ArrowDown');
        assert.ok((await focusedInfo(tv)).y > sugTop, 'the next row');
        await tv.keyboard.press('ArrowUp'); await tv.keyboard.press('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvquery');
        // A physical keyboard can also type, with suggestions under the recent searches.
        await tv.keyboard.type('rob');
        await tv.waitForSelector('#ytc-sug .sg:not(.hs)', { timeout: 5000 });
        await tvShot(tv, 'search-typed');
        assert.deepEqual(await tvAudit(tv, `search typed ${W}`), []);
        assert.ok(await tv.evaluate(() => [...document.querySelectorAll('#ytc-sug .sg')].filter(e => e.getClientRects().length).every(e => e.getBoundingClientRect().bottom <= innerHeight * 0.965 + 1)), 'every row inside the safe area');
        // Back closes the screen and the focus returns to the rail's Search.
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.equal(await tv.getAttribute('#ytc', 'data-tvq'), '');
        assert.equal((await focusedInfo(tv)).nav, 'search');
        // A search from the screen: the results grid takes the focus; the query stays on top as the field.
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-tvk .tvk-keys button'));
        for (let i = 0; i < 3; i++) await tv.keyboard.press('Backspace');
        await tv.keyboard.type('lofi');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => location.pathname === '/results');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-list a.t'), null, { timeout: 5000 });
        assert.equal(await tv.getAttribute('#ytc', 'data-tvq'), '');
        await tv.waitForTimeout(200);
        assert.deepEqual(await tvAudit(tv, `results ${W}`), []);
        assert.deepEqual(await tvRowEnd(tv), [], `the results' row end ${W}`);
        await tvShot(tv, 'search-results');
        await tv.keyboard.press('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvquery', '↑ from the results reaches the query');
        assert.equal(await tv.getAttribute('#ytc', 'data-tvq'), '1', 'the field opens the search screen');
        await back(tv);

        // Channel and playlist pages.
        await tvGo(tv, 'code', 'https://www.youtube.com/channel/UCaZO-66UcqyVT9btiqdDryA');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-list a.t'), null, { timeout: 5000 });
        await tv.waitForTimeout(200);
        assert.deepEqual(await tvAudit(tv, `channel ${W}`), []);
        await tvShot(tv, 'channel');
        await tvGo(tv, 'code', 'https://www.youtube.com/playlist?list=PLe2eTVplaylist');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-list a.t'), null, { timeout: 5000 });
        await tv.waitForTimeout(200);
        assert.deepEqual(await tvAudit(tv, `playlist ${W}`), []);
        await tvShot(tv, 'playlist');
        // Signed out on an account page: the calm sign-in card. A page that failed: the reason and Try again.
        await tvGo(tv, 'code', 'https://www.youtube.com/playlist?list=WL');
        await tv.waitForSelector('.tvpc [data-act=tv-signin]');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('.tvpc .tvb'));
        assert.deepEqual(await tvAudit(tv, `needs sign-in ${W}`), []);
        await tvShot(tv, 'needs-sign-in');
        await tvGo(tv, 'signed', 'https://www.youtube.com/feed/history');
        await tv.waitForSelector('.tvpc a.tvb[data-act=retry]');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('.tvpc .tvb'));
        assert.deepEqual(await tvAudit(tv, `failed page ${W}`), []);
        await tvShot(tv, 'page-failed');
      }
      sameGeometry(geo[960], geo[1920], 'Home');
      assert.deepEqual(tv.errors, []);
    } finally { HOME.rich = false; FX.homeChips = false; HOME.delay = 0; await tvClose(tv); }
  });

  await step('TV sign-in and account screens (960×540 and 1920×1080): full screen, the code large, the URL and its QR code, Back first and focused', async () => {
    const tv = await tvOpen();
    try {
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        for (const mode of ['code', 'nocode', 'signed']) {
          await tvGo(tv, mode, 'https://www.youtube.com/');
          await tv.waitForSelector('#ytc-acct .rl');
          await tv.focus('#ytc-acct .rl');
          await tv.keyboard.press('Enter');
          await tv.waitForSelector('#ytc-tvsign.on');
          if (mode === 'code') await tv.waitForSelector('#ytc-tvsign .tvc', { timeout: 5000 });
          if (mode === 'nocode') await tv.waitForSelector('#ytc-tvsign [data-act=tv-newcode]');
          if (mode === 'signed') await tv.waitForSelector('#ytc-tvsign [data-act=tv-signout]');
          await tv.waitForTimeout(300);
          assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-close', 'Back is focused');
          const box = await tv.evaluate(() => { const r = document.querySelector('#ytc-tvsign').getBoundingClientRect(); return [r.width, r.height]; });
          assert.deepEqual(box, [W, H], 'a full screen');
          if (mode === 'code') {
            const code = await tv.evaluate(() => { const c = document.querySelector('.tvc'); return { text: c.textContent, size: parseFloat(getComputedStyle(c).fontSize) }; });
            assert.ok(code.text === 'WMB-QZX-TPR' && code.size >= H * 0.075, `the code is large: ${JSON.stringify(code)}`);
            assert.equal(await tv.textContent('.tvu'), 'google.com/device', 'the URL the device flow returned');
            const qr = await tv.evaluate(() => { const s = document.querySelector('.tvq'); const r = s.getBoundingClientRect(); return { w: r.width, d: s.querySelector('path').getAttribute('d').length, vb: s.getAttribute('viewBox') }; });
            assert.ok(qr.w >= H * 0.3 && qr.d > 200 && (+qr.vb.split(' ')[2] - 29) % 4 === 0, `a QR code: ${JSON.stringify(qr)}`);
          }
          assert.deepEqual(await tvAudit(tv, `sign-in ${mode} ${W}`), []);
          await tvShot(tv, mode === 'signed' ? 'account' : `signin-${mode}`);
          const w = await tvWalk(tv, '#ytc-tvsign button');
          assert.deepEqual([w.dead, w.missing], [[], []], `sign-in ${mode} walk ${W}`);
          await tv.focus('#ytc-tvsign [data-act=tv-close]');
          assert.deepEqual(await back(tv), { handled: true, exit: false });
          await tv.waitForSelector('#ytc-tvsign:not(.on)', { state: 'attached' });
          assert.equal((await focusedInfo(tv)).nav, 'acct', 'Back returns to the rail item');
        }
      }
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV About & updates (960×540 and 1920×1080): the rail\'s last item, signed out too; versions and the last check; Check for updates → up to date / failed with the reason and Retry / Update ready → Update now reloads into the new interface / a new app → Install update with its progress, then the installer; an app too old to install from here; the Update ready notice', async () => {
    const tv = await tvOpen();
    // The shell's answer (shell.rs `shell_info`): bundle seq, update state, an offered APK.
    const info = (seq, update, offer = null) => JSON.stringify({ version: '0.1.0', version_code: 1000, bundle: { seq, client: '0.9.' + (seq / 100), source: seq > 100 ? 'release' : 'builtin' }, update, offer, repo: 'o/r' });
    // (Kept across reloads too: e2e:tvshell is merged into the test shell on every load.)
    const answer = (cmd, json) => tv.evaluate(([c, j]) => { for (const k of ['ytc:shell', 'e2e:tvshell']) { const t = JSON.parse(localStorage.getItem(k) || '{}'); if (j == null) delete t[c]; else t[c] = j; localStorage.setItem(k, JSON.stringify(t)); } }, [cmd, json]);
    const screen = () => tv.evaluate(() => ({ head: document.querySelector('#ytc-tvabout .tvus b')?.textContent, text: document.getElementById('ytc-tvabout').textContent, focus: document.activeElement.dataset.act || document.activeElement.className }));
    const press = async (act, until) => { await tv.focus(`#ytc-tvabout [data-act=${act}]`); await tv.keyboard.press('Enter'); await tv.waitForFunction(re => new RegExp(re).test(document.querySelector('#ytc-tvabout .tvus b')?.textContent || ''), until, { timeout: 5000 }); await tv.waitForTimeout(250); };
    try {
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        await tv.evaluate(() => localStorage.removeItem('e2e:tvshell')).catch(() => {});
        await tvGo(tv, 'code', 'https://www.youtube.com/');
        await tv.waitForSelector('.tvpc .tvb');
        // The rail's last item, under Sign in (signed out).
        assert.deepEqual(await tv.$$eval('#ytc-side .rla .rl', a => a.map(e => e.dataset.nav)), ['acct', 'about']);
        await tv.focus('#ytc-side .rl[data-nav=acct]');
        await tv.keyboard.press('ArrowDown');
        assert.equal((await focusedInfo(tv)).nav, 'about', '↓ from Sign in');
        assert.deepEqual(await tvAudit(tv, `rail about ${W}`), []);
        await tvShot(tv, 'rail-about');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-tvabout.on');
        await tv.waitForTimeout(300);
        let sc = await screen();
        // The header is YouTube's logo and name; the app's own name is Redline for TV, with its versions.
        assert.equal(await tv.textContent('.tvp .rmk b'), 'YouTube');
        assert.ok(sc.focus === 'tv-check' && /Redline for TV 0\.1\.0/.test(sc.text) && !/Lumen/.test(sc.text) && /0\.9\.0 · build 100/.test(sc.text) && /not yet/.test(sc.text), `versions, Check for updates focused: ${JSON.stringify(sc)}`);
        assert.deepEqual(await tvAudit(tv, `about ${W}`), []);
        await tvShot(tv, 'about');
        // Up to date.
        await answer('shell_check', info(100, { state: 'ok', at: Date.now() - 60e3, needs_shell: false }));
        await press('tv-check', 'up to date');
        sc = await screen();
        assert.ok(/1 min ago · up to date/.test(sc.text) && sc.focus === 'tv-check', JSON.stringify(sc));
        await tvShot(tv, 'about-uptodate');
        // Failed: the reason in words, the raw one under it, Retry focused.
        await answer('shell_check', info(100, { state: 'failed', at: Date.now(), error: 'release lookup: HTTP 403' }));
        await press('tv-check', 'limit');
        sc = await screen();
        assert.ok(/HTTP 403/.test(sc.text) && /Retry/.test(sc.text) && sc.focus === 'tv-check', JSON.stringify(sc));
        assert.deepEqual(await tvAudit(tv, `about failed ${W}`), []);
        await tvShot(tv, 'about-failed');
        // Update ready (the shell installed a newer interface): Update now loads it, still signed in the same.
        await answer('shell_check', info(200, { state: 'ok', at: Date.now(), needs_shell: false, installed: { client: '0.9.2', seq: 200 } }));
        await press('tv-check', 'Update ready');
        sc = await screen();
        assert.ok(sc.focus === 'tv-apply' && /build 200/.test(sc.text), JSON.stringify(sc));
        await tvShot(tv, 'about-ready');
        await answer('shell_info', info(200, { state: 'ok', at: Date.now(), needs_shell: false }));
        await tv.evaluate(() => { window.__e2eOld = 1; });
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => !window.__e2eOld && document.querySelector('#ytc-side .rl[data-nav=about]'), null, { timeout: 8000 });
        await tv.focus('#ytc-side .rl[data-nav=about]');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => /up to date/.test(document.querySelector('#ytc-tvabout .tvus b')?.textContent || ''), null, { timeout: 5000 });
        assert.match((await screen()).text, /0\.9\.2 · build 200/, 'the new interface runs');
        // A new app: Install update, its download progress, then the installer.
        await answer('shell_check', info(200, { state: 'ok', at: Date.now(), needs_shell: false, apk: { version: '0.2.0', version_code: 2000 } }));
        await press('tv-check', 'Redline 0\\.2\\.0 is available');
        assert.equal((await screen()).focus, 'tv-install');
        await tvShot(tv, 'about-app');
        await answer('shell_install', info(200, { state: 'downloading', at: Date.now(), got: 4e6, total: 1e7, apk: { version: '0.2.0' } }));
        await answer('shell_info', info(200, { state: 'downloading', at: Date.now(), got: 4e6, total: 1e7, apk: { version: '0.2.0' } }));
        await press('tv-install', 'Downloading the update… 40 %');
        const pg = await tv.evaluate(() => getComputedStyle(document.querySelector('#ytc-tvabout .tvpg')).getPropertyValue('--pg'));
        assert.equal(pg.trim(), '0.4');
        assert.deepEqual(await tvAudit(tv, `about downloading ${W}`), []);
        await tvShot(tv, 'about-downloading');
        await answer('shell_info', info(200, { state: 'installing', at: Date.now(), apk: { version: '0.2.0' } }));
        await tv.waitForFunction(() => /installer/.test(document.querySelector('#ytc-tvabout .tvus b')?.textContent || ''), null, { timeout: 5000 });
        await tvShot(tv, 'about-installing');
        // An app from before shell_install: the reason, nothing else breaks.
        await answer('shell_info', info(200, { state: 'ok', at: Date.now(), needs_shell: false, apk: { version: '0.2.0' } }));
        await answer('shell_install', null);
        await answer('error', 'unknown command shell_install');
        await tv.waitForFunction(() => document.querySelector('#ytc-tvabout [data-act=tv-install]'), null, { timeout: 5000 }).catch(() => {});
        await tv.focus('#ytc-tvabout [data-act=tv-install]').catch(() => {});
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => /its own update dialog/.test(document.getElementById('ytc-tvabout').textContent), null, { timeout: 5000 });
        // Back closes it, the focus returns to the rail item.
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.equal((await focusedInfo(tv)).nav, 'about');
        // The notice: a new app offered when the page starts → a small notice top right and the rail's dot; its
        // button opens the screen (and it goes).
        await answer('error', 'HTTP 503');
        await tv.evaluate(j => localStorage.setItem('e2e:tvshell', JSON.stringify({ shell_info: j })), info(100, { state: 'ok', at: Date.now(), apk: { version: '0.2.0' } }));
        await tvGo(tv, 'code', 'https://www.youtube.com/');
        await tv.waitForSelector('#ytc-tvup.on');
        await tv.waitForTimeout(300);
        assert.ok(await tv.evaluate(() => document.querySelector('#ytc-side .rl[data-nav=about]').classList.contains('upd')), 'the rail item carries the dot');
        await tv.focus('#ytc-tvup .tvb');
        assert.deepEqual(await tvAudit(tv, `notice ${W}`), []);
        await tvShot(tv, 'update-notice');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-tvabout.on');
        assert.equal(await tv.locator('#ytc-tvup.on').count(), 0, 'the notice went');
        await tv.evaluate(() => localStorage.removeItem('e2e:tvshell'));
      }
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV About & updates → Developer (960×540): hidden by default; → five times on the last button shows it, Off; Turn on → the page loads again into telemetry and returns to the screen, On with the address; an address submitted is saved; an app without telemetry says so', async () => {
    const tv = await tvOpen();
    const tel = (on, extra = {}) => JSON.stringify({ on, url: 'http://192.168.1.20:47321', dev: true, queued: 0, sent: on ? 3 : 0, error: null, ...extra });
    const answer = (cmd, json) => tv.evaluate(([c, j]) => { for (const k of ['ytc:shell', 'e2e:tvshell']) { const t = JSON.parse(localStorage.getItem(k) || '{}'); if (j == null) delete t[c]; else t[c] = j; localStorage.setItem(k, JSON.stringify(t)); } }, [cmd, json]);
    const text = () => tv.evaluate(() => document.getElementById('ytc-tvabout').textContent);
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tv.evaluate(() => localStorage.removeItem('e2e:tvshell')).catch(() => {});
      await tvGo(tv, 'code', 'https://www.youtube.com/');
      await tv.waitForSelector('.tvpc .tvb');
      await answer('tel_info', tel(false));
      await tv.focus('#ytc-side .rl[data-nav=about]');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-tvabout.on');
      await tv.waitForTimeout(300);
      assert.equal(await tv.locator('#ytc-tvabout .tvdv').count(), 0, 'hidden by default (telemetry off)');
      assert.ok(!/telemetry/i.test(await text()), 'no word of it on the screen');
      await tv.focus('#ytc-tvabout [data-act=tv-about-close]');
      for (let i = 0; i < 4; i++) await tv.keyboard.press('ArrowRight');
      await tv.waitForTimeout(200);
      assert.equal(await tv.locator('#ytc-tvabout .tvdv').count(), 0, 'not after four');
      await tv.keyboard.press('ArrowRight');
      await tv.waitForSelector('#ytc-tvabout .tvdv');
      await tv.waitForTimeout(200);
      assert.match(await text(), /Developer · speed telemetry.*Off\./s);
      assert.equal((await focusedInfo(tv)).cls, 'tvb', 'Turn on takes the focus: ' + JSON.stringify(await focusedInfo(tv)));
      assert.equal(await tv.inputValue('#ytc-tvdc-q'), '192.168.1.20:47321');
      assert.deepEqual(await tvAudit(tv, 'about developer 960'), []);
      await tvShot(tv, 'about-developer');
      // Turn on: the shell answers that the page must load again (its script changed); it comes back here, On.
      await answer('tel_set', tel(true, { reload: true }));
      await answer('tel_info', tel(true));
      await tv.evaluate(() => { window.__e2eOld = 1; });
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => !window.__e2eOld && document.querySelector('#ytc-tvabout.on .tvdv'), null, { timeout: 8000 });
      await tv.waitForTimeout(300);
      assert.match(await text(), /On: this TV's speed measurements go to the debug center at 192\.168\.1\.20:47321 \(3 sent, 0 waiting\)/);
      assert.equal(await tv.locator('#ytc-tvabout [data-act=tv-tel-off]').count(), 1);
      // A new address, submitted from the field (the TV keyboard's action key): saved, no reload.
      await answer('tel_set', tel(true, { url: 'http://192.168.1.30:47321' }));
      await tv.evaluate(() => { window.__e2eOld = 1; });
      await tv.fill('#ytc-tvdc-q', '192.168.1.30');
      await tv.press('#ytc-tvdc-q', 'Enter');
      await tv.waitForFunction(() => /Saved\./.test(document.getElementById('ytc-tvabout').textContent), null, { timeout: 5000 });
      assert.ok(await tv.evaluate(() => window.__e2eOld === 1), 'no reload for an address');
      assert.equal(await tv.inputValue('#ytc-tvdc-q'), '192.168.1.30:47321');
      // A refusal (an address outside the network) is shown as the reason.
      await answer('tel_set', null);
      await answer('error', 'only an address on your own network (e.g. 192.168.1.20)');
      await tv.fill('#ytc-tvdc-q', '8.8.8.8');
      await tv.press('#ytc-tvdc-q', 'Enter');
      await tv.waitForFunction(() => /only an address on your own network/.test(document.querySelector('#ytc-tvabout .tvdv .tvun')?.textContent || ''), null, { timeout: 5000 });
      // Back closes the screen; the next time it opens, the section stays (telemetry is on).
      assert.deepEqual(await back(tv), { handled: true, exit: false });
      await tv.focus('#ytc-side .rl[data-nav=about]');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-tvabout.on .tvdv');
      // An app from before telemetry (no tel_info): revealed, it says so; nothing else breaks.
      await answer('tel_info', null);
      await answer('error', 'unknown command tel_info');
      await tvGo(tv, 'code', 'https://www.youtube.com/');
      await tv.waitForSelector('.tvpc .tvb');
      await tv.focus('#ytc-side .rl[data-nav=about]');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-tvabout.on');
      await tv.focus('#ytc-tvabout [data-act=tv-about-close]');
      for (let i = 0; i < 5; i++) await tv.keyboard.press('ArrowRight');
      await tv.waitForFunction(() => /cannot send telemetry/.test(document.querySelector('#ytc-tvabout .tvdv')?.textContent || ''), null, { timeout: 5000 });
      await tv.evaluate(() => localStorage.removeItem('e2e:tvshell'));
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV bundle (format 2): content.js without the Wasm boots the way the app injects it (shim + bundle at document start, no extension), its Wasm streamed from /__lumen/client.wasm as the shell serves it; a missing Wasm falls back to YouTube\'s page', async () => {
    const root = path.join(__dirname, '..'), bdir = path.join(root, 'tv', 'shell', 'src-tauri', 'bundle');
    const loader = () => fs.existsSync(path.join(bdir, 'client.wasm')) && fs.readFileSync(path.join(bdir, 'content.js'), 'utf8').includes('/__lumen/client.wasm');
    if (!loader()) require('node:child_process').execFileSync(path.join(root, 'build.sh'), ['--tv'], { cwd: root, stdio: 'ignore' });
    const js = fs.readFileSync(path.join(bdir, 'content.js'), 'utf8');
    assert.ok(js.length < 700_000 && !js.includes('function payload(){/*'), 'no Wasm inside content.js (' + js.length + ' bytes)');
    const wasm = fs.readFileSync(path.join(bdir, 'client.wasm'));
    const shim = fs.readFileSync(path.join(root, 'tv', 'shim', 'shim.js'), 'utf8');
    const css = fs.readFileSync(path.join(bdir, 'client.css'), 'utf8');
    // As tv/shell/src-tauri/src/script.rs composes it, with a stand-in for the shell's IPC.
    const script = `globalThis.__TAURI_INTERNALS__ = { invoke: (_, a) => Promise.resolve({ auth_token: '{"token":null}', shell_info: '{"version":"0.3.0","version_code":3000,"bundle":{"seq":1,"client":"x","source":"builtin"},"update":{"state":"idle"},"offer":null}', store_get: '{}', store_set: '{}', ready_take: '{"record":null}' }[a.cmd] || '{}') };
const ytcRun = () => { 'use strict'; const CFG = ${JSON.stringify({ version: '0.3.0', version_name: 'e2e', tel: false })}; const CSS = ${JSON.stringify({ client: css, native: '' })}; const ytcClient = () => {\n${js}\n};\n${shim}\n};
// (Playwright runs init scripts before the root element exists; the WebView's document-start script runs once it does.)
if (document.documentElement) ytcRun(); else new MutationObserver((_, o) => { if (document.documentElement) { o.disconnect(); ytcRun(); } }).observe(document, { childList: true });`;
    const browser = await chromium.launch(chromiumOptions());
    try {
      for (const served of [true, false]) {
        const c = await browser.newContext({ viewport: { width: 960, height: 540 } });
        await c.route('**/*', r => {
          const u = new URL(r.request().url());
          if (u.hostname === 'www.youtube.com' && u.pathname === '/__lumen/client.wasm') return served ? r.fulfill({ status: 200, contentType: 'application/wasm', body: wasm }) : r.fulfill({ status: 404, body: '' });
          // The shell's own document for the client's pages (tv/core rules::CLIENT_DOCUMENT; YouTube's own for ?yt-native=1).
          if (r.request().isNavigationRequest() && u.hostname === 'www.youtube.com' && !u.searchParams.has('yt-native')) return r.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Redline</title></head><body></body></html>' });
          return handle(r);
        });
        await c.addInitScript(script);
        const p = await c.newPage();
        const errors = []; p.on('pageerror', e => errors.push(String(e)));
        const logs = []; p.on('console', m => logs.push(m.text().slice(0, 300)));
        await p.goto('https://www.youtube.com/results?search_query=tv+format+two', { waitUntil: 'commit' });
        if (served) {
          await p.waitForSelector('#ytc-list .c a.t', { timeout: 10000 }).catch(e => { throw new Error(e.message + ' ' + JSON.stringify(logs.slice(0, 5))); });
          const d = await p.evaluate(() => ({ ...document.documentElement.dataset }));
          assert.ok(d.ytcTv === '1' && +d.ytcWasm >= +d.ytcScript && +d.ytcShell >= +d.ytcWasm, 'booted in TV mode from the served Wasm: ' + JSON.stringify(d));
          assert.deepEqual(errors, []);
        } else {
          await p.waitForURL(/yt-native=1/, { timeout: 10000 });
        }
        await c.close();
      }
    } finally { await browser.close(); }
  });

  await step('TV shim (tv/shim/shim.js): the client\'s reports (ytcPerf, ytcIncident) reach the shell (tel_send) only while telemetry is on; off they go nowhere', async () => {
    const vm = require('node:vm');
    const shim = fs.readFileSync(path.join(__dirname, '..', 'tv', 'shim', 'shim.js'), 'utf8');
    const run = tel => {
      const calls = [];
      const g = {
        location: { hostname: 'www.youtube.com', pathname: '/', search: '' },
        document: { adoptedStyleSheets: [] }, navigator: {}, console,
        CSSStyleSheet: class { replaceSync() {} }, addEventListener() {},
        __TAURI_INTERNALS__: { invoke: (cmd, a) => { calls.push([cmd, a.cmd, a.json]); return Promise.resolve('{}'); } },
      };
      g.window = g.top = g.globalThis = g;
      vm.createContext(g);
      vm.runInContext(`(() => { const CFG = ${JSON.stringify({ version: '0.2.0', version_name: 'x', tel })}; const CSS = { client: '', native: '' }; const ytcClient = () => {
        chrome.runtime.sendMessage({ ytcPerf: '{"kind":"perf","schema":2,"events":[]}' });
        chrome.runtime.sendMessage({ ytcIncident: '{"kind":"js-error"}' });
        chrome.runtime.sendMessage('ytc-dev-check');
      };\n${shim}\n})();`, g);
      return calls.filter(c => c[1] === 'tel_send');
    };
    assert.deepEqual(run(false), [], 'off: nothing is handed to the shell');
    const on = run(true);
    assert.deepEqual(on.map(c => [c[0], JSON.parse(c[2])]), [['ytc', { perf: '{"kind":"perf","schema":2,"events":[]}' }], ['ytc', { incident: '{"kind":"js-error"}' }]]);
  });

  await step('TV focus engine: a held arrow passes cards without prefetching each one (only where it rests); the walk lands on the same cards as single presses', async () => {
    const tv = await tvOpen();
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', 'https://www.youtube.com/results?search_query=tv+held+key');
      await tv.waitForSelector('#ytc-list .c a.t');
      await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-list a.t'), null, { timeout: 5000 });
      await tv.waitForTimeout(1500); // the speculative prefetch of the cards in view has run
      const cards = await tv.$$eval('#ytc-list .c a.t', a => a.map(e => new URL(e.href).searchParams.get('v')));
      assert.ok(cards.length >= 20, 'enough cards: ' + cards.length);
      const at = () => tv.evaluate(() => new URL(document.activeElement.href).searchParams.get('v'));
      const prefetched = since => new Set(log.api.slice(since).filter(r => r.ep === 'next' || r.ep === 'player').map(r => r.body.videoId).filter(Boolean));
      const a0 = log.api.length;
      // Held: one press, then the key repeats (keyboard.down on a held key repeats), released on the fourth row down.
      await tv.keyboard.down('ArrowDown');
      for (let i = 0; i < 2; i++) { await tv.waitForTimeout(60); await tv.keyboard.down('ArrowDown'); }
      const rest = await at();
      await tv.waitForTimeout(300);
      const before = prefetched(a0);
      await tv.keyboard.up('ArrowDown');
      await tv.waitForTimeout(600);
      // (Five cards per row: the press went to the second row, the repeats passed the third and rest on the fourth.)
      const passed = [cards[10]];
      assert.equal(rest, cards[15], 'three rows down, the same card as three presses would reach');
      for (const v of passed) assert.ok(!before.has(v), 'a card passed by the held key is not prefetched: ' + v + ' ' + JSON.stringify([...before]));
      assert.ok(prefetched(a0).has(rest), 'where it rests is prefetched once the key is released');
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('debug center (tools/debug-center): 127.0.0.1 only unless started with --lan; with --lan another device can deliver reports and find it, never read them', async () => {
    const dir = path.join(__dirname, '..', 'tools', 'debug-center');
    const bin = path.join(dir, 'target', 'release', 'lumen-debug-center');
    if (!fs.existsSync(bin)) require('node:child_process').execFileSync('cargo', ['build', '--release', '--locked'], { cwd: dir, stdio: 'ignore' });
    const lan = Object.values(os.networkInterfaces()).flat().find(i => i && i.family === 'IPv4' && !i.internal);
    const file = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'ytc-dc-')), 'i.ndjson');
    const start = async (args, port) => {
      const p = require('node:child_process').spawn(bin, [...args, file], { env: { ...process.env, YTC_DEBUG_CENTER_ADDR: `127.0.0.1:${port}` }, stdio: 'ignore' });
      for (let i = 0; i < 50; i++) { try { if ((await fetch(`http://127.0.0.1:${port}/health`)).ok) return p; } catch {} await new Promise(r => setTimeout(r, 50)); }
      p.kill(); throw new Error('debug center did not start');
    };
    const reach = async url => { try { const r = await fetch(url, { signal: AbortSignal.timeout(1500) }); return r.status; } catch { return 0; } };
    const port = 47390 + Math.floor(Math.random() * 8);
    let p = await start([], port);
    try {
      if (lan) assert.equal(await reach(`http://${lan.address}:${port}/health`), 0, 'default: not reachable from the network');
      assert.equal(await reach(`http://127.0.0.1:${port}/perf`), 200);
    } finally { p.kill(); }
    await new Promise(r => setTimeout(r, 200));
    p = await start(['--lan'], port);
    try {
      if (!lan) { console.log('     (no network interface: the --lan half is covered by its unit tests)'); return; }
      const post = await fetch(`http://${lan.address}:${port}/incident`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ build: 'x', platform: 'tv', kind: 'perf', schema: 2, page: 'e2e', events: [{ m: 'ui.act', v: 20, at: Date.now() }] }) });
      assert.equal(post.status, 200, 'a device on the network delivers');
      for (const r of ['/', '/perf', '/perf.json', '/timeline']) assert.equal(await reach(`http://${lan.address}:${port}${r}`), 403, 'the network never reads ' + r);
      const rows = await (await fetch(`http://127.0.0.1:${port}/perf.json?platform=tv`)).json();
      assert.ok(rows.some(x => x.page === 'e2e' && x.platform === 'tv'), 'read here, the TV report is a TV row');
      // Discovery: the TV's UDP probe is answered with the port.
      const sock = require('node:dgram').createSocket('udp4');
      const got = await new Promise(ok => { sock.on('message', m => ok(String(m))); sock.send('LUMEN-DC?', port, lan.address); setTimeout(() => ok(''), 1500); });
      sock.close();
      assert.equal(got, `LUMEN-DC ${port}`);
    } finally { p.kill(); }
  });

  await step('TV player (960×540 and 1920×1080): full screen at once, overlay on any key and gone 4 s after, title/channel/time/bar/60 px round buttons in the safe area, ←/→ scrub with the storyboard frame and one seek, menus as a right panel on the current value, captions bottom-centre above the bar, the skip notice, More info, Up next, Back rules, failure and offline panels', async () => {
    const tv = await tvOpen();
    try {
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=descchaps01`);
        await tv.waitForSelector('#ytc-player video');
        await tv.waitForTimeout(1500);
        await tv.waitForFunction(() => document.activeElement && document.activeElement.id === 'ytc-seek', null, { timeout: 8000 });
        await tv.waitForFunction(() => { const v = document.querySelector('#ytc-player video'); return v && v.currentTime > 0.3; }, null, { timeout: 15000 });
        const full = await tv.evaluate(() => { const r = document.getElementById('ytc-player').getBoundingClientRect(); return [r.left, r.top, r.width, r.height]; });
        assert.deepEqual(full, [0, 0, W, H], 'the player fills the screen');
        assert.equal(await tv.locator('#ytc-player :is([data-act=p-mute],[data-act=p-theater],[data-act=p-fs],.pvol,.pct)').count(), 0, 'no volume, theater, fullscreen or caption sizing');
        // Any key shows the overlay; 4 s without one hides it while playing.
        await tv.keyboard.press('ArrowDown');
        assert.ok(await tv.evaluate(() => !document.getElementById('ytc-player').classList.contains('idle')), 'shown');
        await tv.waitForTimeout(300);
        await tvShot(tv, 'player-overlay');
        const lay = await tv.evaluate(() => {
          const r = s => { const e = document.querySelector(s); const q = e.getBoundingClientRect(); return { l: q.left, t: q.top, r: q.right, b: q.bottom, w: q.width, h: q.height }; };
          return { title: r('#ytc-info .wt'), titleFs: parseFloat(getComputedStyle(document.querySelector('#ytc-info .wt')).fontSize), seek: r('#ytc-seek .psegs'), time: r('#ytc-time'), rem: r('#ytc-trem'), play: r('#ytc-player .prow .pb'), btns: [...document.querySelectorAll('#ytc-player .prow > .pb, #ytc-player .prow .wl')].filter(b => b.checkVisibility()).map(b => { const q = b.getBoundingClientRect(); return { w: q.width, h: q.height }; }), row: [...document.querySelectorAll('#ytc-player .prow > .pb, #ytc-player .prow .wl')].filter(b => b.checkVisibility()).map(b => (b.getAttribute('aria-label') || b.className).replace(/:.*/, '')) };
        });
        assert.ok(lay.title.l >= W * 0.025 - 1 && lay.title.t >= H * 0.035 - 1 && lay.title.t < H * 0.1, `title top-left: ${JSON.stringify(lay.title)}`);
        assert.ok(lay.time.b <= lay.seek.t && lay.rem.r <= W * 0.975 + 1 && lay.rem.l > W / 2, `elapsed / remaining over the bar ends: ${JSON.stringify([lay.time, lay.rem])}`);
        // One left edge: the title, the time and the first button start at the same x.
        assert.ok(Math.abs(lay.title.l - lay.time.l) < 1 && Math.abs(lay.title.l - lay.play.l) < 1, `title, time and buttons aligned: ${JSON.stringify([lay.title.l, lay.time.l, lay.play.l])}`);
        // 4 px at 1080p (6 px focused, a scale of the same bar).
        assert.ok(Math.abs(lay.seek.h - H * 4 / 1080) < 0.6 || Math.abs(lay.seek.h - H * 6 / 1080) < 0.6, `a 4 px bar (${lay.seek.h})`);
        // Round buttons of 60–64 px at 1080p (not the old 96 px), the text pills as tall.
        assert.ok(lay.play.w >= H * 56 / 1080 && lay.play.w <= H * 64 / 1080 && Math.abs(lay.play.w - lay.play.h) < 1, `round buttons of 60–64 px at 1080p (${lay.play.w})`);
        assert.ok(lay.btns.every(b => Math.abs(b.h - lay.play.h) < 1 && b.w <= H * 160 / 1080), `every action as tall as the round ones: ${JSON.stringify(lay.btns)}`);
        // The title overlay: about 32 px, the channel about 22 px and never cut while it fits ("Blende" for "Blender").
        assert.ok(lay.titleFs <= H * 34 / 1080 && lay.titleFs >= H * 30 / 1080, `the title at about 32 px (${lay.titleFs})`);
        // The channel name stays readable over a white frame (the worst picture): ≥ 4.5:1 between its glyphs and the
        // scrim-covered picture right of it, measured on the screenshot's pixels.
        {
          await tv.evaluate(() => { document.querySelector('#ytc-player video').style.visibility = 'hidden'; document.getElementById('ytc-player').style.background = '#fff'; });
          await tv.evaluate(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))));
          assert.ok(await tv.evaluate(() => !document.getElementById('ytc-player').classList.contains('idle')), 'the overlay is still shown');
          const t = await tv.evaluate(() => { const rg = document.createRange(); rg.selectNodeContents(document.querySelector('#ytc-info .wc')); const q = rg.getBoundingClientRect(); return { x: q.left, y: q.top, w: q.width, h: q.height }; });
          const k = H / 1080, text = await tvPixels(tv, t), bg = await tvPixels(tv, { x: t.x + t.w + 12 * k, y: t.y, w: 40 * k, h: t.h });
          const ratio = (Math.max(...text.lum) + 0.05) / (Math.max(...bg.lum) + 0.05);
          await tv.evaluate(() => { document.querySelector('#ytc-player video').style.visibility = ''; document.getElementById('ytc-player').style.background = ''; });
          console.log(`     (channel name over a white frame at ${W}: ${ratio.toFixed(2)}:1)`);
          assert.ok(ratio >= 4.5, `the channel name at ${ratio.toFixed(2)}:1 over a white frame`);
        }
        for (const name of ['Blender', 'MonkevDev', 'Blender Foundation']) {
          const c = await tv.evaluate(async n => { const c = document.querySelector('#ytc-info .wc'), was = c.textContent; c.textContent = n; await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); const s = getComputedStyle(c), rg = document.createRange(); rg.selectNodeContents(c); const out = { tw: rg.getBoundingClientRect().width, sw: c.scrollWidth, cw: c.clientWidth, sh: c.scrollHeight, ch: c.clientHeight, fs: parseFloat(s.fontSize), lh: parseFloat(s.lineHeight) }; c.textContent = was; return out; }, name);
          assert.ok(c.sw <= c.cw && c.sh <= c.ch && c.lh >= c.fs && c.cw > c.tw + H * 0.1, `the channel name ${name} shown whole: ${JSON.stringify(c)}`);
        }
        assert.deepEqual(lay.row, ['Play/pause', 'Captions', 'Speed', 'Quality', 'I like this', 'Save to Watch later', 'Comments', 'More info'], JSON.stringify(lay.row));
        assert.deepEqual(await tvAudit(tv, `overlay ${W}`), []);
        const wk = await tvWalk(tv, '#ytc-seek,#ytc-player .prow :is(.pb,.wl)', 20);
        assert.deepEqual([wk.dead, wk.missing], [[], []], `overlay walk ${W}`);
        await tv.focus('#ytc-seek');
        await tv.keyboard.press('ArrowUp');
        await tv.waitForFunction(() => document.getElementById('ytc-player').classList.contains('idle'), null, { timeout: 6000 });
        // Scrub: ←/→ move the target (the storyboard frame and the time above it), the seek goes out once.
        // (From near the start, so two steps never reach the end however long the page took.)
        await tv.evaluate(() => { document.querySelector('#ytc-player video').currentTime = 2; });
        await tv.waitForTimeout(300);
        const t0 = await tv.evaluate(() => document.querySelector('#ytc-player video').currentTime);
        await tv.keyboard.press('ArrowRight');
        assert.ok(await tv.evaluate(() => { const p = document.getElementById('ytc-player'); return !p.classList.contains('idle') && p.classList.contains('drag'); }), 'the first → shows the overlay and scrubs');
        await tv.keyboard.press('ArrowRight');
        await tv.waitForTimeout(120);
        const tip = await tv.evaluate(() => ({ t: document.getElementById('ytc-tipt').textContent, op: getComputedStyle(document.getElementById('ytc-tip')).opacity, frame: getComputedStyle(document.querySelector('#ytc-tip .ptf')).display }));
        assert.ok(tip.op === '1' && tip.frame !== 'none' && /^0:[23]\d$/.test(tip.t), `the preview over the target: ${JSON.stringify(tip)}`);
        await tvShot(tv, 'player-scrub');
        assert.deepEqual(await tvAudit(tv, `scrub ${W}`), []);
        await tv.waitForFunction(() => !document.getElementById('ytc-player').classList.contains('drag'), null, { timeout: 3000 });
        const t1 = await tv.evaluate(() => document.querySelector('#ytc-player video').currentTime);
        assert.ok(t1 >= t0 + 17 && t1 <= t0 + 23, `two steps of 10 s: ${t0} → ${t1}`);
        // Menus: a panel from the right edge with the focus on the current value; Back closes it.
        for (const b of ['#ytc-speed', '#ytc-qual', '#ytc-ccb']) {
          await tv.focus(b);
          await tv.keyboard.press('Enter');
          await tv.waitForSelector('#ytc-player.menu #ytc-menu');
          await tv.waitForTimeout(300);
          const m = await tv.evaluate(() => { const r = document.getElementById('ytc-menu').getBoundingClientRect(); const a = document.activeElement; return { l: r.left, r: r.right, t: r.top, b: r.bottom, inMenu: !!a.closest('#ytc-menu'), on: a.classList.contains('on') || !!a.closest('.mtab') }; });
          assert.ok(m.r === W && m.t === 0 && m.b === H && m.l > W / 2 && m.inMenu, `${b}: a right-side panel ${JSON.stringify(m)}`);
          if (b === '#ytc-speed') assert.ok(m.on, 'the current speed is focused');
          assert.ok(+await tv.evaluate(() => getComputedStyle(document.querySelector('#ytc-watch .wp'), '::before').opacity) < 0.05, 'no scrim over the panel');
          if (b === '#ytc-qual') assert.ok(await tv.evaluate(() => document.activeElement.matches('.mql button.on')), 'the current quality is focused');
          assert.deepEqual(await tvAudit(tv, `menu ${b} ${W}`), []);
          await tvShot(tv, `player-menu-${b.slice(5)}`);
          // The playing rung's dot and the chosen value's check share a centre (±1 px).
          if (b === '#ytc-qual') {
            const rows = await tv.evaluate(() => ['.mql button.cur:not(.on)', '.mql button.on'].map(sel => { const e = document.querySelector('#ytc-menu ' + sel); if (!e) return null; const q = e.getBoundingClientRect(), s = getComputedStyle(e), pad = parseFloat(s.paddingRight), ring = parseFloat(getComputedStyle(document.documentElement).fontSize) / 6; return { x: q.right - pad - 1.75 * parseFloat(getComputedStyle(document.documentElement).fontSize), y: q.top + ring + 2, w: 1.75 * parseFloat(getComputedStyle(document.documentElement).fontSize) + 2, h: q.height - 2 * ring - 4, top: q.top, right: q.right }; }));
            assert.ok(rows[0] && rows[1], 'the quality menu has a playing rung and a chosen value: ' + JSON.stringify(rows));
            const [dot, check] = await Promise.all(rows.map(async r => { const c = brightCentre(await tvPixels(tv, r)); return c && [Math.floor(r.x) + c[0] - r.right, Math.floor(r.y) + c[1] - r.top]; }));
            console.log(`     (menu marks at ${W}: dot ${dot && dot.map(v => v.toFixed(2))}, check ${check && check.map(v => v.toFixed(2))} from their rows' right/top)`);
            assert.ok(dot && check && Math.abs(dot[0] - check[0]) <= 1 && Math.abs(dot[1] - check[1]) <= 1, `dot and check on one centre: ${JSON.stringify([dot, check])}`);
          }
          await tv.keyboard.press('ArrowDown');
          assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-menu')), 'the remote walks the menu');
          assert.deepEqual(await back(tv), { handled: true, exit: false });
          await tv.waitForSelector('#ytc-player.menu', { state: 'detached', timeout: 3000 });
          assert.equal(await tv.evaluate(() => document.activeElement.id), b.slice(1), 'Back returns to its button');
        }
        // Captions: bottom centre; above the bar while the overlay shows, near the bottom when it is gone.
        if (!(await tv.locator('#ytc-player.cc').count())) { await tv.focus('#ytc-ccb'); await tv.keyboard.press('c'); }
        await tv.waitForSelector('#ytc-player.cctext .pcw', { timeout: 8000 });
        await tv.focus('#ytc-seek'); await tv.keyboard.press('ArrowUp');
        await tv.waitForTimeout(300);
        const ccOn = await tv.evaluate(() => { const c = document.getElementById('ytc-cc').getBoundingClientRect(), s = document.querySelector('#ytc-seek').getBoundingClientRect(), t = document.querySelector('#ytc-player .ptr').getBoundingClientRect(); return { cx: (c.left + c.right) / 2, b: c.bottom, seek: s.top, time: t.top, fs: parseFloat(getComputedStyle(document.getElementById('ytc-cc')).fontSize) }; });
        assert.ok(Math.abs(ccOn.cx - W / 2) < W * 0.01 && ccOn.b < ccOn.seek - 10, `captions centred above the bar: ${JSON.stringify(ccOn)}`);
        assert.ok(ccOn.b <= ccOn.time - W / 120, `captions clear the elapsed time and chapter row: ${JSON.stringify(ccOn)}`);
        assert.ok(ccOn.fs >= H * 0.035, `large captions (${ccOn.fs})`);
        await tvShot(tv, 'player-captions-overlay');
        await tv.waitForFunction(() => document.getElementById('ytc-player').classList.contains('idle'), null, { timeout: 6000 });
        await tv.waitForTimeout(400);
        const ccOff = await tv.evaluate(() => { const c = document.getElementById('ytc-cc').getBoundingClientRect(); return { cx: (c.left + c.right) / 2, b: c.bottom }; });
        assert.ok(Math.abs(ccOff.cx - W / 2) < W * 0.01 && ccOff.b <= H * 0.965 + 1 && ccOff.b > H * 0.85, `captions bottom-centre in the safe area: ${JSON.stringify(ccOff)}`);
        await tvShot(tv, 'player-captions');
        // More info: the description as a right panel; Back closes it.
        await tv.keyboard.press('ArrowDown');
        await tv.focus('#ytc-player [data-act=tv-info]');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-tvinfo.on');
        await tv.waitForTimeout(300);
        assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-tvinfo .tvd')), 'the description has the focus');
        assert.deepEqual(await tvAudit(tv, `info ${W}`), []);
        await tvShot(tv, 'player-info');
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.ok(await tv.evaluate(() => document.activeElement.dataset.act === 'tv-info'), 'Back returns to More info');
        // Up next: ↓ from the buttons slides the overlay up and shows the related row; ↑ returns; Back too.
        await tv.keyboard.press('ArrowDown');
        await tv.waitForTimeout(300);
        assert.ok(await tv.evaluate(() => !!document.activeElement.closest('.wr #ytc-list')), '↓ reaches Up next');
        assert.deepEqual(await tvAudit(tv, `up next ${W}`), []);
        await tvShot(tv, 'player-upnext');
        await tv.keyboard.press('ArrowRight');
        assert.ok(await tv.evaluate(() => { const r = document.activeElement.closest('.c').getBoundingClientRect(); return r.right <= innerWidth * 0.975 + 1; }), 'the next card is brought into the safe area');
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-player .prow')), 'Back from Up next returns to the buttons');
        // Back with the overlay open while playing hides it; with it hidden, the watch page is left (the shell goes back).
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.ok(await tv.evaluate(() => document.getElementById('ytc-player').classList.contains('idle')), 'Back hid the overlay');
        assert.deepEqual(await back(tv), { handled: false, exit: false }, 'Back with the overlay hidden leaves the video');

        // The end: YouTube's autoplay countdown, Play now focused (bottom right); Back cancels it.
        await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=-Bxc4gJluC0&t=35');
        await tv.waitForSelector('#ytc-up .upc', { timeout: 20000 });
        await tv.waitForFunction(() => document.activeElement && document.activeElement.id === 'ytc-up-now', null, { timeout: 3000 });
        assert.deepEqual(await tvAudit(tv, `countdown ${W}`), []);
        await tvShot(tv, 'player-end');
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        await tv.waitForSelector('#ytc-up .upc', { state: 'detached', timeout: 3000 });
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-seek', 'Back cancelled the countdown');

        // The skip notice (SponsorBlock: the fixture's sponsor at 8–14 s is skipped): bottom right, its button a stop.
        await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${SB_ID}&t=6`);
        await tv.waitForSelector('#ytc-player .psbt.on', { timeout: 15000 });
        await tv.focus('#ytc-seek'); await tv.keyboard.press('ArrowDown');
        await tv.focus('#ytc-player .prow .pb[data-act=tv-info]');
        await tv.keyboard.press('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-sbtb', '↑ from the row\'s end reaches the skip notice');
        const sb = await tv.evaluate(() => { const r = document.querySelector('#ytc-player .psbt').getBoundingClientRect(); return { r: r.right, b: r.bottom, l: r.left }; });
        assert.ok(sb.r <= W * 0.975 + 1 && sb.l > W / 2, `bottom right: ${JSON.stringify(sb)}`);
        assert.deepEqual(await tvAudit(tv, `skip ${W}`), []);
        await tvShot(tv, 'player-skip');

        // Failure: the panel in the middle, Try again focused, ←/→ between its actions.
        await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=unplayable1');
        await tv.waitForSelector('#ytc-player .pfail');
        await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-player .pfbt'), null, { timeout: 5000 });
        await tv.waitForTimeout(400); // (the panel fades in)
        assert.deepEqual(await tvAudit(tv, `failure ${W}`), []);
        await tvShot(tv, 'player-failed');
        await tv.keyboard.press('ArrowRight');
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Open on YouTube');
        await tv.keyboard.press('ArrowLeft');
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Try again');
        // Offline: "No connection" (the network drops mid-video).
        NET.down = true;
        try {
          await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=netdown0001');
          await tv.waitForSelector('#ytc-player .pfail', { timeout: 20000 });
          await tv.waitForFunction(() => document.activeElement && document.activeElement.matches('#ytc-player .pfbt'), null, { timeout: 5000 });
          await tvShot(tv, 'player-offline');
          assert.deepEqual(await tvAudit(tv, `offline ${W}`), []);
        } finally { NET.down = false; }
      }
      assert.deepEqual(tv.errors, []);
    } finally { NET.down = false; await tvClose(tv); }
  });
  await step('TV app (signed in): the ready Home response and the hedge are the account\'s TVHTML5 Home request with the access token; a reload paints the ready response', async () => {
    const tv = await ctx.newPage();
    const token = 'ya29.' + 'h'.repeat(60);
    const wasRich = HOME.rich;
    HOME.rich = true; HOME.stamp = true;
    const homeReqs = () => log.api.filter(a => a.ep === 'browse' && a.body?.browseId === 'FEwhat_to_watch');
    // YouTube's own TV app has no server-rendered Home and sends Redline's web "shown" signals never: none here.
    let tvImp = 0, counting = false;
    tv.on('request', r => { if (counting && (r.url().includes('/youtubei/v1/log_event') || (r.url() === 'https://www.youtube.com/' && r.resourceType() === 'fetch'))) tvImp++; });
    try {
      await tv.addInitScript(t => {
        localStorage.setItem('ytc:platform', 'tv');
        localStorage.setItem('ytc:shell', JSON.stringify({ auth_token: JSON.stringify({ token: t, exp: Date.now() + 3600e3 }) }));
      }, token);
      // (The tab's first document may boot before the init script sets the TV flag: let it settle, then
      // start from an empty tab session.)
      await tv.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await tv.waitForTimeout(2500);
      await tv.evaluate(() => { sessionStorage.clear(); localStorage.removeItem('ytc-test-noready'); });
      counting = true;
      const n0 = homeReqs().length;
      await tv.reload({ waitUntil: 'commit' });
      await tv.waitForFunction(() => /Fresh #/.test(document.querySelector('#ytc-list .c .t')?.textContent || ''), null, { timeout: 15000 });
      await tv.waitForFunction(() => { const r = sessionStorage.getItem('ytc:ready'); return r && JSON.parse(r).i.length > 30; }, null, { timeout: 15000 });
      const reqs = homeReqs().slice(n0);
      assert.ok(reqs.length >= 2, `the page and the ready response (${reqs.length})`);
      for (const r of reqs) {
        assert.equal(r.body.context.client.clientName, 'TVHTML5');
        assert.equal(r.headers.authorization, `Bearer ${token}`);
      }
      const title = await tv.evaluate(() => JSON.parse(sessionStorage.getItem('ytc:ready')).i[0].t);
      const n1 = homeReqs().length;
      await tv.reload({ waitUntil: 'commit' });
      await tv.waitForFunction(() => document.querySelector('#ytc-list .c[data-v] .t'), null, { timeout: 15000 });
      assert.equal(await tv.evaluate(() => document.documentElement.dataset.ytcHomeSrc), 'ready');
      assert.equal(await tv.evaluate(() => document.querySelector('#ytc-list .c[data-v] .t').textContent), title);
      assert.ok(homeReqs().slice(n1).every(a => a.t > Date.now() - 3000), 'no Home request before the paint');
      // A stalled TV Home request: the identical TVHTML5 request with the token at the deadline (after the
      // next ready response has landed, so the stall meets the page's own request).
      await tv.waitForFunction(() => { const r = sessionStorage.getItem('ytc:ready'); return r && JSON.parse(r).i.length > 30; }, null, { timeout: 15000 });
      await tv.evaluate(() => { localStorage.setItem('ytc-test-noready', '1'); sessionStorage.removeItem('ytc:ready'); sessionStorage.setItem('ytc:bt', '100,100,100,100,100'); });
      HOME.stall = 1;
      const n2 = homeReqs().length;
      await tv.reload({ waitUntil: 'commit' });
      await tv.waitForFunction(() => document.documentElement.dataset.ytcHedge === 'hedge' && document.querySelector('#ytc-list .c[data-v]'), null, { timeout: 5000 });
      const h = homeReqs().slice(n2);
      assert.equal(h.length, 2);
      assert.equal(JSON.stringify(h[0].body), JSON.stringify(h[1].body), 'identical bodies');
      assert.ok(h.every(r => r.headers.authorization === `Bearer ${token}` && r.body.context.client.clientName === 'TVHTML5'), 'both with the access token');
      assert.equal(tvImp, 0, 'TV: no Home document, no "shown" signals');
    } finally {
      HOME.stall = 0; HOME.stamp = false; HOME.rich = wasRich;
      await tv.evaluate(() => { for (const k of ['ytc:platform', 'ytc:shell', 'ytc-test-noready']) localStorage.removeItem(k); }).catch(() => {});
      await tv.close();
    }
  });

  await step('TV app (signed in), cold start: the ready Home response the shell kept (ready_take) paints Home on the first frame, no Home request before it; every ready record the page keeps goes to the shell (ready_put), a used one is dropped; expired, another account\'s or none → the network path', async () => {
    const tv = await ctx.newPage();
    const token = 'ya29.' + 'k'.repeat(60);
    const wasRich = HOME.rich;
    HOME.rich = true; HOME.stamp = true;
    const homeReqs = () => log.api.filter(a => a.ep === 'browse' && a.body?.browseId === 'FEwhat_to_watch');
    const shellLog = () => tv.evaluate(() => JSON.parse(localStorage.getItem('ytc:shell-log') || '[]'));
    // A cold app start: no tab session (no token mirror, no ready copy); the shell answers ready_take with `rec`.
    const cold = async rec => {
      await tv.evaluate(r => {
        const t = JSON.parse(localStorage.getItem('ytc:shell')); t.ready_take = JSON.stringify({ record: r }); localStorage.setItem('ytc:shell', JSON.stringify(t));
        localStorage.setItem('ytc:shell-log', '[]'); sessionStorage.clear(); addEventListener('pagehide', () => sessionStorage.clear());
      }, rec);
      await tv.reload({ waitUntil: 'commit' });
    };
    try {
      await tv.addInitScript(t => {
        localStorage.setItem('ytc:platform', 'tv');
        if (!localStorage.getItem('ytc:shell')) localStorage.setItem('ytc:shell', JSON.stringify({ auth_token: JSON.stringify({ token: t, exp: Date.now() + 3600e3 }), ready_put: '{}', ready_drop: '{}', ready_take: '{"record":null}' }));
      }, token);
      await tv.goto('https://www.youtube.com/', { waitUntil: 'commit' });
      await tv.waitForTimeout(2500);
      await tv.evaluate(() => { localStorage.removeItem('ytc-test-noready'); localStorage.setItem('ytc:shell-log', '[]'); });
      // A run that fetched its ready response: the page hands the record to the shell (page, then with its continuation).
      await cold(null);
      await tv.waitForFunction(() => /Fresh #/.test(document.querySelector('#ytc-list .c .t')?.textContent || ''), null, { timeout: 15000 });
      await tv.waitForFunction(() => { const r = sessionStorage.getItem('ytc:ready'); return r && JSON.parse(r).i.length > 30; }, null, { timeout: 15000 });
      await tv.waitForTimeout(300);
      let calls = await shellLog();
      assert.ok(calls.some(c => c[0] === 'ready_take'), 'a cold start asks for the kept one');
      const puts = calls.filter(c => c[0] === 'ready_put').map(c => JSON.parse(JSON.parse(c[1]).record));
      assert.ok(puts.length >= 2 && puts[puts.length - 1].i.length > 30, 'the record and its continuation went to the shell: ' + puts.length);
      const rec = JSON.stringify(puts[puts.length - 1]);
      const title = puts[puts.length - 1].i[0].t;
      // The next cold start: Home from the kept record on its first frame, before any Home request; then it is used up
      // (dropped at the shell) and refilled as usual.
      const n1 = homeReqs().length;
      await cold(rec);
      await tv.waitForFunction(() => document.querySelector('#ytc-list .c[data-v] .t'), null, { timeout: 15000 });
      assert.equal(await tv.evaluate(() => document.documentElement.dataset.ytcHomeSrc), 'ready');
      assert.equal(await tv.evaluate(() => document.querySelector('#ytc-list .c[data-v] .t').textContent), title);
      assert.equal(homeReqs().slice(n1).filter(a => a.t < Date.now() - 2500).length, 0, 'no Home request before the paint');
      await tv.waitForFunction(() => JSON.parse(localStorage.getItem('ytc:shell-log') || '[]').some(c => c[0] === 'ready_put'), null, { timeout: 15000 });
      calls = await shellLog();
      assert.ok(calls.some(c => c[0] === 'ready_drop'), 'the used one is dropped at the shell');
      // Expired (11 min old) or another account's: the network path, as without one.
      for (const [what, patch] of [['expired', r => { r.t = Date.now() - 11 * 60e3; }], ['another account', r => { r.a = 'o123:en'; }]]) {
        const r = JSON.parse(rec); patch(r);
        await cold(JSON.stringify(r));
        await tv.waitForFunction(() => document.querySelector('#ytc-list .c[data-v] .t'), null, { timeout: 15000 });
        assert.equal(await tv.evaluate(() => document.documentElement.dataset.ytcHomeSrc), 'network', what);
      }
      // A shell without the command (an older app): the network path, nothing breaks.
      await tv.evaluate(() => { const t = JSON.parse(localStorage.getItem('ytc:shell')); delete t.ready_take; localStorage.setItem('ytc:shell', JSON.stringify(t)); sessionStorage.clear(); addEventListener('pagehide', () => sessionStorage.clear()); });
      await tv.reload({ waitUntil: 'commit' });
      await tv.waitForFunction(() => document.querySelector('#ytc-list .c[data-v] .t'), null, { timeout: 15000 });
      assert.equal(await tv.evaluate(() => document.documentElement.dataset.ytcHomeSrc), 'network');
    } finally {
      HOME.stamp = false; HOME.rich = wasRich;
      await tv.evaluate(() => { for (const k of ['ytc:platform', 'ytc:shell', 'ytc:shell-log', 'ytc-test-noready']) localStorage.removeItem(k); }).catch(() => {});
      await tv.close();
    }
  });

  await step('no live network traffic; no YouTube player JS or embed ever requested', async () => {
    assert.ok(log.external.every(u => /ytimg\.com|ggpht\.com|googleusercontent\.com/.test(new URL(u).hostname)), log.external.filter(u => !/ytimg|ggpht|googleusercontent/.test(u)).join(','));
    // (The DNR step fetches /s/player/abc/…/base.js itself to prove it is not blocked.)
    const playerJs = log.assets.filter(u => /\/s\/player\/|base\.js|www-player|widgetapi|iframe_api/.test(u) && !u.includes('/s/player/abc/'));
    assert.deepEqual(playerJs, []);
    assert.deepEqual(log.embeds, []);
  });
  await step('no player failure ever left by itself: every native watch page was opened by the test or the panel\'s Open on YouTube', async () => {
    const auto = log.docs.filter(u => /\/watch\?/.test(u) && u.includes('yt-native=1') && !nativeOk.includes(u));
    assert.deepEqual(auto, []);
    assert.ok(log.docs.some(u => u === 'https://www.youtube.com/watch?v=unplayable1&t=7s&yt-native=1') || ONLY, 'the manual button was exercised');
  });
  await step('no page errors', async () => { assert.deepEqual(errors, []); });

  await ctx.close();
  collector.close();
  dg.close();
  fs.rmSync(profile, { recursive: true, force: true });
  if (load !== EXT) fs.rmSync(load, { recursive: true, force: true });
  const failed = results.filter(r => r[0] === 'FAIL'), skipped = results.filter(r => r[0] === 'skip');
  console.log(`\n${results.length - failed.length - skipped.length}/${results.length - skipped.length} passed` + (skipped.length ? ` (${skipped.length} skipped: development build only)` : '') + ` · ${DEV_BUILD ? 'development' : 'public'} build`);
  process.exit(failed.length ? 1 : 0);
})().catch(e => { console.error(e); process.exit(1); });
