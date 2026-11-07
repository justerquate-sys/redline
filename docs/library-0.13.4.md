# Watch later and sidebar interaction

> A record of the release named in the title, when the product was called Lumen and had the charcoal and mint
> palette; since 0.14.0 it is Redline, with one graphite, white and red palette (docs/architecture.md).

Lumen 0.13.4 adds hover activation to the six desktop sidebar links, removes the owner/count/views row
under Watch later, requests newest-save order from YouTube, and removes unbookmarked cards immediately.
It includes the previously prepared selectable palette and toolbar preferences from 0.13.3.

## Direction and audit

- Product: Lumen for browsing and watching your saved videos.
- Character: familiar, quick, restrained.
- World: quiet YouTube-style chrome in the existing Lumen and YouTube palettes.
- Moments: thumbnails and video carry the color.
- Signature: a short hover dwell with immediate click and keyboard activation.

This is a focused polish pass. The screenshots show redundant library metadata, and the reported
interactions require clicks or a refresh to finish. The existing layout, icons, type and palettes stay.
No generated image or prompt is needed.

## Nine passes

1. Spacing: removing the metadata row lets the existing heading sit directly above the grid.
2. Color: the hover fill uses the selected palette’s existing tokens; no new accent or surface ramp.
3. Icons: preserve the six navigation icons and bookmark; card keys keep reflow tied to the right video.
4. Copy: retain **Watch later** and remove the owner, total-video and views line.
5. Type: keep the established heading, label and metadata typography.
6. Layout: retain sidebar width, card proportions, grid breakpoints and page order.
7. Details: unbookmark hides the card before the response. Failure restores its original position with
   a retry reason. Keyboard focus moves to the next bookmark and survives rollback; the last removal
   shows the empty state and focuses the Watch later sidebar link.
8. Visuals: use the product’s own content. Chromium fixture screenshots were inspected in both palettes.
9. Motion: reuse the logo’s 250 ms dwell, with cancellation on pass-over, dragging, typing and menus.
   Clicks and keyboard activation are immediate. Surviving cards reflow without keeping a ghost of the
   removed card; reduced motion and keyboard removal are instant. The TV rail keeps its remote behavior.

## Ordering and state

Newest means date saved, not upload age. Rust follows YouTube’s **Date added (newest)** menu command,
including its supplied order value and params, before rendering the fresh list. YouTube remembers that
native playlist preference, and all continuations use the resulting order. Already-newest makes no
edit; a rejected sort offers Retry. Old Watch later snapshots with the previous metadata header are
discarded. Other playlist headers and snapshots retain their behavior.

Removing a video updates the visible list in the click’s task. Confirmation prunes the feed and
invalidates Watch later’s old cached copies, including when the response finishes after navigation.
Tab-memory removal state keeps stale responses from bringing the card back. Failed removals remain
retryable in their original position, including when the response arrives after navigating away and
returning with Back. The player's Save chooser confirms through the same Rust state, so re-saving a
removed video clears its removal marker and invalidates old Watch later copies. Search, Home and
related cards keep their place.

The native command shape was checked against the primary
[yt-watch-later-tools implementation](https://github.com/jadenjsj/yt-watch-later-tools/blob/main/yt-watch-later-tools.user.js).
No real-account playlist was edited during verification.

## Proof

The real unpacked extension in Chromium reproduced all four original failures before the fix.
The focused checks pass for sidebar intent/cancellation, the header, newest-first ordering across
60 videos and multiple continuations, optimistic removal/rollback/back/reload, sort failure/retry,
and the last-card empty state. Rust tests cover supplied sort values and stale-response removal state.
Two additional review reproductions cover failure after navigation and re-saving through the player;
both fail on the initial 0.13.4 build. The final `npm test` passes 279 Rust tests (5 ignored) and
186/186 browser checks (3 development-only skips) on the public build. The expanded focused run passes
13/13, including import with its original exact count. `npm run check:public` and whitespace checks pass.

## Independent review

Three sol-loop rounds used GPT-6.1 Sol with max reasoning effort. Round 2 approved the behavior;
round 3 retained approval after isolating the re-save test's history storage.

- S1 (should): fixed rollback after navigation by invalidating cached Watch later copies on failure.
- S2 (nit): fixed re-saving through the player by confirming its Watch later edits through shared state.
- Both reproductions failed before the fixes; the expanded focused run passes 13/13, including the
  existing sidebar, Watch later, Like, Subscribe, Save and import checks. No findings were declined
  or remain open. The re-save test restores the exact prior search-history key in `finally`, including
  absence, leaving import's exact count and application behavior unchanged.
- The base diff was 37 files, 1,610 insertions/394 deletions before review and 38 files,
  1,691 insertions/398 deletions at final review, including generated public code and the reproductions.
  This final review/proof note is documentation only. Review usage was not exposed by the tool.

Design process adapted from [Unslop](https://heydamir.design/unslop) by
[Damir Sokolovsky](https://x.com/heydamir), licensed under CC BY 4.0.
