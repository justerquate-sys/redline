# Live speed benchmark (optional developer tool)

`bench.cjs` drives the built **development** extension in Playwright's Chromium against the real
youtube.com, signed out, and reads the client's own speed telemetry (the perf batches go to a collector the
script runs on a free 127.0.0.1 port; the debug center is not involved). It prints count/p50/p90/max per
metric and scenario. This is the only place the network is used on purpose: the e2e suite stays offline.

```sh
./build.sh --dev
cp -R extension /tmp/redline-a              # benchmark a copy, so rebuilding does not change a running run
node tools/bench/bench.cjs --runs=8 --ext=/tmp/redline-a
node tools/bench/bench.cjs --only=latency --runs=6 --ext=/tmp/redline-a          # injected slowness, hedging on/off
node tools/bench/bench.cjs --only=latency --uniform --slow=10 --ext=/tmp/redline-a
node tools/bench/bench.cjs --runs=2 --ndjson=/tmp/run.ndjson   # then: YTC_DEBUG_CENTER_ADDR=127.0.0.1:47399 \
                                                               #   tools/debug-center/target/release/lumen-debug-center /tmp/run.ndjson
```

Scenarios: a cold boot of Home (signed out it is the "Your YouTube history is off" hero, so it measures the
generic boot), a cold boot of a channel's Videos page (the same card grid, chunks and continuations as a
feed), its reload, channel → watch (hover, click: the tab's first watch open) → a related card (a later
open) → back, a search typed into the box with Enter at once and with a 350 ms pause before Enter (the
typing-pause prefetch; `nav.search push · Enter→results, … pause`), a cold boot of a results URL; `latency`: boots of a watch, results and channel URL while a share of the `next` / `search` /
`browse` requests answers late. `doc … ·` rows are the document's navigation timing and the loader's marks
(`response_start` = YouTube's document, `script` = the content script started, `wasm`, `shell`, `ready`;
`doc boot-channel · first-row images loaded (from goto)` = when the last of the first row's thumbnails finished
loading, wall clock from the navigation's start; `nav.channel boot/network` is the cards' own first frame on the
same clock as the `doc` marks).
`nav.watch push first` / `later` split the first watch open of a document from the later ones; `· parse` /
`· render` are the client's own parse and render times; `req.<ep> (<via>) · decoded KB` the answer sizes.
The last table is InnerTube (and the i.ytimg.com video thumbnails) on the wire per run and scenario (encoded response bodies, per endpoint, from
the browser's own request sizes). `--cpu=4` throttles every page's CPU 4× (Chromium's emulation, a slower
machine); `--loaf` prints the long frames of watch pages. Live numbers drift with the network and the
machine: compare builds in alternating rounds (`--json=` per round, then pool the series).
Only numbers, metric names and route kinds are printed or saved; the search terms and channels are the
script's own fixed ones.

Home (each script's header has its options): `home-reload.cjs` (reload → YouTube's answer painted, Redline vs YouTube's
own page; `--yt --parity` for the overlap with YouTube's own reloads, `--nodoc` / `--noimp` to switch off the Home
document or the "shown" signals), `home-newtab.cjs` (a new tab → Home painted: another tab on Home, on a video, playing a video; `--noready`, `--nokeep`; vs video → Home, vs YouTube's own new
tab), `home-parity.cjs` (how the request's shape changes the answer), `home-drift.cjs` (how fast YouTube's answer
drifts), `home-session.cjs` (a realistic session: where each Home paint came from).
