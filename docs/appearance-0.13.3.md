# Selectable YouTube palette and toolbar controls

> **Since 0.14.0** the product is called Redline and this graphite, white and red palette is the only one, on the
> desktop and the TV: the palette choice, the Lumen palette (charcoal and mint) and the new-tab hint described below
> are gone (docs/architecture.md, "Shared design tokens"). A stored `theme` is ignored. The rest is kept as the
> record of 0.13.3.

The browser extension adds YouTube’s graphite, white and red colors to Lumen’s existing renderer.
Choose **Lumen → Color palette → YouTube** in the browser toolbar or Extensions menu. Lumen’s
charcoal and mint remains the default. The choice updates open tabs and is remembered locally.
SponsorSkip’s switch and category settings move to the same popup; the player keeps segment marks
and transient skip/Undo feedback.

## Direction and audit

- Product: Lumen, a Rust YouTube client for watching and browsing.
- Character: familiar, restrained, readable.
- World: YouTube’s graphite surfaces and white controls.
- Moments: thumbnails and the video carry the rich color.
- Signature: red logo and playback progress.

The existing layout and motion already have a direction. This pass changes colors and the requested
settings location. The audit found one shared mint token serving brand, focus, progress and links;
using red for that entire token would overwhelm the interface. Native fallback colors also needed
to follow the selection. SponsorSkip settings occupied a permanent video-control button, and there
was no browser popup for either preference.

## Design passes

1. Spacing: popup padding 20 px, section separation 24 px, consistent 36 px category rows and 72 px
   palette choices. Existing page geometry stays shared between palettes.
2. Color: graphite surface ramp, three text roles, white focus/selection, red brand/progress and blue
   content links. Default semantic tokens alias the original mint values. Segment category colors retain
   their meanings. Text contrast is checked at WCAG AA on the surfaces where it appears.
3. Icons: the existing flat play mark and simple checkmarks, with fixed aligned boxes. No emoji or
   decorative badges.
4. Copy: **Color palette**, **Lumen**, **YouTube**, **SponsorSkip**, **Skip categories**, **Undo last skip**.
   The database credit remains **Segment data from SponsorBlock**.
5. Type: existing system font, 14 px control labels, 12 px supporting text, weights 400 and 600.
6. Layout: browsing, watch pages, player, comments and descriptions keep their existing structure.
   Only settings move into the browser popup.
7. Details: pressed-state palette/category buttons, a labelled switch, keyboard focus, aligned counts,
   persistent controls and an accessible save-error message. Failed saves roll back and can be retried.
8. Visuals: the product’s own thumbnails and video remain the main visual. No generated image or prompt.
9. Motion: shared client motion is preserved. The popup uses the existing press and switch timing;
   keyboard selection is instant, and reduced motion removes movement. No action waits for motion.

## Behavior and verification

Rust owns the markup, selection, preference patches, category masks and player messages. The popup
reuses the client Wasm; JavaScript adds generic extension-context, storage and message primitives.
Only `ytcConfigV1.theme = "youtube"` is added to the existing bounded preferences; the default is
omitted. No new permission, service, telemetry or stored page data.

Checks cover live changes across tabs, a new boot, reopening the popup, keyboard activation, AA text
contrast, unchanged player geometry/media identity and no new player request for the active video.
They also cover rejected and thrown storage writes, rapid edits, concurrent player/category preferences,
SponsorSkip counts and Undo, off with no new lookup, and an off/on cycle during a delayed lookup.
Each lookup owns its timeout, so an aborted request cannot cancel its replacement.

The interface was visually inspected in Chromium using signed-out fixtures. For automated popup checks,
Chromium’s headless action popup does not expose a Playwright page; the suite opens the exact manifest
popup document as an extension tab, with the YouTube tab active. Storage and tab messaging use the real
extension APIs. Final validation: public build 0.13.3; 277 Rust tests passed (5 manual tests ignored),
178/178 browser checks passed (3 development-only skips), and the public-build/whitespace checks passed.

Design process adapted from [Unslop](https://heydamir.design/unslop) by
[Damir Sokolovsky](https://x.com/heydamir), licensed under CC BY 4.0.
