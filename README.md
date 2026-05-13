# oxideav-cinepak

Pure-Rust Cinepak (CVID) video decoder for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Rounds 1 + 2 + 3 + 4 + 5 + 6 + 7 + r47-encoder-RDO + r4-LBG — clean-room rebuild from `docs/video/cinepak/spec/`.**
The prior implementation was retired by the OxideAV docs audit dated
2026-05-06; the rebuild replaces it from public reverse-engineering
references (multimedia.cx wiki, Tim Ferguson's `videocodec/cinepak.txt`,
US patent 5,467,413, behavioural observation of system FFmpeg as a
black-box CLI). FFmpeg source is not consulted at any phase.

Round 1 landed a full clean-room decoder. Round 2 added a
median-cut-quantiser **encoder** (V1+V4 codebooks, intra only), the
**Sega FILM (CPK) demuxer** for the non-deviant `film_cpk` variant,
synth-test coverage for the four selective-update codebook chunk types
the FFmpeg encoder never emits (`0x2100` / `0x2300` / `0x2500` /
`0x2700` — spec §4.4), and a **probe function** wired into the codec
registry for `CVID` FourCC disambiguation. Round 3 extended the
encoder with **multi-strip** output + per-strip codebook adaptation, an
**inter-frame encoder** with skip-MB selection (per-pixel MSE
threshold), a **PSNR-driven quality knob** (`EncoderOptions::from_quality(q)`
mapping `q ∈ 0..=100` to codebook size + strip count + skip threshold),
and a **black-box behavioural-verification test** that wraps an
encoder-emitted frame in AVI, decodes it through system `ffmpeg`, and
asserts PSNR ≥ 28 dB versus the source (≈ 30 dB on the synthetic 320×240
gradient fixture). Round 4 added a stateful **`CinepakEncoder`** that
tracks the rolling V4/V1 codebook across strips and frames so inter
frames can emit `0x2100` / `0x2300` **selective-update** codebook
chunks (only changed slots) — or omit the chunk entirely when the
previous codebook is already correct for the strip's referenced slots.
On a static fixture the round-4 path drops 91.6% of the inter-frame
wire bytes versus the stateless round-3 `encode_rgb24_inter` helper.
Round 5 added **cross-frame codebook persistence** at the median-cut
training step (Lloyd warm-start with prior centroids; preserves slot
identity for chunk-omission), **multi-strip inter selective-update**
verification (44.0% wire savings on slow-pan 4-strip content vs the
stateless full-replace path), a **two-pass rate control** wrapper
(`TwoPassRateControl` — grid-search on quality knob to hit a
target byte budget per frame), and a fix for a **`0x3100`
selector-spillover decoder bug** uncovered by the new
multi-strip-inter wire patterns. Round 6 added a **windowed bisection
rate control** path (`encode_at_target_window_bytes` — rolling N-frame
budget with binary search instead of grid search), **tighter Lloyd
refinement** (`EncoderOptions::lloyd_max_iter` + `lloyd_eps` knobs;
multi-pass with eps-based early stop), and an **ffmpeg-emitted Cinepak
AVI roundtrip fixture** that drives a known-good ffmpeg-encoded frame
through our decoder (PSNR ≈ 36.9 dB locally; skips when ffmpeg or its
cinepak encoder is unavailable). Round 7 added **empty-cluster slot
reclamation** (`EncoderOptions::stale_slot_threshold`; per-slot
staleness counters; reseed-from-high-residual + forced full-replace
chunk on reclamation) — closes the cross-frame-persistence
"frozen slot" issue where a stale slot would survive forever holding
content the codebook can no longer use; also added
**adaptive-tolerance windowed bisection**
(`TwoPassRateControl::encode_at_target_window_bytes_adaptive` —
couples bisection tolerance to the running stdev of prior frame
sizes, tighter on stable scenes / looser on cuts) and a
**`CinepakEncoder::last_frame_stats() -> FrameStats`** telemetry
accessor for reclamation-count debugging.
Round-47 (encoder-RDO) added **Lagrangian V1/V4 rate-distortion
selection** (`EncoderOptions::rdo_lambda`, default `Some(5.0)`):
per-MB V1-vs-V4 decisions now compute pixel-domain Y SSE for both
candidate reconstructions and apply `D + lambda · R` with a 24-bit
rate delta favouring V1, replacing the round-2 codebook-distance
comparison that under-utilised V4's per-sub-block fidelity. PSNR_Y
lifts by **+1.10 dB** on the 64×64 gradient (now 36.69 dB,
essentially parity with ffmpeg's reference 36.9 dB on this fixture)
and **+0.43 dB** on the 320×240 gradient at q=50, at +9% and +26%
wire respectively. Round-47 also added a **per-frame strip-count
picker** (`encode_rgb24_best_strips`) — trial-encodes the input at
each supplied strip count and returns the bitstream with the lowest
Lagrangian cost, **breaking 38 dB PSNR_Y on the 320×240 gradient**
in a single intra frame (38.17 dB at 8568 B, vs the fixed-2-strip
default's 35.61 dB at 8548 B).
Round 4 added **Linde-Buzo-Gray (LBG) split refinement**
(`EncoderOptions::lbg_max_passes`, default `8`) — after the
median-cut + Lloyd warm-build, iteratively split the highest-distortion
codebook slot into the lowest-population slot and re-Lloyd until no
further split improves total SSE (Linde-Buzo-Gray 1980, IEEE TComm
28(1) — published VQ-design math, no proprietary source consulted).
PSNR_Y lifts by **+1.16 dB** on the 64×64 gradient
(`encode_rgb24`: 36.69 → 37.85 dB), **+2.19 dB** on the 320×240
gradient (35.61 → 37.81 dB), and **+0.85 dB** on a pure-LCG-noise
64×64 fixture (21.54 → 22.39 dB). Combined with the round-47
strip-picker the 64×64 gradient now reaches **40.77 dB Y at 2689 B**
— a +3.87 dB lead over ffmpeg's reference encoder on the same
fixture.

The previous on-disk history is preserved on the `old` branch; it is
**not** an input for this rebuild.

## What's implemented

Decode side, end-to-end:

- 10-byte frame header (`flags`, 24-bit `frame_length`, `width`,
  `height`, `strip_count`).
- 12-byte strip header with Y-coordinate sentinel rule for
  multi-strip frames.
- Full and selective codebook chunks for both V4 (12-bit YUV) and V1,
  plus the 8-bit grayscale variants
  (`0x2000` / `0x2100` / `0x2200` / `0x2300` /
  `0x2400` / `0x2500` / `0x2600` / `0x2700`).
- Vector chunks `0x3200` (V1-only intra), `0x3000` (mixed V1/V4 intra),
  `0x3100` (inter with skip codes — handles selector-bit spillover
  across flag-word boundaries).
- V1 quadrant macroblock expansion, V4 sub-block macroblock expansion.
- Codebook inheritance across strips and across frames (header-only
  chunk = "reuse previous codebook").
- YUV → RGB conversion with truncation-toward-zero on `U / 2` and
  per-channel clamp to `[0, 255]`.
- Skip macroblocks copy 4×4 blocks from the previous frame's
  reconstructed buffer.

Encode side (rounds 2 + 3 + 4 + 5 + 6 + 7 + r47-encoder-RDO + r4-LBG):

- `encode_rgb24` / `encode_gray8` — multi-strip intra encoder with
  configurable codebook entry counts (default 64 V4 + 64 V1, matching
  FFmpeg's `-q:v 10` "default quality" point per spec §4 of
  `05-container-carriage.md`).
- `encode_rgb24_inter` — inter-frame encoder. Compares each macroblock
  against the same-position 4×4 block in the previous reconstructed
  frame; macroblocks whose per-pixel MSE is below `skip_threshold` are
  emitted as `0x3100` SKIP codes, the rest carry V1/V4 updates.
- `EncoderOptions::from_quality(q)` — single PSNR-style knob `q ∈
  0..=100` mapping to codebook size (8..=256, log scale), strip count
  (1..=4), and skip threshold (256.0..=16.0, exponential).
- `CinepakEncoder` (round 4, stateful) — tracks the rolling V4/V1
  codebook the decoder will hold across strips and frames; emits
  `0x2100` / `0x2300` **selective-update** codebook chunks (only
  changed slots) when smaller than full-replace, or omits the
  codebook chunk entirely (spec §3.4: "inherit previous strip's
  codebook") when the previous codebook is already correct. Saves
  91.6% of inter-frame wire bytes on a static 32×32 fixture vs. the
  stateless `encode_rgb24_inter` path.
- `CinepakEncoder::set_cross_frame_codebook_persistence(bool)` (round
  5, default `true`) — the median-cut quantiser warm-starts each inter
  frame's training with the prior frame's codebook centroids (one
  Lloyd refinement pass), preserving slot identity so chunk-omission
  / selective-update fires on slow-pan content too. Multi-strip inter
  on slow-pan: **44.0% wire savings** vs the stateless full-replace
  path on a 64×64 4-strip fixture.
- `TwoPassRateControl` / `RateControlledFrame` (round 5) — first
  pass collects per-frame byte stats at a reference quality, second
  pass picks the largest grid-quality whose byte count is `≤ target`.
- `TwoPassRateControl::encode_at_target_window_bytes` (round 6) —
  windowed bisection rate control: binary-searches `q ∈ [0, 100]`
  per-frame against a rolling N-frame byte budget, holding quality
  steady when the projected window sum stays within ±tolerance of
  target.
- `EncoderOptions::lloyd_max_iter` / `lloyd_eps` (round 6) —
  iterative Lloyd refinement controls for the cross-frame
  codebook warm-start (default 2 iterations + eps-based early stop;
  set `lloyd_max_iter = 1` for round-5 single-pass, `0` for
  cold-start).
- `EncoderOptions::stale_slot_threshold` (round 7) — per-slot
  staleness counter + reclamation. When a codebook slot has been
  unreferenced for strictly more than `n` consecutive inter frames
  (default `Some(8)`), the encoder reseeds it from a high-residual
  sample MB and forces a full-replace codebook chunk so the decoder
  sees the new value. Closes the persistence "frozen slot" issue
  on scene-change content. `None` disables.
- `CinepakEncoder::last_frame_stats() -> FrameStats` (round 7) —
  per-frame telemetry: `reclaimed_v4_slots`, `reclaimed_v1_slots`,
  `forced_full_chunks`. Resets at the start of each frame.
- `TwoPassRateControl::encode_at_target_window_bytes_adaptive`
  (round 7) — adaptive-tolerance windowed bisection. Tolerance
  shrinks on low byte-size variance (stable scenes), grows on high
  variance (scene cuts), saturating between
  `tolerance_pct_min` and `tolerance_pct_max` per the
  `variance_scale_pct` cutoff. Equal-bound input matches the
  round-6 fixed-tolerance behaviour exactly.
- Median-cut codebook quantiser builds V1 and V4 codebooks
  per-strip; per-MB nearest-neighbour selection picks V1 vs V4 by
  squared-error against the source.
- `EncoderOptions::rdo_lambda` (round-47, default `Some(5.0)`) —
  Lagrangian V1/V4 rate-distortion selection. Per-MB V1-vs-V4
  decisions compute pixel-domain Y SSE for both candidate
  reconstructions and apply `D + lambda · R` with a 24-bit rate
  delta (V4 carries 4 index bytes vs V1's 1). Lifts PSNR_Y by
  +1.10 dB / +0.43 dB on 64×64 / 320×240 gradient fixtures (q=50)
  versus the round-2 codebook-distance comparison. Set to `None` to
  recover the round-2 behaviour.
- `encode_rgb24_best_strips` (round-47) — per-frame strip-count
  picker. Trial-encodes the input at each supplied candidate strip
  count and returns the bitstream with the lowest Lagrangian cost
  `R + lambda · D`. Breaks 38 dB PSNR_Y on the 320×240 gradient
  fixture at q=50 (selects 4 strips, 38.17 dB / 8568 B). Intra-only,
  stateless; cost is N self-decodes per call.
- `EncoderOptions::lbg_max_passes` (round 4, default `8`) —
  **Linde-Buzo-Gray (LBG) split refinement**. After median-cut +
  Lloyd warm-build, iteratively splits the highest-distortion
  populated codebook slot into the lowest-population slot, runs one
  full Lloyd pass over all vectors, and continues until total SSE
  stops decreasing. Reference: Linde-Buzo-Gray 1980, IEEE TComm
  28(1). Lifts PSNR_Y by +1.16 dB / +2.19 dB on 64×64 / 320×240
  gradient fixtures at q=50; combined with the strip-picker the
  64×64 gradient now reaches **40.77 dB Y at 2689 B** vs ffmpeg's
  reference 36.9 dB on the same fixture (+3.87 dB lead). Set
  `lbg_max_passes = 0` to disable.
- RGB→YUV forward transform algebraically inverts the spec's decoder
  matrix; round-trips primaries `(255,0,0)` / `(0,255,0)` / `(0,0,255)`
  to within the codec's quantisation tolerance.

Container side (round 2):

- `FilmDemuxer::parse` — Sega FILM / CPK header walker (FILM + FDSC +
  STAB chunks). Standard 32-byte FDSC (FFmpeg `film_cpk`) and
  abbreviated 20-byte FDSC (early Sega CD variants) both supported.
- `probe_film` — lightweight `'FILM'` signature check.
- `SampleRecord::is_keyframe` / `is_audio` / `timestamp_ticks` —
  decode `sample_info_1` per spec §2.4.
- Deviant Saturn variant (§2.6) is documented but not yet handled
  beyond signature recognition (no Saturn fixture in corpus).

## Output pixel formats

- `Rgb24` — 12-bit YUV streams converted via the spec's inverse
  matrix.
- `Gray8` — 8-bit luminance-only streams (chunk family `0x24xx`/`0x26xx`).

## FourCCs registered

`CVID` (AVI `biCompression`, QuickTime `cvid` codec tag, Sega FILM
`cvid` — all upper-cased through `CodecTag::fourcc`). Round 2 attached
a probe function (`probe_cvid`) that confirms the 10-byte frame header
+ 12-byte first-strip header structure, returning `1.0` confidence
when the structural fields are all valid and `0.0` when any check
fails. Without packet bytes, returns `0.5` (Cinepak's `'cvid'` FourCC
is unique enough that even weak evidence dominates).

## Standalone vs registry-integrated

The default `registry` cargo feature wires the crate into
`oxideav-core` (Decoder trait, `register(ctx)` entry point). Disable
the feature for an `oxideav-core`-free build that exposes only the
crate-local `CinepakDecoder`, `CinepakFrame`, `CinepakPixelFormat`,
and `CinepakError` types.
