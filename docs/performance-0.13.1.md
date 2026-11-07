# Rust performance pass: 0.13.1

> A record of the release named in the title, when the product was called Lumen and had the charcoal and mint
> palette; since 0.14.0 it is Redline, with one graphite, white and red palette (docs/architecture.md).

This pass compares the Rust client with `a9110e9` (0.13.0). It removes work from
JSON parsing, page snapshots and MP4 quality switches. The interface, motion
tokens, media-quality choices, request policy and storage rules are unchanged.

## Measurements

These are CPU measurements of the named operations, not claims about network
latency or the time to load a whole page. Both versions used Rust 1.99.1,
`wasm32-unknown-unknown`, `opt-level = "z"`, fat LTO, one codegen unit and the
default scalar target. The Wasm modules ran in Node 24.22.1 / V8, alternating
the old and new versions after warmup.

| Operation | Before | After | Method |
|---|---:|---:|---|
| Parse/drop Search JSON, 754,834 bytes | 1.297 ms | 1.056 ms | Median of 41 batches of 20 parses |
| Parse/drop watch metadata, 442,219 bytes | 0.804 ms | 0.660 ms | Same |
| Serialize a 150-item snapshot | 0.466 ms | 0.146 ms | Median of 12 batches of 300 calls |
| MP4 switch assembly/coverage, 4.07 MB segment, 16 KiB chunks | 0.226 ms | 0.061 ms | Median of 7 batches of 100 segment runs |
| MP4 bytes retained for that switch | 4,194,304 B | 8,192 B | Actual vector capacities |

All nine recorded JSON fixtures improved by about 15–20% in the Wasm run.
Their complete serialized JSON checksums and root lengths matched. The
150-item snapshot was byte-identical (87,089 bytes). A separate native
allocation check reduced snapshot allocations/reallocations from 7,262 to 9,
and temporary peak allocation from 734,290 to 89,344 bytes.

### JSON objects

Unsorted object members used to be sorted, moved into a second vector, and
then deduplicated. They now deduplicate in place after the same stable sort.
The last duplicate value still wins. Shrinking the vector preserves the
previous retained-memory footprint; the faster variant that kept excess
capacity was rejected because it increased that footprint by 7–12%.

### Snapshots and ready Home

Borrowed Serde serializers write the existing compact schema directly.
They avoid a temporary `serde_json::Value` tree and copies of every title,
URL and metadata string. Field order, omissions, nulls, escaping, numeric
handling and item limits remain the same. Existing decoders and validation
are unchanged. The direct Serde dependency uses the version already in the
lockfile, with `std` and without derive macros.

### MP4 quality switches

Once a complete `moof` arrives, a switch retains sample byte boundaries and
cumulative durations. Later chunks use the existing assembler's contiguous
byte count to determine coverage. The copied media payload is released.
The switching threshold, appended bytes and first usable sample remain the
same. Ordinary playback and WebM retain their existing paths.

Every byte prefix of three recorded public MP4 segments was compared with
the original coverage reader: 4,935,256 matching prefixes. Focused tests also
cover default sample fields, signed offsets, zero durations, payload release
and reset at a new segment.

## Repeating the checks

Current-code native CPU benchmarks use the repository's signed-out fixtures:

```sh
cargo test --release --locked json::tests::benchmark_response_parsing -- --ignored --nocapture
cargo test --release --locked serialization_benchmark -- --ignored --nocapture
```

Timing is reported rather than asserted. Compare builds in alternating rounds
on the same machine. The original/new Wasm proof used separate modules and
asserted equal outputs before measuring; MP4 measurements include the actual
assembler, copies and coverage state, rather than only a table lookup.

The existing browser timing table is also available:

```sh
YTC_TIMING_ONLY=1 npm run test:e2e
# YTC_EXT=/path/to/another/public/extension selects another build.
```

One fixture sanity run (150 ms simulated InnerTube latency) reported Home
first results at 188 → 180 ms and reload at 192 → 186 ms. These small wall-time
differences include browser scheduling noise; the isolated CPU measurements
above are the evidence for the optimization.

The full-suite restore check also isolates the independently tested ready-Home
prefetch. Its previous request counter could count a background `HOMECONT1`
as a restoration request; the same failure was reproduced on 0.13.0. The strict
zero-request and scroll-restoration assertions remain in place, and the test
restores its previous prefetch flag in `finally`.

## Interface and motion audit

- Product: YouTube browsing and playback through Lumen.
- Character: calm, legible, responsive.
- World: a dark cinema screen with quiet controls.
- Moments: thumbnails and the playing picture carry the color.
- Signature: the mint focus frame makes the next remote action clear.

The existing design passes the audit, so this pass preserves its spacing,
color, icons, copy, type, layout, details, visuals and motion. No new image or
decorative treatment is needed. Runtime proof uses the real extension in
Chromium: the offline regression suite and TV visual checks, plus signed-out
live video playback, seeks and quality switches. Public builds still exclude
diagnostic telemetry.

The visual audit follows [unslop by Damir Sokolovsky (@heydamir)](https://x.com/heydamir),
CC BY 4.0; motion follows the project's [motion law](motion.md).

## Validation and review

- `npm test`: 272 Rust tests and 170/170 browser checks passed. Three manual
  Rust checks and three development-only browser checks are skipped by that
  normal public-build run.
- Both manual CPU benchmark commands above passed separately.
- Focused TV keyboard/player visual checks passed at 960×540 and 1920×1080;
  the current screens were inspected against the baseline.
- The signed-out live video benchmark passed ten public videos with zero
  stalls, including seeks and quality changes.
- `npm run check:public` passed for the generated 0.13.1 public build.
- Two sol-loop rounds with `gpt-6.1-sol` on `max` approved the changes with
  no actionable findings. No application changes were requested by review;
  the second round covered the restore-test isolation. Review token usage
  was not returned.

The implementation keeps the existing size-oriented Rust build profile and
uses the memory-flat parser variant. The TV shell stays at 0.4.0: this change
advances the shared client, with no new shell format or production release.
