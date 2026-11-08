# Redline for TV (Google TV / Android TV)

The client on a TV, as the app **Redline** (until app 0.4.0 "Lumen"), driven by the remote, signed in to the same YouTube account as your
other devices, updating itself from GitHub Releases.

It is the **same client** as the browser extension (`src/`: Rust → Wasm, `browser/bridge.js`,
`extension/client.css`), not a fork. The TV app is a thin [Tauri v2](https://v2.tauri.app) shell that opens
`https://www.youtube.com` in an Android WebView and injects the client at document start, the way
the extension's content script does. The client switches to its TV mode from a platform flag the
shell sets (`YTC_PLATFORM = "tv"`).

## Install it on your TV (first time)

(The same steps are in the main [README](../README.md#google-tv--android-tv).) The newest APK is always at
**`is.gd/lumentv`** →
https://github.com/justerquate-sys/lumen/releases/latest/download/lumen-tv.apk (every release uploads it as `lumen-tv.apk` besides `lumen-tv-<version>.apk` and is
marked *latest*). After this first install the app keeps itself up to date (see *Updates*).

**Option A: the Downloader app (no computer needed)**

1. On the TV: *Settings → System → About* → select *Android TV OS build* (or *Build*) seven times →
   "You are now a developer".
2. Install **Downloader** (by AFTVnews) from the Play Store on the TV.
3. *Settings → Apps → Security & restrictions → Unknown sources* (on Google TV: *Settings → Apps →
   Install unknown apps*) → allow **Downloader**.
4. In the Downloader app, enter: **`is.gd/lumentv`**, then install.
5. Find **Redline** in *Apps* (it shows its banner); move it to your favourites if you like.

**Option B: ADB over Wi-Fi (from your computer)**

1. Enable developer options (step 1 above), then *Developer options → USB debugging* (on newer
   Google TV: *Wireless debugging*).
2. Note the TV's IP address (*Settings → Network*).
3. On the computer (Android platform-tools): `adb connect <tv-ip>:5555` (with *Wireless debugging*:
   `adb pair <ip>:<pair-port>` with the shown code, then `adb connect <ip>:<port>`), accept the
   prompt on the TV.
4. Download the APK (the link above) and `adb install lumen-tv.apk`

**Sign in:** open the app, choose *Sign in* (the rail's last item) or *Sign in with your phone* on Home, then
scan the QR code with your phone, or go to the address shown (google.com/device or youtube.com/activate), enter
the code shown on the TV and pick your account. That is YouTube's own TV sign-in: your history, subscriptions,
Watch later and playlists are the same as everywhere else. *Account → Sign out* revokes the TV's access.

**Remote:** the arrows move the focus (a steady white frame marks the focused card), OK selects, Back closes what
is open, then goes to the rail, then back (on Home it leaves the app). ← from the first column opens the rail
(Search, Home, Subscriptions, History, Watch later, Playlists, Liked, Account). *Search* opens the search screen:
the built-in keyboard opens at once, with letters, numbers/symbols, Space, Delete, Clear and Search, all operated
with the remote. Back closes the keyboard onto the query control; ↓ then walks recent searches and suggestions,
and OK on the query reopens the keyboard. A physical keyboard can type too. Another Back returns to the page.
A video
plays full screen at once; any key shows the controls (they hide 4 s after the last key while playing): ←/→ on the
bar seek 10 s (faster while held, with the storyboard frame), OK plays/pauses, ↓ goes to the buttons (captions,
speed, quality, like, Watch later, Comments, More info), with a visible label for the focused action, and once
more to Up next. Comments opens a readable side panel with Top/Newest, replies and more pages; it loads only
when opened. More info includes the description, channel, Subscribe when signed in, and chapters when available.
Back or Back to video closes either panel and returns to its player control. Media keys (play/pause, rewind,
fast-forward) work everywhere on the watch page. The look is documented in [design/](design/README.md).

## Updates

Everything is in the app: **About & updates**, the last item of the rail (under Sign in / Account, so it is there
signed out too). It shows the app version, the interface version and build, when the last check ran and what it
found, with one action focused:

The two versions advance independently: **App** changes after Android installs an APK; **Interface** changes
when the signed client update is applied. An interface update can add TV features while the installed app
version stays the same. A local build is not available to the TV until its signed release is published.

**Repository name (7 Nov 2026):** builds from shell 0.5.0 read the releases of `justerquate-sys/redline`; apps
installed before that ask `justerquate-sys/lumen` and reach the same releases through GitHub's rename redirect.
So the repository must be renamed to `redline` before any 0.5.0 build is released or installed, and no new
repository may ever be created under the old name `lumen` (that would end the redirect for every older app).

- **Check for updates** asks GitHub now (the shell's `shell_check`).
- **Update now** (a new interface was found): it is already downloaded and verified; the screen restarts with it at
  once, no relaunch, still signed in.
- **Install update** (a new app was found): downloads it with its progress, checks its hash against the signed
  manifest and opens Android's installer (`shell_install`; the first time Android asks to allow installs from
  this app: allow it and press Back, the install goes on). Choose *Update*, then *Open*. Downloader is only ever
  needed for the very first install.
- **Retry** after a failed check, with the reason (no connection, GitHub's rate limit, a signature that does not
  verify).

The automatic checks stay: at launch and every 6 hours the app reads the repository's releases, takes the newest
one carrying `lumen-tv-manifest.json`, checks its ed25519 signature against the public key compiled into the app
(`tv/keys/release.pub`) and the SHA-256 of every file, and caches it. When one finds something, a small *Update
ready* notice appears at the top right of the pages (never over a video; the rail item gets a white dot) and its
button opens About & updates. A new interface also goes live by itself the next time the app comes back to the
foreground (never mid-video). Offline, the app runs what it has; a broken or tampered download is ignored.

An app from before this screen (0.1.0 builds of 2 Nov 2026) cannot install from it: it offers a new app in its own
dialog once (*Install*); after that update everything is one click here.

Testing the flow on the emulator: a debug build reads `files/dev.json`; `node tv/scripts/release.cjs --dry-run
--no-build` signs `tv/out/release/` (`LUMEN_TV_KEY=<test key>`, `--apk <file>`, `SEQ`, `APK_VERSION`), `node
tv/scripts/release.cjs --serve 8775` serves it as a fake GitHub, and
`echo '{"release_api":"http://10.0.2.2:8775/repos/o/r/releases"}' | adb shell "run-as app.lumen.tv sh -c 'cat > files/dev.json'"`
points the app at it. (A throwaway key needs a test build with its public key in `tv/keys/release.pub`; never
commit that.)

## Build

Toolchain (all in `$HOME`, no Homebrew or sudo; `tv/scripts/env.sh` points at it and honours `JAVA_HOME`,
`ANDROID_HOME` and `NDK_HOME` if you keep them elsewhere): Temurin JDK 17, Android SDK cmdline-tools,
platform-tools, platform 36, build-tools 35, NDK 28.2, rustup targets
`aarch64-linux-android armv7-linux-androideabi` (for the shell's toolchain; the client keeps its own pinned one
from `rust-toolchain.toml`), `cargo install tauri-cli --version ^2`, plus what `./build.sh` needs
(wasm-bindgen-cli 0.2.130, Node 22+).

```sh
tv/scripts/build.sh              # debug APK (arm64) → tv/out/lumen-tv-<v>-debug.apk
tv/scripts/build.sh --release    # release APK (arm64 + armv7, signed) → tv/out/lumen-tv-<v>.apk
tv/scripts/build.sh --no-client  # reuse the last client bundle (shell-only changes)
tv/scripts/run.sh                # debug build + install + launch on the attached TV/emulator
(cd tv/core && cargo test --locked)   # the shell's logic: updates, sign-in, rules, storage, telemetry
node tv/scripts/bench.cjs [--cpu 6] [cold reload keys hold open seek menu back]
                                 # speed bench on the attached emulator/TV (debug builds), see "Speed"
```

The first step is `./build.sh --tv`: the shared client built as the **public** build into
`tv/shell/src-tauri/bundle/` (`content.js`, its Wasm `client.wasm`, the extension's `client.css`, `native.css` and
`rules.json`, a bundle sequence `seq` and the client version `client`; `browser/build.cjs --target=tv`). Since app
0.3.0 the bundle is **format 2**: `content.js` (130 KB) carries no Wasm; it fetches `/__lumen/client.wasm`, which the
shell answers from the bundle, right after stopping YouTube's page, and compiles it while it streams in (the
extension keeps the Wasm inside its `content.js`). A format-1 bundle (the Wasm inside `content.js`) still runs on
0.3.0. That folder is the
APK's built-in client and what a release publishes; `extension/` is not touched. `./build.sh --tv --dev` adds
`content-dev.js` and `client-dev.wasm` there, the same client with its development-build speed telemetry: `tv/scripts/build.sh` builds
both (the APK carries both, a release signs both), and the app runs it only while its owner turned telemetry on
(see *Speed telemetry*). `SEQ=<n>` sets the
sequence (default: the current Unix time; the app installs a bundle only when its `seq` is higher than the
one it runs).

In a browser, the TV mode can be tried without the shell: `localStorage['ytc:platform'] = 'tv'` on
youtube.com switches the client to it, and `localStorage['ytc:shell']` (JSON `{ "<command>": "<answer>" }`)
answers its shell calls (`tests/e2e.cjs` uses both).

### Keys (never in the repository)

All signing material lives in `~/.config/lumen-tv/` on the release machine; `.gitignore` refuses key and
keystore files anywhere in the tree. Only the public key `tv/keys/release.pub` is committed (the app compiles
it in).

| File | What | Lost? |
|---|---|---|
| `apk-release.jks` + `keystore.properties` (`storeFile`, `storePassword`, `keyAlias`, `keyPassword`) | signs the release APK (`gen/android/app/build.gradle.kts` reads the properties file) | Android refuses to update an installed app with an APK signed by another key: every TV must uninstall (losing its sign-in) and install by hand. |
| `release-key.pem` (ed25519, PKCS#8) | signs `lumen-tv-manifest.json` of every release | TVs never accept another client update until an APK with a new `release.pub` is installed by hand. |

**Back up both** (e.g. in a password manager or an encrypted drive, not in a cloud folder next to the
repository). Creating them the first time:

```sh
mkdir -p ~/.config/lumen-tv && cd ~/.config/lumen-tv
openssl genpkey -algorithm ed25519 -out release-key.pem
# the public key for tv/keys/release.pub (raw 32 bytes, base64):
openssl pkey -in release-key.pem -pubout -outform DER | tail -c 32 | base64
keytool -genkeypair -keystore apk-release.jks -alias lumen-tv -keyalg RSA -keysize 4096 -validity 36500
printf 'storeFile=%s\nstorePassword=…\nkeyAlias=lumen-tv\nkeyPassword=…\n' "$HOME/.config/lumen-tv/apk-release.jks" > keystore.properties
chmod 600 *
```

## Release

```sh
GITHUB_TOKEN=… node tv/scripts/release.cjs            # build, sign, publish
node tv/scripts/release.cjs --dry-run                 # build and sign into tv/out/release/ only
```

Before a release, advance the shared client version in `Cargo.toml`, `package.json` and
`browser/manifest.json` together (including their lockfiles). For a new APK, advance the shell version in
`tv/shell/src-tauri/Cargo.toml` and its lockfile too. Verify the APK's actual Android version name and code,
then check the staged package against the previous published release with the Rust updater:

```sh
LUMEN_TV_RELEASE_DIR="$PWD/tv/out/release" \
LUMEN_TV_PREVIOUS_RELEASE_DIR=/path/to/previous-release \
cargo test --locked --manifest-path tv/core/Cargo.toml --test release_package -- --ignored
```

The previous-release directory contains its published manifest and signature. This check verifies both
signatures, every staged file, increasing version/sequence values, the fixed APK download alias, the older
app's update offer and download, and the new app's up-to-date state. It serves artifacts only on loopback.

It builds the release APK (`tv/scripts/build.sh --release`), signs `lumen-tv-manifest.json` (the bundle's files and
the APK with their SHA-256, the bundle sequence, the client version) with
`~/.config/lumen-tv/release-key.pem` (`LUMEN_TV_KEY` overrides), verifies the signature with
`tv/keys/release.pub` (a mismatched key would publish a release no TV accepts) and publishes a GitHub release
`lumen-tv-<shell version>-<seq>`, marked *latest*, carrying the bundle, the APK twice (`lumen-tv-<version>.apk`,
which the signed manifest names, and `lumen-tv.apk` for the first-install link
`…/releases/latest/download/lumen-tv.apk`), `lumen-tv-manifest.json` and `lumen-tv-manifest.json.sig`.
`GITHUB_TOKEN` needs *contents: write* on the repository. GitHub's *latest* is the newest non-prerelease
release of the whole repository: if desktop releases are published there later, they must either come before
the next TV release or also carry `lumen-tv.apk`, or the first-install link answers 404.

The repository the app reads its releases from is compiled in: `LUMEN_TV_REPO=owner/name` when building (default
`justerquate-sys/redline`); give `release.cjs` the same `LUMEN_TV_REPO` when publishing from a fork. The repository
was renamed from `justerquate-sys/lumen`: apps built before 0.5.0 still ask for that name, and GitHub's redirect for
renamed repositories (which the app follows) brings them to the same releases, as long as no new repository takes
the old name. The tag prefix `lumen-tv-` and the asset names stay as they are: installed apps look for exactly
these. A client-only
release (most of them) needs no new APK: the TVs pick up the bundle. A format-2 bundle (`client.wasm`) needs app 0.3.0:
`release.cjs` then sets the manifest's `min_shell` to 0.3.0's version code (3000) at least, so an older app never
installs it: its update check reports that the interface needs a newer app, keeps running the bundle it has and offers
the release's APK (About & updates → Install update). For a new APK, bump `version` in
`tv/shell/src-tauri/Cargo.toml` (the version code is derived from it) and release.

Testing an update locally: `node tv/scripts/release.cjs --dry-run --no-build` then
`node tv/scripts/release.cjs --serve 8775`, and in a **debug** build point the app at it:
`echo '{"release_api":"http://10.0.2.2:8775/repos/o/r/releases"}' | adb shell "run-as app.lumen.tv sh -c 'cat > files/dev.json'"`
(`"oauth": "http://10.0.2.2:<port>"` points the sign-in at a mock the same way). Release builds
ignore `dev.json`.

## Speed telemetry (the owner's TV, off by default)

The TV can send the same speed telemetry as the development extension (docs/debug-center.md) to the debug center on
your Mac, so the real device can be measured. It is **off** in every install until you turn it on, and then:

- the app runs the bundle's telemetry variant (`content-dev.js`, signed with the release like the rest), the page shim
  hands its reports (`ytcPerf` batches, `ytcIncident` reports) to the shell, and the shell POSTs them to
  `http://<your Mac>:47321/incident`, oldest first, retrying once a minute; each report carries
  `"platform":"tv"` and its build (`0.10.0 (…) (TV 0.2.0, release, telemetry)`), so `/perf` and `/timeline` show the TV
  apart from the desktop (*Platform* filter, `&platform=tv`);
- the address can only be on your own network (private IPv4, `*.local`, Tailscale's 100.64/10): never the internet;
- reports are kept in memory only (last 30 batches, 100 incidents) and contain what the desktop's contain: timings,
  route kinds and video ids, never titles, searches (an incident's page path loses its query), tokens, cookies or
  the account.

**Turn it on**

1. On the Mac: `cd tools/debug-center && cargo build --release --locked`, then
   `./target/release/lumen-debug-center --lan` (the default stays 127.0.0.1 only; `--lan` also listens on the network,
   answers the TV's discovery probe and prints the address to type; devices on the network can only deliver reports,
   the dashboards stay readable on the Mac only). macOS may ask once whether it may accept incoming connections: allow.
2. On the TV: *About & updates* (the rail's last item) → on the **Back** button press **→ five times**: the
   *Developer · speed telemetry* card appears on the right.
3. *Find on network* fills in the Mac's address (or OK on the field, type it, e.g. `192.168.1.20`, and the keyboard's
   action key), then **Turn on**. The screen loads again with telemetry and comes back to the card, which now says
   *On* with the number of reports sent and waiting (and the last delivery error, if any).
4. Watch it arrive: http://127.0.0.1:47321/perf?platform=tv and http://127.0.0.1:47321/timeline?platform=tv on the Mac.

*Turn off* on the same card stops it at once (the queue is dropped, the public client runs again). Debug builds can
also turn it on from `files/dev.json`: `{"telemetry": "http://10.0.2.2:47321"}` (the emulator's host).

## Speed

Measured on the emulator (1920×1080, DPR 2, Android 16, WebView 143, signed out, real YouTube) with
`tv/scripts/bench.cjs`, 3 Nov 2026; before → after this round, interleaved A/B runs (the Mac was busy, so absolute
numbers drift; the comparison does not):

| Path | Before p50 / p90 | After p50 / p90 |
|---|---|---|
| Cold start → Home painted | 1462 / 2192 ms | 1317 / 1570 ms |
| … YouTube's document (response) | 368 / 410 ms | 34 / 58 ms (the shell's own) |
| Reload → Home painted | 312 / 380 ms | 223 / 434 ms |
| D-pad press → focus painted, 6× CPU (a TV) | 144 / 493 ms (handler 93) | 53 / 74 ms (handler 23) |
| Held arrow (20 repeats/s), 6× CPU, per repeat | 45 / 277 ms, long frames 5.6–7.6 s per 7 s | 28 / 103 ms, 1.1–2.5 s |
| D-pad press, 1× CPU | 20 / 36 ms (handler 9) | 16 / 22 ms (handler 3) |

Second round (3 Nov 2026, same method; the Mac at load ≈ 11, so absolute numbers are higher):

| Path | Before p50 / p90 | After p50 / p90 |
|---|---|---|
| Cold start → Home painted, format 1 → 2 (no Wasm in the injected script) | 2122 / 2331 ms | 1943 / 2202 ms |
| … YouTube's document → the client's shell | 291 / 374 ms | 215 / 323 ms |
| Cold start, the client's shell → Home painted: network path → the kept ready response | 477 / 698 ms (signed out: a small Home request; signed in it is the 0.9–1.7 s TVHTML5 request) | 92 / 106 ms |
| Player overlay, one button move, 6× CPU | ~200 ms (12 presses: 77–398) | ~40 ms (15–248) |

What changed in the second round: the Wasm left the injected script (format 2, above; the script V8 parses on every app
start went from 1.6 MB to 130 KB, and the Wasm compiles while it streams, off the main thread); a cold start paints the
ready Home response the shell kept (below); and the player's `:has(.pctl :focus) *` cursor rule applies only with a
pointer (it restyled the whole player, ~280 elements three times, on every button move). Reloads gained little from the
split: V8 already reuses its compile of the injected script within a run.

What changed: the shell serves the document for pages the client renders (`rules::client_page`; the client stops
YouTube's page at its first byte, so YouTube's HTML only cost a round trip; not before the WebView holds YouTube's
visitor cookies, so the first start ever still gets YouTube's page); the document-start script is composed off the UI
thread while the WebView starts; the focus engine measures a card by its own box (an off-screen
`content-visibility:auto` card needs no layout for that; its title link did, one forced layout per card and key); a
held arrow is not link intent (only where it rests: no watch prefetch for every card it passes); and two desktop
keyboard rules with `:has(:focus-visible)` apply only where there is a pointer (`@media (any-pointer:fine)`): on every
focus move they made the browser restyle the whole grid twice (~340 elements, 70 ms per press at a TV's CPU).

**The ready Home response across app starts** (owner decision, 3 Nov 2026): the page's ready Home response (what
YouTube answered to its Home request, fetched ahead for the next Home) is also kept by the shell, sealed with the
account's Android Keystore key in `files/ready.bin`, for that account only and at most 10 minutes; it is read and
deleted at the next launch, so a cold start within 10 minutes paints Home on its first frame (`readystore.rs`).

## Architecture

```
tv/
  shell/src-tauri/        the Tauri app (Rust)
    src/lib.rs            window on https://www.youtube.com, the page's one command `ytc`
    src/shell.rs          state: account, storage, bundle in use, update thread, telemetry delivery
    src/host.rs           JNI: Kotlin → Rust (init, script, user agent, request rules) and
                          Rust → Kotlin (Keystore seal/open, bundle swap, update dialog, installer)
    src/script.rs         the document-start script: shim + client bundle; the desktop user agent
    gen/android/…/app/lumen/tv/MainActivity.kt, Host.kt
                          what Android forces into Kotlin: activity hooks (Back first to the client,
                          media keys, swappable document-start script), Keystore AES-GCM, the
                          WebViewClient wrapper applying the rules, the update dialog and installer
  core/                   the shell's logic without Tauri/Android, tested on the host:
    release.rs            signed manifest, verified cache, GitHub release lookup, APK download
    auth.rs               OAuth device flow of YouTube's TV client, refresh, revoke
    rules.rs              extension/rules.json (declarativeNetRequest subset) for the WebView; the client's own
                          pages (served the shell's document)
    store.rs              chrome.storage.local as a JSON file
    readystore.rs         the ready Home response kept across app starts (sealed, one account, 10 min, read once)
    tel.rs                speed telemetry to the owner's debug center (off by default; LAN only, memory only)
  shim/shim.js            the extension runtime for the page: YTC_PLATFORM/YTC_SHELL, chrome.storage
                          and chrome.runtime (reports to the shell while telemetry is on), the stylesheet,
                          keyboard-on-OK for text fields
  keys/release.pub        the release public key compiled into the app
  scripts/                env.sh, build.sh, run.sh, release.cjs, bench.cjs (speed bench), keys.sh/cdp.cjs (debug helpers)
```

In the shared client (`src/`): `tv.rs` (pure TV logic: spatial navigation `pick`,
remote keys, token and device-code parsing, TVHTML5 request rewriting; unit-tested) and
`app/tvglue.rs` (wiring: D-pad focus moves with the row anchoring and focus memory, the rail, the search
screen, the player overlay keys and scrubbing, More info, Back, the sign-in screen, the token from the shell,
focus that is never lost), `qr.rs` (the sign-in QR code), plus small hooks listed in `PORTING.md`; the TV styles
are the block `/* ===== TV app` at the end of `extension/client.css` (everything under `html[data-ytc-tv]`; one
layout at every resolution, rem = 1/80 of the width). Design reference: `tv/design/` (look sheet + tokens).

**What replaces the extension's parts on TV**

| Extension | TV app |
|---|---|
| content script at document_start (+ `client.css`/`native.css`) | `WebViewCompat.addDocumentStartJavaScript` for `https://www.youtube.com`, shim + bundle |
| `chrome.storage.local` | shell command → `files/storage.json` |
| declarativeNetRequest (`rules.json`) | `Interceptor.shouldInterceptRequest` → `rules.rs` (block; googlevideo fetched natively with `Access-Control-Allow-Origin: *`) |
| background.js: incidents, dev reload, voice socket relay | off on TV (no mic); incidents and speed telemetry only while the owner turned telemetry on (`tel.rs`) |
| YouTube's page as the document the content script stops | the shell's own document for the client's pages (`rules::client_page`) |
| browser cookies (Google sign-in) | YouTube TV device sign-in; `TVHTML5` + `Authorization: Bearer` for account calls |

**Account calls on TV.** Home, Subscriptions, History, Library, the account's playlists (`VL…`),
their continuations, the watch page's `player` call (so history pings carry the account) and actions
(like, subscribe, Watch later) go out as `TVHTML5` with the access token; search, channels, comments
and the streams (VISIONOS) stay anonymous WEB calls as on desktop. History pings
(`/api/stats/playback`, `watchtime`) carry the bearer and report `c=TVHTML5&cplatform=TV`.

## Risks and limits

- The shell's own document (app start, reload) means YouTube's page response no longer refreshes cookies on those
  loads; its API answers still do, and the first start ever (no visitor cookies yet) gets YouTube's page as before.
  If YouTube ever needs its page for something (a consent step), `?yt-native=1` pages and every path outside the
  client's (`rules::client_page`) still load it.

- Whether YouTube honours every account call made with the TV token (feeds, like/subscribe, the
  watch history pings) can only be confirmed with a real sign-in; the flow up to the token is
  verified against Google (code issued, polling answers `authorization_pending`) and the exchange
  against a mock. If an account surface misbehaves, the fix is in `tv.rs`/`tvglue.rs`
  (`account_browse`, `tv_account_req`) and the TV parser in `parse.rs`.
- The client ID/secret are YouTube's public TV values (`tv/core/src/auth.rs`); if Google rotates
  them, update them there (new APK).
- googlevideo is fetched by the shell (Kotlin `HttpURLConnection`) to add the CORS header the
  extension's rule adds; on very slow TVs this costs some throughput. The rule can be dropped from
  `rules.json` (a bundle update) if it ever hurts. Each such request holds one of the WebView's request
  threads until its headers arrive, even if the page gave it up (a hedged request won on a mirror):
  hence 6 s to connect / 15 s per read. The client warms the shell's connection with
  `/generate_204` (a `<link rel=preconnect>` would warm only the WebView's own). Up to 16 keep-alive
  connections per host.
- Android TV WebView versions differ by device; the client needs a recent one (Chrome 120+; the
  emulator ran 143). Very old Android TV 8/9 sets with an outdated WebView may misbehave.
