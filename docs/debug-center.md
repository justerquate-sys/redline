# Debug center and speed telemetry (development builds)

Both exist only in the development build (`./build.sh --dev`). The public build has neither: its background
worker keeps no incidents and sends nothing, and the Rust telemetry is compiled off (Cargo feature `dev`).

## Debug center

Every incident (a player failure `player-failed`, one that gets an automatic Try again `player-retry`, a stuck episode `player-stuck` and its `player-recovered` outcome, InnerTube error, a Home document without Home data `home-doc`, Rust panic, boot failure) becomes a JSON report: reason, route, client version, timings, sign-in state, and for the player its notes, every request with status/bytes/time (hosts and ranges only, no signatures, IPs or cookies), the format ladder and the bad itags. The Rust app hands it to the background worker (`browser/background.js`), which keeps the last 100 in `chrome.storage.local` (`ytcIncidentsV1`) and delivers them to the local collector (the public build's worker has none of this).

The collector is `tools/debug-center` (Rust, standard library only). It listens on `127.0.0.1:47321` and appends one line per incident to its incidents file, which lives in your user data folder, never in the repository (macOS `~/Library/Application Support/Lumen/incidents.ndjson`, Linux `~/.local/share/lumen/incidents.ndjson`, Windows `%APPDATA%\Lumen\incidents.ndjson`). Open http://127.0.0.1:47321/ in the browser for the latest reports and http://127.0.0.1:47321/perf for speed telemetry (see "Speed telemetry"). Running it, and starting it at login: [tools/debug-center/README.md](../tools/debug-center/README.md). Undelivered reports are retried once a minute while YouTube is open. chrome.storage.local `ytcCollectorV1` may name another collector URL.

**Redline for TV.** The TV app sends the same reports while its owner turned telemetry on (About & updates →
Developer; tv/README.md "Speed telemetry"): the app then runs its bundle's development variant (`content-dev.js`), the
page shim hands the reports to the shell (`tel_send`) and the shell delivers them to the debug center on the owner's
network (`tv/core/src/tel.rs`: one POST per report, oldest first, memory only, private addresses only). The debug
center must be started with `--lan` for a real TV (an emulator reaches 127.0.0.1 through 10.0.2.2). Every TV report
carries `"platform":"tv"`; `/perf`, `/perf.json` and `/timeline` take `platform=tv|desktop`, and every row says its
platform (reports without one are `desktop`).

## Speed telemetry

Goal (2 Nov 2026): track every speed metric of starting a video and everything else, so it can be
inspected at any time to find what slows the client down. Code: `src/telemetry.rs` (buffer, gate,
spans), `src/app/telglue.rs` (app hooks), `src/player/own/tel.rs` (player hooks), the bridge's
`tel_*` primitives (observers, input timestamps, frame callbacks) and `browser/background.js` (queue).

**How it is sent.** Each event is one small JSON object `{m, v, at, …breakdown}`: `m` the metric, `v` its value
(ms unless noted), `at` epoch ms. Events collect in memory and go out as one batch (≤ 200 events) at most
once a minute, when 200 are waiting, or when the tab is hidden or left, through the existing incident
channel. The background worker keeps batches in their own queue (`ytcPerfQueueV1`, last 30, removed once
delivered), so they never push an incident out, stamps every report with the build (`"build": "0.8.0
(<build id>)"`), and the collector appends it to the incidents file like any report. A batch:

```json
{"build":"0.8.0 (b9c1ba6d02c4c684)","kind":"perf","schema":2,"reason":"speed events","at":1793000000000,
 "client":"0.8.0","client_version":"2.20251030.01.00","signed_in_cookie":false,"page":"19a4…",
 "seq":3,"dropped":0,"overhead_ms":1.2,"js_overhead_ms":0.7,
 "events":[{"m":"nav.watch","v":38.4,"at":1793000000123,"route":"watch","mode":"push","input":"pointer",
            "src":"cache","handler_ms":0.6,"insert_ms":22.1,"reqs":[…]}, …]}
```

`page` identifies the document (one tab's page load), `seq` the batch within it, `dropped` events lost to the
caps, `overhead_ms` the time spent in the telemetry code since the last batch and `js_overhead_ms` the
time the browser observers' callbacks took.

**What is measured** (`v` in ms unless noted; every event also carries `at`):

| Metric | `v` | Breakdown |
|---|---|---|
| `nav.<route>` (`home`, `subs`, `search`, `watch`, `channel`, `playlist`, `wl`, `liked`, `history`, `playlists`) | input → the frame after the page's content was written (first cards / the watch page) | `mode` push / pop / boot, `input` pointer / keyboard / history / auto (autoplay) / boot (from the document's navigation start), `src` memory / snapshot / cache (watch data) / prefetch (hover/intent page cache) / inflight (a prefetch still running) / ready (Home: the ready Home response; with `age_ms`) / network, Home's network path also `path` (network / slow: the old Home shown dimmed after 4 s), `hedge_fired`, `hedge_won` (first / hedge), `handler_ms` (input → app), `insert_ms` (input → DOM write), `wait_ms`, `parse_ms`, `render_ms`, `reqs` (each InnerTube request it waited for: `ep`, `ms`, `bytes`, `status`, `via`, the hedge fields), `act` (the `ui.act` that started it); a boot also `boot` = where `handler_ms` went, performance.now ms from the document's navigation start: `type` (navigate / reload / back_forward), `fetch`, `dns`, `connect`, `request`, `response_start` / `response_end` (YouTube's document), `worker`, `script` (the content script started), `wasm` (Wasm instantiated), `app` (the app's boot) |
| `nav.fresh` | input → the fresh page replacing a snapshot or cached page | `route`, `parse_render_ms` |
| `ui.act` | one user interaction: input → the next frame after its handler (`v`, "paint") | see "Interaction log" below |
| `req.<endpoint>` (`browse`, `home-doc` (YouTube's Home document, `src/homedoc.rs`), `log_event` (Home's "shown" batches), `next`, `search`, `navigation/resolve_url`, `suggest`, `sw.js_data`, `player-visionos`, `player-web`, `timedtext`, `sponsorblock`, `visitor_id`) | request → answer (for a hedged read: the original request → the first good answer) | `status`, `bytes`, `via` (page / cont / prefetch / spec / bg / bg-cont / ready / ready-cont / shown / suggest / config / resolve), `nav` (the current navigation waited for it), `route`; hedged reads (see "Hedged reads") also `hedge_fired` (the second copy went out), `hedge_won` and `path` (`primary` / `hedge`: which copy answered first); VISIONOS also `route` (anonymous / account / fresh visitor) and `refresh` |
| `feed.more` | the end of the list reached → the next cards' frame (only when the user had to wait) | `route` |
| `video.start` | input → first frame | `vid`, `input`, `watch_ms` (input → watch page visible), `vision` {`src` cache / prefetch / fetch, `age_ms`, `ms`, `route`, `bot_checks`}, `caps_ms` (decodingInfo), `mse_open_ms`, `streams_ms` (input → streams chosen), `start_p` → `target_p` (equal since 3 Nov 2026: no low start rung), `gv` (the first googlevideo requests: `kind`, `itag`, `ms`, `bytes`, `status`, `mirror`, `at`), `refreshes`, `mirror`, `stuck`, `hidden_wait`, `mse_rebuilt`, `nopic` (media covered the playhead without a picture: another format), `live`, `media_pre` {`v`, `a`: "hit" (a finished hover media prefetch was used) / "inflight" (one still loading was taken over) / ""}, `slow` {`vision_hedge` {`fired`, `won`}, `hedge_fired`, `hedge_won` (googlevideo mirror twins), `throttle_detected`, `path` (steps taken: `mirror`, `refresh`, `drop`, `back` = the cap lapsed)} |
| `video.throttle` | a googlevideo request judged throttled: how long it had run | `vid`, `kind`, `itag`, `why` (`ttfb` headers late / `rate` body too slow), `step` (`mirror` / `refresh` / `drop`), `kbps` (its body rate), `bw_kbps`, `p`, `started`, `mirror` (≤ 30 per video) |
| `video.fail` / `video.abandon` | input → the player gave up / was left before a first frame | as `video.start`, plus `reason` |
| `video.target`, `video.1080` | input → the target quality / ≥ 1080p on screen | `vid`, `p`, `start_p` |
| `gv.head`, `gv.seg`, `gv.live`, `gv.hls` | googlevideo request → answer (every index request; segments: the first 8 per video plus every slow > 1.5 s or failed one) | `vid`, `kind` v / a, `itag`, `host`, `mirror`, `bytes`, `status`, `kbps`, `started` |
| `video.seek` | seek input → the frame at the new position (or playing again when it had to wait) | `vid`, `input`, `waited`, `paused`, `at_s`, `pre` (its segments were fetched ahead: seek-bar hover, TV scrub) |
| `video.switch` | quality decision → the new resolution on screen | `vid`, `from_p`, `to_p`, `why` (user / auto) |
| `video.stall` | stall duration | `vid`, `cause` (switch / network / underrun), `p`, `at_s`, `bw_kbps` |
| `video.drops` | dropped frames, % (per rung shown) | `vid`, `p`, `dropped`, `frames` |
| `video.session` | the player's life | `vid`, `started`, `failed`, `first_frame_ms`, `target_ms`, `hd_ms`, `refreshes`, `mirror`, `stuck`, `stalls`, `stall_ms`, `seeks`, `seek_pre` (seeks that found their segments fetched ahead), `switches`, `bad_itags`, itags, `bw_kbps`, `live`, `slow` (as in `video.start`, for the whole session) |
| `ui.menu`, `ui.toast` | input → the frame showing the player's menu / toast | `menu`, `decide_ms` |
| `lag.loaf` (`lag.longtask` where Long Animation Frames are missing) | frame duration (≥ 50 ms) | `block`, `render`, `layout`, `scripts` (top 3: `ms`, `type`, `inv`, `fn`, `src` path, `forced` layout), `route` |
| `lag.event` | event → next paint for one interaction (its longest event, ≥ 40 ms) | `name`, `delay`, `proc`, `pres`, `target` (tag, id, `data-act`), `route` |
| `lag.cls` | cumulative layout shift of a page (score, not ms), one per page left | `route` |
| `res.storyboard` | storyboard image load | `bytes`, `cached`, `route` |

**Interaction log** (`ui.act`, 3 Nov 2026: every entry of every click, in milliseconds, with what is needed to
analyse it). Every user interaction is one event: a pointer click or tap, a key the app handled (shortcuts,
Enter in the search box, arrows in a menu), a search submitted, back/forward, a wheel gesture over a wheel
zone (the player's Speed/Quality, one per gesture), a drag released, a context menu taken, and the hover
activations a handler names: the logo's dwell going Home (`logo.home`, `input: hover`), a dwell choosing a
Speed/Quality row (`player.dwell`), the wheel's quality choice (`player.quality.wheel`), the microphone's
hover start (`voice.hover`). Fields:

| Field | Meaning |
|---|---|
| `v` | input → the next rendered frame after the handler (requestAnimationFrame + task, like every "visible" time) |
| `act` | stable action id: `card.open` / `card.channel` / `card.playlist` (a card in the list), `side.<route>`, `logo.home`, `<area>.<route>` for other links (`link.<route>` outside an area), `player.<data-act rest>` (`p-*`; the menus' choices by their key: `player.quality`, `player.speed`, `player.captions`, `player.audio`, `player.codec`, `player.sponsorblock`), `fx.*`, `voice.*`, `action.*` (`a-*`), `upnext.*`, other `data-act` values with `-` → `.`; `search.submit`; `key.<key>` / `mod.<key>` (Ctrl/⌘) / `field.key.<key>` (a handled key in a text field: never a character); `history.pop`; `wheel.<zone>`, `drag.<zone>`, `ctx.<zone>` |
| `kind` | what was used, no content: `tag@area` (`a@list`, `button@player`; the area is the closest element id without `ytc-` and without any part that carries an id or an index), `key`, `field`, `form`, `history`, `wheel`, `drag`, `hover@<zone>` |
| `input` | pointer / touch / keyboard / wheel / hover / history |
| `route` | the route kind it happened on; `to` the route kind a link leads to; `vid` the video id of a watch link |
| `t` | input time, performance.now ms of the document (orders a timeline below 1 ms) |
| `handler_ms` | input → the app's handler started (input delay) |
| `dom_ms` | input → the DOM was written (the handler's end; for a navigation its content write) |
| `nav`, `content_ms`, `wait`, `req_ms` | `nav` true when it started a navigation; then `content_ms` = input → that page's content visible, `wait` = what it waited on (memory / snapshot / cache / prefetch / inflight / network, as `nav.*` `src`; `none` without a navigation) and `req_ms` = the longest InnerTube request it waited for. `superseded` when a newer navigation replaced it first |

What is never recorded: text, titles, search terms or typed characters, URLs or queries, `data-arg` values.
A key typed into a text field is not an interaction (only keys the app handled there, by name). Cost: two
bridge calls per interaction (`performance.now`, the input's target descriptor) and one frame callback;
hover zones and timers that no handler names cost one `performance.now` and nothing is kept. Code:
`src/telemetry.rs` (`act_*`, the naming functions, unit-tested), the wrappers in `src/lib.rs`, the bridge's
`tel_target` (target and pointer type recorded by the input-timestamp listener).

**Hedged reads** (`src/hedge.rs`, 3 Nov 2026: speed holds when YouTube is slow). A read (`browse`, `next`,
`search`, `navigation/resolve_url` and their continuations) that someone waits for and that has not answered by its
endpoint's deadline is sent once more, identically (URL, body, headers: same session, account, visitor); the
first good answer is used and the other request is aborted; a failed answer waits for the other one. Deadline ≈
the session's p90 of that endpoint's original requests (last 40 good answers, clamped 700–2500 ms), 1.3 s / 1.3 s /
1.2 s (next / browse / search) until 8 answers are in. When every request is slow (4 copies in a row lost) the
endpoint is not hedged for a minute: a copy then only costs bandwidth. A background request (an idle feed
continuation, a speculative or background prefetch) gets its deadline when the user starts waiting for it (the end
of the list reached, a click on a card still being fetched); its copy goes out at normal priority. Never hedged: actions (like, subscribe, comment, save,
playlists: they never go through this path), background (low-priority) requests, and Home's first page (the
Home path's own). `localStorage` `ytc-test-nohedge = 1` switches it off (the benchmark's A/B runs). Measured with the live
benchmark (`tools/bench`, `--only=latency`, 3 Nov 2026; 18 boots of watch / results / channel URLs per row, a
share of the `next` / `search` / `browse` requests answering (F − 1) × 400 ms late):

| YouTube made slow | hedging off p50 / p90 | on p50 / p90 | copies fired / won |
|---|---|---|---|
| none (×1) | 732 / 899 ms | 792 / 959 ms | 0 / 0 of 24 |
| 20 % of requests ×10 | 728 / 4059 ms | 785 / 880 ms | 2 / 2 |
| 20 % of requests ×100 | 751 / 40326 ms | 865 / 1886 ms | 4 / 4 |
| every request ×10 | 4273 / 4413 ms | 4211 / 4433 ms | 12 / 0 (hence the rest rule) |

(The ×1 rows differ by network noise only: no copy went out.)
A second host (`youtubei.googleapis.com`) was checked on 3 Nov 2026: signed out it answers the same, but the
page's YouTube cookies never reach it, so a signed-in copy would not be personalised; not used.

**Cost.** No per-frame bridge calls and no layout reads: a mark is one `performance.now()`; "visible" is one
`requestAnimationFrame` + task after the write (`tel_frame`); browser entries are buffered by the observers and
pulled once per batch. Volume caps: 200 events per batch, 400 kept while undelivered, 300 browser entries,
32 open spans, per video 40 seeks / 30 switches / 30 stalls / 8 sampled segments. Measured in the e2e
run: the e2e timing lines stay where they were (see the timing table) and a whole search → watch → seek →
leave session spends ~1–2 ms in Rust telemetry and < 1 ms in the observers.

**Privacy and the gate.** Video ids and route kinds only: no search terms, URLs, tokens, cookies, headers,
visitor data, descriptions or keys (the e2e test checks a batch for the SAPISID cookie, visitor data, the
search terms and googlevideo signatures). The gate is the build: telemetry exists only in the development
build (Cargo feature `dev`, the `@dev` blocks of `browser/*.js`, the `http://127.0.0.1:47321/*` host permission);
there chrome.storage.local `ytcTelemetryV1: false` turns it off. The public build records, stores and sends
nothing (its background worker also stores no incident).

**Reading it.** http://127.0.0.1:47321/perf: per metric count/p50/p90/p99/max (window: hour … everything,
version filter, metric filter such as `nav.*`), the p50 trend per build (▲/▼ for > 20 % moves), incidents
and errors per build (per 100 navigations, telemetry overhead per batch) and the 20 slowest events per
metric with their full breakdown. Two sections serve the interaction log: **Interactions** (per action:
count, paint p50/p90/max, content p50/p90/max for the ones that navigated, handler and DOM p50, the input and
waited-on mix) with the 25 slowest recent interactions, each linking to its moment in the timeline, and
**Hedged requests** (per endpoint: requests, p50/p90/max, hedges fired and their share, hedges won). The
**timeline** (http://127.0.0.1:47321/timeline) lists the documents seen (one tab's page load each: started,
length, events, interactions, navigations, long frames); `/timeline?page=<id>` shows one document's every
event in order of its start (interactions, navigations, requests, long frames, video events) with +ms from the
first, the clock time, the value with a bar, and its breakdown. From the shell:

```sh
# every video start of the last 24 h, slowest first
curl -s 'http://127.0.0.1:47321/perf.json?metric=video.start&since=24h' | jq 'sort_by(-.v) | .[:5]'
# the slowest interactions of the last hour (content time when they navigated)
curl -s 'http://127.0.0.1:47321/perf.json?metric=ui.act&since=1h' | jq 'sort_by(-(.content_ms // .v)) | .[:10] | map({act, v, content_ms, wait, req_ms, route, page})'
# navigations of one build since an epoch-ms time (metric prefix with *)
curl -s 'http://127.0.0.1:47321/perf.json?metric=nav.*&since=1793000000000&version=0.8.0%20(b9c1ba6d02c4c684)' | jq 'group_by(.src) | map({src: .[0].src, n: length})'
```

`/perf.json` parameters: `metric` (exact, or a prefix ending in `*` or `.`; empty = all), `since` (epoch ms,
a duration ago in ms, or `30m` / `6h` / `7d`, `all`), `version` (the exact `build` string), `limit`
(newest rows, default 10000). Each row is the event plus `received`, `version` and `page`.
