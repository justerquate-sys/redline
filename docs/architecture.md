# Redline architecture and behaviour

The design reference for contributors: how the client works, page by page, and why. For installing and
building see the [README](../README.md); for the motion rules see [motion.md](motion.md); for the
development-only debug center and speed telemetry see [debug-center.md](debug-center.md).

Instead of modding YouTube's web app, the extension stops it at `document_start` and renders its own pages
from YouTube's InnerTube API. The logic is Rust compiled to Wasm (`src/`); JavaScript is only the generated
loader and a generic bridge (`browser/bridge.js`).

## Build pipeline

`build.sh` compiles the crate for `wasm32-unknown-unknown` (`--features dev` for the development build), runs
`wasm-bindgen` into `target/pkg/`, then `browser/build.cjs` writes the loadable extension:

- `extension/content.js`: the bridge, the wasm-bindgen glue and the Wasm bytes (base64 inside a block comment, read
  back with `Function.prototype.toString`), then `boot()` (or `boot_native()` on `?yt-native=1` pages). YouTube's
  document is stopped first (`window.stop()`), except Home's (`/`): there `boot()` stops it (`bridge::page_stop`,
  before any request of ours, since a stop aborts the document's requests) unless it is the Home answer the client
  reads (`bridge::doc_hold`, "Home reload" below); the HTML parser cannot run while the content script does, so
  nothing of the document runs or renders in between. A boot failure stops it and falls back to YouTube's own page.
  (The TV bundle stops the document in its loader as before.)
- `extension/background.js` and `extension/manifest.json` from `browser/background.js` and the manifest template
  `browser/manifest.json`. The public build drops every `/* @dev-start */ … /* @dev-end */` block; the development
  build keeps them and adds the debug-center host permission, a "(dev)" name and a build id (`version_name` and
  `build-id.txt`, which drive the auto-reload).
- `client.css`, `transcripts.css`, `pages.css`, `popup.css`, `native.css` and `rules.json` are hand-written and live
  directly in `extension/`.
- The second Wasm: `transcripts/` is built into `target/pkg-tr/`, and `browser/build.cjs` writes `extension/transcripts.wasm`,
  `extension/transcripts-core.js` (for `reader.html`, `settings.html` and the popup's Transcripts section), and appends the same
  glue to `background.js` (see "Transcripts"). `./build.sh --tv` builds none of it and compiles the feature out of the
  client (`--no-default-features`).
- `extension/popup.html` is copied from `browser/popup.html`. The browser action loads the same public
  `content.js`/Wasm in an extension document; Rust `popup.rs` renders and handles it. The loader skips stopping
  or navigating extension documents. No duplicate Wasm or JavaScript feature logic.

Browser targets: `browser/build.cjs` has a `TARGETS` table with one entry today, `chromium` → `extension/`.
A second browser (Firefox: `background.scripts` instead of a service worker, `browser_specific_settings`)
gets its own manifest template next to `browser/manifest.json` and its own output folder; the Rust crate,
the bridge and the CSS are shared.

## Escape hatch (native YouTube)

- The player's failure panel has an **Open on YouTube** button (there is no header link any more). It opens the
  video with `?yt-native=1`, which loads YouTube's own site for that page (the content script is excluded by glob and the DNR rules allow
  everything that document needs). Reloading without the parameter returns to the client.
- These paths always stay native: `/embed/*` (YouTube's embed; not used by the client, which refuses to play on youtube.com), `/live_chat*`, `/account*`, `/premium*`,
  `/paid_memberships*`, `/signin*`, `/logout*`, `/redirect*`.
- If the Wasm ever fails to boot, the loader falls back to `?yt-native=1` for that page.

### YouTube's own pages keep the client's setup (native mode cleanup)

Pages the client leaves to YouTube (the player panel's **Open on YouTube**, routes the client does not
render) get a slim version of the old extension's cleanup (2 Nov 2026), so no second extension is needed for them. Only these pages: the manifest's second
content script (`include_globs: *yt-native=1*`) injects `native.css` and boots `content.js` in native
mode (`boot_native`); client pages are untouched.

- **`native.css`** (hiding and theming, no JavaScript timing): the old `theme.css`, `watch-page.css`,
  `player-controls.css`, `captions.css` and `promotions.css` rules with their verified selectors, gated on
  `html[data-ytc-native]`. Header without voice search, Create or the locale label, with YouTube's own
  logo exactly as YouTube draws it; YouTube's guide trimmed to Home, Subscriptions, History, Playlists,
  Watch later, Liked videos (no Shorts, You/Your channel/Your videos/Downloads/courses/podcasts, channel
  list, Explore, More from YouTube, settings rows, footer); no Playables; Shorts shelves and cards out of
  feeds unless the client's Shorts switch (`shortsInFeeds`) is on (`/shorts/` itself untouched); Premium /
  membership banners and their feed slots hidden; watch page without Join, Thanks, the action overflow,
  Auto-dubbed badges, hashtags above the title, Summary / How this was made / Games / Transcript / repeated
  channel / Video details sections, and without the related chip row; player without the fullscreen More
  videos grid, quick actions, title overlay, channel watermark, gradients and cinematics, black around the
  picture; the client’s palette (graphite, white, red) over YouTube’s theme variables. The header logo is never
  touched (owner decision, 7 Nov 2026).
- **`src/native/clean.rs`** (what needs logic; pure parts unit-tested): captions in the client's look
  and place (cyan Arial with a dark outline, no backgrounds, upper-left corner and size from the same
  `ytcConfigV1.p.cs` the client player uses, via CSS variables on `<html>`); drag a cue to move it, wheel
  over it or use the slim range beside it to size it, and the result is written back into `p.cs` (read,
  modify, write: other config fields are kept); the "click gear for settings" notice is hidden. CC on/off
  follows the client's `p.cc` in a video's first seconds (at most 3 clicks on YouTube's own CC button), and
  a deliberate toggle here (click or `c`) becomes the remembered choice. 0–9, `+`/`-`/`=`/`_` and `V` are
  swallowed while the page or player has the keyboard (not in text fields, dialogs or with modifiers); the
  fullscreen bare-video wheel never opens YouTube's More videos drawer (a passive window listener decides;
  only the player box can cancel a scroll). Offer dialogs (upsell / Premium spotlight / members-only) and
  offer mealbars are closed through their own dismiss button, never a trial button; account notices stay.
  "Auto-dubbed" badges are matched by text and marked for the stylesheet.
- **`bridge.js`**: generic page primitives for this (`root_attr`, `root_var`, `page_watch` = one
  MutationObserver coalesced to a frame, `page_attr`, `page_texts`, `page_mark`, `page_click`,
  `page_mount`, `page_value`, `page_guards`); every selector and decision comes from Rust.
- Leaving that native video for anything the client renders still reloads into the client (`native/mod.rs`).
- Size: `content.js` +26 KB (Wasm +10 KB, glue and bridge +12.5 KB); `native.css` 17 KB, loaded on native pages only.
- Left out (old extension features not carried over): the player hover menus (direct Speed/Quality/CC
  entries, compact menus, hidden settings rows), the seven-link pinned sidebar (YouTube's guide is trimmed
  instead), description resource-link panel and website blocking, Watch later card buttons, Music/Live/
  duration/order filters, comments/notifications/account-menu restyling, search page chip cleanup, history
  and resume records, separate speaker caption windows (every cue window takes the shared place).

## Use

Layout: a top bar with YouTube's logo (its red play tile and the "YouTube" wordmark, aria label "YouTube home"; → Home; **resting the pointer on it for 250 ms goes Home without a
click**, see below), a centered search box (≤ 720 px; `/` focuses it from
anywhere) and, on Home and Search, **Filters** left of the bell; a left sidebar with Home, History, Watch later, Subscriptions,
Playlists, Liked videos (no Shorts entry; Shorts appear only with the Filters **Shorts** switch). The sidebar is gone on watch pages,
where the player is as large as the window allows (16:9, up to the window height) and the top bar scrolls
away. Grid of five cards per row above 1700 px (4 / 3 / 2 / 1 as the window narrows).
**Motion law** ([motion.md](motion.md)): motion never delays anything. Content is final
and usable on the first frame; animation is a compositor-only layer on top (feedback on pointerdown,
interruptible transitions, ease-out or critically damped springs computed in Rust, no motion on keyboard
or back/forward). Skeletons have the final geometry (measured layout shift 0 on a cold search).

**Navigation motion** (`src/app/motionglue.rs`). *Watch open:* a pointer click on a card (or a playlist
row) reads the thumbnail's viewport rect (`bridge::click_rect`, one layout read before the swap), the page
is written exactly as before, and at the end of the same task `bridge::flip_from` paints `#ytc-player` from
that rect to its own box: one compositor-only `element.animate` (translate + scale, origin top-left),
easing = the critically damped spring from `motion.rs` over `--d-spring-soft` (458 ms). The player is final,
playing and clickable from the first frame; what grows into place is the video's own first frame when the
pointer rested on the link (the *warm first frame*, see "Start path"), else the dark player surface until the
first frame (never the card's 320 px thumbnail scaled up); `data-flip` lifts it above its neighbours while it moves. The skeleton areas that `next` fills
(description, playlist panel, Up next, related list; the whole info block when no card data was known)
settle in with a 180 ms opacity fade. No morph and no fades for keyboard activation, `g`+letter, back/forward
or restored pages (instant); no morph when the thumbnail is off-screen; reduced motion fades the player in
(200 ms) instead. Back from a watch page has no reverse morph: back/forward are instant by law, and the
source card is not on screen before the page swaps. *Theater:* a pointer click on the theater button morphs
the player from its old box to the new one (`--spring`, 351 ms); `t` is instant. Measured (e2e, cached
page): click → next frame 16.1 ms with the morph vs 16.8 ms without; → frame painted 24.2 vs 29.5 ms.
*Gate:* keyboard and back/forward paints set `#ytc[data-still="1"]` until their first frame has rendered
(`bridge::frame`), which turns `@starting-style` entrances off, so a restored page (with its comments)
never fades in. *Comments:* appended pages and replies settle in (opacity + 8 px × `--mv`, 220 ms);
Top/Newest switch the persistent tabs at once and keep the old list dimmed until the new first page
replaces it (the section never collapses under the reader). *Up next:* the countdown panel enters
(`@starting-style`) and runs out as a linear progress line; the Autoplay switch is toggled in place (its
knob transitions). *Sidebar:* hover wash instant in / eased out (`::before` opacity) and each icon's own small
gesture on the spring (Home lifts, History turns back, Watch later turns forward, Playlists nudges, Liked tips),
press scale .97 with the icon squeezing. The active pill is one persistent element (`#ytc-sdi`, a strip as tall as
the six links clipped to the active row by `--st`/`--sb`, registered `@property` lengths written by
`App::side_move`): a pointer navigation moves both edges on the house spring, the leading edge on `--d-spring`
and the trailing one on `--d-spring-soft` (`data-dir`), so it stretches toward the new page and settles while the
label turns to the accent; g+letter, back/forward and restored pages move it at once (`data-still`); on a page without a
link it fades where it is. *Header:* YouTube's logo tile swells a touch and its play mark leans
forward on hover, the whole logo gives on press. **Zero-click Home** (2 Nov 2026: hovering the logo
activates it, no click needed; `app/logo.rs`): resting the pointer on the logo (tile or word) for 250 ms
(`logo::DWELL_MS`) opens Home; a soft accent pill behind the logo fills left to right over that time (a linear clip-path
wipe, like the player's Speed/Quality rows; reduced motion fades it). At the end of the dwell Home paints the
ready Home response (see "Home reload" under Speed); without one, Home's first page is requested the moment the
pointer enters the logo, so it has a 250 ms head start. Passing over
never navigates: leaving the logo cancels (the pill retracts), coming back starts over, and after a dwell or a
press the pointer must leave before another starts. Nothing starts while a popover or menu is open (account,
notifications, Save, Share, Filters, the topic editor, the search dropdown, a player menu, the shortcut sheet, the
import dialog) or a text field has focus. A click is instant as before, the keyboard is unchanged. **Home on
Home** (click, dwell or `g h`; like YouTube's logo on its own Home, 3 Nov 2026) jumps to the top at once and
shows a new Home from YouTube: a held **New videos** feed, else the ready Home response, else the request (the
page stays until YouTube answers); no extra history entry. With a Topic or YouTube chip showing it goes back to
the plain Home feed as before. The search field is one surface under the input and button (`.qf`: a border layer and a fill
layer, both clipped): pointer focus widens it 8 px each side (clip-path on the house spring, exact 21 px ends on
every frame), the button slides flush and turns to the accent and the magnifier glances once; the text box never moves and
the first keystroke paints on its keydown frame (e2e). `#ytc-search.now` (set on every key press, cleared on every
pointer press, i.e. before focus moves) makes keyboard focus and the blur after Enter instant. A clear control (×)
is there from the first character, empties the field without blurring it (`data-keep-focus`) and spins out. The
dropdown unfolds from the field (its surface grows from a 14 px strip, `--e-out` over `--d-panel`) and its rows
settle in 25 ms apart, clickable at once; typing, arrows, Escape and Enter change it with no motion. The bell tilts
on hover and rings once when the unread count rises; the badge is always in the DOM and grows in, pulses on a new
number (the number itself is final at once) and shrinks away when the list opens. The avatar leans in on hover and
an open account menu rings it in the accent. A keyboard focus ring settles in from 4 px out (`ytcfocus`).

**Player and watch-page motion** (`src/player/own.rs`, `src/app/motionglue.rs`, client.css "Icon morphs";
pointer only, every state final first; keyboard changes are instant: a key sets `.kf` on the player for 450 ms).
*Icons change form:* one persistent icon per control, its SVG `d` paths interpolating on `--spring` when Rust flips the
state class on the media event: play's triangle halves become the pause bars, the theater frame widens/narrows, the
fullscreen corners fold inward; the volume waves leave toward the speaker (outer one below 50 %) and the mute cross
turns in; CC wipes its filled artwork in (clip-path, `--spring-pop` scale). *Center pulse:* a click on the video leaves
the action's icon growing and fading (560 ms keyframes, `bridge::replay`). *Seek bar:* a press swells a red ripple
where it landed; the knob grows while held; the tooltip's surface morphs its width between chapter titles as you sweep
(`bridge::box_morph`), still centred 1:1 on the pointer; the control-bar chapter label's new title rises in. Volume
slider: accent fill (`--v`) and a thumb that grows on hover/press. *Menus:* switching Speed/Quality/CC (or the Audio tab,
or a codec) while the panel shows morphs the panel's surface (its `::before`, under a drop-shadow) and content box
with clip-paths from the old menu's height to the new one's on `--spring`, the new content sliding in from its
shortcut's side (160 ms); a sweep retargets from the on-screen size; content is final and clickable at once. A
choice's selected tint is one persistent mark per group (`bridge::mark_slide`) that springs to the new choice. Pointer
toasts pop in; LIVE breathes a ring at the edge. *Watch page:* a pointer open settles the title and channel row in
under the growing player, then the description's lines and the related cards as a short wave when `next` fills them;
description links draw their underline from the left (across wrapped lines); Block turns into "Blocked · Undo" growing
from its left edge. The chapter list's highlight and the playlist panel's current row are persistent marks that spring
to the new row as playback crosses a chapter or the playlist moves on (instant for keys, back/forward and first
placement); Top/Newest's accent pill travels over the tabs; a reopened reply thread unrolls from the top; Up next's Play
now fills with the accent as the countdown runs (its label inverts exactly at the fill edge).

**Feed and chip motion** (`src/app/fxglue.rs`, client.css cards/chips/filters; pointer only, every
write final first). *Cards:* hover eases the picture (and a playing preview) 4.5 % closer inside its fixed
rounded frame on `--spring` over `--d-spring-soft`; the frame, duration badge and progress bar stay crisp;
a depth gradient (`.th::after` opacity) rises; the watched bar thickens (scaleY); the title turns to the accent;
press gives the frame .985 and settles the picture back. The hover preview fades in once it lands
(`fade_imgs`). *Entrance wave* (`bridge::stagger`, no layout read, ≤ 15 cards, 24 ms apart, ≤ 7 steps,
320 ms `--e-out`): when a pointer-initiated load replaces the skeleton, the text rises 8 px into place where
the bars were and the thumbnails fade (their grey boxes stay); a pointer-picked YouTube chip or topic
brings its first cards in whole (12 px). Never on the first page load, keyboard, a submitted search,
back/forward (`mo.calm`), restores, snapshot repaints, SWR refreshes or appended chunks. *Chip row and
Filters segments:* a sliding selection pill (`bridge::pill_slide`, "duplicate, clip, slide": a copy of
the row in its selected look, `.fx-pg`, is clipped to the old chip's rounded box and the clip slides and
morphs to the new chip on `--spring`, so the text inverts exactly at the pill edge; the real chip is final
and clickable at once; a chip feed that answers mid-slide re-renders the row and `pill_carry` continues the
slide; keyboard selections are instant). Removing/moving/hiding a chip reflows the row
(`bridge::flip_mark`/`flip_play`: survivors slide from their old place, a copy of the removed chip shrinks
out in 160 ms). *Panels:* Filters and + Topic morph out of their button (clip-path from a button-sized
rounded box + a 43 px drop on `--spring`, opacity `--d-pop`; back into the button in 110 ms). Switch knobs
stretch while held; the length slider has an accent fill (`--v`) and a thumb that grows on hover/press; the
Filters summary's new text slides in (`bridge::nudge`). *Watch later:* the bookmark fills bottom-up
(clip-path on a filled copy, soft spring) and a pointer save pops once (`--spring-pop`, the one
underdamped spring, from `motion::pop()`); removing drains it faster. The note under the bar and active
search-filter chips enter with `@starting-style` (gated by `data-still`).

**Voice search** (2 Nov 2026; `src/features/voice.rs` decides, `src/app/voiceglue.rs` wires it,
`background.js` relays the socket). A mic sits in the search field before the search button. *Hover:* resting the
pointer on it 200 ms starts dictation (leaving earlier does nothing); the glyph turns to the accent over exactly that dwell,
then the 32 px circle stretches leftwards into an 80×36 listening pill on the house spring, which is also the new,
bigger hover target (the button's box is the pill; at rest its clip-path, which clips hit-testing too, leaves only
the circle; the glyph never moves under the pointer). Five bars show the live level: the bridge writes `--lv`
(0..1, RMS −60…−10 dBFS) on the button at most every 33 ms and each bar is a `scaleY` of it with its own weight and
response time. The field shows the live transcript (finals in the text colour, interim words muted; a long one keeps
its end in view). Moving the pointer out of the pill ends dictation and searches; an empty dictation searches nothing
and the field returns to its text. A pointer click on the mic starts at once (or, once words were heard, searches).
*Keyboard:* hold **⌥ Option (Alt)** alone (≥ 250 ms, so Alt+← and Option+letter never start it) to talk, release to
search; any other key or a click while held cancels; Enter/Space on the focused mic toggles. Keyboard dictation shows
the pill at once (no morph). Escape cancels any dictation (nothing searched); navigation and hiding the tab stop it.
No sound effects. *Recognition:* Deepgram streaming, `nova-3`, `language=multi` (Russian + English code-switching),
`smart_format`, `interim_results`, `endpointing=300`, raw `linear16` 16 kHz mono (no container: the lowest latency
path) in 32 ms chunks (AudioContext at 16 kHz + a ScriptProcessor whose output is silent). Ending: if the last result
already settled the utterance (`speech_final`) the search runs at once; otherwise the mic runs 60 ms more (the tail
still in the audio pipeline), then `Finalize` + `CloseStream` and the search runs on Deepgram's flushed final (≤
1.5 s cap: then what was heard). A settled phrase warms its results page while you are still hovering. Trailing
sentence punctuation from smart_format is dropped from the query. *Where the socket lives:* in the background worker
(`chrome.runtime` port `ytc-sock`; audio chunks base64 over the port). YouTube's document CSP has no `connect-src`
(checked 2 Nov 2026), so the page could open it too, but then the key would have to enter the youtube.com renderer;
in the worker it never does. The worker sends the stored key only to `api.deepgram.com` over wss/https. *Key:* the
panel (right-click / menu key on the mic, or shown by itself when no key is set; grows out of the mic) takes the key
in a password field; `bridge::secret_save` stores it in `chrome.storage.local` `ytcVoiceKeyV1` without handing it to
Wasm and empties the field. Each state is one line: no key, saved, removed, rejected (the worker checks
`/v1/projects` with the key when a handshake is refused: 401/403 → "Deepgram rejected this key"), unreachable,
microphone blocked / missing. The microphone permission is the browser's own prompt for youtube.com (asked once).
The debug overlay shows `voice · key set|no key · phase · dictations · last: end → search ms`.

**Keyboard first** (`?` shows the sheet): `g h` Home, `g s` Subscriptions, `g y` History, `g w` Watch
later, `g p` Playlists, `g l` Liked; arrow keys walk the cards on feed pages (↑/↓ by the rendered row),
Enter opens; on watch pages the player keys below apply.


The six desktop sidebar links share the logo’s 250 ms dwell (`app/logo.rs`): the row fills in the chosen
palette, the page prefetch starts on entry, and resting opens it once. Passing over, held travel, a press,
a key, a focused text field or an open menu cancels/blocks it. The pointer must leave before another
activation; moving between links starts a new dwell. Clicks and keyboard focus/activation stay immediate.
The TV rail keeps its remote interaction and does not get desktop hover activation.

Pages: Home (`FEwhat_to_watch`; signed out or history off → a search-first page), Subscriptions,
History, Search (YouTube's recommendation shelves inside the results are dropped, first page and
continuations: "People also watched", "For you", "People also search for", "Previously watched", "From
related searches", … by title in English, Russian and German, and any video shelf that links nowhere; real
result shelves, e.g. a channel's linked "Latest from …", and channel/playlist results stay; the debug overlay
counts them as `recommendation shelves`), Watch (`/watch?v=…&t=…`, our own player, see below, title/channel/views/date/description/related),
Channel (`/@handle`, `/channel/UC…`, `/c/…`, `/user/…`, with its tabs: see "Channel pages" below), Playlist (`/playlist?list=…`,
including `WL`). Feeds, search, channel tabs and related videos scroll infinitely. Shorts are hidden
unless the Filters **Shorts** switch is on (then Home, Search and related only; a channel's Shorts tab always shows its own); `/shorts/ID` opens as a normal watch page. The **description** is shown as the old extension showed it (
`src/description.rs`): one panel 12px below the channel/actions row, capped at 760px, fully open at natural
height, with views and date on top, then only the lines that carry a resource link (http(s)/mailto or a YouTube page,
redirects unwrapped to the real destination), each with its own text minus hashtags; a bare link takes the short line
above it as its label. Prose, hashtags and timestamp links are left out (chapters are listed separately).
Hovering a video link prefetches its watch data, so Watch opens with title, description and related
already loaded; hovering a channel, playlist or top-bar feed link prefetches that page too (kept 2 min,
memory only). Hovering a thumbnail for 0.5 s plays YouTube's own animated preview (`an_webp`, only when
the response lists one). The search box suggests as you type (YouTube's same-origin
`/complete/search`, shown as the box's native list; picking one searches). Back/forward restores pages
from memory instantly.

**Watch-page actions** (`src/actions/`): **Like / Dislike** (YouTube's own count label; click again
to remove), **Subscribe** beside the channel (unsubscribing needs a second click on "Unsubscribe?"),
**Share** (youtu.be link, optional "Start at" the current time, Copy), **Save** (a bookmark that is
filled in the accent and reads "Saved" while the video is in any of your playlists: signed in, the membership is
asked once, quietly, 1.2 s after the watch page opened (`get_add_to_playlist`, kept in memory, the same answer
the chooser shows, so opening it sends nothing more; a failed quiet check stays quiet); ticking/unticking in
the chooser fills/empties it at once (optimistic, back on a rollback); the fill sweeps up on the soft spring,
keyboard changes are instant. Hover opens the
chooser with the player menus' intent/grace timing via `MenuCtl`; click pins it, Enter/Space opens it
from the keyboard, ↑/↓ move, Escape closes; hover never selects; checkboxes add/remove through
`browse/edit_playlist`; "New private playlist" creates one with the video). All actions are
**optimistic**: the control changes at once, one request per target is in flight, and a failure rolls
back to YouTube's last confirmed state with the reason beside the row (and an incident in the debug
center). Signed out, the controls ask you to sign in and send nothing.
**Chapters** (player-bar chapters, else the chapters panel, else the description's timecodes by YouTube's
rules: a timestamp at the start of a line, after an optional bullet/dash/bracket/list number, then the
title, or the title then a timestamp ending the line; `m:ss`/`h:mm:ss`; the list starts at the first
0:00 and runs while the times ascend; at least 3 chapters, each ≥ 10 s, else none:
`actions::watch::desc_chapters`) sit under the description in one panel that rests **compact**: a single
72 px row showing the chapter playing now (its storyboard frame, title, start time, "Chapter 3 of 30", a chevron),
updated in place when playback crosses a boundary (its text slides in from the side the new chapter lies on). It
**unfolds on hover** into the whole list *over* the content below (the section only ever reserves the compact row,
so the comments never move on any frame): one persistent overlay whose border layer, fill and content are clipped
from the compact row to the full list (clip-path on the house spring after a 60 ms intent delay, so a pointer
passing through never flashes it; the rows settle 6 px down into place); moving the pointer out folds it back
faster (180 ms ease-out). Only real pointer movement opens it (`data-track`), never the page scrolling under a
still pointer. Keyboard focus moving into it opens it with no motion, Escape folds it at once onto its compact row
(focus stays there; Tab opens it again), a click on the compact row folds/pins it (touch), and the player's chapter
label pins it open and scrolls it into view (a press elsewhere folds it). A row click seeks, and the chapter playing
now is marked (`li.on`, toggled in place when a boundary is crossed; its highlight springs between rows).
**Frames are the video's real content**: the storyboard (`player/storyboard.rs`) from the WEB `player` response's
`storyboards.playerStoryboardSpecRenderer.spec` (`base|w#h#count#cols#rows#interval_ms#name#sigh|…`, level `i` =
`$L`, `$N` = the name with `$M` → sheet number, `&sigh=`; the last sheet is cropped to its frames) gives every
chapter the frame taken first inside it, as a CSS background into the sheet (percentages, so the cell fills its
16:9 box exactly; other frame shapes letterboxed). The level is the smallest whose frames are at least the box's
device pixels (96 px boxes: 160 px frames at 1×, 320 px at 2×). The frames are written once per list after the
video's first frame (the sheets never compete with the start); without a storyboard YouTube's chapter thumbnails
are used. Sheets are cached by the browser only. Live storyboards are not used. **Comments** load when their section nears the viewport (observed only once the main thread is
idle, so they never compete with the player start): Top/Newest, replies on demand (collapsible, "Show
more replies"), paging, a compact **End** marker only after the last top-level page (never during
loading, sort changes, errors or empty/disabled notices), pinned/creator/heart/verified marks, links and
timestamps that seek our player, "(edited)" suffix dropped. Both comment formats are parsed (entity
`commentViewModel` + `frameworkUpdates`, and the older `commentRenderer`). Back/forward keeps a video's
loaded comments (memory, last 4 videos). **Top bar** (signed in): a bell with the unread count taken
from the `topbar` that page responses already carry (no polling), the list from
`notification/get_notification_menu` (More pages; opening clears the badge; a click marks it read via
`notification/record_interactions` and opens it in the client), and an avatar menu (name/handle from
`account/account_menu` on first open; Google Account, Switch account, Settings, Sign out: account pages open
on YouTube's own site).
All these popovers (Save, Share, bell, account) stay in the DOM: opening and closing flip a class, so each
grows out of its button: the panel's surface (`::before`, reaching up to the trigger) starts clipped to the
trigger's own box and radius (`bridge::morph_vars`, read once before the content is written) and opens to the
12 px panel (`--e-out`, `--d-panel`), border and shadow follow, and the rows settle in 25 ms apart (`.in`);
closing folds it back into the button, faster. The panel itself is never clipped or moved, so every row is where
it lands and takes clicks on the frame it opens; keyboard opens/closes (Enter, Escape) have no motion. Rows are
inset pills (their wash never reaches the rounded corners); long lists scroll inside the panel. Save and "Start
at" checkboxes fill and draw their check mark in (stroke-dashoffset on a persistent path; keys toggle at once);
Copy morphs into "✓ Copied" (one button, its label rolls up and blurs away, the check draws, the fill settles to
the tinted done look) and back after 1.8 s. The action-row status message unfolds from the row's right corner and
folds back into it when it expires.

### Channel pages

(Owner, 8 Nov 2026: "why are there no videos, live streams, playlist in a channel tab"; `src/app/changlue.rs`, `parse::channel`.)

- **The tabs are YouTube's own**, from the channel response's `tabRenderer` list: title (in the page's language) and
  `browseEndpoint.params`, in its order. A channel without live streams has no Live tab. The client shows the ones it can
  render as cards, by their URL's last segment: `videos`, `shorts`, `streams` (Live), `shows`, `podcasts`, `courses`,
  `releases`, `playlists`. Out: Home (`featured`; Videos is what opens first), Posts, Store, Membership, Playables
  (`miniGameCardViewModel`) and Search; nothing that promotes paid membership. A tab YouTube lists whose answer holds
  nothing the client can show leaves the row for this visit and the first tab opens, instead of an empty page.
- **URLs**: `/@x`, `/@x/videos`, `/@x/streams`, `/@x/shorts`, `/@x/playlists`, … and the same under `/channel/UC…`, `/c/…`,
  `/user/…` open that tab directly, on reload and from a pasted link; another segment (`/featured`, `/posts`) opens
  Videos. A tab link pushes history; Back and Forward walk the tabs. A direct link asks `navigation/resolve_url` for the
  URL, which names the channel and the tab's `params` (live, for all four URL forms, 8 Nov 2026); a bare `/channel/UC…`
  needs no such step. Asked for a tab it does not have, YouTube answers with its Home (`featured` selected): the first tab
  the channel has opens, in the same history entry. With no renderable tab at all (a channel with only Home and Posts):
  the answer as it came, without a tab row.
- **Each tab keeps its list while the channel is open.** The tab change is a navigation (`App::navigate`: history,
  telemetry, what a page change tears down), but the header and tab row stay on the page and only the list under them
  changes. The list on screen is set aside whole (`bridge::park`: its nodes, so decoded pictures come back with them, and its
  `FeedState`, scroll position and stats; at most 12 lists, in memory). A tab seen before comes back from there in the
  press, with no request, no skeleton and no flash (`unpark`); it opens at its own scroll position. A video opened from a
  tab and Back keep them all; anywhere else (Home, Search, a playlist) lets them go, a TV has little memory to spare, and opening
  the channel anew starts over. Pressing the tab on screen goes back to its first cards. A tab that failed is not kept: it asks
  again when it comes back. A tab left writes no snapshot (walking one channel's tabs would push other pages' out of the six
  kept); the tab on screen is written with the page, as any feed.
- **A new tab** shows skeletons in its cards' shape and loads through the page prefetch (`load`): a hover or focus prefetch
  in flight or in the page cache is adopted (hovering a tab: 70 ms, as any link), the hedge applies, and its continuations
  follow the usual pipeline (`pump`, `CHUNK`, the preload rules). Because it goes out as a prefetch, an answer that lands after
  the user moved on goes to the page cache for coming back, and never touches the tab on screen. A tab that fails shows the
  reason in place of its list; the header and tabs stay, and going back to it asks again. The first tab is the channel response itself: no extra request.
- **Sort**: Latest / Popular / Oldest from the response's own chip bar. Two shapes, both live: three chips, each with its
  continuation token; or, on channels with members-only videos, one drop-down chip whose sheet holds the three (its
  `commandExecutorCommand`), beside "Members only" and "Public" filter chips, which are not sorts and stay out. Playlists' sort
  menu (`channelSubMenuRenderer`: "Date added (newest)", "Last video added") is the same row, with `browse` params. Nothing
  is hardcoded. A sort's answer is a reload (`reloadContinuationItemsCommand`: header slot and body slot); the old list is
  set aside under its label, so going back is instant, a late answer is ignored (`generation`), a failed one puts the old list
  and chip back. The URL does not change (YouTube's does not either). Tokens stay in memory and in this tab's snapshot.
- **Cards** follow `Surface::Other` as before, with two exceptions by tab. The Shorts tab shows its Shorts whatever the
  Shorts switch says (`changlue::admit_tab`: there they are ordinary cards, vertical, `sardefault` art, `Lay::Shorts`, seven to
  two per row), opening in the normal player. Live: YouTube's own badges, `LIVE` (`THUMBNAIL_OVERLAY_BADGE_STYLE_LIVE`) and
  `Upcoming` (a default-style badge, in the page's language) with "Scheduled for …"; nothing filters them here. Playlist
  cards keep YouTube's count badge, open `/playlist?list=…`, drop the byline (every card is this channel's; YouTube's first row
  is the word "Playlist" linked to the channel) and name their kind (Playlist, Podcast, Course, Release, Show). Shows are
  `gridShowRenderer`, Courses and Releases `playlistRenderer`, Podcasts square-framed `lockupViewModel`s (YouTube's art is
  mostly 240×240; the others are letterboxed, never cropped).
- **Look (unslop, on this page only)**: one left edge for the header, tab text, chips and grid; a 4 px spacing grid; three text
  roles (name, secondary line, tab label); no heading above the grid, since the tabs say it. The description line loses
  a channel's own list mark and doubled spaces (`parse::about_line`) and is at most two lines; a channel with no
  description has no line. The tab row sticks under the top bar while the list scrolls, so the next tab is a press away, and
  a new list opens with the row where it is. Its 2 px indicator is the selected tab's own until a pointer changes tabs, then
  one persistent mark (`bridge::mark_slide`, `.chk`) that springs to the new tab: state first, motion on top. Keys,
  back/forward and restored pages move it at once. Keyboard: Tab reaches the row, ←/→ walk it, ↓ goes to the chips and the
  cards, ↑ from the first row of cards comes back. Narrow windows: the row and chips scroll sideways, the page does not, and the row keeps its selected tab in view.
- **Measured** (e2e fixture, public build, 8 Nov 2026): press → a loaded tab final in the DOM 0.8–1.0 ms (p50 per run, n = 16,
  18–27 cards), no long animation frame in the one run that recorded them; a hover-prefetched tab (parsed and written in the
  press) 0.9–1.2 ms (n = 3, 10 cards). Live (signed out, real channels, home connection): a new tab 253–436 ms click → cards, a
  new sort 244–359 ms, a loaded tab or sort no request at all.

### Speed

- **Preloading:** after the first screen, feed continuations are fetched in idle time with
  `priority: 'low'` until **100 videos** are in memory on Home and Subscriptions (one page ahead on
  search, channel and playlist pages). Scrolling moves them into the page 24 cards per DOM write
  (`content-visibility:auto` cards), so reaching 100 cards needs no network wait. Past 100, Home and
  Subscriptions keep two chunks (48 cards) ahead. A watch page's related column fetches nothing ahead until the
  page is scrolled (most never are); from the first scroll it keeps one chunk ahead. Once a list is scrolled, a
  buffer below one chunk asks for the next page at once (low priority) instead of waiting for idle time.
  Measured (e2e fixtures, the end of the list in view while more loads; 7 Nov 2026): the related column at
  ~2,000 px/s with 900 ms continuations 3× / 572 ms → 0; Home at ~10,000 px/s with 400 ms continuations
  373 → 138 ms (at ~5,000 px/s both 0). Search is not changed: its pages come one after the other (each token is
  in the previous answer), so more ahead would not arrive sooner.
- **Home reload** (3 Nov 2026, owner: "the home reload should be the fastest thing out there, but it should
  actually reload whole videos with the same exact way the YouTube does"; `src/ready.rs`, `app/homeready.rs`).
  A reload of Home, opening it by URL, the logo (click or dwell), the sidebar and `g h` show **what YouTube
  answers to its own Home request at that moment, unmodified** (same videos, same order, its sticky picks; only
  your own filters apply as everywhere), and never this tab's old Home first. See "Home reload" below.
- **Instant reload / back (other feeds):** feed pages (Home, Subscriptions, History, search, channel, playlist;
  last 6) are kept as parsed data in **`sessionStorage`** (this tab only, gone when it closes; never
  `chrome.storage` or `localStorage`). A reload of a feed other than Home, or a return from another page (e.g.
  YouTube's native watch page), paints the snapshot in the first document write, then the fresh response
  replaces it in place; if you have already scrolled down, a **New videos** pill offers it instead of moving
  the page. Back/forward within 30 min restores the snapshot at its scroll position without a request (Home
  too: back/forward is not a reload). Snapshot data is validated again on read like a fresh response and
  rendered through the same escaping.
- **First request** goes out at boot with this tab's config mirror (`sessionStorage`) or the built-in
  fallback; `chrome.storage` refines later requests (a different `au` session index redoes the page). Home in a
  new tab asks at once: it is YouTube's own Home document, whose request carries the visitor in the browser's
  cookies (a signed-out `browse` without the visitor gets YouTube's empty "Try searching to get started" Home,
  measured live 3 Nov 2026; only the `browse` fallback still waits for the stored config, or `sw.js_data` when none
  was ever stored), and the document's `ytcfg` gives the tab its visitor and YouTube's current client version.
  The player still waits for `chrome.storage` (its preferences).
- **Thumbnails:** `srcset` width descriptors (`vi_webp/…/mqdefault.webp` 320w, YouTube's listed 360w and
  720w) with `sizes` per layout, so the browser takes the smallest sharp file; the feed grid's `sizes` is the card's
  real width per breakpoint (`render::GRID_SIZES`). Until 3 Nov 2026 it said 460 px above 950 px, so 1x screens
  fetched the 43 KB 720w file for a ~275 px card instead of the 11 KB webp (measured on i.ytimg.com); now thumbnails
  per channel boot are 1449 → 365 KB and the first row has loaded 121 → 103 ms after the cards' frame (p50, live
  bench). Retina keeps the 720w file. `fetchpriority=high` for the first row, `loading=lazy` below the first screen,
  `decoding=async`, `preconnect` to `i.ytimg.com` and `yt3.ggpht.com` in the shell.
- **Build:** `opt-level = "z"` (Wasm −23 %, no measurable speed change in the e2e timings).
- **Startup** (Chrome for Testing, fixtures, median of 30 loads, 1 Nov 2026): content script start → shell
  ≈ 1.4 ms: base64 decode 0.2, `WebAssembly.Module` 0.2 (V8 compiles lazily), instantiate 0.1, `boot()` →
  shell 0.8. Script start does not move with `content.js` size (a doubled payload started as fast), so size
  cuts do not buy startup; `wasm-opt` (binaryen 133: −8.6 % with `-Oz`) changed neither startup nor the Wasm
  CPU per flow beyond noise and is not used. Splitting rarely used parts would not help either.
  Live (signed out, youtube.com, 3 Nov 2026, `tools/bench`): a boot's `handler_ms` (navigation start → the
  app) is YouTube's own document request (`response_start` 130–260 ms p50) + ~20 ms until the browser runs
  the content script + ~9 ms of ours (decode + compile 2.3, app setup 2, shell 1–2, first request ~2); eager
  Wasm compilation (`--no-wasm-lazy-compilation`) changed neither startup nor the first watch page. So the
  owner's 210–440 ms `handler_ms` is mostly YouTube's document; every boot event now carries that breakdown
  (`boot`, docs/debug-center.md) to confirm it on real data. Re-measured 3 Nov 2026 (`tools/bench`, 16 boots per
  build): content script → Wasm instantiated 2 ms (p50), so a smaller Wasm (it is already `opt-level = "z"`, fat
  LTO, one codegen unit, `panic = "abort"`, stripped) or lazy-loaded modules cannot buy a measurable boot.
- **Hedged reads** (3 Nov 2026; `src/hedge.rs`): a `browse` / `next` / `search` / `resolve_url` request (and
  its continuation) that someone waits for and that has not answered by its deadline (≈ the session's p90 per
  endpoint, 0.7–2.5 s; 1.3 / 1.3 / 1.2 s for next / browse / search until 8 answers are in) goes out once more,
  identically; the first good answer wins, the other is aborted. Actions (like, subscribe, comment, save,
  playlists) never go out twice, background requests and Home's first page are not hedged. Measured with the
  live benchmark's injected slowness (`tools/bench`): see docs/debug-center.md "Hedged reads".
- **Lighter `next`** (3 Nov 2026; `api::NEXT_FIELDS`): every watch-page `next` (the page, hover and cards-in-view
  prefetches, the playlist and autoplay prefetches, their hedge copies) carries a `fields=` mask naming what the
  watch page reads: title, views, date, owner, subscribe and like state, description, comments token and count,
  chapters (panel, player bar and entity), the related column (lockups trimmed to id, type, image, title, avatar,
  metadata rows and tap command; other column renderers whole), playlist, autoplay, topbar, `responseContext`,
  `frameworkUpdates`. Dropped: the end screen, the structured-description panel, ticket/merchandise shelves, the
  lockups' menus and colour palettes, tracking. Measured live (24 videos, signed out): the decoded answer
  478 → 206 KB (43 %), gzip on the wire 62 → 28 KB, YouTube's time to first byte unchanged (774 vs 807 ms, noise);
  per page view (`tools/bench`, 8 interleaved runs per build): InnerTube on the wire for a channel boot 625 → 383 KB,
  its reload 583 → 375 KB, channel → watch → a related watch 1503 → 960 KB, a search session 1814 → 1284 KB (`next`
  alone −55 %); a speculative `next` decodes to 201 instead of 537 KB (p50) and answers in 661 vs 679 ms. `api::tests` applies the mask to the recorded fixtures the way InnerTube does
  and compares every parsed field; an ignored test (`YTC_NEXT_DIR=… cargo test -- --ignored next_mask_live`) does
  the same on live answers and against YouTube's own masked answers; the e2e stub serves masked answers. InnerTube
  answers a mask that names a field it does not know with **HTTP 400** for the whole request: then the same request
  goes out at once without the mask (same id; the 400 shows in the telemetry as a `req.next`) and no mask is used
  for the rest of the document. Continuations, search, browse and Home are not masked (Home must stay YouTube's
  exact answer; search: "Known gaps"). Redline for TV: `next` is the anonymous WEB request there too, so the same mask
  applies. YouTube's InnerTube answers only in gzip (with `br` or `zstd` alone it sends the body uncompressed), so
  the browser's default `Accept-Encoding` is already the best; HTTP/3 is advertised and reused.
- **Search ahead** (3 Nov 2026): a typing pause of **250 ms** (was 300) or resting **120 ms** on a suggestion chosen
  with the arrow keys fetches that query's results at low priority, one at a time (more typing or another row
  aborts the one in flight, unless a submitted search already waits for it), at most 10 a minute
  (`util::budget_take`), kept in memory only (the page cache; never `sessionStorage` or `chrome.storage`; the
  telemetry sees `req.search` `via: prefetch`, never the text). Enter then paints from memory or adopts the request
  in flight; a pointer resting on a suggestion row prefetches it like any link. Redline for TV types into the same field
  (the Android keyboard), so the same pause applies there. Measured live (8 runs per build): Enter after a 350 ms
  pause 645 → 615 ms p50 (the request is in flight both times and now starts 50 ms earlier); Enter at once waits for
  YouTube either way (its search answers in 0.7–0.9 s).
- **The first watch open of a tab** (3 Nov 2026; Chrome traces at 4× CPU throttling): its click task is ~90–140 ms vs
  ~50 ms for later opens. What is left: (1) the FLIP's destination read lays the new page out in the click (277 dirty
  objects, 30–45 ms vs 12 ms later: first-time text shaping, the `<video>` element's 119 KB UA stylesheet, cold
  layout); (2) ~300 Wasm functions run for the first time and V8 compiles each on its first call (lazy compilation):
  eager compilation took the click's script time from 41–63 ms to ~27 ms, but an extension cannot set V8 flags.
  Tried and dropped (no change in click → painted frame, `tools/bench --cpu=4`, 9 runs per build: first open p50
  197 vs 198 ms): reading the FLIP destination in the next animation frame (the layout only moves into the frame);
  running the watch page's pure markup builders once in idle time (compiles only ~25 of the ~300 functions; most are
  the player mount's); a pre-warmed hidden `<video>` (saves the 2 ms stylesheet parse only).
- **No layout reads per card.** The speculative prefetch of the cards in view (idle time after every paint and
  scroll chunk) measures each card's own box (`bridge::visible_hrefs(list, ".c", "a.t", n)`), never an element
  inside it: inside a `content-visibility:auto` card a measurement lays that card out on its own, one forced
  layout per card (125 layouts + 350 style recalcs, 40–130 ms blocking idle tasks at 4× CPU on a scrolled
  Home; 3 Nov 2026), which were most of Home's long frames after the first paint.
- **TV focus moves** (3 Nov 2026, measured on the emulator at 6× CPU: a D-pad press 144 → 53 ms to the painted
  focus, p90 493 → 74 ms): the remote's spatial navigation measures a card by the card's own box
  (`spatial_rects(…, ".c")`; an off-screen `content-visibility:auto` card needs no layout for that, the title link
  inside it did); a held arrow is not link intent, only where it rests on the key's release (before, every card it
  passed started a watch prefetch, `next` + VISIONOS, 20 a second); and the desktop's two `:has(:focus-visible)`
  keyboard rules apply only where there is a pointer (`@media (any-pointer:fine)`): every focus move flipped them
  false → true, a whole-grid restyle twice per press (~340 elements, ~70 ms at a TV's CPU); the same for the player's
  `:has(.pctl :focus) *` cursor rule (the whole player restyled on every overlay button move, ~200 → ~40 ms at 6×).
  The TV app's own document
  and the rest of its start path: tv/README.md "Speed".
- **Restored pages** (back to a feed) lay all cards out for the scroll jump (`.rs`, 1 s): cards are
  `content-visibility:auto` with a 240 px placeholder, so a freshly written page would otherwise land on other
  cards and then be moved by scroll anchoring.

**Home reload** (3 Nov 2026; `src/ready.rs` pure + unit-tested, `src/app/homeready.rs`). The rule: the first videos
a reload or a logo-Home shows are what YouTube answers to that request at that moment, unmodified; old videos are
never shown first and then flipped (the only exception is a YouTube that has not answered for 4 s, below).

- **YouTube's own Home** (3 Nov 2026, owner: "the thing that matters more is the quality of a refresh"; `src/homedoc.rs`).
  Every Home request (a reload, a new tab, the logo, the ready response, the hedge) is what a reload of YouTube's own
  page asks for: `GET https://www.youtube.com/` with the browser's cookies (an HTML `Accept`, no HTTP cache,
  redirects not followed). Its server renders the Home feed into the document (`var ytInitialData = {…};`, the
  `browse` response's shape) and names that answer in `ytcfg` (`EVENT_ID`); Rust cuts the data out of the bytes
  and parses it like a `browse` answer (nothing in the document runs). Its continuation is the usual `browse`
  continuation, as on YouTube's page. Same speed as `browse` (live, signed out: p50 1030 vs 1022 ms; 140 vs 61 KB
  on the wire). A document without Home data (a consent or sign-in page; a redirect answers 302) or a session
  index other than the first: `browse` (`api::home_body`, below) for the rest of that document. Why: YouTube's own
  reloads are its reference, and its page ties the "shown" signals to the document's answer id.
- **"Shown" signals** (owner decision, 3 Nov 2026; `src/impress.rs`, `app/impglue.rs`; CONTRIBUTING "Privacy"). YouTube's
  own Home page logs which cards it showed (`log_event`, captured live from its page: ~150 events per Home view), and
  YouTube rotates out what was shown and passed over. Redline's Home sends the same for its own screen: a
  `screenCreated` (a new 16-character screen nonce per Home answer, its page type 3854 and the document's
  `EVENT_ID`, also as `serializedClientEventId`), then per card that is at least half in view, once:
  `visualElementStateChanged` (the grid item's tracking id, its column and row) and `visualElementShown` (eventType
  1, the grid item's and the card's own tracking ids, its box in the viewport), and `scrollEvent` when a scroll comes
  to rest (where it ended, how far). Cards are watched by an IntersectionObserver (`bridge::shown_watch`: the box
  comes with the observer's entry; no layout read of ours, nothing in a hot path). A batch goes out 1 s after its
  first event (YouTube's own page: right after load, then every few seconds), gzip, at low priority, `keepalive`
  when the page is hidden or left; to `log_event?alt=json&prettyPrint=false`, the one `log_event` URL the request
  rules let through (rule 33; every other, e.g. on YouTube's own pages, stays blocked by rule 30). Signed out too
  (YouTube's page sends them signed out). Not sent: latency, heartbeats, experiments, ad state, hovers, offline
  state, anything on other pages; not on TV (YouTube's TV app has no such page to copy). The ready response waits for
  a screen's first batch (YouTube's next answer then knows those cards); a later batch with newly shown cards
  (scrolling) makes the waiting one stale (`ready::taint`, refilled lazily).
- **The ready Home response.** After every Home paint from YouTube, in idle time (≤ 2 s) and at low priority,
  the Home request is made once more and its answer, first page plus that page's first continuation, is kept
  for the next reload, logo-Home or **new tab**: in the desktop extension one per browser, in the extension's
  `chrome.storage.session` (`ytc:ready`, owner decision 3 Nov 2026: memory only, never on disk, cleared when the
  browser closes; the background worker opens it to content scripts) and in each tab's memory (kept current by the
  storage's change events); in the TV app in the page's `sessionStorage`. The snapshot record format plus the
  answer id and the cards' tracking ids, re-validated on read; never `chrome.storage.local` or `localStorage`. A
  document's first Home reads it once (`shared_take`: get and remove, a few ms after the shell), paints it (no
  request, no parse of 0.9 MB: the record is ~50 KB of parsed items) and so uses it up for every tab; the next one
  is fetched after that paint. A personalizing event in any tab removes it for all (`ytc:taint`, `{by, at}`: the
  other tabs drop what they fetched before it). So a reload or a new tab costs one Home request, like YouTube's own.
- **Only while it is what YouTube would answer now**: the same account (`ready::acct`: signed in or out, a hash
  of the session cookie, the session index, the signed-out visitor's id, the language); no personalizing event
  since it was fetched (`ready::taint`: opening a video, YouTube's own pages, a like/dislike, subscribe,
  playlist or Watch later change, YouTube's feedback/history endpoints; it is dropped from memory and the
  tab, and a refill in flight is ignored); younger than **10 min**. The age was chosen from YouTube's drift
  (`tools/bench/home-drift.cjs`, two signed-out profiles, 3 Nov 2026): YouTube's answer shares 46 / 48 / 39 /
  50 / 35 % of its top 24 with its answer from 1 / 3 / 5 / 10 / 20 min earlier, while two of its answers a
  second apart share 46–74 % and its own page's reloads 33–51 %; so up to 10 min a ready answer is as close to
  "now" as YouTube's own next reload, at 20 min it is not.
- **Refills** after a personalizing event or expiry are lazy: on a page that is not a watch page, in idle time,
  at most once a minute, only while the tab is in use (input in the last 5 min) and visible; when the tab is
  shown again, at once. (The old 2-min background refresh of Home is gone; Subscriptions keeps it.)
- **While a video plays** (3 Nov 2026: a new window opened while watching was the slow case): a watch page refills
  once its playback is healthy (`Own::healthy`: 8 s played since the first frame, not paused, stuck or failed, and
  20 s buffered ahead in both tracks or up to the end; checked with each 10-s position report), so the video never
  competes for bandwidth: one request (no continuation: the Home that paints it preloads that itself), low
  priority, once per watch page (again once it expired), at most once a minute. Opening the video already counted
  as a personalizing event, so the answer is what YouTube would show now; the next one (another video, a like, …)
  discards it as everywhere. Measured live (`home-newtab.cjs --modes=playing`, another tab plays a video 70 s, n = 7
  each, side by side): a new window → Home painted p50 1627 → 193 ms, p90 1835 → 241 ms (7 of 7 from the ready
  response).
- **The tab's own document** (3 Nov 2026; extension, Home with nothing ready). The browser already asks for exactly
  the Home document at navigation; instead of stopping it and asking again, `boot()` keeps it streaming
  (`bridge::doc_hold`): every node the HTML parser inserts at the top of the document is detached in the same task,
  before anything of it renders, and watched where it lies (a detached script never runs: the parser runs a script
  only while it is in the document, after a microtask checkpoint, where the observer has already detached it). A
  response header from the request rules (rules.json 20, Home's main frame only, outranked by the `yt-native=1`
  allow rule) adds `script-src 'wasm-unsafe-eval'; style-src 'unsafe-inline'; object-src 'none'`: YouTube's scripts
  and stylesheets are not even downloaded (also not by the preload scanner: live, 0 JS / 0 CSS requests), while the
  client's Wasm still compiles (the page CSP governs a content script's Wasm: `script-src 'none'` broke the boot) and
  its inline styles apply. When `ytInitialData`'s script is complete (the parser has inserted a node after it), its
  text and the `ytcfg.set` scripts before it go to Rust (`on_doc_script`) and Home paints from them like a fetched
  document: one Home request per load, as on YouTube's own page (before: the stopped one plus the fetch). A ready
  response (read a few ms after boot) stops the document first. A YouTube page without Home data (`ytcfg` but no
  `ytInitialData`): `browse` at once; any other page (the e2e stub): the document fetch. The hedge and the 4 s
  fallback count from the document's response start. Measured live, interleaved (n = 20 per path, hedge off):
  reload with nothing ready p50 1590 → 1372 ms (mean 1724 → 1432); a new tab with nothing ready p50 1567 → 1582 ms
  (mean 1554 → 1563, no difference; a first round of n = 16: p50 1677 → 1487). The data arrives with the document's
  last bytes (response end 1076 ms → data 1077 → painted 1097): YouTube's server time dominates either way.
- **Without a ready response** (after watching, expired, used by another tab): the skeleton in its final geometry
  and the request at once (the Home document needs no stored config). If the request
  has not answered by this tab's p90 Home time (`ready::hedge_ms`: the last 20 durations in `ytc:bt`, clamped
  to 0.7–2.5 s, 1.3 s before 5 samples: the owner's measured p90 of `req.browse`), the **identical request** goes
  out again; the first answer paints once, the other is aborted (at p90, about one Home request in ten is
  hedged). After **4 s** without an answer this tab's old Home shows dimmed with a quiet "Updating…" note
  (`#ytc-main[data-upd="1"]`) and is replaced in place when YouTube answers; a dropped connection shows it
  with "No connection" (`data-upd="2"`), probes `generate_204` every 3 s and asks again by itself (never
  YouTube's own page).
- **The `browse` request** (TV, a second session index, a document without Home data) is the one YouTube's web app
  sends for its Home (`api::home_body`): `browse FEwhat_to_watch`
  with the WEB context plus the window as YouTube describes it (screen/window size, pixel density, form factor,
  theme, application state, `mainAppWebInfo`) and the Home URL wherever it is sent from. Without those window
  fields YouTube's answers measurably differed (`tools/bench/home-parity.cjs`, signed out, n=12–16 per shape:
  top-24 overlap with answers to YouTube's own context 36–40 % vs 42–53 % among those; with them 41–49 %, and
  YouTube's sticky picks as often as in its own answers). Not sent (YouTube's own page state, no measured
  effect on the answer): `configInfo`, click tracking, ad signals, experiment ids, gzip request bodies.
- **Redline for TV** (the same code in the Android WebView): the ready response and the hedge go through `App::send`, so
  for a signed-in TV they are the account's TVHTML5 Home request with the access token (`tv::finish`); the account
  key is "signed in" (the token changes every hour; signing in or out clears the tab's session data and reloads).
  The WebView's `sessionStorage` lives as long as its session: it survives page reloads in the app (a client
  update switching in, a reload, the screen loading again for a telemetry change) and the app going to the background (the activity is
  never recreated: it handles its configuration changes, and Back on Home only moves the task back), but not a new
  app process. So the shell's memory would keep it no longer than the page's `sessionStorage` already does (checked
  3 Nov 2026); the ready response stays tab-scoped (the extension shares it between its tabs; the WebView has one page) and a cold app start would take the network path. On a
  cold app start the shell's kept copy stands in (owner decision, 3 Nov 2026; CONTRIBUTING "Privacy"): every record
  the page keeps also goes to the shell (`ready_put`), which seals it with the account's Android Keystore key and
  writes it to `files/ready.bin` for that account (`readystore.rs`; signing out or another account deletes it). At
  launch the shell unseals and deletes it in the background, keeping it in memory only if it is that account's and
  younger than 10 min; the held first page asks for it with the token (`ready_take`), and Home paints it on its
  first frame, then refills as usual. A used or tainted ready response is dropped at the shell too (`ready_drop`). That path's first step is the OAuth refresh behind `auth_token`
  (a new process holds no access token): the shell now starts it at launch (`shell::init`), in parallel with the
  WebView, the document and the client's Wasm, instead of when the page first asks. While a cold start holds the
  first page for the token, nothing goes out without it (no snapshot refresh, no redo for another session index).
- **Measured** (`tools/bench/home-reload.cjs`, signed-out seeded profile, real youtube.com, Playwright Chromium,
  reload → the first frame showing YouTube's answer): 0.9.0 p50 1337 / p90 1655 ms with the old Home painted
  first in 7 of 8 reloads; now p50 189–316 / p90 285–897 ms from the ready response (what remains is the
  browser's own document request before the content script runs), network path p50 ≈ 1.4 s; YouTube's own page
  p50 1064–1411 / p90 1429–1604 ms. The body arrives 10–107 ms (p50) after its first byte, so painting the
  first rows from a streamed body could gain little and is not done. A first Home request 10× / 100× slower
  (+7.2 s / +79 s, network path): with the hedge YouTube's answer paints at p50 2.5 s (the hedge won 13 of 13);
  without it at 8.5 s / ~80 s (the dimmed old Home from 4.3 s).
- **Measured, Home's quality** (3 Nov 2026, signed out, seeded profiles, n = 12 per mode; `home-reload.cjs --yt
  --parity`): with the Home document and the "shown" signals, Redline's reload answers share 28 % of their top 24 with
  YouTube's own page reloads (was 21 % with `browse`), as much as YouTube's own reloads share with each other (29–36 %);
  Redline's reload-to-reload overlap stays 31 % (YouTube's 29–36 %); vs YouTube's own page at the same moment in the
  same profile 38 % (was 35 %). The 48 % (`browse`) vs 33 % (page) seen the day before did not come back: that day
  YouTube's signed-out answers varied as much through `browse` (19 % among 8 answers) as on its page (17 %). The
  "shown" signals: in a loop of Home documents 6 s apart, the 12 cards shown and signalled came back in the next
  answer's top 24 20 % of the time, 27 % without signals (n = 12 each; `browse` with signals 14 %): the direction
  YouTube's rotation implies, small at this n; reload parity with the signals switched off was 40 % (no measurable
  difference at this n). Reload → painted from the ready response unchanged (p50 169 → 172 ms).
- **Measured, a new tab** (`tools/bench/home-newtab.cjs`, live, signed out, n = 10 per mode; Home painted from
  the tab's navigation start): another tab on Home (its ready response shared): p50 1071 → 194 ms, p90 1330 →
  379 ms (the ready response, used by the new tab; what remains is YouTube's document for the tab, ~150 ms, + 20 ms
  until the content script runs + 6 ms to the shell + the shared read). Nothing ready (another tab is on a video):
  the network path, now the Home document: p50 1281 → 1341 ms run side by side (video → Home 1081 → 1133 ms): YouTube
  renders its Home document ~100 ms slower than it answers `browse` (interleaved n = 20: p50 1129 vs 1024 ms; another
  hour +8 ms), the price of its own reload's answer (owner: quality over speed for Home). YouTube's own new tab: p50
  1433 ms. The rest of a new tab's cold path is small: document → content script 19–20 ms, script → shell 6 ms (the
  1.2 MB Wasm inside content.js included), no wait for the stored config any more.

**Player.** Watch pages play in our own `<video>` player (no YouTube player JavaScript, no iframe). The
streams come from the InnerTube `player` call with the `VISIONOS` client (direct googlevideo URLs, no
cipher, `n` or PO token, found by research on 1 Nov 2026), requested at navigation in parallel
with `next` and prefetched on hover. A second, normal `WEB` `player` call with your session supplies
captions and the watch-history URLs.

The VISIONOS call, including prefetches, recovery routes, hedges and URL refreshes, asks only for
`responseContext,playabilityStatus,streamingData,captions,videoDetails` (4 Nov 2026). These are whole
subtrees: every format, codec, HDR field, audio track, caption option and HLS field stays intact. The
WEB metadata/history call stays whole. A successful live pair retained all 32 format entries while
reducing the gzip response from 13,728 to 7,868 bytes (−43 %), decoded from 77,749 to 58,044 (−25 %);
the pair's 252/270 ms response times do not establish a latency gain. If YouTube rejects the mask
with HTTP 400, App and Own resend the retained body, headers and fetch options at once under the same
id, without the mask, and disable this mask for the document. That leaves `next`'s independent mask
enabled. The e2e stub applies the mask and checks exact anonymous, fresh-visitor and account resends,
prefetch adoption and subsequent requests; `api::tests::vision_mask_live` checks a successful local
capture without storing its signed URLs in the repository. Prefetch capability warming also reuses
the parsed response instead of parsing it a second time.

**Start path** (3 Nov 2026, measured live with `tools/bench/video.cjs`, numbers under "Speed" below):
- *Straight to the target rung* (≥ 1080p when the screen allows; 1080p is the floor): no low start rung.
  The old ≤ 360p start rung was replaced before its first frame showed anyway (live 2 Nov 2026: every
  start reported 360 → 1080 with HD at the first frame), so its 0.3–0.6 MB only took bandwidth from the
  1080p data.
- *Index + the start of segment 0 in one request*: the index range plus up to **128 KB** (video;
  audio 11 s of its bit rate, ≤ 128 KB). First segments are 33 KB … 4.4 MB at 1080p and their size is
  unknown until the index is parsed, so the guess is bounded; what of segment 0 came along is appended
  and only the rest is asked for. (512 KB cost ~150 ms on cold connections still in TCP slow start; a
  bit-rate-sized guess of up to 3 MB made the first frame wait for bytes past segment 0.)
- *Media is appended while it downloads* (3 Nov 2026, `player/frag.rs` pure, `own/stream.rs`; the
  bridge's generic `chunks` fetch option). Every googlevideo index and segment body is handed over piece
  by piece and each piece goes into its SourceBuffer at once: Chromium parses a partial `moof`+`mdat` (or
  WebM `Cluster`) sample by sample, so the first frame shows once the keyframe is in (measured: `loadeddata`
  after 12 KB of a 108 KB 1080p AV1 first segment). YouTube's segments are one fragment with **one keyframe**
  each (checked on AV1, H.264 and VP9 rungs), so appending whole boxes would gain nothing. The index request
  is parsed as soon as its index bytes are in; its first-segment bytes are appended while it still loads and
  the request for the rest of segment 0 goes out right then (in parallel, not after the index request
  ended). `frag::Asm` keeps each segment's bytes in order when several requests deliver them (the index
  request, the rest, a mirror twin, a retry from where a broken one stopped: only the missing bytes are
  asked for again); a new segment after a partly appended one first drops that part (`abort()`).
  Live (`video.cjs`, interleaved with 0.10.0, signed out, n = 30 each): cold watch URL first frame p50 711 →
  518 ms (p90 941 → 707), SPA click 385 → 291 ms (p90 558 → 358), hovered click unchanged (88 → 87 ms; its
  prefetch already holds segment 0); the picture moves as soon after the first frame as before (131 ms, rVFC);
  googlevideo 10× slower: cold p50 2268 → 1301 ms, no stall time in either. Quality switch (n = 9 per
  scenario): down 5.8 s → 72–138 ms, up 2.7–3.8 s → 89–105 ms; a seek after resting on the bar ~105 → ~30 ms.
- *decodingInfo before the first request*: answers are cached for the page (`CAPS`, memory) and asked
  as soon as a VISIONOS response is prefetched (`warm_caps`), and at boot for the usual YouTube formats
  (AV1/VP9/H.264 at 720p–2160p, 30/60 fps, SDR; `warm_caps_boot`, answered in a few ms, before any
  player response). A format without its own answer uses the boot answer for its codec, rung and frame
  rate (`caps_norm`) and its own question goes out anyway (a different answer still replaces the format
  before anything of it is appended); only a format with neither (HDR, odd rungs) waits for its answer, at
  most 150 ms (seen: 2–55 ms), so no request goes to a codec that is about to be ruled out.
- *No layout read in the click task*: the quality cap comes from the screen; the player box (which
  can only raise it) is read on the player's tick, at most once a second. Before, `limit()` read the
  box five times right after the watch page was written, each a forced layout (≈ 13 ms, 52 ms at 4×
  CPU throttling; found by the boot work on 3 Nov 2026).
- *Instant quality switch* (3 Nov 2026, `own/stream.rs`): a quality the user chooses (and any cut while
  paused) shows at once instead of at the next segment (3.2–3.9 s before, one keyframe per segment). The new
  quality's segment under the playhead is appended over the old one (Chromium keeps playing the old frames
  meanwhile); once the appended part covers the playhead (+ 0.12 s; `frag::mp4_covered`/`webm_covered`
  read the fragment's samples), the player seeks to where the playhead is and the decoder restarts from the
  new keyframe: the picture holds for 51–96 ms (1080p60 AV1, 1–3.5 s into a segment; e2e: 22 ms for two
  switches). When the next segment starts sooner than that seek could show (its keyframe in before the
  playhead gets there), it is used instead: no seek, no freeze. Audio is never touched. Auto's own
  switches keep the seamless cut at a segment boundary.
- *Seeks*: requests in flight (and their mirror twins) are dropped at once; the target segment streams in
  and the picture shows when its bytes up to the target are in. Resting the pointer on the seek bar for
  200 ms (or a TV ←/→ scrub target) fetches that spot's video and audio segments ahead (one spot at a time,
  dropped when the pointer leaves without a click, last two kept per track, never judged throttled); a seek
  there appends them at once or takes the request over while it loads. What is behind the playhead stays
  buffered (60 s), so back-seeks need nothing. Seek hover keeps one pending rest timer: moves update
  its target and deadline in Rust, and its wake waits any remaining time. Leaving cancels the intent;
  the existing wake can serve a new rest without a timer per pointer event.
- *Hover media prefetch* (`own/pre.rs`): once the pointer or focus has rested on a video link for
  150 ms and its VISIONOS response and decodingInfo are available, the index + up to 1 MB of segment 0 of exactly the formats the player will choose
  (same ladder, decodingInfo, size cap and ABR) are fetched; the click then appends at once (a body still
  loading becomes the player's own request). One video at a time, cancelled when the pointer leaves the
  link before it finished, kept in memory 5 min (last 3 videos). Live: ~1.2 MB per rested-on link.
  Focus counts like a resting pointer (the TV remote, keyboard focus): the bridge reports a link's
  `focusin` as intent in a microtask, since focus moved by Rust itself (`spatial_focus`) fires inside
  that call. The dwell starts at intent, overlapping the response and capability requests; an answer
  arriving after that rest starts media immediately. Capability answers retry a waiting rest, keeping
  the same format checks. A new link, leaving, or navigation ends the previous rest. In the controlled
  0.13.2 regression (300 ms player response), response → media fell from 155–158 ms to 3–4 ms.
- *Warm first frame* (3 Nov 2026, `own/warm.rs`; the owner: "ideally it should just be the start of the video
  already, without the preview"): once that prefetch's video body is in and the pointer or focus still rests on
  the link, a muted `<video>` outside the document with its own MediaSource decodes the first frame from those
  bytes (no request; only when the start point lies in what came along, `warm::frame_bytes`), the frame is drawn
  into a canvas at the video's resolution (≤ the screen's width in device pixels) and the decoder is released at
  once (it lives ~10–50 ms). The click puts the canvas over the player's `<video>` (`Own::warm_poster`), so the
  zoom grows the real, sharp first frame; when the player has presented its own first frame (the same picture,
  `requestVideoFrameCallback`) the canvas goes. One warm frame at a time; another video under the pointer or
  focus drops it, so does a minute without a click; at the mount anything else warm is dropped; on TV no warm
  decode runs next to a player.
  Without one (no rest, a resume point past the first segment, a cold page, keyboard without a rest) the box
  is the dark surface until the first frame. Many videos open on black (a fade-in: frame 0 is black, live
  3 Nov 2026 on Big Buck Bunny, nature reels, MKBHD, even Rick Astley's single first frame), so for them the
  warm frame is black too, which is what playback starts with. *Evaluated, not used:* adopting the warm
  `<video>` as the player: Chromium keeps an MSE `<video>` playing when it moves between parents in one task
  (detaching it for a task pauses it, the MediaSource stays open), but it needs a second player state machine
  running hidden and holds a (TV hardware) decoder for as long as the pointer rests, for the same picture.
  Measured 3 Nov 2026 (live, signed out, Chromium on macOS): the decode takes 8–15 ms (54 ms for the first, decoder
  start-up included), no long animation frame while it runs, a 1920×1080 canvas (~8 MB) for a 1080p start;
  first frame (`video.cjs --starts`, interleaved with 0.11.0, n = 20 each, p50 / p90 ms): hovered 55 / 59 →
  58 / 61, quick 142 / 230 → 169 / 246, SPA 312 / 382 → 264 / 375, cold 430 / 480 → 453 / 525 (cold runs none of
  this code: ±25 ms is the noise of the run).
- *HDR only on an HDR screen*: HDR formats count as not smooth unless `(dynamic-range: high)`
  matches, so an SDR screen gets the SDR format of each rung (decodingInfo is also asked with an HDR
  format's transfer function and BT.2020). Live 3 Nov 2026 on the Android TV emulator decodingInfo
  called 1440p60 HDR AV1 smooth even so, and no picture came (7.6 s, MediaSource rebuilt); with the
  rule the same video starts in 1.2–1.5 s on 1080p60 SDR.
- *TV app*: googlevideo goes through the shell (Kotlin `HttpURLConnection` in `Host.kt`, the CORS
  header of rule 50), so `<link rel=preconnect>` would warm only the WebView's unused connection: on TV
  the warm-up is a `GET <host>/generate_204` (what YouTube's player warms with) through the shell. The
  shell keeps up to 16 keep-alive connections per host (Android's default is 5) and gives a host 6 s to
  connect / 15 s per read (was 15 / 30 s): `shouldInterceptRequest` holds a WebView request thread until
  the headers come, even after the page gave the request up (a hedge won elsewhere). Ranges are URL
  parameters, bodies stream through, aborts close the stream once headers are in.

**Slow YouTube** (2 Nov 2026: "if requests to YouTube would slow down like 100 times, we should have a
workaround, but still connecting with YouTube"; `player/throttle.rs` decides, `own/hedge.rs` acts; nothing
here leaves the player):
- *Player request hedge*: no VISIONOS answer after **400 ms** → the same request again; the first usable
  answer plays, the other is aborted (an error from one waits for the other). 400 ms ≈ 2 × the owner's
  p90 (205 ms) and above p99 (348 ms) of 158 answers: it fires on ~1 % of starts and turns a hung request
  into ~0.5 s instead of the 8 s timeout. Racing another client is not done: no other client gives direct
  URLs that play past ~60 s without a PO token (see "Bot check recovery").
- *googlevideo requests stream* (the bridge's generic `progress` fetch option: headers, then bytes so far
  every 100 ms), so a slow host shows early. A request is *throttled* when no headers came after **700 ms**
  before the first frame (the slowest cold connection seen: 639 ms) or 1.5 s while playing with < 12 s
  buffered, or when after ≥ 0.5 s of body it runs **6× slower than the measured bandwidth**, or (before
  the first frame / with < 12 s buffered) at **under half the media's bit rate**, with > 0.8 s left.
- *Recovery, in the owner's order*: (1) the same range on the next mirror the URL lists (`mn`/`fvip`,
  `fallback_count`, as YouTube's player does); both run, the first full answer wins, and later requests
  go to the winner's host; (2) when the mirror twin is slow too (or there is no mirror): fresh URLs (a new
  player response); (3) another client: none qualifies (above); (4) only with a measured rate: the highest
  rung that rate sustains (70 %), held 15 s (then 30, 60, 120 s if it comes back), after which Auto goes
  back to ≥ 1080p by itself. A host that sends nothing at all is left to the stuck schedule ("Stalls are
  not failures"). A cached player response whose URLs hang still gets fresh URLs at 0.8 s instead of a
  mirror (those URLs are the usual culprit).

Controls (24 px outline icons): play/pause, mute + volume, time, then the shortcuts **CC** (thin outline
when off, filled cut-out letters when on: the old extension's artwork), **Speed** (shows the rate as text:
`1×`, `1.25×`), **Quality** (shows the rung playing now in a small frame: `360`, `1440`, `4K`), theater,
fullscreen. The text buttons have a fixed width, so a label change never moves the toolbar. **No loading
indicator** (1 Nov 2026) **and no poster** (3 Nov 2026): the box shows the warm first frame (below) or is the
dark player surface at the final geometry until the first frame; the card's `mqdefault` thumbnail (320 px, a
different picture from the video's start) is no longer scaled up to the player. The three shortcuts **open on hover** (16 ms intent; leaving closes
after 400 ms, or 180 ms once the pointer was in the menu; travel with a button held is not intent and a
held slider is never dismissed). In CC hover never selects anything; **Speed and Quality choose with zero clicks**
(since 2 Nov 2026, see below). Clicking **CC** toggles captions immediately; its outlined/filled icon
and `aria-pressed` follow the remembered state. Hover opens its language/audio options without changing
captions. Enter/Space toggle too; ↑/↓ on the focused CC button opens the options with immediate focus,
and Escape returns to CC. Speed/Quality clicks pin their menus (a second click, Esc
or a click on the video closes it); after a mouse choice it closes when the pointer leaves. Enter/Space on
Speed/Quality opens the menu with focus on the current choice, arrows move, Esc returns to the shortcut.
**Motion** (motion law; all class toggles on the persistent `#ytc-player`, transform/opacity only): the
menu is one persistent panel: the first open fades + scales from `.97` at its shortcut (170 ms ease-out,
clickable on its first frame), moving between shortcuts only swaps the content (no re-animation), it
closes in 110 ms; choices toggle classes in place. The
control bar fades out on idle or when the pointer leaves the player while playing and comes back on the
same frame; a stall over 0.6 s brings it back softly and the empty track breathes softly (no spinner).
The seek track grows with `scaleY` (instant in, eased out), the knob rides on `translateX`; while playing
the fill glides at real speed (Rust aims `--p` one 250 ms tick ahead), seeks/drags/pauses land 1:1.
**Chapters on the player** (like YouTube): the seek bar splits into one segment per chapter with
2 px gaps (placed once per video on whole pixels; every fill is the same `--p`/`--b`/`--h` mapped into
its segment, so per tick nothing more is written); hovering shows a tooltip above the bar (the storyboard frame
at the pointer's time, 160×90, written only when the frame changes; then chapter title over the time; the time
alone without chapters, no frame without a storyboard) centered on the pointer and clamped inside the bar, a
lighter hover fill, and the segment under the pointer grows taller (`.on`, instant). The pointer is
reported by the bridge's generic `data-track` (rect read once on enter). The current chapter sits beside
the time (`0:41 / 12:03 • Title ›`, text written only on a boundary); clicking it opens the chapter list
and scrolls it into view on the current chapter (YouTube opens its chapters panel; in fullscreen it
leaves fullscreen first). Ctrl/⌘+← / → jump to the previous (or the current chapter's start when > 3 s
into it) / next chapter, with the title as the toast; without chapters these keys stay the browser's.
Buttons scale on press; toasts appear on the key's frame and fade out; a new caption cue fades in
quickly (lines that stay never move). Keyboard shortcuts add no motion.

- **Speed**: a vertical list centred above its shortcut, **1×** at the bottom (next to the shortcut), then
  1.25, 1.5, 1.75, 2, **2.5×** at the top, under a header − exact rate + (0.05 steps, 0.25–2.5×; slower
  rates live there). `<`/`>` step 0.25 and stop at 2.5×. The shortcut always shows the rate.
- **Quality**: a vertical list centred above its shortcut, **Auto** at the bottom, then the rungs worst →
  best going up (Premium/enhanced at its numeric tier); a dot marks the rung playing now. A compact codec
  row (Any / VP9 / AV1 / H.264, only the supported ones) on top when there is a choice.
- **Zero clicks** (2 Nov 2026: fewer clicks; the wheel or a resting pointer chooses without clicking;
  this overrides the hover-never-selects rule for these two menus).
  *Wheel:* over the Speed or Quality shortcut (open or not) or anywhere in its menu, the wheel chooses and
  never scrolls the page (`bridge::wheel_take`, preventDefault on `[data-wheel]` only): up = the next value
  up the list (faster / better), down = down; over the Speed readout it steps 0.05. Mouse-wheel clicks
  are one step each; trackpad pixels accumulate (`ui::Wheel`: first step after 3 px, the second after 50,
  each later one 40 px dearer, a pause of 120 ms starts a new gesture), so a swipe moves a few rows and a
  flick with momentum a handful, never the whole list. Speed applies per step; Quality moves the
  selection mark and the toast every step but switches the stream once, 200 ms after the wheel rests (the
  rows passed are never fetched). At either end the list (or the closed shortcut's label) nudges 4 px
  once per gesture and the toast repeats the value. A short player scrolls the list itself, following the
  selection. *Dwell:* resting the pointer on a row for 220 ms (`ui::DWELL_MS`) chooses it; the row fills
  left to right over that time (a linear clip-path wipe; reduced motion fades it), moving to another row
  restarts it, and rows passed over on the way (the bottom ones, entering from the toolbar) never select.
  A row that came under a still pointer by itself (the list scrolled after a wheel step, the panel opened
  from the keyboard) starts no dwell for 250 ms. A click still chooses at once; the keyboard is unchanged.
- **CC**: caption languages, with on/off handled by the desktop CC shortcut. An **Audio** tab
  (switches on hover) appears only when the video has several audio tracks; choosing one cuts the new
  language in about 1 s later through the normal quality-switch path. The TV remote keeps its existing
  menu opener and full-width Captions On/Off row, since it has no hover.
- **SponsorBlock** (2 Nov 2026; `player/sponsor.rs` pure, `player/own/sb.rs` glue): sponsor
  segments from SponsorBlock's public database are skipped in our own player (the SponsorBlock extension cannot act
  on it). *Lookup:* k-anonymous: `GET https://sponsor.ajay.app/api/skipSegments/<first 4 hex of SHA-256(video id)>`
  with every category (`sponsor`, `selfpromo`, `interaction`, `intro`, `outro`, `preview`, `music_offtopic`,
  `filler`, `poi_highlight`) and action types `skip`/`poi`; the answer lists every video in that bucket (~150) and
  ours is picked locally. SHA-256 is a small pure-Rust implementation (NIST vectors in the tests). Sent with the
  player's own requests at start, after the two `player` calls, at low priority; nothing waits for it (first frame,
  seeking and chapters are independent); no answer in 6 s, an error or garbage → nothing happens. *Skipping:*
  checked on the player tick, on `timeupdate` and by one timer aimed at the exact start when the next skip is less
  than a tick away. Playing into a chosen segment (from 0.1 s before its start) seeks to its end, carried through
  chosen segments that touch or overlap it; a toast slides in from the left: **Skipped sponsor · Undo** (5 s; Undo
  seeks back to the start and that span no longer auto-skips in this video). A seek by the user *into* a segment
  only shows it (until playback leaves it); the start position (resume, `t=`) is not such a seek. Defaults like
  SponsorBlock's: sponsor, self-promotion and interaction reminders skip; intro, outro, preview, non-music and
  filler only show. *Seek bar:* every range is drawn in its category's color inside the chapter segments (above the
  fills; they fade in when the data arrives), the highlight as a pink tick, and hovering a range names it in the
  tooltip; a highlight further ahead is offered once as **Highlight at m:ss · Jump**. *Controls:* the browser’s
  Redline extension popup owns the **SponsorSkip** switch, eight category choices and the active video’s counts.
  **Undo last skip** appears there while the video’s transient Undo notice is available. Settings no longer
  occupy a video-control button. Off aborts any in-flight lookup and hides ranges/notices; a new video makes no
  lookup at all. Re-enabling looks up the current video. Remembered in `ytcConfigV1` `p`: `sbo:false` only when
  off, `sb` (category keys) only when not the default. The popup reads the latest config before each serialized
  write, patches only edited fields/category bits, and preserves other preferences. Optimistic changes roll
  back with a visible error if storage rejects the write. Existing client tabs adopt the
  SponsorSkip settings in place. Status/Undo use
  same-extension messages to the active tab; one local poll per
  second while the popup is visible, no external request.
- **Caption language**: the source language is YouTube's speech-recognition (ASR) track's
  language. Russian or English source → captions in that language; any other source → an offered English
  track, else YouTube's English translation of a source-language track (`&tlang=en`, listed as
  "English (translated)"); human tracks before ASR. No ASR evidence (or conflicting) → YouTube's default
  track (`defaultCaptionTrackIndex`), else the old rule. A language picked in the CC menu is kept for that
  video for this tab session (memory only, 100 videos); other videos keep the rule. On/Off stays remembered.
- **Captions**: cyan Arial with a dark outline, no background, 75 % of the base size. Drag the text to
  move it; a slim vertical range at its right edge (shown on hover/focus) scales it 0.5–2×; the **mouse
  wheel** over the caption (or the range) resizes it too, 5 % a notch, up = larger (trackpad deltas add up
  to a notch; the page does not scroll; Ctrl/⌘+wheel stays zoom), and ↑/↓ (←/→) step it 5 % while the range
  is focused (the old extension's behaviour). Both are one object in storage (`p.cs = {v:2, x, y, s}`:
  upper-left corner as player fractions, scale 50–200 %), written only after a deliberate drag/resize
  (batched). Default position: near the upper-left.

Keys (YouTube's): space/`k`, `j`/`l` (±10 s), ←/→ (±5 s), ↑/↓ (volume ±5 %), `f`, `m`, `c`, `t`,
`<`/`>` (speed); `0`–`9` are **blocked** (2 Nov 2026, as in the old extension: taken on watch pages
and do nothing, no jump to 0–90 %, not in the `?` sheet); Ctrl/⌘+←/→ (previous/next chapter; the bridge reports
Ctrl/⌘ keys outside text fields as `Mod+<key>`, unhandled ones stay the browser's). Captions on/off (default **on**), volume/mute, quality,
codec, theater and caption placement/size are remembered (`p` in `ytcConfigV1`; writes are batched
300 ms so slider gestures write once).

**Live streams** play in our player too (`player/live.rs`). VISIONOS returns an HLS master playlist whose
variants are MPEG-TS video with separate packed-ADTS audio, and `adaptiveFormats` without byte ranges;
those adaptive URLs serve the same segment numbers as self-initializing fragmented MP4 with `&sq=N`
(checked 1 Nov 2026 on two public streams; audio and video share one timeline). So the timeline comes
from HLS (master → the variant nearest the playing rung → media playlist: sequence numbers, durations,
sliding window, `#EXT-X-ENDLIST`; refreshed every target duration, half of it when nothing changed;
~14 KB gzip for a one-hour window) and the segments are fetched as fMP4 and appended to the same MSE
pipeline as VOD (`ftyp`+`moov` when the rung changes, then `moof`+`mdat`; `emsg` dropped). No TS→fMP4
remux is needed. It starts two target durations behind the newest segment (10 s on 5 s segments: about
16 s behind the broadcast, measured from YouTube's `Ingestion-Walltime-Us`), follows the edge (jumps back
when > target + 15 s behind after a stall or hidden tab), and the seek bar/keys/`j`/`←` seek within the
playlist window (DVR); the time shows `−m:ss` behind live and **LIVE** (red dot at the edge) jumps back.
ABR, the size cap and remembered quality apply as for VOD. A stream whose playlist ends plays to its last
segment and ends normally. Upcoming streams and premieres (`LIVE_STREAM_OFFLINE`/`isUpcoming`) show the
failure panel with YouTube's own words ("Premieres in 2 hours") without an incident; a live stream without
fMP4 formats or playlists shows the panel with the reason as an incident. Live chat and live captions stay on YouTube's page (not built).

**Next video** (for the playlist / Up next logic): when playback ends the player emits `Out::Ended`
(the app marks `ytcEnded` and calls `App::on_ended`, an empty hook to fill); `Own::hint_next(id)` makes
the app prefetch that video's `next` and VISIONOS responses once, so starting it needs only googlevideo.
Reusing the `<video>`/MediaSource was measured and not built: a new element + MediaSource + two
SourceBuffers is ready in 0.2–0.5 ms (4 ms only the first time in a tab).

**Failures stay in the player; the client never leaves for YouTube's page by itself** (2 Nov 2026: an
automatic switch to YouTube's page after a few seconds hid problems; always staying in the client's own player
means every failure can be seen and inspected where it happens; `src/player/fail.rs`). If our player cannot play a video (VISIONOS not OK: age-restricted,
members-only, private, unplayable, the bot check on every route; upcoming streams/premieres; no compatible
format; googlevideo 403 again after a `player` refresh; index/parse errors after their retries; an
MSE/media error (only after three automatic recoveries within two minutes, at any time); a stuck episode that never recovered), it releases the media and, for a permanent reason or after the
automatic Try agains below, the app shows a **panel
in the player box** over the dimmed poster (a compact card centred in the box; it settles in: `@starting-style`, no spinner): a one-line
message, the technical reason in smaller, selectable monospace text, followed by what ran (` · 3 automatic Try agains · 0.14.3`: the client's version
and the Try agains made without being asked; the public build has no telemetry, so the next screenshot settles which code ran and what it tried), and two buttons. A verdict about the video itself reads differently (below: *Verdicts*). **Try again**
re-mounts the player for the same video and time with a fresh VISIONOS call (never the cached or a
prefetched response; it also lifts the bot-check block, so every route is tried again). **Open on
YouTube** opens YouTube's own watch page at that time (`/watch?v=ID&t=Ns&yt-native=1`); it is the only
way a watch page goes native. Enter/Space on a focused button work (native button activation). The URL
and the page stay; the reason is kept in `chrome.storage.local` (`ytcPlayerLogV1`, last 10) and shown in
the debug overlay (`player failed …`). Real failures are `player-failed` incidents with the full player
report (was `player-fallback`); expected ones (upcoming, age-restricted, members-only, own player off)
show the panel without an incident. YouTube's embed is no option: on a youtube.com page it shows "Error
code: 152 - 4". Messages (`fail::message`, unit-tested):

| Reason | Panel message |
|---|---|
| upcoming / premiere | YouTube's words, e.g. "Premieres in 2 hours" (else "This live stream hasn't started yet") |
| `VISIONOS LOGIN_REQUIRED` bot check | "YouTube asks to sign in to confirm you're not a bot" |
| `VISIONOS LOGIN_REQUIRED` age | "Age-restricted — sign-in needed" (other sign-in reasons: "Sign-in needed to watch this video") |
| a `VISIONOS` reason mentioning members | "Members-only video" |
| `VISIONOS ERROR` | YouTube's words (e.g. "This video has been removed by the uploader"), else "Video unavailable" |
| `VISIONOS UNPLAYABLE` | "YouTube won't play this video here" |
| `VISIONOS CONTENT_CHECK_REQUIRED` | "YouTube asks to confirm before showing this video" |
| VISIONOS HTTP error / no answer in 8 s | "YouTube's player service didn't answer" |
| no compatible formats | "No format this browser can play" |
| media error, appendBuffer, SourceBuffer, changeType | "The browser couldn't play this video" |
| live playlist / segment errors | "This live stream isn't loading" |
| own player turned off (`op: false`) | "The client's own player is turned off" |
| anything else (googlevideo, index, stuck to the end) | "This video isn't loading" |

*Verdicts* (8 Nov 2026, owner on a members-only video: "the window should be better"; `fail::offers_retry`, `fail::line`, `fail::verdict_buttons`).
What Try again can't change has no such button and no technical line: the headline above, one plain line (YouTube's sentence without our
`VISIONOS <STATUS>:` prefix; nothing when the headline already said it), the buttons, and a very quiet version under them. **Next video**
(`upnext::next_pressed`: the next item or YouTube's target, else the first related card, never a locked one; whatever Autoplay says, the press asked for it) comes first and takes the
focus, unless the viewer is typing somewhere; where YouTube's page knows more than we do (sign-in, age, a content check, no format here, the own player off) **Open on YouTube** does. `next` often
answers after the player: the panel then gets the button in place (`App::panel_sync`; its card doesn't enter again). The raw reason stays in the debug overlay, the
reports and incidents, and the panel's `data-r`. Try again stays where it can help: everything that retries, the bot check (it asks every route again), no connection, any reason where YouTube says "went wrong"
or "try again", a stream or premiere that may start any moment. Only hours, days or a date away has none (`far_off`): the viewer opens it again when it's time.
Members-only (YouTube's own sentences only: `fail::members_only`, never a name that says it, "content from Members Only"): "This video is for this channel's members." (owner: "I'd rather not get the wantiness to buy anything": no tier, no channel name, no
invitation); the one sentence added after it says when the video goes public. An invitation ("Join", "Become a member", "Upgrade") is dropped from any verdict's line. The line stops at three lines.

*The automatic Try again* (8 Nov 2026, owner: "why not make it so that it refreshes by itself"; nobody presses Try again for
what Try again would fix; `fail::permanent`, `fail::Auto`, `App::after_player`'s `Out::Failed`, the one choke point of every
`Own::fail`). A reason that isn't permanent shows no panel: the app does what Try again does (a new player, VISIONOS asked
afresh, though the bot-check block stays: only a pressed Try again lifts it) by itself, **after 1 s, then 4 s, then 10 s; at most 3 within a sliding 5 min and 9 while the video stays open** (a new
navigation starts over), at the same position and play/pause state, audio track and speed, in the same viewing session (the same `cpn`, no second
`playback` ping; the failed player's last watchtime ping waits, with its position: it goes as usual at the next player's first frame, or as `final` when the panel shows, the
viewer leaves or presses a button; a player without a first frame reports nothing of its own). The player service's refusals are not hammered (`fail::retries`): `VISIONOS player HTTP 429` gets one retry
a minute later, 403 and 400 one after 4 s, none if others went out in the window; `VISIONOS ERROR` during a refresh of a video that has played gets one (the
new player's answer at its start is the verdict). Media errors (live and post-live ones too, which have
no recovery in place), stalls ("no first frame", "stalled for"), index and googlevideo failures and "VISIONOS player" without an
answer all go this way. Only then the panel, its technical line naming them. *Permanent* (the panel at once): upcoming
streams/premieres, members-only, sign-in and the bot check (every route was tried), YouTube's own `VISIONOS <STATUS>` answers
(`ERROR`, `UNPLAYABLE`, `LOGIN_REQUIRED`, `CONTENT_CHECK_REQUIRED`, …), no compatible format, own player turned off. During a wait
the picture stays (the failed player's frame as a held canvas, which the next player takes over until its first frame) and, from
the 4 s wait on, the calm "Still loading — retrying…" status; its buttons work, and pause/play (the button, Space, k) sets how the next player
starts. Leaving the video, or opening another, ends the wait
(a generation on `Timer::AutoRetry`). A dropped connection (a reason with `HTTP 0` or "no answer") keeps its own flow: the wait
ends unspent at the probe's verdict, "No connection" shows, and it continues by itself when the connection is back; a retry goes
out only after the probe answered. (No probe for other reasons: its own request may be blocked, and a verdict from that would
cost the retries; a retry that meets a dropped connection fails with a network reason, so at most one is spent.) Each failure is
kept in `ytcPlayerLogV1` (debug overlay) and is a `player-retry` incident in development builds; `player-failed` is the last one.
*Back at the panel* (`Auto::may_return`, `App::panel_return`): the tab shown again, or the window focused after at least 60 s
without it (300 ms later, and not if that press was on the panel), retries once more when the reason isn't permanent, keeping position, play/pause, audio track
and speed, and the panel is gone if it works.
One retry per return (never two within 5 s, a return fires both events), at most 3 while the video stays open, none within 60 s of a 429, and a return's
retry that fails before a picture goes back to the panel without a chain of its own.
*A stale tab* (`bridge::ext_gone`, `App::stale_reload`). Reloading or updating the extension leaves open tabs running the old
content script, for as long as they navigate inside the page: its `chrome.runtime.id` is gone and `chrome.*` calls throw (cheapest
reliable signal; TV and other hosts never report it). Nothing of the player goes through the extension (requests are the page's,
the Wasm is in the script, the network rules stay while the extension is loaded; measured with a real `chrome.runtime.reload()`:
playback goes on, in-page navigation and new videos play; e2e "a tab orphaned for real …"), only `chrome.storage` writes are lost. So
a stale tab gets the ordinary automatic retries first, and only when those are used up, for a reason that isn't permanent or the connection's (a reload offline
would land on the browser's error page), the page loads again at the video and position (`location.replace`; the current code then runs) instead of the panel;
once (`fail::STALE_KEY`, `sessionStorage`: at most one such reload in 10 min), and a paused video stays paused. Never in fullscreen or while text is being typed
(a focused field, a text area): the panel instead.
*The dubbed-audio bug behind an owner report* (8 Nov 2026, Brave: "The browser couldn't play this video / SourceBuffer error
(audio/webm; codecs="opus")"). On a video with several audio tracks every track has the same itag. Fresh URLs (near the expiry, a
403, a throttled or stuck request) gave the audio representation the URL of the first track listed while it kept its own index, so
the next audio range cut another file in the middle of a cluster, and the append failed; a recovery with the same URL failed
the same way, and only a new index (fresh URLs in `media_error`, or Try again) mended it. Now only the same track and file (`audio_id`, `contentLength`) gets
the new URL; a track in use whose file changed keeps its old URL, the URLs' deadline does not move, and no more proactive refreshes go out (when it runs out,
the 403 reaches the automatic Try again and a new index). e2e "fresh URLs on a dubbed video …", "a track whose file changed …". (That report's exact text, with no " (after N automatic retries)", cannot come
from 0.13.8 or later for a WebM track: the only bare path is a live stream's, and live formats exclude WebM; so that tab ran older code.)

**Stalls are not failures** (`fail::START`, `fail::PLAY`, `own.rs` `stuck_tick`). No first frame
**4 s** after the streams were chosen, or buffering **15 s** mid-playback (after the 3 s rung step-down),
begins a *stuck episode*: a small non-blocking status at the player's top right, "Still loading —
retrying…" with smaller Try again / Open on YouTube buttons (fades and settles in, fades out faster; only
its buttons take the pointer), and one `player-stuck` incident with the full report. The player keeps
retrying in place with **fresh player URLs** (a new VISIONOS response; requests in flight are dropped and
asked again with the new URLs; a non-OK answer to such a retry is not a verdict). Start: 0.7.6's refresh
at 2.5 s when googlevideo never answered, then retries at **6 s** and **15 s** (if googlevideo answered but
no picture came, the 6 s retry also rebuilds the MediaSource and SourceBuffers once), the panel at
**30 s**. Playback: retries at **15 s** and **25 s** of buffering (paused: the schedule waits), the panel at
**40 s**. When the picture comes or playback resumes, the status fades and nothing else changes; a
`player-recovered` incident records the outcome (`… recovered after 3.2 s (1 retry with fresh URLs)`);
when it never does, the `player-failed` incident is the outcome. A background tab's start waits until the
tab is shown, as before. *Data but no picture* (3 Nov 2026, `own/stream.rs`): when the start's video and
audio cover the playhead for **1.5 s** and the element still has no frame (a decoder that takes data and
shows nothing, as 1440p60 HDR AV1 on the Android TV emulator did), that video format is given up and the
start goes on with the next one in a fresh MediaSource at once, before any stuck status.
*Media errors recover by themselves, at any time* (7 Nov 2026, owner: "fix all this kind of problems … why not make
it so that it refreshes by itself"; `fail::media_retry`, `fail::fault`, `own.rs` `media_error`). An element `error`, a
SourceBuffer error, an append the browser refuses, a refused `changeType`, the MediaSource closing by itself (or ending
together with an element error), or a decoder that stopped (playing, the tab visible, ≥ 1.5 s buffered ahead in both
tracks and the playhead still for 8 s) is never shown at once. Before or after the first frame, the player goes on in
place **at the same position and play/pause state** (a paused video stays paused; the picture on screen stays as a
canvas until the fresh MediaSource shows the same spot; our own positioning, so no request is redone and SponsorBlock
is unaffected): (1) a fresh MediaSource with the data already in (it also recreates the decoders), (2) a fresh
MediaSource with fresh URLs and the indexes asked again (what Try again does), (3) the failing track's next format (audio:
in the same language). URLs within 30 min of running out are refreshed first. A *codec fault* (the browser's message
names decoding, the codec or a hardware decoder; never the codec in use alone, nor a stall) goes to another codec family
(AV1 → VP9/H.264, `Out::CodecFault`) as its second step, before fresh URLs, when the video is offered in one; that codec
is avoided for the rest of this player, and a codec that faults twice in a tab for the rest of the tab session (memory
only, `App::codec_avoid`). Bounds (`fail::recent`): at most **3 recoveries within a sliding 2 min**, and 8 in a player's
life (never a loop); only then does the panel show, its reason ending `… at 0:12 (after 3 automatic retries (fresh
MediaSource, fresh URLs, another format))`. A recovery's fresh URLs are not the player's one refresh for a 403, and a
failed answer keeps the URLs. A recovered episode reports `player-recovered` once the picture stayed 1.5 s
(`SourceBuffer error (…) at 0:03: recovered after 0.2 s (2 automatic retries)`; before the first frame: `… before the
first frame: recovered after …`). An error while the tab is hidden and the video paused waits until the tab is shown or
played (a background tab may hold its decoders back; recovering then could spend the recoveries for nothing). Live
streams have no recovery in place: their media errors go straight to the automatic Try again.
*URLs that run out, a forgotten tab* (`own.rs` `proactive_refresh`, `wake_check`, `App::on_show`). The URLs' deadline is
their own lifetime from when they came (`streamingData.expiresInSeconds`, else 5.5 h; a cached response counts as 30 min
old), measured as wall-clock time elapsed since they arrived (a system sleep counts; the monotonic clock stops during one
on macOS), never `expire=` against the device clock (a TV box without NTP can be hours off; a clock set back counts as
run out: one refresh). A background refresh has its own 8 s timeout (no answer: URLs kept, no more proactive refreshes),
and the health check on a wake skips a recovery still on its way. When the tab is shown again or
playback resumes after a pause (once the picture was there), the element and the MediaSource are checked (an error on
the element, a closed MediaSource, an element that lost its media: recovered as above at once); and on those wakes and
once a minute, URLs within 30 min of their deadline are refreshed **in the background**: requests in flight go on and
playback is not held (a wake also asks again, with the new URLs, requests that got no answer in 3 s). A proactive refresh
that fails, meets the bot check or brings URLs as short-lived is not repeated for that player, and none goes out while
the bot check blocks VISIONOS: never a request a minute on healthy playback. (Owner report, Brave, 7 Nov 2026:
"SourceBuffer error (video/mp4; codecs="av01.0.08M.08")" on a video opened in a tab and come back to about an hour
later; Try again played it. Not reproduced: a headless Chromium never hides or suspends a tab — a page behind another one
stays `visible`, and a page frozen for 90 s (`Page.setWebLifecycleState`) with a paused MSE video played on unchanged;
googlevideo URLs live ~6 h, so an hour does not expire them. Likely causes on the owner's Mac: the AV1 decoder lost
after a sleep or a long background suspend, then a failed append on resume. e2e: "a media error mid-playback recovers by
itself …", "an AV1-only media error steps down …", "when every automatic recovery … fails …", "a video left paused with
URLs near their expiry …", "URLs that stay near their expiry get one proactive refresh, then none"; tests only:
`ytc-test-nothrottle` (localStorage) turns the throttle verdicts off for requests the fixtures hold on purpose.)
*The end of stream* is signalled only
once the element has its metadata (readyState ≥ 1: every SourceBuffer has its init segment), and a start always
fetches at least one segment per track (`sched::start_index`); a start at or within 0.5 s of the end (the tab's
remembered position of a video that played to its end, `t=` past the end, a resume record past it) plays from the
beginning. Before, such a start found nothing to fetch and ended the stream before any init segment, which
Chromium fails as `DEMUXER_ERROR_COULD_NOT_OPEN: MediaSource endOfStream before demuxer initialization completes`
(owner report, Brave, 7 Nov 2026; e2e "a start at or past the end of the media").

**Bot check recovery** (`own.rs` `recover`, `api.rs` `VisionRoute`). When VISIONOS answers
"Sign in to confirm you're not a bot", the player sends the other routes **in parallel** at once:
VISIONOS with **your account session** (cookies + `SAPISIDHASH`, only when signed in; one extra round
trip) and a **fresh anonymous visitor** (`/youtubei/v1/visitor_id` without cookies, ~40–60 ms, then
anonymous VISIONOS with it). The first OK plays; the other answer is ignored. The route that recovered
is kept **for the rest of the tab session** (memory only; the fresh visitor is never stored) for later
videos and for hover/speculative prefetches. A prefetch that meets the bot check is not a verdict: it
holds further VISIONOS prefetches until the player's own request (which recovers) succeeds. Only when
every route was bot-checked does the player show the panel ("YouTube asks to sign in to confirm you're
not a bot"), with the steps in the reason (`… · tried: anonymous (bot check), account session (bot
check), fresh visitor (bot check)`); for 10 minutes then, no VISIONOS prefetches, and a new video tries
the remembered route only (no parallel recovery; blocked again → the panel, whose Try again recovers
fully; an OK answer lifts the block). The happy path is unchanged (one anonymous VISIONOS call).
Other clients are no fallback: on 2 Nov 2026, from a network that was bot-checked, every anonymous client
(VISIONOS, ANDROID_VR, IOS, ANDROID, TV/Tizen, WEB, MWEB) got the same bot check with fresh and old
visitors, and IOS/ANDROID/ANDROID_VR also stop after ~60 s of media without a PO token.

**Debug overlay:** press `` ` `` or `Shift+D` (outside text fields). It shows the client version,
visitor/sign-in state, parse counts (Shorts parsed, ads skipped, duplicates, and every **unknown
renderer** YouTube sent that we don't handle yet), cache sizes, timing marks, and the player's state (formats, first frame, buffer ahead, throughput,
pings, its event notes, a stuck episode) plus recent player failures.

## Hook points (for feature modules)

Other modules plug into the markup without editing `render.rs` or the player:

| Hook | Where | How |
| --- | --- | --- |
| `render::set_card_hook(fn(&mut String, &Video))` | every video card (feeds, search, related), inside `.d` | Call once at boot. Markup with class `ca` sits centered beside the title (e.g. the Watch later button); anything else follows the metadata lines. Cards carry `data-v="<videoId>"` on `.c`. |
| `#ytc-filters` (`render::FILTERS`) | right side of the heading row on every feed page (Home, Search, Subscriptions, …) | Fill with `bridge::html` after the page renders. On Home/Search the visible title is hidden, so the row is only as tall as its content. |
| `#ytc-sub` (`render::SUBSCRIBE`) | watch page, beside the channel name | Inside `#ytc-info`, which is rewritten when `next` arrives: fill after `render_watch`. Hidden while empty. |
| `#ytc-acts` (`render::ACTIONS`) | watch page, right end of the channel row (Like/Dislike/Save…) | Same lifetime as `#ytc-sub`. Wraps on narrow widths; hidden while empty. |
| `#ytc-comments` (`render::COMMENTS`) | watch page, below the description | Outside `#ytc-info`, so it survives the info re-render. Hidden while empty. |
| `data-act` / `data-arg` | any element | Clicks go to `App::on_click(href, act, arg, mods)` (bit 32 of `mods` = keyboard activation). `p-*` acts belong to the player. |
| `data-hov` / `data-arg` | any element | The bridge reports the hovered zone to `on_hov(zone, arg, held)`; `MenuCtl` in `player/ui.rs` is a ready-made intent/grace state machine for hover-open popovers (the Filters and Save menus reuse it with their own timings). |
| `bridge::focus_in(id, delta)`, `bridge::focus_grid(id, sel, delta)` | keyboard | Focus the selected/first/next control in a popover; roving focus over a list (±1, ±1000 = one row). |
| `bridge::morph_vars(btn, pop)` | motion | A popover grows out of its trigger: writes the trigger's box relative to the panel (`--bw --bh --br --up --bx`) on the panel; client.css clips the panel's surface from it. Layout offsets, read once at a pointer open (before the content is written) or close. |
| `bridge::media_video_new(h, events)`, `bridge::media_grab(h, id, cls, max_w)`, `bridge::media_frame(h, id)`, `bridge::held_put(id, after)`, `bridge::held_drop(id)` | media | A `<video>` outside the document; its current frame drawn into a canvas held by the bridge until Rust places it after an element (or drops it); `on_timer(id)` once a video has presented a frame. |
| `bridge::click_rect(scope, inner)`, `bridge::rect(id)`, `bridge::flip_from(id, x, y, w, h, easing, ms)`, `bridge::fade_in(id, easing, ms)` | motion | Shared-element morphs (FLIP): read the source rect before a swap, write the page, then paint the destination from that rect (compositor-only). `fade_in` only on content just written. Easings/durations come from `motion.rs`. |
| `bridge::stagger(id, sel, sub, max, step, dy, easing, ms)`, `bridge::nudge(id, dy, easing, ms)`, `bridge::pill_slide(scope, from, to, cls, easing, ms)`, `bridge::pill_carry(scope)`, `bridge::flip_mark(scope, sel)`, `bridge::flip_play(scope, sel, easing, ms, out)` | motion | Entrance wave over freshly written items (no layout read); a persistent element's new text sliding in; a selection pill sliding between two items of a row (an empty `to` stops a running slide; `pill_carry` after re-rendering the row); reflow FLIP around a re-render that removes/moves items (keyed by `data-k`, else text). Reads happen only at the click. |
| `bridge::wheel(id, name)` → `on_wheel(name, dir)`, `bridge::read_value(id)` | input | Wheel notches over one element (−1 up / 1 down; trackpad deltas add up to a 50 px notch; a handled one does not scroll the page; Ctrl/⌘ stays zoom), bound once per element. The `value` of a textarea/input (the import paste box). |
| `#ytc[data-still="1"]` | CSS | Set during the first frame of keyboard and back/forward paints: gate every `@starting-style` entrance with `#ytc:not([data-still="1"])`. |

Shared design tokens are CSS variables on `:root` (client.css, one palette for the desktop, the toolbar popup and the TV):
surfaces `--bg --pn --rs --cb` (background, panel/popover, raised, control), `--hv` (the one hover wash),
`--on`/`--onh` (selected row tint / its hover), `--fi --bd` (text field, field/outline border), `--tk`
(switch track), `--ln` (hairline), `--wa` (translucent wash over video), `--sc` (scrim behind badges and
toasts); text `--tx --mu --fa` (≥ 10:1, ≥ 5:1, ≥ 4.5:1 on bg/pn/rs), accent `--ac` `#f1f1f1` (white) / `--ach`
(hover), `--rd` (live, destructive), `--er` (error text); `--sh` (popover shadow); radii `--r-card` 14,
`--r-pop` 12 (popovers, panels), `--r-ctl` 10 (chips, buttons); press depths `--pd` .97, `--pd-card` .985,
`--pd-icon` .95 (icon-only buttons), `--pd-glyph` .84 (an icon squeezing inside its control). Selected:
rows get `--on` + accent text; chips and segments solid `--ac` with `--bg` text.
The palette (0.14.0, Redline: owner decision 7 Nov 2026) is graphite surfaces (`--bg` `#0f0f0f`, `--pn` `#181818`,
`--rs` `#212121`, `--cb` `#272727`), white text, selection and focus (`--ac`), red for the brand and playback
(`--brand`, `--progress` `#ff0033`) and blue for content links (`--link`); SponsorBlock's category colors keep their
meanings (highlight pink, never the progress red). `--brand --brand-ink --progress --link --link-h` keep these roles
apart. Text contrast is WCAG AA on every surface it sits on (an e2e step computes it from the live tokens). There is
no palette choice any more: the 0.13.3–0.13.8 popup choice, `html[data-ytc-palette]` and the `ytc-palette-youtube`
`localStorage` hint are gone. The first frame of a new tab is already in the palette (client.css is injected by the
manifest), so nothing flashes and nothing is read first; a hint an older version left is removed at boot
(`appearance::forget_old_hint`). A stored `ytcConfigV1.theme` (`"youtube"`, written by those versions) is ignored and
never an error; the toolbar's patches keep it as it is, and the client's own config writes no longer carry it.
`src/appearance.rs` holds the toolbar's preference patches (SponsorSkip); `src/popup.rs` owns the toolbar UI.

### YouTube's logo inside the product, Redline outside it

Owner decision (7 Nov 2026): "don't change youtube logo, it should always be there, it's just a name for extension
and github, that's it". 0.14.0 had put a Redline mark into the interface; 0.14.1 takes it out again.

- **Inside the product, YouTube's branding as before 0.14.0:** the top bar shows YouTube's logo (`render::LOGO`: the
  red rounded tile with its white play mark, viewBox `0 0 28 20`, `--brand` and `--brand-ink`) and the "YouTube"
  wordmark, aria label "YouTube home"; window titles are YouTube's ("<page> - YouTube", Home "YouTube",
  `render::SITE`); the TV rail, the TV sign-in and About headers and the calm cards show the same logo
  (`render::MARK`, class `lmk`) and wordmark ("Make YouTube yours" on signed-out Home); YouTube's own pages
  (`?yt-native=1`) keep YouTube's header logo untouched. The top bar's dwell pill (`app/logo.rs`) is the logo link's
  own `::before`, independent of the logo's shape.
- **Redline is only the name of the extension, the TV app and the GitHub project** (`render::NAME`): the manifest's
  name, description and action title; the toolbar icons; the toolbar popup's header (the extension's own UI, with the
  Redline mark); the TV app's launcher label, icon and banner and its name on About & updates ("Redline for TV" with
  its versions, "Redline is up to date"); the README and these docs.
- **The Redline mark** (the extension's icon): a red rounded parallelogram leaning forward with a chevron cut out (one
  even-odd path, viewBox `0 0 32 24`). Its one source is `browser/icons/redline.svg`; `src/render.rs` inlines the
  same path only for the toolbar popup (`POPUP_MARK`; a Rust test keeps them equal), `node browser/icons.cjs` renders
  the extension's icons (`extension/icons/16|32|48|128.png`, the manifest's `icons` and the toolbar's
  `default_icon`), and `node tv/scripts/banner.cjs --all` the TV launcher icons and banner (the adaptive icon's
  vector, `drawable-v24/ic_launcher_foreground.xml`, carries the same path).

### Names kept for compatibility

The extension was called Lumen until 0.13.8 (the TV app "Lumen for TV"). Wherever the extension or the app names
itself, it now says Redline (inside the product the interface shows YouTube's name, above); these internal names keep
the old one on purpose, because stored data, installed apps or tools depend on them:

- Storage keys and DOM ids/classes: `ytcConfigV1` and every other `ytc*` key, `#ytc*` ids, `data-ytc-*` attributes.
- The crate and package names `ytc-client`, `lumen-tv` (its library `lumen_tv_lib`, loaded by name from Kotlin),
  `lumen-tv-core` (`lumen_tv_core`) and `lumen-debug-center`; the agent skill `.claude/skills/lumen-motion`.
- The extension's internal identifiers: the request-rule set `ytc_rules`, the Trusted Types policy name `lumen`.
- The TV app: the Android application id and package `app.lumen.tv` (a new id would be a different app that cannot
  update the installed one), its theme `Theme.lumen_tv`, the JNI names, the shell's internal paths (`/__lumen/client.wasm`)
  and environment variables (`LUMEN_TV_*`), the user agent `Lumen-TV/1`, the debug-center discovery probe `LUMEN-DC`.
- TV releases: the signing key and keystore under `~/.config/lumen-tv/`, the release tag prefix `lumen-tv-`, and the
  asset names `lumen-tv-manifest.json`, `lumen-tv-manifest.json.sig`, `lumen-tv.apk` and `lumen-tv-<version>.apk`
  (installed apps look for exactly these; the short install link `is.gd/lumentv` too).
- The GitHub repository: renamed from `justerquate-sys/lumen` to `justerquate-sys/redline`. Apps built before
  0.5.0 have `justerquate-sys/lumen` compiled in; GitHub redirects a renamed repository's API and download URLs
  (301 for `api.github.com/repos/{owner}/{repo}/…`, the release assets' `browser_download_url` carry the new name),
  and the shell's HTTP client (ureq 3, up to 10 redirects, GET kept) follows them, so those apps keep updating
  (`tv/core/tests/core.rs`, `update_through_a_renamed_repository`). This holds only while no new repository takes
  the old name `justerquate-sys/lumen`. New builds use `justerquate-sys/redline` (`LUMEN_TV_REPO` overrides it).
- Transcripts and its helper, because installed helpers, their service and the browser's registration depend on them:
  the binary and crates `scribe-helper` and `scribe-*`, the data folder layout (`.env`, `.data`), the native host
  `com.fulltranscript.helper`, the HTTP routes on `127.0.0.1:8765` and the log file. The manifest's `key` keeps the extension id
  `lamdpdfcpkihifbpmcalmjdnbfegjjjh`, the one the helper's native host allows. Worker record keys such as
  `allZipExclusions:` keep their shape too. People never see these names: the feature is called Transcripts.
- The debug center's LaunchAgent and data folders (`…/Application Support/Lumen`, `%APPDATA%\Lumen`,
  `~/.local/share/lumen`), so collected data stays where it is.

### Google TV interaction layer

The TV platform flag keeps these behaviors separate from the desktop extension. Since 0.14.2 the TV layout is the
one in tv/design/README.md: rem = 1/80 of the width, a 2.5 % / 3.5 % safe area (no overscan margin), the rail on the
left safe edge, five cards per row, 60 px player buttons; the one palette (graphite, white, red) remains; focus uses a stationary white frame (the
accent `--ac`: about 13:1 against the focused `--cb` surface and 17:1 against the graphite background; red stays for
the brand, playback and live, so a focused control never reads as an error or as progress). Remote focus, rail
expansion, panels and late-arriving page content are immediate, without the desktop's entrance waves or card zoom.

Search uses a Rust-rendered D-pad keyboard (`app/tvkeyboard.rs`, `tv/keyboard.rs`) and a query activation button,
with a hidden backing input for the shared search pipeline. It does not depend on Android's VirtualKeyboard API,
including on existing TV 0.3.0 shells whose shim intercepts Enter on text inputs. Its layouts are data
(`keyboard::LAYOUTS`: a–z, Russian, Ukrainian on a Ukrainian device, numbers and symbols, 36 keys each): the first open
takes the remembered one (`kb` in `ytcConfigV1`), else the device language's (`navigator.language`). Letters, symbols, space, deletion,
clear and submission reuse the shared suggestion/prefetch/request logic. Back first dismisses the keyboard to
the query, then closes Search; a physical keyboard remains usable. The bridge's `data-key-input` opt-in forwards
keyboard events from controls; Rust decides how they edit text or activate controls.

With TV app 0.6.0 the search screen also has a mic control (`app/tvglue.rs`, shown while `shell_info` reports
`caps.voice`), which opens Android's speech recognizer; that needs no audio permission and no socket, unlike the
desktop's dictation (`#ytc-mic`, which stays as it is and never shows on TV). What it hears, a search or a link another
app hands the app (validated in `tv/core/src/incoming.rs`), waits in the shell; a `ytc-wake` event tells the page to
fetch it (`shell_events`), and every boot does so too. tv/README.md has the details, including what only a real TV can
confirm. The update check reaches each release file by two routes, so it survives an outage of GitHub's website
(tv/README.md "Updates").

The TV player exposes Comments and More info as remote-operated side panels (`app/tvwatch.rs`). Comments uses
the existing anonymous WEB parsing, sorting, replies and continuation state, and fetches only after explicit
opening. Closing retains that state in memory. More info restores its content after Back/Forward, with the
description, channel, subscription action and available chapters. Each panel has Back to video, contains remote
focus while open and restores it to its originating player button on close. Navigation closes panels before
the watch snapshot is saved, preventing stale overlays on another page. Desktop markup and behavior are preserved.

The player row (`ui::shell_tv`) also carries what the official TV app has. **Channel** (`render::tv_channel`) is a plain
link: the history entry of a channel page opened from a video is left by one Back (`tv::History`, a mirror of the tab's
history kept entry by entry: `on_back` lets the shell go back instead of focusing the rail; the same channel reached another
way is another entry), and the video comes back through the usual Pop path as it was left (`Tv::left`: its position, over
the URL's `t`, and whether it was paused). A held OK is one press: the bridge marks a click activated by a key's
auto-repeat (`on_click` bit 64) and on TV only typing and the exact-speed steps take it (`tv::repeats`); a press within 400 ms
of "Unsubscribe?" does not confirm it. **Subscribe** shares
`Actions::subs`, the confirming second press and the rollback with the desktop button: `tv_row_sub` in the row,
`tv_channel` on the channel page (`#ytc-chsub`, the header's right end; it points `acts` at that channel, the channel id coming from the route or
`App::channels`), the More info button. **Audio** is its own menu (`ui::audio_menu`, `p-menu audio`); on TV the Captions
menu never has tabs. `set_audio` writes, per video in memory, the track (`formats::remember_audio`) and, for a dub, its language (`p.al`,
the id's tag: `en-US.4` → `en-US`); choosing a video's original clears `p.al`, because it means "no dubs": a language
remembered from it would dub the next video that has another original. `build_ladder` then picks: the track chosen for this video
(`audio_sel`, also what `own::Carry` hands a new player), else the remembered language where the video has it
(`formats::track_for_lang`: the exact tag, else the same language), else YouTube's default; no locale guess, nothing
replaces a choice made for a video. The focus prefetch (`own/pre.rs`) asks for the same track. **Auto-translate**: `captions::list_tv` adds, after the video's tracks, the
languages from YouTube's `translationLanguages` that have no track: the device's, Russian, English, then common ones, 8
at most, each `&tlang=` on the spoken language's track. A choice there is remembered (`p.cl`) and found again
(`prefer_lang`); choosing one of the video's own tracks clears it. **Dislike** shares `Actions::likes` with Like (one
state, so they exclude each other). **Save** is a panel (`#ytc-tvsave`) (`save::tv_inner`) with, the same `a-pl` clicks and
`Toggle`s as the desktop chooser, without its new-playlist form. **Next/Previous**: `tv_next` is the playlist's next, YouTube's
autoplay target whatever the Autoplay switch says, else the first related video; `tv_prev` is the playlist's previous
(`upnext::prev`), else walks `Tv::trail`, this session's videos, or restarts the video when there is none. When that video
is right behind in the history Previous is `history.back()`: otherwise Home → A → Next → B → Previous → A would leave Back
going to B, the video just left, and only then to A. Anything else opens as a new entry.

Every TV account action uses the desktop's request path (`actions::post` → `tv::finish`: `TVHTML5` with the bearer). The
anonymous watch and channel pages cannot tell the account's rating or subscription, so a quiet request as the account asks once
(`Job::TvState`, `actions::watch::account_state`, read loosely: a subscription counts only when it names this channel, else
it stays unknown); it never overrides a change made on this device (`tv_changed`). A refused action rolls back and writes the reason into `#ytc-tvactnote`, the only part of `say` that can be
seen with no panel open; the Save panel uses its own line.

`decoder_watch` used to skip when the end of stream had been signalled, so a fully buffered short video whose playhead froze
never recovered. Only the buffered-ahead guard (`av`/`aa` < 1.5 s) and the last second (`t > duration - 1`) stand for the true
end now; a media shorter than `lengthSeconds` ends with nothing ahead, so it never counts either.

## Transcripts

Full saved transcripts for people who feed them to AI, optional and invisible until set up. It needs the person's own
Azure Speech resource and a local helper. Three parts, each carrying only what it must:

| Part | Where | Runs |
| --- | --- | --- |
| Card chip, watch button, channel controls | `src/features/transcribe.rs` (Cargo feature `transcribe`; `transcribe_off.rs` when out) | In the YouTube page's Wasm: about 30 KB (2.4 %), and only shows what the worker answers |
| Worker service, reader, settings, popup section, icon | `transcripts/` (own crate and lockfile, `extension/transcripts.wasm`), host `browser/transcripts-host.js`, glue in `background.js` and `transcripts-core.js` | Loaded on the first message or alarm: never for someone who has not set it up |
| Helper | `helper/` (own workspace and lockfile; `helper/README.md`) | A native service on `127.0.0.1:8765`: downloads, FFmpeg, Azure, files |

**The gate.** `chrome.storage.local` `ytcTranscriptsV1` `{on:true}`, written only by the worker: when a settings read or
save returns `configured` and `has_api_key` it is set, when it returns otherwise it is removed. `enabled()` in the client
is that flag and not TV. While it is off nothing is written into a card and nothing is sent. The popup (`src/popup.rs`)
shows one row, `Transcripts: Set up`, which opens settings; only settings pairs and probes.

**Who talks to whom.** A YouTube tab → `chrome.runtime.sendMessage` (`bridge::worker_send`) → the worker → the helper.
The worker checks every message: `sender_trust` (this extension, an https YouTube tab or its own page), then `plan`
(fixed routes, validated ids, a short allow-list for tabs; `transcripts/src/plan.rs`, unit tested). The pairing token
comes from native messaging (`getConnection`) and lives in IndexedDB in the extension's own origin: a content script
cannot open it, and an old token found in `chrome.storage.local` moves there on install. `chrome.storage.local` itself must
stay readable by content scripts, so the worker sets its access level at every start (an older install under this id
may have closed it).

**States on a page.** One batched `getVideoStatuses` per page render (`page_changed` asks again, a continuation asks only
for the new cards). While jobs run the worker reads `/queue/status` every 5 s (plus an alarm every 30 s to survive being
put to sleep) and `/jobs`, and pushes `tsc:{live:{id:{s,p}},offline}` to the YouTube tabs; when a job leaves the list a tab
asks what became of it. Idle means no timer, no alarm, no message. A stopped helper: the toolbar goes grey, active chips
say so in their tooltip, no message appears, a page load tries the native host at most once in 5 minutes, and the alarm
gives up after 10 minutes without an answer until the next page asks. Explicit acts are never repeated after an
ambiguous failure: a POST can cost money.

**The channel panel** is written by the worker (`transcripts/src/channel.rs`): the tab holds an opaque state, sends it back
with each step and shows the markup it gets, so the page's Wasm carries none of that logic.

**The toolbar.** `transcripts/src/icon.rs` paints RGBA pixels from the mark's path, with no canvas: a progress line of
the batch's verified saved fraction, a dot for attention, grey offline, six seconds of check after work this worker saw
active finished (`core::lifecycle`, shared with the popup so reopening never replays it). Idle restores the manifest's
icons and title.

**Reader timestamps.** The helper's document route adds `sections` (start time and text per recognized segment, from the
saved `transcript.json`); the rendered documents and exports are unchanged, and a helper without it shows plain
paragraphs.

**Testing without a helper or Azure.** `tests/transcripts-mock.cjs` answers the routes; `tests/transcripts-steps.cjs`
patches only the temporary copy of the build (the helper's address, native messaging recorded). Its real-helper step builds
`helper/` with the feature `loopback-speech` (never in a release) and runs it on a temp root and a free port, with a
loopback Speech server. `YTC_TR_SPEED=1` prints first-card and hover timings.

### One extension id, and moving to it

Redline's manifest carries the `key` Scribe used, so the extension id is fixed and an installed helper pairs with no
reinstall. A new id means new extension storage: everything in `chrome.storage.local` belongs to the old id, and nothing
under `youtube.com`'s own storage matters (`localStorage` holds test switches only, `sessionStorage` one tab's snapshots).

| Stored in `chrome.storage.local` | What it is |
| --- | --- |
| `ytcConfigV1` | Player preferences (quality, volume, captions, SponsorSkip choices), client version, visitor data |
| `ytcVoiceKeyV1` | The Deepgram key |
| `yourTubeHistoryV1`, `yourTubeProgressV1` | Searches, resume positions |
| `yourTubeBlockedWebsitesV1`, `yourTubeRecommendationTopicsV1`, `yourTubeNativeTopicsV1` | Blocked websites, topics, Home chips |
| `yourTubeMinimumHomeDurationV1`, `yourTubeHomeOrderV1`, `yourTubeShowMusicV1`, `yourTubeShowLiveV1`, `yourTubeHomePlaylistsOnlyV1`, `shortsInFeeds` | Home filters |
| `ytcPlayerLogV1` | The last ten player failures, for the debug overlay |
| Scribe's `allZipExclusions:*` | Folders left out of the All ZIP |

The app's Import dialog reads the older YourTube extension's records only, so it does not cover this. The move is a copy
in the two workers' consoles (`chrome.storage.local.get(null, d => copy(JSON.stringify(d)))`, then
`chrome.storage.local.set(<paste>)`). Lost with the old id only what is rebuilt by itself: the session's ready Home
(memory) and the pairing token (paired again through native messaging).

## Moving from the old YourTube extension

An earlier, private YourTube extension (a native-YouTube restyling) kept its data in its own storage. The client
can import it once: **+ Topic** → **Import data from the old YourTube extension…**, run the line the dialog shows
in the old extension's service-worker console (`chrome.storage.local.get(null, d => copy(JSON.stringify(d)))`,
nothing is sent anywhere), paste, **Import**.

What happens (`src/features/import.rs`, pure Rust, unit-tested): the paste must be one JSON object of
at most 4 MB with at least one YourTube key, and every known key must have the old extension's exact shape,
else nothing at all is written and the dialog says which key was wrong. Single bad records inside a good
store (an invalid domain, a broken resume entry) are skipped and counted. The client's bounds apply: 100
searches, 2,000 resume records, 50 blocked websites, 20 topics, 20 YouTube-topic records.
- Merged: search history and resume positions (newest wins per query / per video, the newest 100 / 2,000
  kept); blocked websites, your topics and YouTube's chip order/names/hidden state (union, the client's own
  entries first and winning on conflicts).
- Taken from the old extension: the Home filter settings (`yourTubeMinimumHomeDurationV1`,
  `yourTubeHomeOrderV1`, `yourTubeHomePlaylistsOnlyV1`, `yourTubeShowMusicV1`, `yourTubeShowLiveV1`,
  `shortsInFeeds`) and the captions: `yourTubeCaptionsV1` `{version 1|2, position {x, y}, scale}` becomes the
  player's `ytcConfigV1.p.cs` `{v:2, x, y, s}`, `yourTubeCaptionsEnabledV1` becomes `ytcConfigV1.p.cc`.
- Left out: `yourTubeStatsV1` (viewing statistics) and the legacy `yourTubeDiscoveryTopicsV1` /
  `yourTubeHomeTopicsV1`; any other key is ignored.

The old data holds no descriptions, recommendation payloads, continuation tokens or verdicts, and none are
created. Importing twice is harmless (the second run merges into the same records).

## Code map

| Piece | What it does |
| --- | --- |
| `transcripts/`, `helper/`, `src/features/transcribe.rs` | The optional Transcripts feature: see "Transcripts". |
| `manifest.json` | MV3; `key` (the fixed id, "Transcripts"); `storage` + `declarativeNetRequest` (+ `nativeMessaging`, `alarms` for Transcripts); host `https://www.youtube.com/*` (plus `*.googlevideo.com` for the player and `https://sponsor.ajay.app/*` for SponsorBlock; `https://api.deepgram.com/*` for voice search's key check); two `document_start` content scripts: the client (with exclusions) and native mode (`*yt-native=1*`: `native.css` + `content.js`). |
| `native.css` | YouTube's own pages only: the old extension's hiding/theming rules (see "YouTube's own pages keep the client's setup"). |
| `src/native/` | Native pages: `mod.rs` returns to the client when leaving the fallback video; `clean.rs` the cleanup logic (captions, keys, CC, promotions, Shorts switch). |
| `content.js` (generated) | `window.stop()` first (except Home's document: `boot()` stops or holds it), then the bridge, the wasm-bindgen glue and the Wasm bytes (base64 in a block comment, read via `Function.prototype.toString`), `initSync`, `boot()`. |
| `bridge.js` | Generic primitives only: replace the document, set/append HTML/text by id, attributes, CSS variables, history, `location.assign`, scroll, fetch (bytes back to Rust, optional init options; `progress: 1` also reports headers and bytes so far), abort, `chrome.storage`, timers, IntersectionObserver, postMessage, fullscreen, element size, `fade_imgs` (images still loading fade in when they land; cached ones are never hidden); media objects by numeric handle (bind a `<video>` and forward its events, `MediaSource`, `addSourceBuffer`, `appendBuffer`, `remove`, `changeType`, `buffered`, get/set/call properties, `isTypeSupported`); delegated click/dblclick/submit/pointerover/pointermove/keydown/input/drag/popstate/pagehide; voice: microphone → 16 kHz PCM chunks with a level custom property, a WebSocket relayed by the background worker (a stored secret appended there), a held bare key, contextmenu on `[data-ctx]`, secrets (only whether one is set; saved straight from an input). No YouTube or player knowledge: every decision is in Rust. |
| `rules.json` | Static DNR. YouTube's site bundles are not blocked (YouTube's service worker fetches them outside the page frame, so a block broke the native fallback; `window.stop()` keeps them from running on client pages). `allowAllRequests` for `yt-native=1` and the native paths at priority 2; rule 20 (priority 1) adds a script-free CSP to Home's own document (read by the client, never run); block `log_event`, `/pagead/`, `csi_204` and doubleclick (from youtube.com) at priority 3, so telemetry stays blocked even on native pages; rule 33 lets Redline's own "shown" batches (`log_event?alt=json&prettyPrint=false`) through; rule 50 sets `Access-Control-Allow-Origin: *` on googlevideo responses to youtube.com, because googlevideo randomly redirects chunk requests to another node and the redirected request carries `Origin: null`. `/s/player/` and `/api/stats/` are never blocked. `background.js` handles auto-update, debug-center delivery and the voice socket relay. |
| `client.css` | The whole look (graphite/white/red tokens, layout, player), injected by the manifest (no flash). 15 KB (4.7 KB gzip). |
| `src/route.rs` | URL → page, a channel's tab included (`ChannelTab`). |
| `src/app/changlue.rs` | Channel pages: the tab row, one list per tab and sort set aside whole, new tabs through the page prefetch, sort chips (see "Channel pages"). |
| `src/config.rs` | Replaces the stopped page's `ytcfg`: client version + visitor data from `/sw.js_data` (2.7 KB, the endpoint YouTube's own service worker uses), cached in `chrome.storage.local` key `ytcConfigV1` for 24 h; fallback version `2.20261030.01.00`; session index `au` (default 0); SAPISID cookies → `SAPISIDHASH`/`SAPISID1PHASH`/`SAPISID3PHASH`. |
| `src/api.rs` | InnerTube bodies and headers (`X-Youtube-Client-Name/Version`, `X-Origin`, `X-Goog-AuthUser`, `X-Goog-Visitor-Id`, `Authorization`). Same-origin `fetch`, cookies included by the browser. |
| `src/parse.rs` | **All** response parsing: `videoRenderer`, `compactVideoRenderer`, `gridVideoRenderer`, `playlistVideoRenderer`, `lockupViewModel` (video/playlist/podcast/album/course), `channelRenderer`, legacy playlist renderers, `continuationItemRenderer`/`continuationItemViewModel`, watch primary/secondary info, attributed descriptions (UTF-16 offsets, redirect links unwrapped, only http(s) kept), channel/playlist headers. A walker descends only through known containers; Shorts (`shortsLockupViewModel`, `reelItemRenderer`, Short `videoRenderer`/`lockupViewModel`, Shorts shelves) become video items marked `short` and are counted; ads are dropped and counted; unknown renderers are counted, never fatal. |
| `src/json.rs` | The JSON reader for every response and stored record: accepts and reads exactly what `serde_json` would (same numbers, members in key order, last duplicate wins, same errors; checked against serde_json on the fixtures and 3,000 mangled documents), but borrows unescaped strings from the body and keeps objects as sorted vectors. Halves the Wasm CPU per flow (e2e fixtures: search load 10 → 4.5 ms, Home preload to 100 videos 34 → 18 ms, hover + open a watch page 37 → 20 ms). `serde_json` only builds outgoing JSON. |
| `src/description.rs` | Pure port of the old extension's description rules (`resource`, line walk, bare-link labels), unit-tested case for case against the old extension's cases. |
| `src/render.rs` | HTML strings; every text/attribute escaped; one `innerHTML` per region; event delegation on the document. |
| `src/player.rs` | The player boundary (the app only talks to this): markup for the player box, the manual Open on YouTube URL, and `Own`; `player/fail.rs` the failure panel, its messages and the stuck episodes' retry schedule. |
| `src/player/own.rs` | Our player's state machine: VISIONOS + WEB `player` calls, format choice, index fetch (with the start of the first segment in the same request), hover media prefetch (`own/pre.rs`) and the warm first frame (`own/warm.rs`), hedging and throttle recovery (`own/hedge.rs`), per-track fetch/append queues, seek, quality/codec switch (`changeType`), eviction, end of stream, 403 → one refresh, retries, stall/first-frame watchdogs, captions, pings, controls and keys. |
| `src/player/{formats,index,sched,stats,captions,storyboard,throttle,ui}.rs` | Pure, unit-tested parts: when YouTube is slow (hedge deadlines, the throttle verdict, escalation order, the drop rung and its hold); storyboard spec parsing, level choice and frame/cell math (real WEB spec fixture `player_web_storyboard.json`); format ladder (codec order by `isTypeSupported`, size cap = player height × devicePixelRatio with 20 % slack, throughput ABR at 70 % with a 1080p floor; the ≤ 360p start rung is used by live streams only); MP4 `sidx` and WebM `Cues` parsing; buffer decisions (30 s ahead, evict 60 s behind, seek resume, gap-free switch cut); history ping URLs as base.js `Q4a` builds them (`s.youtube.com` → `www.youtube.com` like the web player's `vss_host`); json3 captions; control markup, the three menus and the hover-menu state machine (`MenuCtl`). |
| `src/app.rs` | State: navigation (pushState/popstate), request lifecycle (stale responses dropped, aborts), feed queue (shown / preloaded / continuation, generation-checked), idle preloading, snapshots (stale-while-revalidate), hover prefetch, previews, suggestions, in-memory caches. |
| `src/actions/` | Watch-page actions, comments, chapters, notifications, account menu: `watch.rs` (like/subscribe/comment-token/chapter data from `next`, row markup), `comments.rs` (both comment formats, markup), `save.rs` (playlist membership from `get_add_to_playlist`, chooser), `notify.rs` (topbar count/avatar, notification list, account menu), `req.rs` (request bodies, outcome/reason), `toggle.rs` (optimistic state: one request in flight, queue a change of mind, roll back on failure), `mod.rs` (state, popovers, timers, responses). Unknown comment items are counted (debug overlay line `acts`). |
| `src/telemetry.rs`, `src/app/telglue.rs`, `src/player/own/tel.rs` | Speed telemetry (see "Speed telemetry" below): the batched event buffer, its gate and frame spans (`telemetry.rs`, unit-tested); the app's hooks (navigations, InnerTube requests, flushes) and the player's (start breakdown, googlevideo requests, seeks, switches, stalls, drops, menus, toasts, session summary). The bridge only observes (`tel_*`), buffers and forwards. |
| `src/hedge.rs` | Hedged InnerTube reads (pure state: which requests may go out twice, the per-endpoint deadline from the session's latencies, which answer wins); the app sends, times and aborts (`App::send`, `hedge_fire`, the top of `on_fetch`). See "Speed". |
| `src/snap.rs` | Feed snapshots ↔ JSON for `sessionStorage`: compact item records, re-validated on read (ids, hrefs, image hosts), size caps (150 items, 400 KB, 6 pages). |
| `src/ready.rs`, `src/app/homeready.rs` | Home reload: the ready Home response (record ↔ JSON, re-validated on read; whose it is; taint by personalizing events; age), the hedge deadline (`ready.rs`, pure, unit-tested); fetching it ahead, using it once, the hedged network path and the slow/offline fallback (`homeready.rs`). |
| `src/features/*.rs`, `src/app/fxglue.rs` | The extension's features rebuilt natively (see "Features" below): pure, unit-tested logic per feature (`filters`, `history`, `progress`, `websites`, `watch_later`, `topics`, `search_filters`, `home_chips`, `upnext`); `fxglue.rs` (a child module of `app`) wires them in with one-line hooks in `app.rs`; `app/upglue.rs` owns what's next on watch pages, `app/speed.rs` the speculative/background prefetch and the perf report (`src/perf.rs`). Cards get the Watch later button through `render::set_card_hook`; the filter bar fills `#ytc-filters`. |

Privacy (features): the keys below are the only additional records in `chrome.storage.local`:
`yourTubeMinimumHomeDurationV1`, `yourTubeHomeOrderV1`, `yourTubeShowMusicV1`, `yourTubeShowLiveV1`,
`yourTubeHomePlaylistsOnlyV1`, `shortsInFeeds` (the Shorts switch, boolean; the old extension's key for it), `yourTubeShowMembersV1` (the Members-only switch), `yourTubeHistoryV1` (last 100 searches), `yourTubeProgressV1` (≤ 2,000 resume
records: id, position, duration, completed, time), `yourTubeBlockedWebsitesV1` (≤ 50 domains),
`yourTubeRecommendationTopicsV1` (≤ 20 topics), `yourTubeNativeTopicsV1` (order/hide of YouTube's own Home
chips: `{version:1, topics:[{key, name, hide?}]}`, ≤ 20 records). The Autoplay switch is `ap` in `ytcConfigV1` (written only when
off). Never stored: descriptions, topic cards, chip/continuation tokens, search-filter params, website verdicts (description *hosts* for ≤ 500 videos are kept in memory only). The filter
preferences are mirrored in this tab's `sessionStorage` (`ytc:fx`) so a reload filters before storage answers; `ytc:members` (sessionStorage, ≤ 100 video ids) is what the player's verdict said was members-only.

Privacy (voice search): the outside service **Deepgram** receives microphone audio only while a dictation runs (from
the dwell / key press to the end; never otherwise) plus the user's key in the WebSocket handshake. The key is stored
only in `chrome.storage.local` `ytcVoiceKeyV1` (local, not synced), read only by the background worker, never in the
page, Wasm, logs, incidents, the debug overlay or the repo (e2e checks storage, incidents, the page and the overlay).
Transcripts live in the field until the search; nothing of the audio or text is kept.

Privacy: only `ytcConfigV1` (client version, visitor data, session index, refresh time, player preferences; on TV also three
short tags: the keyboard layout `kb`, the audio language `p.al`, the caption translation language `p.cl`;
a `theme` field older versions wrote is ignored) and
`ytcPlayerLogV1` (the last 10 player failures: time, video id, position, reason) are persisted. A fresh visitor from the bot-check
recovery and the chosen VISIONOS route live in memory only (never stored, never in reports).
SponsorBlock (an outside service, besides YouTube itself): requests carry only the 4-hex SHA-256 prefix
of the video id (never the id, no cookies `credentials:"omit"`, no referrer `referrerPolicy:"no-referrer"`, no HTTP
cache `cache:"no-store"`); answers are kept in memory for this tab (100 videos) and never persisted; skips are not
reported back (no `viewedVideoSponsorTime`, no votes, no user id). Turning it off sends nothing at all. Only the
two small preferences above are stored.
Feeds, descriptions, recommendations and continuation tokens live in memory, plus feed snapshots (items,
continuation token, scroll), the ready Home response (`ytc:ready`: the same compact item records plus its
continuation token, YouTube's Home chips with their tokens and the bell count/avatar; one at a time, used once)
and a config mirror in this tab's `sessionStorage` (cleared when the tab closes; YouTube's own pages in the same
tab share that origin storage). `ytc:stale` (sessionStorage, written only by a stale tab's reload) keeps when it reloaded, whether
the video was paused and the video id. `ytc:bt` (sessionStorage) keeps the last 20 Home request durations (numbers only)
for the hedge deadline.

## Features (ported from the extension)

- **Watch later button** on every video card (feeds, search, related; signed in only): a clock in the card's
  `ca` slot. First hover/focus asks `playlist/get_add_to_playlist` once per video; click saves/removes through
  `browse/edit_playlist` (`WL`, `ACTION_ADD_VIDEO` / `ACTION_REMOVE_VIDEO_BY_VIDEO_ID`) with your session.
  States: idle → saving → saved (YouTube's bookmark, filled), or error with YouTube's reason ("Couldn't save to Watch later:
  Sign in required (401). Click to retry."), also sent to the debug center as an incident. Cards on the
  Watch later playlist are known saved. Keyboard: Tab to it, Enter/Space. The bookmark fills on the click
  (optimistic, class `on` while the request runs) and empties at once on a remove; a press squeezes it and it
  springs back to size.
  On the Watch later page, a removal takes the card out of the grid in the click’s task; surviving cards
  reflow on the existing curve (keyboard/reduced motion instant), and keyboard focus moves to the next
  bookmark, or the previous one/Watch later navigation link at the end. A failure restores the card at its
  original place with the reason and retry action. Confirmed removals are kept in tab memory so old
  continuations cannot resurrect them; removed items are pruned from the feed and Watch later’s saved-page,
  prefetch and session snapshot copies are invalidated. A removal that settles on another page also
  invalidates those copies. Only Watch later cards disappear; cards in search/Home/related remain.

  Watch later has the plain **Watch later** heading; its owner/video-count/views metadata row is omitted.
  Before its first fresh paint, `watch_later::newest_order` reads YouTube’s **Date added (newest)** menu
  choice and applies the supplied `playlistEditEndpoint` order/params through `api::Req::WatchLaterOrder`,
  then browses the newly ordered playlist. This uses YouTube’s native playlist preference, so subsequent
  pages and other YouTube views follow it too. Already-selected newest makes no edit, and once a tab has seen
  it selected it neither checks nor waits again. The page is in the user's language; the menu is read by its
  English labels, so in another language a small English copy of the first page (`Req::WatchLaterMenu`) goes
  out beside it (same moment, no extra round trip) and only the sort command is English. A page that is not
  newest first waits for the sort; a failed sort (or check) shows the list as YouTube sent it, with
  "Couldn’t sort newest first · Try again" under the heading, never a blocked page. Order values and context
  params are copied from the response. Old Watch later snapshots, identified
  by their pre-0.13.4 playlist metadata header, are rejected; other snapshots keep their existing behavior.
- **Filters** (`#ytc-filters` on Home and Search; hover opens after 40 ms, 180 ms departure grace; keyboard
  focus or a click opens it too): minimum length Any/10+/20+/30+/60+ or a slider to 180 min (Home + Search,
  known durations only; live and unknown stay), **Shorts** (default off: hidden everywhere exactly as before;
  on: Shorts from Home, Search and related become cards with a "Shorts" badge in place of the duration and
  open our normal watch page; they pass the other filters but are kept at any minimum length; other pages never
  show them; hidden Shorts don't count toward preloading or chunks), Music (shows/hides videos YouTube labels "Music"/"Music
  video"; default hidden), **Members-only videos** (below), Home only: Relevance/Popularity (sorts everything loaded, on screen and preloaded,
  by view count; unknown counts last; Relevance restores YouTube's order), Live (Home + related; Search keeps
  YouTube's own), Playlists only (with a Show all reset). Everything applies to parsed items at once; no
  request is ever made for a filter. A note says how many loaded cards are hidden. If four chunks in a row
  are hidden entirely, scrolling stops fetching and offers "Load more".
- **Members-only videos** (8 Nov 2026, owner: "it shouldn't be a thing where you click a video, it looks like a normal video"; `parse.rs`, `features/members.rs`, `upnext.rs`).
  YouTube's signal, read from real signed-out responses (a channel's `UUMO…` playlist on WEB and TVHTML5, its "Members only" channel chip, 8 Nov 2026): `lockupViewModel`
  `metadata.contentMetadataViewModel.metadataRows[].badges[].badgeViewModel` (`badgeStyle: BADGE_MEMBERS_ONLY`, `iconName: SPONSORSHIP_STAR`, `badgeText` localized: "Members only", "Только для спонсоров",
  "Nur für Kanalmitglieder"); `metadataBadgeRenderer` (`style: BADGE_STYLE_TYPE_MEMBERS_ONLY`, `icon.iconType: SPONSORSHIP_STAR`) in `videoRenderer.badges`, a TV `tileRenderer`'s
  `lines[].lineRenderer.items[].lineItemRenderer.badge` and the watch page's own `videoPrimaryInfoRenderer.badges`. The enum or the icon decide; the words are a fallback, compared whole with a
  badge's own text only, so a title that says "members" never matches. Any other `…MEMBERS…` enum counts the same, so "Members first" (early access) is locked while YouTube sends it and free once it stops; that
  badge itself was never seen in the recordings (34 channels scanned). A playlist panel's rows carry no badge: a 26-character `UUMO…` id says it for them (a 24-character one is the ordinary uploads list of a channel `UCMO…`). Every video card shows a **Members only** label on the duration's row, at the
  other end (`features::card_badge`; desktop grid, Search, related, channels, playlists, History, Watch later, the TV's grid and Up next row; a playlist panel's row in its byline). Beside a duration of 8+ characters on a related thumbnail it reads "Members"; a duration is never covered.
  **The switch** (default on: shown and labelled, the owner's public default; `yourTubeShowMembersV1`) off hides them on Home, Subscriptions (`Surface::Subs`: only this switch applies there), Search and next to
  videos; channels, playlists and History keep them, like every filter's `Surface::Other`. A feed whose first page the switch emptied says nothing false: more comes in silently; Home's note and, at the very end, Subscriptions say "Your Filters hide the members-only videos here." when that's really the cause; no button undoes the choice, the switch is in the Filters panel. The TV app has no Filters panel: labelled and shown. **Never next**: `upnext::next` skips what's known to be locked whatever the switch says: the next
  playlist item that isn't, and in place of a locked autoplay target the first related card that isn't, visible and not yet seen in this tab, so autoplay never bounces (end of video, countdown, prefetch). A viewer who is a member can still click the card: the player decides, and with
  the usual anonymous route that means the panel with Open on YouTube, where the account is known. **Learning without the signal**: YouTube doesn't always say (a playlist row, a card without the signal, a Short): any VISIONOS answer saying so, the check near the end of a video or of a card in view included, or the player's verdict on a click,
  labels the id from then on, in place, by the switch, never next (`features::members`, ≤ 100 ids, this tab's `sessionStorage` `ytc:members`, nothing leaves the device); a page kept in memory gets it when it returns
  (`fx_members_stale`); a video that plays after all is forgotten at once and loses its label. **If an automatic advance still lands on one** (the check couldn't tell), it moves on by itself, at most three in a row (`App::up_skip_locked`; `Mode::Replace`: a skipped video never enters the history, one Back goes to the video that played); a viewer's own click always shows the panel. A video's check near the end asks about a replacement at most three times.
- **Home topic row** (one row: All · YouTube's own chips · your topics · + Topic). **YouTube's chips** are the
  Home response's feed filter chips (signed in, e.g. Gaming, Podcasts); a click loads that chip's feed through
  the chip's own continuation token (the next chip uses the token from the current response's bar), All
  restores the native feed from memory with no request. The + Topic editor lists them with earlier/later,
  Hide/Show and **rename** (`yourTubeNativeTopicsV1`): each name is an inline field (it reads as text until
  hovered or focused); Enter saves and the chip shows the new name at once, Escape (or leaving the field)
  cancels, an empty name restores YouTube's label. Only a renamed chip stores a `name` (≤ 40 characters), so
  YouTube's own label keeps showing for the rest. **Your topics** (1 Nov 2026: fast and fresh, following the
  pattern YouTube uses for its own topics): one anonymous `search` (hl=en) for the topic with YouTube's own filter params
  for This month + Videos gives the first screen; This year and any date (and their continuations) enrich in
  the background, fresher tiers always first; lexical scoring (Roblox topics need Roblox + tutorial evidence;
  RDC talks count), ≤ 3 per channel per batch; ≤ 30 requests and 160 cards per session; aborted on
  navigation. The old seed → related chain (≈ 15 s, old videos) is gone. Each topic chip has an × (hover /
  focus) that removes it. Chip and topic feeds stay in memory (no snapshot); back/forward returns to them, a
  fresh Home starts on All. The editor also lists/removes blocked websites.
- **Search filters** (Search page Filters panel, same hover-open/keyboard behavior): YouTube's own groups from
  the search response (Type, Duration, Upload date, Features: Live/4K/HD/Subtitles/Creative Commons/360°/VR180/
  3D/HDR/Location/Purchased, and Prioritize: Relevance/Popularity; YouTube no longer offers upload-date or
  rating sort). Each option is a link to the same query with the `params` YouTube returned (`sp=` in the URL,
  sent verbatim); hover prefetches it, nothing applies on hover, a click re-queries (instantly when
  prefetched), keyboard activation keeps focus on the option. YouTube's Duration options and the shared
  minimum length are one section. Active filters are chips beside Filters; one click removes one: YouTube
  sends selected options without an endpoint, so the removal is the current `params` minus that option's
  protobuf field (the field learned from the response's own options; bytes otherwise unchanged; matches
  YouTube's own params in the recorded cases); unreadable params offer one "Filters ×" chip (plain search).
  The Shorts type option is not offered.
- **Watch: playlist panel, Up next, Autoplay**: with `list=`, `next` is asked with the `playlistId`; the
  panel (title, playlist owner, current index / total or Mix, current row marked and scrolled into view, collapsible;
  no Loop/Shuffle) sits above **Up next** with the **Autoplay** switch. The app owns what's next
  (`App::next_video()`): the playlist's next item (always), else YouTube's autoplay target when Autoplay is
  on (never wrapping to an earlier playlist item). The player only reports `Out::Ended`: playlist items
  follow at once, plain autoplay counts down YouTube's `countDownSecs` with Play now / Cancel (player input
  cancels). With ≤ 45 s left the next video's `next` + VISIONOS are prefetched once.
- **Speed without waiting** (1 Nov 2026): video links prefetch `next` (+ VISIONOS) at once on hover,
  pointerdown and keyboard focus; in idle time the first 8 video cards in view are prefetched speculatively
  (low priority, two at a time; ≤ 30 speculative VISIONOS calls per 10 min because volume risks the bot
  check), cancelled on navigation except the page being opened; watch data and VISIONOS caches hold 40.
  Subscriptions is refreshed in the background (first page + first continuation, low priority)
  when older than 2 min while the tab is visible and was used in the last 5 min; a click paints the snapshot
  and the fresh page in the same task, and a snapshot younger than 2 min skips the slow refresh (Home: the
  ready Home response instead, see "Home reload"; 3 Nov 2026). e2e speed
  table (Home first page +1.5 s, `next` +300 ms, before → after): Home click → fresh Home 1783 → 16 ms; idle
  results, click a card in view → related visible 437 → 13 ms; focus a card, Enter 150 ms later → 368 →
  162 ms.
- **Speed telemetry** (`kind: "perf"`, `schema: 2` in the debug center): every user-visible path as
  structured events, batched; see "Speed telemetry" below and the dashboard at http://127.0.0.1:47321/perf.
- **Perf reports** (`kind: "perf"` in the debug center): per-endpoint request → response ms (n/p50/p90/max),
  parse+render ms, navigation → cards/related visible, Home fresh, cache hit rates, errors per endpoint.
  Numbers and endpoint names only (no URLs, ids, tokens or cookies). At most every 10 min with new samples,
  on tab hide (≥ 1 min apart) and when the document is left.
- **Blocked websites**: beside every external description link a "Block example.com" control appears on
  hover/focus; "Blocked · Undo" reverses it. **Hold-back:** while at least one domain is blocked, cards of
  a newly appended batch without a verdict are hidden in the same task as the append (class `fxh`) until
  their own check answers (blocked → removed, allowed/unknown → shown); held cards jump the check queue
  (still 2 at a time) and the next chunk is checked ahead of scrolling. After a page load, batches appended
  before the rules arrive from storage wait for them. The first paint is never held. If no check answers
  for 4 s, held cards are shown unchecked. Cost (e2e, 150 ms checks, scrolling straight through 150 cards):
  held cards waited 6.5 s on average, 12.9 s at most, all below the viewport; with normal scrolling the
  look-ahead usually has the verdict ready. The debug overlay line `websites ·` shows the counts. Domains are normalized (no `www.`, lower case, YouTube/Google
  redirects unwrapped, shorteners not resolved); a rule covers subdomains, never lookalikes. While at least
  one domain is blocked, rendered Home/Search/related/topic cards are checked against their own description
  (anonymous `next` with a field mask, 2 at a time, once per video; descriptions already fetched by hover
  or a visit are reused) and hidden when they link to a blocked domain. Unknown stays visible.
- **Search history** in the search box dropdown (with YouTube's suggestions below it): recent searches on
  focus, prefix/substring matches while typing; ↑/↓ select (the box shows the row), Enter searches, Delete
  removes the selected history row, Escape closes; × removes with the mouse (focus stays in the box).
- **Resume and progress bars**: the player reports its position on pause, at the end and every 10 s (and
  when leaving). Opening a video without `t=` resumes there (not when finished or < 5 s in); cards show
  the local bar (a faint full track under a rewatch of a finished video); YouTube's bar elsewhere.
- Captions on/off and caption placement stay with the player (`p` in `ytcConfigV1`).

## Tests

- `cargo test --locked`: parsers, escaping, routes, auth, the player's pure parts (formats, WebM Cues / MP4 `sidx`,
  scheduler, captions, storyboards, live HLS + fMP4), features, actions, voice state machine, motion tokens (kept in
  sync with `client.css`), on recorded signed-out InnerTube responses in `tests/fixtures/` (IP and visitor data
  replaced with documentation values).
- `cargo test --locked --manifest-path transcripts/Cargo.toml`: queue state, success window, URL rules, request plans and
  sender checks, the toolbar's pixels, the reader's text. `(cd helper && cargo test --locked)`: the helper (persisted data,
  filesystem races, ZIPs, cancellation, native framing, the installer; audio tests need FFmpeg, else they say so and skip).
- `tests/check-tv-bundle.cjs` (also run by `./build.sh --tv`): nothing of Transcripts in the TV bundle.
- `tests/e2e.cjs`: the real unpacked extension in Chromium with every request stubbed (YouTube-like CSP incl. Trusted
  Types): pages, navigation, escaping of hostile data, DNR rules, native-page cleanup, the own player on locally
  encoded media (`make-media.cjs`, `make-live.cjs`: WebM VP9/Opus and H.264/AAC fMP4 made by WebCodecs), failures and
  stalls, actions, features, voice search against a local WebSocket stand-in (`ws-mock.cjs`), SponsorBlock, motion
  and pixel checks, speed timings. Nothing reaches the network.
- `tests/live-*.cjs`: a few real requests against live YouTube / Deepgram, run by hand and sparingly.
- `tools/bench/` (live, by hand, rarely; a signed-out profile with a little watch history is seeded once outside
  the repository by `profile.cjs`): `home-reload.cjs` (reload → first paint of YouTube's answer, Redline vs YouTube's
  own page, parity of the two feeds, artificial latency on the first Home request, with or without the ready
  response and the hedge), `home-drift.cjs` (how YouTube's Home answer drifts with time) and `home-parity.cjs`
  (which parts of the request change YouTube's answer).
- `tools/bench/video.cjs`: the live video-path benchmark (development build, signed out, real youtube.com
  and googlevideo, by hand): first frame, ≥ 1080p, seek and quality-switch times from the client's own
  telemetry for cold loads, SPA clicks with and without hover, and slow YouTube (a drip-feeding googlevideo
  proxy 10×/100× slower per node, per URL or for everything; a hung player request). Several builds are
  interleaved per round (`--ext a,b`): live timings drift ±30 % within a quarter of an hour.

## Known gaps / next steps

- Player UI: the CC popup's **Audio tab and the audio-language switch are unit-tested only** (the e2e
  media has one audio track); try a dubbed video.
  The `/feed/playlists` page (`FEplaylist_aggregation`)
  is parsed by the generic walker and untested signed in.
- Actions not built: writing comments/replies, rating comments, the subscription bell (notification
  level), playlist privacy choice on create (always Private), "Save" for playlists. No live chat or channel
  tabs other than Videos.
- Player: VISIONOS is one unofficial client profile (it can break whenever YouTube changes it); no daily
  canary yet; no live chat or live captions (HLS WebVTT), no low-latency live tuning (1 s segments work
but use the same rules), Shorts-style looping, storyboard thumbnails on the seek bar,
  audio-track (dub) choice, or caption styling/translation; MP4 (`sidx`) playback is unit-tested but
  the e2e media is WebM only; ABR is a simple throughput rule; volume keys ↑/↓ are not bound.
- Bundle size: the player adds ~171 KB of Wasm (297 → 468 KB; content.js 417 → 657 KB).
- Bundle size 0.3.0: the speed work adds ~45 KB of Wasm at the old `opt-level = 3`; with `"z"` the
  whole Wasm is 393 KB (content.js 562 KB) vs 513 KB / 722 KB.
- Timing table: `node tests/e2e.cjs` prints Home timings with 150 ms simulated InnerTube
  latency; `YTC_EXT=<another extension folder> YTC_TIMING_ONLY=1` runs the same table on another build.
- Since the `/s/desktop` DNR block was removed (0.2.1), the stopped YouTube document's bundles can still
  be *downloaded* (not run) when its first bytes arrive before `window.stop()`; the e2e check "site
  bundles never requested" fails about 1 run in 4, on `main`'s build as well. (Home's own document carries a CSP
  that keeps them from being fetched at all: rules.json 20.)
- Suggestions come from YouTube's `/complete/search` with your cookies (like YouTube's own box).
- Search answers are not field-masked: ~60 % of a search answer (928 KB decoded, 104 KB gzip) is
  `expandableMetadata` (key-moment cards, up to 75 KB per result) that Redline never renders, but a mask can only
  list what to keep, so every result renderer type would have to be named (an unknown name is an HTTP 400, a
  forgotten one silently drops results). Parse is 2–3 ms; the gain would be ~60 KB on the wire per search.
- The YouTube HTML document is still requested (and stopped at the first bytes, except Home with nothing ready,
  which reads it); the shell appears at roughly time-to-first-byte of that document.
- After `window.stop()` the document's `load` event does not fire (readyState is `complete`); no effect
  seen, but noted.

