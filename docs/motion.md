# Redline motion

(The same text is the agent skill `.claude/skills/lumen-motion/SKILL.md`; keep both in step.)

The client should feel silky, alive and instantaneous. The point is speed you can feel; motion is how
speed looks, never a cost on top of it.

## The one law

**Motion never delays anything.** Every result is in the DOM, final and usable on the first frame.
Animation is a visual layer riding on top of it.

- Feedback starts on pointerdown (`:active`), in the same frame.
- Nothing waits for an animation to end: no `animationend`/`transitionend` gating, no timers before
  content, no `await` inside a swap.
- New input interrupts and retargets from the current on-screen value (CSS transitions do this;
  keyframes do not, so keyframes are only for one-shot entrances that nothing can interrupt).
- Exits overlap entrances. An exit is faster than its entrance and never holds the next state back.
- Keyboard actions, shortcuts, focus moves, back/forward and restored pages are instant: no motion.
- No extra frame on any input-to-content path. That rules out `document.startViewTransition` for
  page changes (it paints the new page one frame late and covers it with the old snapshot while
  fading, and blocks pointers unless overridden). Use the FLIP pattern below instead.
- If a measurement shows a path got slower, the animation goes, not the speed.

## Perception rules

- Curves: `--e-out` for enter/exit and most UI, `--spring` (critically damped) for things that move
  and get retargeted, `--e-io` only for an on-screen move between two resting places. Never ease-in.
  `--spring-bounce` only after a gesture that carried momentum (a flick or drag release).
- Durations: press 100–120 ms, hover-out 120–160 ms, popovers 150–200 ms in / ~110 ms out, panels
  200–260 ms, morphs up to the spring's settle time (most of the motion lands in the first 40 %).
- Hover: instant in, eased out. Write `transition` on the base state and `transition-duration:0s` on
  `:hover`, so the pointer gets an immediate answer and leaving looks smooth. Gate scale/movement
  hovers with `@media (hover:hover) and (pointer:fine)`.
- Properties: `transform` and `opacity` only (plus `background-color`/`color`/`border-color` for
  color feedback, `filter` ≤ 4px blur sparingly). Never `transition: all`. Never animate
  width/height/top/left/margin/padding/box-shadow (use `scale`, `translate`, a pseudo-element's
  opacity for shadows).
- Never from `scale(0)`. Enter from `scale(.96–.98)` + `opacity:0`. Press: `scale(.97)` (big
  surfaces like cards `.985`).
- Spatial continuity: a popover grows from its trigger (`transform-origin` at the trigger edge) and
  shrinks back into it; a video thumbnail grows into the player; whatever enters from an edge leaves
  by the same edge. Modals stay centered.
- **A lot of motion, by design** (2 November 2026: as many animations as possible without interfering
  with speed, motion morphed with the UI so that forms change, at the highest quality). So the generic "frequency → less motion" restraint is overridden for pointer interactions:
  hover, press, open/close, selection, state changes and content arriving should all visibly move,
  and shapes should morph (a button becomes its panel, a selection pill slides between chips, a
  control changes form with its state, the search bar widens on focus). What stays motionless is
  only what costs speed or reads as lag: keyboard actions/shortcuts, typing, scrolling, back/forward,
  restored pages, and numbers being read. Fast still beats slow: keep durations in budget.
- **Morph, don't swap.** Prefer one element changing form (size via `scale`, shape via
  `clip-path`/`border-radius` on a persistent element, position via `translate`) over one element
  disappearing and another appearing. Persistent indicators (a sliding pill/underline) are moved
  with `transform` computed once from the target's offset (read at the click, never per frame).
- **Pixel precision is part of motion quality.** Every icon is centred in its control (an e2e check
  measures icon-only buttons ±1 px); inline SVGs in buttons are `display:block` inside a flex box.
- Reduced motion keeps gentle opacity/color fades and drops movement: every distance or scale delta
  is multiplied by `var(--mv)` (0 under `prefers-reduced-motion`), e.g.
  `transform: translateY(calc(6px * var(--mv))) scale(calc(1 - .03 * var(--mv)))`.

## The client's architecture and the four patterns

Rust (Wasm) renders HTML strings and swaps them with `innerHTML` through `browser/bridge.js`
primitives (`bridge::html`, `append`, `attr`, `class_all`, `css_var`...). A swapped element is a new
element: a transition on it has no "before" state. So:

**(a) State changes on persistent elements → class/attribute toggles.** Keep the element, flip a
class or `aria-*` attribute from Rust (`bridge::attr`, `bridge::class_all`), and let a CSS
transition animate it. Player controls, switches, the Watch-later bookmark, chips' pressed state and
popover open/close all work this way. Never re-render a control to change its state if a toggle
will do; re-rendering also kills hover and running transitions.

**(b) Content inserted by `innerHTML` → `@starting-style`.** Declare the resting style with a
transition and its entry style inside `@starting-style { ... }`; the browser animates it on
insertion with no JS and no frame of delay. Use only where entrance motion helps (popover panels,
toasts, appended comment pages, the "new videos" pill), never on whole pages on every navigation.
Stagger only first-time group entrances, 20–40 ms per item, capped at ~6 items, never blocking.

**(c) Shared-element morph (FLIP) instead of view transitions.** For thumbnail → player and similar:
measure the source rect before the swap, swap synchronously, then animate the destination from the
source rect with a compositor-only `element.animate()` transform (a bridge primitive; the keyframes
and the spring easing string come from Rust, computed once). The destination is already final and
interactive at frame one; only its paint is transformed. Skip it for keyboard, back/forward,
restored pages and when the source is off-screen.

**(d) Springs are computed once, in Rust.** `src/motion.rs` turns a spring
(response, damping) into a CSS `linear()` easing plus its settle duration; `extension/client.css` carries the
generated tokens (`--spring`, `--d-spring`, `--d-spring-soft`, `--spring-bounce`), and a Rust test
fails if they drift. Rust never drives an animation frame by frame across the bridge, and never
reads layout in a hot path (a forced layout during a scroll restore moves the page). `element.animate`
is used only where CSS cannot express it (FLIP, retargeting a running morph).

Any new effect that exists in another language or library (JS animation libraries, springs) gets
reimplemented in Rust + CSS this way. No animation library is added.

## Style

Graphite, white and red: background `#0f0f0f`, panels `#181818`/`#212121`, the accent white `#f1f1f1`, red `#ff0033`
only for the brand and playback. Motion is
calm and precise: no bounce on menus, no parallax, no looping decoration, no spinners (skeletons in
the final geometry instead), no motion on information the user is reading (durations, counts,
progress text). Popovers: soft shadow, 12 px radius, grow out of the trigger's own shape: only their surface
(`::before`) is clipped from the trigger's box (`bridge::morph_vars`) to the panel, never the panel itself,
because clip-path also clips hit-testing (rows must take clicks on the opening frame).

## Gotchas learned

- A "no motion" class (`.now`) must be in place before the change it governs. Focus is the trap: the browser
  recomputes the focused style while focus moves, so a class flipped in a `focusin` handler is one style late.
  Flip it on keydown/pointerdown instead (`#ytc-search.now`).
- A layout read (`morph_vars`, `rect`) after `innerHTML` gives the new content its first style right there:
  `@starting-style` rules keyed on a class you add afterwards never apply. Read first, then write, then flip.
- A morph never scales a picture past its own resolution (3 Nov 2026, the owner on the watch-open zoom: "the
  preview in super bad quality"). The thumbnail → player zoom grows the video's own first frame, decoded while
  the pointer or focus rested on the link (`player/own/warm.rs`), or the dark player surface until the first
  frame; never the 320 px card thumbnail stretched to the player.
- A clipped surface that outruns its content: drive the clip with `--e-out` (front-loaded) and delay the rows'
  fade a little, so content never shows outside the surface. Check with slowed screenshots
  (`Animation.setPlaybackRate`), not by eye at full speed.

## Checklist before you ship motion

1. Does any content, click target or keyboard path wait one more frame than before? Then remove it.
2. Can it be interrupted mid-way and retarget smoothly (hover in/out quickly, click twice)?
3. Only `transform`/`opacity` (+ color) animate? No `transition: all`?
4. Ease-out or spring, never ease-in; exit faster than entrance; nothing from `scale(0)`.
5. Grows from and returns to its origin?
6. Reduced motion: movement multiplied by `--mv`, fades kept?
7. Measured: the e2e timing lines (`SPA click → watch page rendered`, `first results`, `first frame`,
   Home preload/scroll/reload timings) are not worse; no long frames during the motion.
