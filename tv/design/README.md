# Redline for TV: design reference

`look-sheet.html` is the static composition reference for the TV screens, one 1920×1080 frame each.
The fixture-driven TV screenshots from `tests/e2e.cjs` verify the shipped interface, including keyboard and comments.
`node tv/design/render.cjs [dir]` renders the frames to PNGs (default `/tmp/redline-tv-look/`). The app's TV
stylesheet is the block `/* ===== TV app` at the end of `extension/client.css` (everything under
`html[data-ytc-tv]`), and it uses the same numbers.

## Grid and tokens

| Token | Value | Notes |
|---|---|---|
| Root size | `font-size: 100vw / 80` | 1 rem = 12 CSS px on a 960×540 WebView (DPR 2), 24 px at 1920×1080, 48 px at 3840×2160: one layout at every resolution |
| Safe area | `--sx: 2rem`, `--sy: 1.575rem` | 2.5 % left and right (48 px at 1080p), 3.5 % top and bottom (38 px): current Google TV sets do not overscan. Content and focus rings stay inside; backgrounds and pictures may bleed |
| Ring | `--ring: calc(1rem / 6)` (4 px at 1080p; 2 CSS px = 4 device px on the 960×540 WebView: outlines snap to whole device pixels), offset `.1875rem` | white `#f1f1f1` (the accent `--ac`), the only focus ring; inside the box where the box itself sits on the safe edge (rail tiles, menu rows, the search query) |
| Focus | Steady white frame, no scale | Focus, rail expansion, panels and key feedback are immediate; the focused surface steps to `#272727` (chips: `#3f3f3f`) |
| Colours | bg `#0f0f0f`, panels `#181818` / `#212121`, surface `#272727`, raised `#3f3f3f`, selected `#303030`, accent white `#f1f1f1`, brand and progress red `#ff0033`, text `#f1f1f1`, secondary `#aaa` | the one palette, the same as the desktop's |
| Type (1080p px) | page headings 33 (`1.375rem`), player title 33, card titles and panel text 22.5 (`.9375rem`), rail labels and buttons 21–22.5 (`.875–.9375rem`), secondary 19.5 (`.8125rem`), never below 18 (`.75rem`: duration badges, the failure reason); captions 43.5 (`1.8125rem`) | card titles 2 lines, ellipsis; the e2e audit fails any text under 17 px at 1080p |

## Layout

- **Rail**: tiles from the safe edge x = 2 rem (48 px; 2.5 rem = 60 px tiles with 1.25 rem = 30 px icons, pitch
  `--tvrow` 2.875 rem from 5 rem: Search, Home, Subscriptions, History, Watch later, Playlists, Liked), YouTube's logo
  (1.75 rem) centred over the icons and on the pages' first row, and below a gap (25.875 rem) the account (Sign in /
  Account) and About & updates (a white dot while an update is ready). Labels open to 10.5 rem.
  The focus entering it reveals labels over the content immediately; the selected row has a quiet neutral
  surface and white text. The content never moves. Back from the content focuses the rail on the page's own item; →
  returns to where the focus was.
- **Pages**: content from x = 5.75 rem (138 px) to 77.5 rem (60 px from the right), starting 2.075 rem from the top
  (the chips row or the heading, 2 rem tall), the grid 1 rem below. **5 cards per row**, 13.35 rem wide (320 px; 16:9
  thumbnail 180 px), 1.25 rem gaps: a focused card and its ring stay inside the safe area without growing. Card
  title 22.5 px (2 lines), channel and views 19.5 px, duration 18 px; about three rows per screen. The focused row keeps the
  first row's place while the remote walks the grid (instant scroll, so a held key never waits); a fixed scrim
  fades what scrolled under the top edge.
- **Search**: a full-screen remote keyboard, independent of the installed Android input method. The 2.5 rem query
  button (centred on the logo's row) stays above a 21 rem-wide grid of six columns × six rows on the left and recent
  searches / YouTube's suggestions on the right. Every layout (`tv/keyboard.rs LAYOUTS`, data: a–z with äöüßñé, Russian
  in ЙЦУКЕН order with all 33 letters, Ukrainian on a Ukrainian device, numbers and symbols) fills all 36 keys, so
  the editing keys never move when the layout changes. Keys are 2.25 rem tall with .375 rem gaps; Space, Delete, Clear,
  the language key (it names the layout that comes next) and Search use the same focus frame. Back first collapses the keyboard, then closes Search. Results keep the
  query on top (↑ reaches it). Edit opens the keyboard again; a physical keyboard can also type. Search opens
  above the playing video and its drawers too, then returns to the originating focus when closed.
  **Mic control** (app 0.6.0, only while the shell reports a recognizer): a round 2.5 rem icon button (1.1875 rem
  outline mic, the desktop's drawing and stroke) at the right end of the query's row, .625 rem from the query, which
  gives up that room; on the search screen only, not on the results. → from the query, ↓ returns to the keys or the list;
  the same steady white frame (inside the box: it sits on the safe edge) and no animation. Its notices (no match, not
  available) sit bottom right in a 2.5 rem panel at 21 px for 4 s, over nothing that takes the focus; closing the
  recognizer says nothing. Android's recognizer draws its own screen over the app.
- **Player**: full screen at once. The overlay (any key; hidden 4 s after the last one while playing): title
  1.375 rem (33 px) and channel (22.5 px white, the name across the row's width, never cut while it fits) top-left over
  a scrim at least .82 opaque down past the channel row (10:1 over a white frame, measured), on the same left edge (2.5 rem) as the time row (elapsed · chapter · remaining, 19.5 px), the bar (4 px, 6 px
  focused, chapter gaps, an 18 px knob) and the 2.5 rem (60 px) round buttons with 28.5 px icons (channel, with the
  channel's picture; subscribe; play/pause, where ↓ from the bar lands; captions; audio, only with several tracks; speed;
  quality; like; dislike; Watch later; save; comments; more info; the speed and quality pills as tall). All thirteen fit
  one row (about 1020 px of the 1800 px between the safe edges at 1080p, clear of the skip notice), so nothing hides behind a
  More control. The focused action has a visible 19.5 px label below it, the row's first one from the safe edge. Like and
  Dislike while pressed are solid white with a filled thumb, focused or not, labelled Liked and Disliked. ←/→ on the bar scrub 10 s per press (30, 60, 120 s while held)
  with the storyboard frame above the target; the seek goes out once, when the remote rests. ↓ from the buttons
  moves the overlay up 10 rem immediately for the Up next row (11 rem cards, about six on screen). Menus, More info
  and Comments are panels from the right edge (18.5 / 25 / 28.5 rem: 444 / 600 / 684 px; 22.5 px text, 57 px menu
  rows; content inside the safe area), visible immediately. Captions lists the tracks, then Auto-translate under a small
  heading (19.5 px), then the on/off switch; Audio has its own panel; Save to playlist is as wide as More info (25 rem), a
  checkbox row per playlist with its privacy on the right (no new-playlist form: a name needs a keyboard). A list longer than the screen (a dubbed video can have sixteen languages and more) scrolls under the focus, the focused row inside the safe area, the first one with the panel's title. More info has Back to video,
  channel/subscription actions, a readable linked description and chapter buttons. Comments keeps a fixed
  header above a scrolling list with Top/Newest, replies, loading/error notices and focusable comment text. Captions bottom-centre at
  1.8125 rem (43.5 px), 8 rem higher while the overlay shows, clear of the time and chapter row; an open settings menu keeps captions in the visible picture
  to its left. The skip notice sits to the right of the action row (↑ or → from the last button), below the time
  row and captions. It follows the controls above Up next, and idle captions make room for a visible notice.
  Menu switches and seek-category labels use the same rem scale as the rest of the TV interface.
- **Channel page**: the header (3 rem avatar, name, details) with Subscribe at its right end, on the grid's right edge (2.25 rem
  tall; solid white, neutral once subscribed); .75 rem below it one row: tabs 2 rem tall with 21 px text and .375 rem corners
  on the content edge, the sort chips at its right end; the grid .75 rem below. The selected tab is a quiet `#303030` surface;
  focus steps it to `#3f3f3f` inside the white frame. The Shorts tab has **7 vertical cards per row** (9:16), Podcasts
  square-framed cards, the others the page's five. No motion; the row does not stick: ↑ from the grid's first row lands on
  the selected tab wherever the card is, ↑ again on Subscribe (↓ returns to the selected tab), → along the row reaches the chips,
  and a tab opened with OK starts at its top. The scrim ends above the first row as on
  every page; the first card keeps the default focus, so OK never subscribes by accident.
- **Notice**: an account action YouTube refused says why top right over the page or picture (21 px, 28 rem at most,
  on the safe edge, clear of a two-line title), for 6 s; beside Comments, a menu or Save, never over their headers; More
  info has its own line for it, and it stays off the search screen.
- **Sign in / Account**: full screen: the steps on the left from the content edge (the URL the device flow
  returned, the code at 3.5 rem = 84 px), the QR code of that URL on a light 15 rem tile on the right, as far from the
  right edge as the steps are from the left (`src/qr.rs`), Back first and focused.

- **About & updates**: full screen like Sign in: App and Interface versions, the last check and its result,
  one status card (up to date / update ready / app available with its download progress / the reason it failed, in
  words on top and the technical part under them, "Trying again automatically." while the app does)
  and its action focused (Check for updates, Update now, Install update, Retry). While an update is ready a small
  notice sits top right on the pages (never over a video) until the screen was opened.

## Direction and finishing

- **Product:** YouTube browsing and watching from the sofa, controlled by a remote.
- **Character:** Calm, legible, responsive.
- **World:** A dark cinema screen with quiet controls.
- **Moments:** Real thumbnails in browse; the playing picture in watch.
- **Signature:** One crisp white focus frame that shows the next remote action. White, not red: it stands out on
  every surface it sits on (about 17:1 on the background, 13:1 on the focused `#272727` surface, far above the 3:1 a
  focus indicator needs), it reads the same over any thumbnail, and red keeps its meanings (the brand, playback
  progress, live, errors) so a focused control never looks like an error or a progress bar. It matches the desktop's
  keyboard focus ring.

Text controls have .375–.5 rem corners; icon-only player buttons stay round. Type remains Roboto/system, with
400/500 weights and 600 only for compact emphasis. Labels use sentence case. The background, raised controls,
and selected surfaces stay graphite; white marks focus and meaningful state, red only the brand and playback. No
generated imagery is needed.

## Applied polish passes

1. **Spacing:** a five-card rhythm and a 2.5 % / 3.5 % safe area (0.14.2: the owner on a Google TV, "the left
   toolbar is too far right, and the UI is too big": four cards, 96 px player buttons and a 5 % overscan margin
   before). Reserve room for focus frames, keyboard keys and the player's action labels; scrolling readers stop above
   the bottom safe margin.
2. **Color:** graphite, white and red (0.14.0; charcoal and mint before), with quieter neutral selected surfaces. Remove the seek knob's
   decorative halo and panel shadows; video remains the main source of color.
3. **Icons:** retain the existing outline family and consistent TV-sized icon boxes. Comments uses the
   same stroke treatment; focused player actions also have readable text labels.
4. **Copy:** use sentence case for Up next and Recent and suggested. Keyboard instructions describe the
   actual remote controls, and every reader has an explicit Back to video action.
5. **Type:** keep Roboto/system and three text roles, on the 0.14.2 scale (about 3/4 of the old one, never below
   18 px at 1080p). Supporting labels
   lose uppercase tracking; numbers keep tabular alignment.
6. **Layout:** preserve the rail, feed order and full-screen picture. Search pairs a six-column keyboard
   with suggestions; Comments and More info use bounded scrolling panels with persistent exit controls.
7. **Details:** rounded rectangles replace excess capsules, while icon-only player controls stay round.
   Focus is visible, stays inside open panels and returns to the originating action when closed.
8. **Visuals:** real thumbnails and the playing video carry the visual detail. No generated images or
   background decoration; the one brand element is YouTube's logo on the rail (Redline is only the app's name).
9. **Motion:** remote input, rail expansion and panel changes are immediate, including late responses.
   Only the idle overlay's short opacity fade remains; reduced motion removes it.

## Motion

Remote focus, key feedback, rail expansion, panel openings, search typing and Back are immediate. No control or
card grows while focus travels. The control overlay can fade out over 120 ms when it becomes idle, using
`cubic-bezier(.22,1,.36,1)`; waking is immediate, and reduced motion removes that fade too. Playback progress still
follows the media. No looping decoration, spring focus, backdrop blur or animated layout/shadows.

The polish process follows [unslop by Damir Sokolovsky (@heydamir)](https://x.com/heydamir), CC BY 4.0.
