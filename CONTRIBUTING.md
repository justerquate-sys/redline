# Contributing to Redline

Thanks for helping. The short version: logic in Rust, privacy rules are strict, motion never costs speed,
and `npm test` stays green with `extension/` holding the public build.

## Set up

See "Build from source" in the [README](README.md). Typical loop:

```sh
./build.sh --dev      # development build: auto-reloads open YouTube tabs after every build
# load extension/ unpacked once (README "Install"); edit; ./build.sh --dev again
npm test              # cargo test --locked (client and transcripts/) + the end-to-end test
npm run test:helper   # the native helper's own tests (the audio ones need FFmpeg, else they say so and skip)
./build.sh && npm run check:public   # before committing: the public build
```

`YTC_ONLY='<regexp>' npm run test:e2e` runs only matching e2e steps (some depend on state from earlier
steps). `tests/live-*.cjs` talk to real YouTube or Deepgram: run them by hand, rarely, and never in CI.

## Where code goes

- **Rust (`src/`) owns every decision**: routing, requests, parsing, rendering, the player, features. Pure
  logic gets unit tests next to it.
- **`browser/bridge.js` stays generic**: DOM, media, storage, timers and events as primitives, with no
  YouTube or player knowledge. New behaviour is new Rust plus, at most, a new generic primitive.
- **Transcripts** has three homes: `src/features/transcribe.rs` (the Cargo feature `transcribe`, on for the extension,
  off for TV: card, watch and channel controls on YouTube's pages), `transcripts/` (the extension's second Wasm: the
  worker's service, the reader, settings and the popup section) and `helper/` (the native helper, its own workspace).
  `browser/transcripts-host.js` is its thin host, generic like the bridge. docs/architecture.md, "Transcripts".
- **`browser/background.js`** is tooling only (the voice-search socket relay, the loader of the Transcripts service; in
  development builds the debug center and auto-reload). Development-only code sits between `/* @dev-start */` and `/* @dev-end */`
  lines (dropped from the public build by `browser/build.cjs`); development-only Rust sits behind the Cargo
  feature `dev`.
- **`extension/client.css`** is the whole look; `extension/native.css` only styles YouTube's own pages
  (`?yt-native=1`).
- Generated files (`extension/content.js`, `extension/background.js`, `extension/manifest.json`) are
  committed so installing needs no build. Never edit them by hand, and always commit the **public** build.
- Effects that exist in a JavaScript library (springs, animation helpers) are reimplemented in Rust + CSS; no
  runtime dependencies are added to the extension.

## Privacy rules (binding)

- **No new outside service** without an issue discussing it first. Today there are three: YouTube itself,
  SponsorBlock and Deepgram (README "Privacy"), plus the user's own Azure Speech resource for someone who set up
  Transcripts, reached only by their local helper.
- **Never persist** feed or recommendation payloads, descriptions, continuation tokens, search-filter params
  or filter/website verdicts. They live in memory, or in this tab's `sessionStorage` for snapshots.
  - Memory shared by the tabs (owner decision, 3 Nov 2026, desktop extension): the ready Home response
    (`src/ready.rs`) is kept in the extension's `chrome.storage.session`, which lives in the browser's memory only
    (never written to disk, cleared when the browser closes), so a new tab or window paints Home at once. Same rules
    as in a tab: the same account, younger than 10 minutes, used once (a tab that uses it removes it for every tab),
    and a personalizing event in any tab removes it for all. Nothing else goes there, and never into
    `chrome.storage.local` or `localStorage`.
  - The one exception (owner decision, 3 Nov 2026), in **Redline for TV only**: the shell keeps the page's ready Home
    response (YouTube's latest answer to the Home request, `src/ready.rs`) on disk for the next app start, so a cold
    start paints Home at once. It is sealed with the Android Keystore key the shell uses for the account, kept for
    that account only (signing out or another account deletes it), used only while younger than 10 minutes, and
    deleted when it is read (once). Nothing else is persisted, and the desktop extension never persists it
    (`tv/core/src/readystore.rs`).
- `chrome.storage.local` holds only the documented, bounded keys (docs/architecture.md, "Privacy"): e.g. 100
  searches, 2,000 resume records, 50 blocked domains, 20 topics, 20 YouTube-chip records. A new key or a
  bigger bound needs a reason in the pull request.
- **SponsorBlock** stays k-anonymous: only the first 4 hex characters of SHA-256(video id), no cookies, no
  referrer, no HTTP cache, answers in memory only, nothing reported back; switched off means no request at all.
- **Deepgram** gets microphone audio only while a dictation runs. The key is read only by the background
  worker and never reaches the page, Wasm, logs, incidents or the debug overlay.
- **Transcripts** (owner decisions, 8 Nov 2026):
  - Someone who has not set it up pays nothing: on any YouTube page no control, no extra DOM on a card, no request to
    `127.0.0.1`, no native-messaging attempt, and the worker does not even load its Wasm. Only opening settings probes.
    The trace is one quiet row in the toolbar popup. No nag, no badge, no page opened on install.
  - The gate is `chrome.storage.local` `ytcTranscriptsV1` (`{on:true}`): set only by the worker, after the helper said
    Azure is configured. With it on: one batched status message per page render, nothing while idle, live stages
    pushed by the worker only while jobs run.
  - The Azure key stays in the helper's owner-only `.env`, never in browser storage. The pairing token lives in the
    worker's IndexedDB in the extension's own origin (`chrome.storage.local` is readable by content scripts, so never
    there) and is never given to a YouTube page, its Wasm, logs, incidents or the debug overlay. Requests from a page
    are messages to the worker, checked against a short allow-list (`transcripts/src/plan.rs`), and answers carry only
    what a card needs.
  - `chrome.storage.local` gains `ytcTranscriptsV1` and `allZipExclusions:<root token>` (at most one list of folder names
    per save folder, bounded by the helper's 1,000). The worker's IndexedDB holds the pairing token, the toolbar's
    lifecycle (counts and a batch id) and up to 200 channel-preview ownership records.
  - The settings file (popup → Export all settings, settings → Import all settings) holds everything in
    `chrome.storage.local` but the pairing token and the switch, so it holds the Deepgram key, and both pages say so. The
    values stay in JavaScript: Rust decides from key names and kinds only, and the key never enters Wasm.
  - Never on TV: compiled out of the bundle (`tests/check-tv-bundle.cjs` fails the build otherwise) and off at runtime
    in TV mode.
  - `helper/` and `transcripts/` are public: `npm run check:public` scans them for personal paths, addresses, IPs and
    keys, and refuses a tracked `.env`, `.data` or `.venv`. Test fixtures stay synthetic.
- **Home's "shown" signals** (owner decision, 3 Nov 2026): Redline's Home sends YouTube the same "shown" events its
  own Home page sends, so YouTube rotates Home as it does for its own page (`src/impress.rs`): to YouTube only
  (`/youtubei/v1/log_event`, with the account's usual session like every other request), only on Home, only for cards
  that were really on screen (at least half in view, an IntersectionObserver) and scrolls that came to rest: the
  screen (`screenCreated`, naming YouTube's answer), each card once (`visualElementStateChanged` with its grid place,
  `visualElementShown` with its two tracking ids and its box), `scrollEvent`; batched, low priority. Nothing else
  YouTube's page logs (latency, heartbeats, experiments, ads, hovers, clicks, offline state) and nothing on other
  pages; YouTube's own pages keep their `log_event` blocked (rules.json 30; Redline's batches pass by rule 33). Not on
  TV. A new event kind needs the owner's decision and a capture of what YouTube's own page sends.
- Reports (development builds only) carry no cookies, tokens, signatures, search terms, visitor data or keys.
- The public build records, stores and sends no telemetry of its own and contains no collector code (Home's "shown"
  signals above are YouTube's own page events, sent to YouTube only).
- **Redline for TV**: the public TV bundle (`content.js`) is the public build. The app's shell can deliver speed telemetry
  only after its owner turned it on (About & updates → Developer): then it runs the bundle's signed development
  variant (`content-dev.js`) and sends its reports to the debug center at an address on the owner's own network
  (private ranges only), from memory only; off, the shell drops whatever reaches it and sends nothing.
- Test fixtures are recorded signed out; replace any IP with a documentation address (`203.0.113.7`) and
  visitor data with the fixed fake used in `tests/fixtures/`.

## Behaviour rules

- **The player never leaves by itself.** Failures and stalls stay in the player box with the reason and Try
  again / Open on YouTube; only that button opens YouTube's own page. What Try again would fix, the client does by
  itself first (docs/architecture.md, "The automatic Try again"); a tab whose extension was reloaded under it loads
  the same watch page again.
- **Motion never delays anything**: follow [docs/motion.md](docs/motion.md) (also the agent skill
  `.claude/skills/lumen-motion/SKILL.md`). Keyboard paths and back/forward are instant.
- Prefer immediate, reversible interactions and remembered settings: no Save buttons or confirmation steps
  for ordinary display preferences; optimistic actions roll back with the reason on failure.
- Every page must survive unknown YouTube data: unknown renderers are counted (debug overlay), never fatal;
  every text and attribute is escaped.

## Versions

`browser/manifest.json`, `Cargo.toml` and `package.json` carry the same version (`npm run check:public`
checks it). It counts like a decimal number: each digit runs 0–9, so 0.8.9 is followed by 0.9.0.

## Pull requests

- One topic per pull request, with what you checked (tests, and for UI: what you looked at, in which browser).
- `npm test` passes; new behaviour comes with a unit test or an e2e step (the e2e suite never touches the
  network: stub what you need in `tests/e2e.cjs`).
- `extension/` holds the public build of your change (`./build.sh && npm run check:public`).
