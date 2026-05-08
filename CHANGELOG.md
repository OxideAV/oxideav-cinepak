# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Round 7: **empty-cluster slot reclamation**
  (`EncoderOptions::stale_slot_threshold`). The stateful
  `CinepakEncoder` now tracks a per-slot staleness counter for each
  V4/V1 codebook flavour: incremented every inter frame the slot is
  not referenced by any macroblock, reset when it is. When a slot's
  counter exceeds `stale_slot_threshold` (default `Some(8)`; `None`
  disables), the encoder reseeds it from the strip's
  highest-residual sample MB (its distance to its nearest seed
  centroid) before Lloyd refinement, then forces a full-replace
  codebook chunk so the decoder sees the reclaimed slot value. Closes
  the long-running cross-frame-persistence "frozen slot" issue where
  an unreferenced slot would survive forever, holding stale content
  the codebook could no longer use. Telemetry exposed via
  `CinepakEncoder::last_frame_stats() -> FrameStats` (reclaimed-slot
  counts per flavour, forced-full-chunk count). Strict-greater-than
  comparison on the threshold means the round-5 8-frame slow-pan
  fixture runs all the way through with persistence active without
  triggering reclamation on the final frame; on a content-cut fixture
  with threshold=2, ~89 slots reclaim across V4+V1 on the cut frame,
  bringing pixel fidelity back to a bounded MAE within 1 frame.
  Per-frame (not per-strip) staleness increments — multi-strip frames
  don't bump the counter once per strip.
- Round 7: **adaptive bisection tolerance**
  (`TwoPassRateControl::encode_at_target_window_bytes_adaptive`).
  Couples the windowed bisection's "are we drifting?" tolerance to
  the running stdev of the prior `window_size` frames' actual byte
  counts: tight on stable scenes (low byte-size variance ⇒ tolerance
  shrinks toward `tolerance_pct_min`), loose on scene changes /
  wipes (high variance ⇒ tolerance grows toward
  `tolerance_pct_max`). `variance_scale_pct` is the stdev-pct above
  which tolerance saturates at the upper bound (default test value
  `25.0` — a common scene-change variance). Equal-bound input
  reproduces the round-6 fixed-tolerance behaviour exactly. First
  two frames use `tolerance_pct_max` (no meaningful stdev yet).

- Round 6: **windowed bisection rate control**
  (`TwoPassRateControl::encode_at_target_window_bytes`). Targets a
  byte budget over a rolling N-frame window rather than per-frame
  (real bitrate-throttled workloads track rolling average, not
  per-frame size). Holds quality steady when the projected window
  sum is within ±tolerance of target; otherwise runs a binary search
  (≤ 8 trials per re-eval) over `q ∈ [0, 100]` for the next frame.
  Composes with round-5's `TwoPassRateControl` (same struct, new
  method) and the existing per-frame
  `encode_at_target_bytes`/`stats_pass`. Empty input returns `Ok([])`;
  starvation-budget paths fall back to `q=0` with positive
  `byte_delta`.
- Round 6: **tighter Lloyd refinement** for the cross-frame codebook
  warm-start (`EncoderOptions::lloyd_max_iter` + `lloyd_eps`). The
  Lloyd loop now iterates up to `lloyd_max_iter` reassignment+update
  passes against the *current* (not seed) centroids, with early stop
  when the largest per-slot Manhattan drift falls to `≤ lloyd_eps`.
  Slot identity is preserved across iterations, so chunk-omission
  / selective-update wins downstream are unaffected. Defaults: 2
  iterations, eps = 1 (round-5 single-pass behaviour preserved by
  setting `lloyd_max_iter = 1`; cold-start by `lloyd_max_iter = 0`).
- Round 6: **ffmpeg-emitted Cinepak-on-AVI roundtrip fixture**
  (`tests/ffmpeg_avi_roundtrip.rs`). Encodes a 64×64 RGB24 frame
  through `ffmpeg -c:v cinepak -f avi -`, parses the resulting AVI's
  `movi/00dc` payload, and decodes it through this crate to assert
  PSNR ≥ 24 dB versus the source. Catches decoder regressions
  against the canonical ffmpeg encoder output without shipping
  binary fixtures. Skips gracefully when ffmpeg is unavailable, the
  cinepak encoder is missing from the local build, or
  `OXIDEAV_SKIP_FFMPEG_TESTS` is set. Observed PSNR locally: 36.9 dB.

- Round 5: **cross-frame codebook persistence at the median-cut
  training step**. `CinepakEncoder::encode_inter` now warm-starts the
  median-cut quantiser with the prior frame's codebook centroids: each
  freshly-sampled vector is assigned to the slot of its nearest seed
  centroid (one Lloyd-style refinement pass), and that slot's new
  centroid is the average of the vectors that landed there. Slots
  with no incoming vectors retain the seed centroid byte-identical,
  which lets the chunk-omission / selective-update path keep firing
  on slow-pan content where most macroblocks shift but the codebook
  population is roughly stable. The seed is also valid across strips
  of the same frame, so multi-strip inter frames inherit the prior
  strip's codebook by default. Toggle via
  `CinepakEncoder::set_cross_frame_codebook_persistence(bool)`
  (default `true`); on a 32×32 slow-pan fixture (8 inter frames at
  q=50) persistence-on shrinks the cumulative inter-frame wire size
  by 5.3% (2807 B vs 2965 B).
- Round 5: **multi-strip inter selective-update verification**. New
  `tests/round5_persistence_and_multistrip.rs::multi_strip_inter_selective_update_beats_full_replace`
  drives a 64×64 4-strip inter encode chain on slow-pan content and
  asserts the stateful selective-update + cross-frame-persistence
  path beats the stateless full-replace path on cumulative wire size
  (observed: **44.0% wire savings** — 4358 B vs 7788 B for 6 inter
  frames). A second test
  (`multi_strip_inter_static_fixture_chunk_omission_across_strips`)
  asserts that on a static 4-strip fixture the inter frame emits
  zero codebook chunks (chunk-omission inheritance across strips).
- Round 5: **two-pass rate control** (`TwoPassRateControl` /
  `RateControlledFrame`). Pass-1 (`stats_pass`) records per-frame
  byte counts at a reference quality; pass-2 (`encode_at_target_bytes`)
  searches an 11-point quality grid for the largest `q` whose
  per-frame byte count is `≤ target_bytes`, falling back to `q=0`
  with a positive `byte_delta` when nothing fits. Per-frame search
  cost is O(N × 11) replays.
- Round 5: **`0x3100` selector-spillover decoder bug fix** + 4 new
  payload-roundtrip regression tests in
  `tests/inter_payload_straddle.rs`. The decoder previously read 1
  index byte for a deferred-V1/V4 placeholder mb in the *current*
  group and never read the rest of its bytes from the *next* group
  (per spec §3.3 step 5 the index bytes belong wholly to the next
  group). The fix defers the placeholder push until step 1 of the
  next iteration so its bytes are read with the correct group;
  static-fixture / mixed-V1+V4-+Skip patterns that triggered this
  now round-trip cleanly.

- Round 4: **selective-update codebook chunks on inter strips**
  (`CinepakEncoder` stateful struct). The encoder tracks the rolling
  V4/V1 codebook the decoder will hold across strips and frames, and
  for each inter strip emits a `0x2100` / `0x2300` selective-update
  chunk (only changed slots) when smaller than the equivalent
  `0x2000` / `0x2200` full-replace — or omits the codebook chunk
  entirely (spec §3.4: "header-only / omitted = inherit previous
  strip's codebook") when the previous codebook already has the
  correct values for every slot the strip's macroblocks reference.
  Free-function entry points (`encode_rgb24` / `encode_rgb24_inter` /
  `encode_gray8`) are unchanged — they remain stateless and always
  emit full-replace. Public surface: `CinepakEncoder::new` /
  `::reset` / `::encode_intra` / `::encode_inter`.
- Round 4: **multi-frame static-fixture wire-size validation test**
  (`tests/static_fixture_multi_frame.rs`). Drives `CinepakEncoder`
  with one intra keyframe + five identical inter frames and asserts:
  (a) every frame round-trips through `CinepakDecoder` within the
  codec's quantisation tolerance, (b) total inter-frame wire size is
  strictly smaller than the equivalent stateless `encode_rgb24_inter`
  run (observed: 91.6% wire savings — 250 B vs 2990 B for 5 inter
  frames at quality 50), (c) the SKIP-MB fraction grows / saturates
  across frames (observed: 64 / 64 macroblocks SKIP'd from frame 1
  through frame 5 once the rolling codebook stabilises).

### Round 3

- Round 3: **multi-strip encoder** (`EncoderOptions::strip_count`).
  Splits the frame into N horizontal bands and runs the median-cut
  quantiser independently per strip, so each band gets its own V1+V4
  codebook pair tuned to its local pixel population. Round-trips
  through the decoder with the y-sentinel rule (first strip carries
  absolute coords; subsequent strips ride on the previous strip's
  `y_bottom`).
- Round 3: **inter-frame encoder** (`encode_rgb24_inter`). For each
  macroblock, computes per-pixel MSE against the same-position 4×4
  block in the previous reconstructed frame; below
  `EncoderOptions::skip_threshold` the macroblock is emitted as a
  `0x3100` SKIP code, otherwise a V1/V4 update is selected by the
  same nearest-neighbour rule as intra. Strips are tagged
  `STRIP_ID_INTER` (`0x1100`) and the vector chunk is `0x3100`.
- Round 3: **PSNR-driven quality knob**
  (`EncoderOptions::from_quality(q)`, `q ∈ 0..=100`). Maps to codebook
  size (8..=256, log scale), strip count (1..=4), and skip threshold
  (256.0..=16.0, exponential). One call covers the full quality–wire-size
  trade-off curve without hand-tuning each knob.
- Round 3: **black-box behavioural verification against FFmpeg**
  (`tests/ffmpeg_psnr.rs`). Encodes a synthetic 320×240 RGB24 gradient
  fixture with `quality = 50`, packages the frame in a minimal AVI
  container, decodes via system `ffmpeg` (treated as an opaque CLI
  oracle), and asserts PSNR ≥ 28 dB versus the source. Observed PSNR
  ≈ 30.1 dB. Test gracefully skips when `ffmpeg` is unavailable or
  `OXIDEAV_SKIP_FFMPEG_TESTS` is set.
- Round 2: synth-test coverage for the four selective-update codebook
  chunk types FFmpeg never emits (`0x2100` V4 YUV update, `0x2500` V4
  grayscale update, `0x2700` V1 grayscale update; the existing
  `0x2300` already had a regression test). Spec §4.4 documents these
  as "wire-legal but encoder-optional"; the corresponding decode path
  was already in round 1 but lacked dedicated test coverage.
- Round 2: median-cut codebook-quantiser **encoder** (`encode_rgb24` /
  `encode_gray8`). Single-strip intra only, configurable per-codebook
  entry count (default 64+64, matching FFmpeg `-q:v 10`). Self-roundtrip
  tests confirm pixel-correctness within codec quantisation tolerance
  (±10 on RGB, ±8 on luma).
- Round 2: **Sega FILM (CPK) demuxer** (`FilmDemuxer::parse`,
  `probe_film`). Parses the FILM header + FDSC + STAB chunks per spec
  §2 of `05-container-carriage.md`. Supports both the standard 32-byte
  FDSC layout (FFmpeg `film_cpk` muxer) and the abbreviated 20-byte
  layout (early Sega CD variants). Sample-record helpers
  (`is_keyframe`, `is_audio`, `timestamp_ticks`) decode the
  `sample_info_1` discriminator per §2.4.
- Round 2: **probe function** for the `CVID` FourCC registration.
  Validates the 10-byte frame header + 12-byte first-strip header
  structure (flags, width/height alignment, strip_id 0x1000/0x1100,
  size accounting). Returns 1.0 on structural validity, 0.5 without
  packet bytes, 0.0 on failed checks.
- Round 2: `encode_selective_chunk` helper for synthesising
  `0x2100`/`0x2300`/`0x2500`/`0x2700` chunks; used by the
  selective-chunk encode-roundtrip test.

- Round 1 clean-room rebuild from `docs/video/cinepak/spec/`. Decode
  path covers:
  - 10-byte frame header parse (`flags`, 24-bit `frame_length`,
    `width`, `height`, `strip_count`).
  - 12-byte strip header parse with the Y-coordinate sentinel rule
    (`y_top == 0` on non-first strips means "starts at previous
    `y_bottom`; the wire `y_bottom` is the strip's height").
  - Codebook chunks `0x20xx` / `0x21xx` / `0x22xx` / `0x23xx` (12-bit
    YUV V4/V1 full + selective) and `0x24xx` / `0x25xx` / `0x26xx` /
    `0x27xx` (8-bit grayscale equivalents).
  - Vector chunks `0x3200` (V1-only intra), `0x3000` (mixed V1/V4
    intra), `0x3100` (inter with `0` skip / `10` V1 / `11` V4 codes;
    selector-bit spillover across flag-word boundaries handled).
  - V1 quadrant expansion and V4 sub-block expansion of 4×4
    macroblocks, plus YUV → RGB matrix with truncation-toward-zero
    on `U / 2` and `[0, 255]` per-channel clamp.
  - Codebook inheritance across strips and across frames.
  - Skip macroblocks copy from the previous frame's reconstructed
    pixel buffer.
- Public surface:
  - Standalone API: `CinepakDecoder`, `CinepakFrame`, `CinepakPlane`,
    `CinepakPixelFormat`, `CinepakError`, `Result`.
  - Framework API (default-on `registry` feature): `register_codecs`,
    `register`, `__oxideav_entry`. Claims FourCC `CVID`.
- Default-on `registry` cargo feature gates the `oxideav-core` dep so
  image-library consumers can depend on `oxideav-cinepak` with
  `default-features = false` for a no-`oxideav-core` build.

### Changed

- Clean-room rebuild from a fresh orphan `master`. The previous
  implementation was retired by the OxideAV docs audit dated
  2026-05-06; the prior history is preserved on the `old` branch and
  is **not** an input for this rebuild.
