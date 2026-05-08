# oxideav-cinepak

Pure-Rust Cinepak (CVID) video decoder for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Rounds 1 + 2 + 3 + 4 + 5 + 6 — clean-room rebuild from `docs/video/cinepak/spec/`.**
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
cinepak encoder is unavailable).

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

Encode side (rounds 2 + 3 + 4 + 5):

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
- Median-cut codebook quantiser builds V1 and V4 codebooks
  per-strip; per-MB nearest-neighbour selection picks V1 vs V4 by
  squared-error against the source.
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
