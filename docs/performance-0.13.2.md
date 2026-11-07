# Interaction performance in 0.13.2

> A record of the release named in the title, when the product was called Lumen and had the charcoal and mint
> palette; since 0.14.0 it is Redline, with one graphite, white and red palette (docs/architecture.md).

This pass prioritizes input feedback, a usable destination page, the first video
frame, and responsive seeking. Picture quality, the charcoal/mint interface,
focus behavior, and motion remain the reference. Application decisions stay in
Rust; the generic browser bridge and both stylesheets are unchanged.

## Overlap the video-intent dwell with useful work

Previously, a hover or focused link waited for its VISIONOS response, then
started another 150 ms dwell before fetching media. The dwell now starts at
intent. The response and decoding-capability answers can arrive during it;
media starts when both the rest and the existing format checks are satisfied.
A capability answer retries a waiting rest. Leaving, another video intent, and
navigation invalidate the rest; re-entering starts a new one.

The same format ladder, capability checks, size cap, throughput rule, resume
position, download limits, and cancellation behavior still apply. The player
adopts completed or in-flight prefetches as before. Its warm first frame uses
the video's own resolution, with no thumbnail poster or retained decoder.

Direct proof uses the real public extension in Chromium with offline fixtures:

| Measurement | 0.13.1 | 0.13.2 |
| --- | ---: | ---: |
| Player response → first media request, three independent documents | 158 / 155 / 156 ms | 3 / 4 / 3 ms |
| Brief-hover click → first frame, median of six, 120 ms media latency | 149 ms | 76 ms |
| The same brief click with immediate local media, median of six | 35 ms | 32 ms |

The controlled response is held for 300 ms. For the click measurement, the
pointer stays on the link and clicks 60 ms after that response completes.
Both builds display the same 360p fixture; the configured latency applies
identically to both. The benefit is overlapping media work with the short rest;
this is not a claim that every video start halves. The response/dwell regression
fails on 0.13.1 and passes on the new build.

## Keep high-frequency hover work small

Seek preview now checks its allocation-free frame index before constructing a
storyboard cell and sheet URL. Frame boundaries, duration fallback, clamping,
DPR selection, and the displayed image remain unchanged. A 1,200-event smooth
sweep used 9,600 → 1,016 allocation calls; the isolated native routine measured
341 → 48 ns/event with matching frame URLs. In an every-event-changes-frame
stress case, allocations stay equal and CPU differs by about 1.4%; the cache
is intended for the common repeated-frame case.

A seek rest also keeps one pending timer. Pointer moves update its deadline
and latest target in Rust. Its wake waits any remaining rest time, rounded up;
leaving clears the deadline, and re-entry can reuse the pending wake. The
200 ms rest, cancelled downloads, track limits, and seek adoption are preserved.

In the actual extension, 240 pointer moves produced 240 timer installations and
callbacks on 0.13.1, versus 11 on the new build (10 at 4× CPU throttling). The
last tooltip time and storyboard frame matched. Input handler p90 was 0.4 ms
for both builds at 4× CPU; the confirmed gain is less timer and allocation work,
not a claimed improvement in that noisy timing sample.

The fixed UI and navigation spring curves are now borrowed static samples.
Tests compare every sample and duration with the original Rust generator and
the CSS tokens. This removes curve generation and string allocation from menu,
tooltip, chip, and navigation input paths without changing the animation.

## Borrow the outgoing snapshot

The prior serializer borrowed its input, but the caller still cloned the
header, URL, continuation, and up to 150 items immediately before encoding.
The outgoing snapshot now borrows the shown and buffered slices. Owned and
borrowed snapshots share one serializer. Exact JSON bytes, order, item/byte
caps, scroll normalization, validation, and storage behavior remain unchanged.

Full preparation plus encoding at 150 items:

| Measurement | 0.13.1 | 0.13.2 |
| --- | ---: | ---: |
| Native allocation calls | 1,332 | 9 |
| Native temporary peak | 200,183 B | 89,344 B |
| Wasm CPU | 0.203 ms | 0.175 ms |

This is an allocation and CPU improvement. The absolute desktop time saved is
about 0.03 ms, so no visible navigation-frame speedup is claimed for it. The
page's DOM preservation and restoration architecture is unchanged.

## Trim the player response without changing playback data

VISIONOS requests keep the complete `responseContext`, `playabilityStatus`,
`streamingData`, `captions`, and `videoDetails` subtrees. Every format and audio
track stays available. A refused mask (HTTP 400) immediately resends the same
body, headers, and fetch options without it, then disables that mask for the
rest of the document. The existing `next` mask is independent. Capability
warming also reuses the already-parsed JSON instead of parsing it again.

Twenty alternating whole/masked pairs on one persistent connection all
returned HTTP 200 with the same 32 formats and consumed metadata. Mean
compressed entity size was 14,189 → 7,855 B (45% less); decoded JSON was
81,664 → 58,137 B. TTFB p50/p90 was 89.8/138.0 → 90.3/138.6 ms; total p50/p90
104.4/140.2 → 98.3/140.5 ms. One masked 409 ms outlier increased its mean.
The byte saving is established; a cold-start latency improvement is not.

## Live playback and limits of wall-clock comparisons

A signed-out Chromium run alternated both builds across six public videos.
For a 350 ms hover before clicking, first-frame p50/p90 was 386/434 →
262/281 ms (six starts per build). All 48 cold, SPA, brief, and quick startup
trials played at the target quality with zero stalls during observation.

Other routes varied: the initial cold result was 501/534 → 680/808 ms. The
breakdown put most of that difference in googlevideo delivery. A follow-up
cold-only run alternated the old build, the new build, and the identical new
build with its outgoing mask stripped: 541/607, 524/552, and 538/793 ms,
respectively (six per variant). The slowdown did not reproduce. The paired
API check above also found no consistent mask penalty. CDN hosts change
between responses; these samples support no general cold or quick-start
speed claim.

Live interaction checks exercised real seeks and 720p → Auto changes. One
nature-video run hit a CDN HTTP 403 after a player refresh; a fresh interleaved
repeat completed those actions on both builds with no stalls. The successful
new-build nature repeat measured seeks at 176/56/44 ms (outside/inside/hovered)
and quality changes at 124/148 ms. Those single-video values are functional
proof, not comparative speed claims.

The full regression initially exposed a test counter that included visitor
requests intentionally made by the new earlier recovery tests. It now counts
only the current happy-path navigation, retaining the strict zero-recovery
assertion. Recovery fault injection is limited to its target video, so an
unrelated speculative request cannot change the document's bot-check hold.

## Repeat the checks

```sh
./build.sh
npm run check:public
npm test
cargo test --release --locked snapshot_preparation_benchmark -- --ignored --nocapture
YTC_ONLY='own player: media intent dwell overlaps' npm run test:e2e
```

The signed-out live tool adds `brief`: a link clicked after 350 ms of hover.
Multiple extension folders alternate order per video. These requests reach
YouTube and should be run manually and sparingly:

```sh
node tools/bench/video.cjs --ext /path/to/before,/path/to/after \
  --label before,after --runs 1 --scenarios cold,spa,brief,quick --starts
```

Timing comparisons must use the same fixtures, screen, machine, build profile,
and alternating order. Network and CDN responses can move independently of the
client; report the scenario and sample count with a wall-clock result.

## Interface audit

- Product: YouTube browsing and playback through Lumen.
- Character: calm, legible, responsive.
- World: a dark cinema screen with quiet controls.
- Moments: thumbnails and the playing picture carry the color.
- Signature: the mint focus frame makes the next remote action clear.

The existing interface passes the audit. This work preserves its spacing,
color, icons, copy, type, layout, details, imagery, and motion. The audit follows
[unslop by Damir Sokolovsky (@heydamir)](https://x.com/heydamir), CC BY 4.0;
interaction changes follow the project's [motion law](motion.md).

## Final verification and decisions

- `npm test`: 276 Rust tests passed, five manual checks ignored; 173/173 browser
  checks passed, with three development-only checks skipped by the public build.
- Public extension, TV public, and TV development builds succeeded;
  `npm run check:public` passed for 0.13.2.
- TV player checks passed at 960×540 and 1920×1080. The seek preview and quality
  panel captures were inspected against the existing interface.
- One sol-loop round with `gpt-6.1-sol` on `max` approved the current behavior
  without findings or requested changes. Review token usage was not returned.

The implementation retains the existing quality floor, HDR/capability rules,
privacy/storage bounds, decoder lifetime, and retry policy. It keeps the DOM
restoration architecture and measures its smaller snapshot cost honestly.
This advances the shared Rust client to 0.13.2; the TV shell stays at 0.4.0,
and this work does not publish a production APK or TV update release.
