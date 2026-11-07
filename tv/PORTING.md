# The TV hooks in the shared client (porting record)

**Done (2 Nov 2026):** the TV app was built in the old private layout (`client/…`) and ported here. This file
stays as the list of every TV hook in the shared client; the paths in the table are the old ones on the left.
Since the port: the TV bundle is `./build.sh --tv` (`browser/build.cjs --target=tv`), the app is named
**Redline for TV** (until 0.4.0 "Lumen"; it keeps `app.lumen.tv`, crates `lumen-tv`/`lumen-tv-core`, `LUMEN_TV_*` build variables, release assets
`lumen-tv-*`, keys in `~/.config/lumen-tv/`), and `#ytc-player.menu` (not `#ytc-menu[data-m]`, which stays set
while a closed menu fades out) is how `tvglue.rs` sees an open player menu.

The TV work was built in the old layout (`client/…`). Everything TV-only lives in `tv/` and moves as
it is; the shared client got the small hooks below. Path map:

| old layout | this repository |
|---|---|
| `client/rust/src/*` | `src/*` |
| `client/bridge.js` | `browser/bridge.js` |
| `client/client.css` | `extension/client.css` |
| `client/build.cjs` | `browser/build.cjs` (no change needed) |
| `client/tests/fixtures/*` | `tests/fixtures/*` |
| `client/rules.json` | `extension/rules.json` |
| `tv/` | `tv/` |

Every TV path is inert on desktop: `tv::on()` is false unless the TV shell set `YTC_PLATFORM`
(or `localStorage['ytc:platform'] = 'tv'`, a development switch), `crate::tv::auth()` is
`Auth::from_cookies` there, and `tv::finish` returns the WEB request unchanged without a bearer.

## New files (copy as they are)

- `src/tv.rs` — pure TV logic: platform flag, access token store, TVHTML5 request rewriting
  (`finish`, `tv_body`, `headers`, `account_browse`, `ping_headers`), spatial navigation (`pick`),
  remote keys, device-code/poll parsing; unit tests.
- `src/app/tvglue.rs` — TV wiring of the app: boot flag + token hold, shell calls (`on_shell`),
  sign-in panel, D-pad moves (`tv_key`, `tv_move`), Back (`on_back`), focus that is never lost
  (`tv_after_paint`, `tv_focus_check`, `tv_focus_default`, `tv_opened`); tests.
- `tests/fixtures/tv_playlist.json.gz`, `tests/fixtures/tv_shelves.json.gz` — TV response shapes
  for the `parse.rs` tests.

## Hooks in existing files

**`src/lib.rs`**
- `pub mod tv;`
- two exports: `#[wasm_bindgen] pub fn on_shell(seq: u32, ok: bool, json: &str)` →
  `app::with(|a| a.on_shell(..))` and `#[wasm_bindgen] pub fn on_back() -> bool` →
  `app::with_ret(|a| a.on_back())`.

**`src/config.rs`** — `Auth` gets `pub bearer: Option<String>`; `signed_in()` counts it;
`header()` returns `Bearer <t>` when set.

**`src/bridge.rs`** (+ the same names in `browser/bridge.js`, inside the `YTC` object next to
`local_flag`): `platform()`, `shell(seq, cmd, json)` (calls `YTC_SHELL.call`, answers
`on_shell`), `back_hook()` (defines `globalThis.YTC_BACK`), `spatial_rects(scope, sel, skip, mark, boxes)`
(a stop inside a `boxes` ancestor — the TV passes `.c`, the card — is measured by that ancestor),
`spatial_focus(i)`, `session_clear()`.

**`src/app.rs`**
- `mod tvglue;`; `App { …, tv: tvglue::Tv }`; `Timer::{TvHold, TvPoll { generation }, TvFocus}`
  handled in `on_timer` (`tv_release`, `tv_poll`, `tv_focus_check`); `Timer::Bg` also calls
  `tv_tick()`.
- `boot()`: `let tv_hold = App::tv_boot_flag();` first; `Tv::new(tv_hold)`; after the voice boot
  `app.tv_boot();` and, when `app.tv.hold`, no snapshot repaint/route start (the shell's token
  answer starts the first page).
- Every `Auth::from_cookies(&bridge::cookie())` → `crate::tv::auth()` (boot `auth` and `fx`,
  `send`, the vision account route, the player mount).
- `send()`: account requests (`tv_account_req`) keep the bearer, others go anonymous; body and
  headers from `crate::tv::finish(..)` instead of `api::headers(..)`.
- `on_click`: `tv-*` actions → `tv_click`; then `self.tv_opened(href)`.
- `on_key`: after `self.acts.on_key(key)`, `if let Some(h) = self.tv_key(key, editable) { return h; }`.
- `after_paint()`: `self.tv_after_paint();` · `ready()`: `self.tv_focus_check();` first.
- `use crate::api::{CHANNEL_VIDEOS_PARAMS, Req};` (the `api` module itself is no longer used there).

**`src/app/fxglue.rs`** — the two `Auth::from_cookies` + `api::headers` pairs (Watch later
membership/edit, chip continuation) → `crate::tv::auth()` + `crate::tv::finish(..)`.

**`src/actions/mod.rs`** — `signed_in()` via `crate::tv::auth()`; `post()` uses `tv::finish`
(comment and reply pages stay anonymous WEB: their continuation tokens are WEB tokens); `boot()`
shows the bell/avatar slot only when not on TV (the TV account control owns `#ytc-acct`).

**`src/player/own.rs`** — the `player` call goes through `tv::finish` (TVHTML5 + bearer when
signed in on TV) and keeps `ping_headers` (`tv::ping_headers`) for the history pings (`ping()`
sends them with the bearer); the session reports `client`/`platform`/`client_version` of the TV
client; the bot-check recovery skips the cookie account route when a bearer is used;
`pub fn paused(&self) -> bool`.

**`src/player/stats.rs`** — `Session { client, platform, … }` (empty = WEB/DESKTOP as before);
`c=` and `cplatform=` come from them; test extended.

**`src/render.rs`** — `home_nudge`: no `autofocus` on TV (the on-screen keyboard would take the
remote's arrows) and a "Sign in with your phone" button instead of the Google sign-in link;
new `tv_slot(signed_in)` (the rail's account item), `tv_panel(code, signed_in, status)` (the full-screen
sign-in with the device flow's URL and its QR code, `src/qr.rs`), `tv_rail`, `tv_prompt`, `tv_info`; on TV
`shell` writes the rail instead of the sidebar, `message`/`home_nudge` write calm cards, card images use `vw` sizes.

**`src/player/ui.rs`** — `shell_tv` (the 10-foot overlay: time row, focusable seek bar, round buttons; `player::html`
picks it on TV); **`src/player/own.rs`** — TV idle 4 s, `wake`, `rest`, `idle`, `tv_scrub`/`tv_scrub_end`, no blur after
a pressed control, elapsed/remaining in `TIME`/`TREM`.

**`src/parse.rs`** — TV renderers: `tileRenderer` (`tile()`), `tvSecondaryNavRenderer` (the
selected tab), the TV containers in the walk lists (`tvBrowseRenderer`, `tvSurfaceContentRenderer`,
`twoColumnRenderer`, TV chrome ignored); tests `tv_playlist`/`tv_shelves`.

**`extension/client.css`** — the block at the end, `/* ===== TV app (tv/, html[data-ytc-tv]) =====`
(everything scoped to `html[data-ytc-tv]` or the `.tvs` panel; copy it as one block).

## In `tv/` when it moves

- `tv/scripts/build.sh`: build the client with `./build.sh` (public build) instead of
  `client/build.sh`, and copy `content.js client.css native.css rules.json` from `extension/`
  instead of `client/`; the client version comes from `extension/manifest.json`.
- `tv/core/tests/core.rs`: `include_str!("../../../client/rules.json")` →
  `include_str!("../../../extension/rules.json")`.
- `tv/core` and `tv/shell/src-tauri` are separate Cargo packages (own lockfiles, own `target/`).
  The root `Cargo.toml` is a plain package, so nothing changes; if it ever becomes a workspace,
  add `exclude = ["tv"]`.
- `.gitignore`: `tv/core/target/`, `tv/shell/src-tauri/target/`, `tv/shell/src-tauri/bundle/`,
  `tv/out/` (and the generated `tv/shell/src-tauri/gen/android/**/build`, already ignored there).
- `tests/check-public.cjs` (if it scans the tree): `tv/keys/release.pub` is a public key on purpose;
  the private key and the APK keystore live in `~/.config/lumen-tv/`, never in the repo.
- The release repo compiled into the app: `LUMEN_TV_REPO=owner/name` when building
  (default `justerquate-sys/redline` in `tv/shell/src-tauri/src/shell.rs`; the repository was renamed from
  `justerquate-sys/lumen`, which GitHub redirects for apps built before 0.5.0).
