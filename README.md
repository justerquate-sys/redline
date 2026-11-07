# Redline

**A fast, ad-free client for YouTube, running at www.youtube.com.** Redline is a browser extension that stops
YouTube's web app before it starts and renders its own fast, quiet pages from YouTube's own data (the
InnerTube API), with its own video player. Your account, subscriptions, history and playlists keep working,
because it is still youtube.com with your session. The logic is Rust compiled to WebAssembly; JavaScript is
only a small generated loader and a generic bridge.

> **Status: experimental.** It relies on unofficial YouTube interfaces that can change at any time. When
> something breaks, the player offers a way to YouTube's own page for that video (see "Escape hatch").

Works in Chromium browsers (Chrome, Brave, Edge, Vivaldi, Opera, Chromium) on macOS, Windows and Linux,
and as an app on **Google TV / Android TV** (see below). A Firefox version is planned.

## Screenshots

_To be added:_

- Home with the topic row and Filters
- Search results with the dropdown (history + suggestions)
- Watch page: player, chapters, description links, comments
- Player menus (Speed / Quality / CC) and the SponsorBlock bar
- Voice search listening pill

## Features

- **Fast pages.** Home, Subscriptions, History, Watch later, Playlists, Liked videos, Search, channels and
  playlists, rendered by Rust from InnerTube responses. Feeds preload up to 100 videos in idle time; hovering
  a link prefetches its page; back/forward and reloads repaint instantly from memory or this tab's session.
- **Its own video player** (no YouTube player JavaScript, no iframe): VP9/AV1/H.264 via Media Source
  Extensions, quality and codec choice, speed 0.25–2.5×, chapters with storyboard frames on the seek bar,
  captions you can drag and resize, live streams with DVR, resume positions, watch history kept in your
  account. Failures stay in the player with the reason and a **Try again** / **Open on YouTube** choice.
- **Captions on one click.** Click CC to toggle them; hover for language and audio options. The icon
  is outlined when off and filled when on. From the keyboard, Enter/Space toggle and ↑/↓ open options.
- **Hover navigation:** rest on a desktop sidebar link for 250 ms to open it. A short pass, dragging,
  typing or an open menu never activates it. Clicks and keyboard shortcuts work immediately.
- **Watch later, newest saves first.** The list uses YouTube’s date-added order across pages, in your
  language. If YouTube refuses the sort, the list still shows (with **Try again**). Removing a bookmark
  removes the card immediately; a failed request restores it in place with a retry action.
- **SponsorSkip** built into the player, with its switch and category choices in the browser’s **Redline**
  extension popup. Segment data from SponsorBlock; privacy-preserving (see "Privacy").
- **No Shorts unless you want them**, no ads, no Premium/membership upsells. Optional filters: minimum length, Music, Live, popularity order, playlists only.
- **Your own Home topics** next to YouTube's own chips (rename, reorder, hide them).
- **Block websites**: hide videos whose description links to a domain you blocked.
- **Search** with your recent searches and YouTube's suggestions in one dropdown; YouTube's own search filters.
- **Watch-page actions**: like/dislike, subscribe, save to playlists, share, comments with replies, the
  notification bell and the account menu, all optimistic (instant, rolled back with the reason on failure).
- **Voice search** (optional, your own Deepgram key): hover the mic or hold ⌥ Option / Alt and speak.
- **Keyboard first**: `?` lists the shortcuts; `/` focuses search; `g h` Home, `g s` Subscriptions…
- **Motion that never costs speed**: every result is usable on its first frame; animation rides on top
  ([docs/motion.md](docs/motion.md)).
- **A familiar look:** graphite surfaces, white text and controls, red for the brand and playback, blue for
  links, on the desktop and on the TV alike.

## Install (no build needed)

The repository contains the ready-to-load extension in [`extension/`](extension/).

1. Get the code: **Code → Download ZIP** on GitHub and unzip it, or `git clone` the repository. Keep the folder
   somewhere permanent: the browser loads the extension from it every time it starts.
2. Open your browser's extensions page:

   | Browser | Address |
   |---|---|
   | Chrome / Chromium | `chrome://extensions` |
   | Brave | `brave://extensions` |
   | Edge | `edge://extensions` |
   | Vivaldi | `vivaldi://extensions` |
   | Opera | `opera://extensions` |

3. Turn on **Developer mode** (a switch at the top right; in Edge it is in the left sidebar).
4. Click **Load unpacked** and choose the **`extension`** folder inside the repository (the folder that
   contains `manifest.json`), not the repository folder itself.
5. Open https://www.youtube.com/. Sign in to YouTube as usual if you want your feeds.

This is the same on macOS, Windows and Linux. To update: replace the folder's contents with the new version
(or `git pull`), then press the extension's reload button (↻) on the extensions page and reload YouTube tabs.
To remove it: **Remove** on the extensions page.

If you use another extension that changes youtube.com (a restyler, an ad blocker's YouTube rules), turn it
off for a first try: two extensions acting on the same page can get in each other's way.

### Optional: voice search key

Voice search uses [Deepgram](https://deepgram.com/)'s speech recognition with **your own** API key.
Right-click the mic in the search box (or focus it and press the context
menu key), paste the key, **Save**. The browser asks once for microphone access on youtube.com. Without a key
the mic only offers this panel; nothing is sent anywhere.

## Google TV / Android TV

The same client runs on a TV as the app **Redline for TV**, with its own 10-foot interface driven by the remote: a rail
on the left (Search, Home, Subscriptions, History, Watch later, Playlists, Liked, Account), big cards under a
white focus frame, a search screen for the TV keyboard, and a full-screen player whose controls appear on
any key (←/→ seek 10 s with a preview frame, OK plays/pauses, ↓ to the buttons and Up next). Back closes what is
open, then goes to the rail, then back. It is a small app around an Android WebView that opens youtube.com with the client injected,
the way the extension does (details in [tv/README.md](tv/README.md)).

### Install

The newest app is always at **`is.gd/lumentv`**
(the short form of <https://github.com/justerquate-sys/lumen/releases/latest/download/lumen-tv.apk>, which every release keeps pointing at the newest APK; each release on
the [Releases](https://github.com/justerquate-sys/lumen/releases) page also carries it as
`lumen-tv-<version>.apk`). You install it once; after that it updates itself.

**With the Downloader app (no computer):**

1. On the TV: *Settings → System → About* → select *Android TV OS build* (or *Build*) seven times until it
   says you are a developer.
2. Install **Downloader** (by AFTVnews) from the Play Store on the TV.
3. Allow it to install apps: *Settings → Apps → Security & restrictions → Unknown sources* (on Google TV:
   *Settings → Apps → Install unknown apps*) → **Downloader**.
4. In the Downloader app, enter: **`is.gd/lumentv`**, let it download and choose **Install**.
5. **Redline** appears in *Apps* with its banner; add it to your favourites if you like.

**With ADB over Wi-Fi (from a computer with Android's
[platform-tools](https://developer.android.com/tools/releases/platform-tools)):**

1. Turn on developer options (step 1 above), then *Developer options → USB debugging* (newer Google TV
   models: *Wireless debugging*).
2. Find the TV's IP address in *Settings → Network*.
3. `adb connect <tv-ip>:5555` and accept the prompt on the TV. With *Wireless debugging*:
   `adb pair <tv-ip>:<pairing-port>` with the code the TV shows, then `adb connect <tv-ip>:<port>`.
4. Download the APK (the link above) and `adb install lumen-tv.apk`

### Sign in with a code

Open the app and choose **Sign in** (the rail's last item) or **Sign in with your phone** on Home. The TV shows
a code and a QR code: scan it with your phone, or go to the address shown (google.com/device or
youtube.com/activate) on your phone or computer, enter the code and choose your Google account. The
TV continues by itself. This is YouTube's own TV sign-in, so your history, subscriptions, Watch later and
playlists are the same as on your other devices. **Account → Sign out** on the TV signs it out again.

### Automatic updates

At launch and every 6 hours the app checks this repository's GitHub Releases. A new client (everything you
see) is downloaded, checked against the release key built into the app (an ed25519 signature over a manifest
of SHA-256 hashes) and used from the next page you open, never in the middle of a video; a download that
does not verify is ignored and offline the app keeps what it has. A new version of the app itself, which is
rare, is offered on screen: **Install** downloads it, checks it the same way and opens Android's installer.

## Escape hatch

- The player's failure panel has **Open on YouTube**, which opens YouTube's own page for that video
  (`?yt-native=1`). Leaving that video brings you back to YouTube's client pages.
- Account pages, sign-in, Premium, live chat and embeds always stay YouTube's own.
- If the extension ever fails to start on a page, that page falls back to YouTube's own site.

## Privacy

Redline has no server, no analytics and no account of its own. What it stores stays in your browser's
extension storage on this device (preferences, the last 100 searches, resume positions for up to 2,000
videos, your topics and blocked websites, an optional Deepgram key). Feeds, descriptions and recommendations
are kept in memory and in this tab's session only.

Every outside service it talks to, and when:

| Service | When | What it sends |
|---|---|---|
| **YouTube** (`www.youtube.com`, `*.googlevideo.com`, `i.ytimg.com`, `yt3.ggpht.com`) | Whenever you use it, like youtube.com itself | The same kind of requests YouTube's own site makes, with your YouTube session if you are signed in. Video streams come from googlevideo. YouTube's own logging/ad endpoints (`log_event`, `/pagead/`, `csi_204`, doubleclick) are blocked. |
| **SponsorBlock** (`sponsor.ajay.app`) | When a video starts, unless you switch SponsorSkip off in the Redline extension popup | Only the first 4 hex characters of the SHA-256 hash of the video id (k-anonymity: the answer covers ~150 videos and yours is picked locally). No cookies, no referrer, nothing reported back. Switched off, nothing is sent. |
| **Deepgram** (`api.deepgram.com`) | Only while you are dictating a voice search, and only if you saved a key | Your microphone audio (16 kHz mono) for that dictation and your key. The key never enters the YouTube page; the extension's background worker adds it. |
| **Google sign-in** (`oauth2.googleapis.com`), TV app only | While the sign-in code is shown, and to refresh or revoke the TV's access | YouTube's TV device sign-in: the request for a code, polls until you approved it, then token refreshes. The tokens stay on the TV, sealed with the Android Keystore. |
| **GitHub** (`api.github.com`, release downloads), TV app only | At launch and every 6 hours | A request for this repository's releases and, when there is a newer one, its files. Nothing about you or your viewing. |

The extension's permissions: `storage`, `declarativeNetRequest` (the static rules in `extension/rules.json`)
and host access to exactly the sites above. The public build contains no telemetry and sends nothing
anywhere else; the development build (below) can report incidents and timings to a collector on your own
computer only.

## Known limits

- Experimental and unofficial: the player uses one unofficial InnerTube client profile (`VISIONOS`), which
  YouTube can change or block at any time. Some networks get YouTube's "confirm you're not a bot" check; the
  player then retries with your signed-in session or a fresh anonymous visitor, and shows the reason if all fail.
- Signed-in feeds were written against current formats but are mostly tested with signed-out data;
  brand accounts and other `authuser` indexes are not supported yet.
- Not built: writing comments, rating comments, the subscription bell levels, channel tabs other than Videos,
  live chat and live captions, Shorts-style looping.
- Age-restricted and members-only videos show the panel; **Open on YouTube** plays them there.
- Chromium browsers and Google TV / Android TV only for now. On TV, account surfaces depend on YouTube
  honouring its TV client's sign-in for each call (see [tv/README.md](tv/README.md), "Risks and limits").

More: [docs/architecture.md](docs/architecture.md) ("Known gaps").

## Build from source

You only need this to change the code. Requirements:

- [Rust via rustup](https://rustup.rs/): the pinned toolchain (1.99.1 + the `wasm32-unknown-unknown` target)
  installs itself from `rust-toolchain.toml` on first use.
- `wasm-bindgen-cli` 0.2.130: `cargo install wasm-bindgen-cli --version 0.2.130 --locked`
- Node.js 22 or newer.
- A POSIX shell for `build.sh` (macOS/Linux terminal; on Windows Git Bash or WSL).

```sh
./build.sh            # public build → extension/content.js, background.js, manifest.json
./build.sh --dev      # development build (see below) into the same folder
npm install           # once: Playwright for the browser tests
npx playwright install chromium   # once: the Chromium the tests drive
npm test              # cargo test --locked + the end-to-end browser test (no network)
npm run check:public  # extension/ holds the public build (run before committing)
```

`npm test` drives Playwright's Chromium; set `LUMEN_EXTENSION_CHROMIUM` to another Chrome/Chromium binary
that can load unpacked extensions to use that instead. `YTC_ONLY=<regexp> npm run test:e2e` runs matching
steps only.

### Public and development builds

`./build.sh` makes the **public build**: what is committed in `extension/` and what users load. `./build.sh
--dev` makes a **development build** in the same folder, which adds, at build time:

- the local debug-center collector: the host permission `http://127.0.0.1:47321/*`, incident and speed-telemetry
  delivery from the background worker to [`tools/debug-center`](tools/debug-center/) (Cargo feature `dev` for the
  Rust telemetry, `/* @dev-start */ … /* @dev-end */` blocks in `browser/*.js`);
- auto-reload: open YouTube tabs ask the worker every 4 s whether the files changed (`build-id.txt`), and the
  extension reloads itself after each build;
- the name "Redline (dev)" and a build id in the version name.

Run `./build.sh` again before committing; `npm run check:public` fails on a development build.

## Repository layout

| Path | What |
|---|---|
| `extension/` | The unpacked extension, ready to load: `manifest.json`, `content.js` (generated), `background.js` (generated), `client.css`, `native.css`, `rules.json`. |
| `src/` | The Rust crate (`ytc-client`): routing, InnerTube requests and parsing, rendering, the player, features. |
| `browser/` | Browser-side sources: `bridge.js` (generic DOM/media primitives for Rust), `background.js` (service worker template), `manifest.json` (manifest template), `build.cjs` (bundler). |
| `build.sh` | Builds `extension/` from `src/` and `browser/` (`--tv`: the TV app's client bundle instead). |
| `tv/` | The Google TV / Android TV app: the Tauri shell (Rust, with the Kotlin Android requires), its tested core (updates, sign-in, request rules), the page shim, build and release scripts. See [tv/README.md](tv/README.md). |
| `tests/` | `e2e.cjs` (Playwright, all traffic stubbed), recorded InnerTube fixtures, media generators, a few `live-*.cjs` scripts that hit real services (run by hand). |
| `tools/debug-center/` | Optional local collector and speed dashboard for development builds. |
| `docs/` | [architecture.md](docs/architecture.md) (how everything works), [motion.md](docs/motion.md) (the motion law), [debug-center.md](docs/debug-center.md). |

## Contributing

Issues and pull requests are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) first: logic goes in
Rust, privacy rules are strict, motion follows [docs/motion.md](docs/motion.md), and every change keeps
`npm test` green with `extension/` holding the public build.

## License

[MIT](LICENSE) © 2026 justerquate-sys. Redline is not affiliated with YouTube or Google; YouTube is a trademark of Google LLC.
