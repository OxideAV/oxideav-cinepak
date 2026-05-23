# oxideav-cinepak

Pure-Rust Cinepak (CVID) video decoder for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Rounds 1 + 2 + 3 + 4 + 5 + 6 + 7 + r47-encoder-RDO + r4-LBG + r5-luma-weight + r6-encoder-PCL + r7-encoder-PCL + r8-per-strip-picker + r9-kmeans++-init + r93-deviant-saturn-decoder + r96-bitrate-target-rate-control + r101-grayscale-rd-grid-picker + r104-grayscale-inter-frame — clean-room rebuild from `docs/video/cinepak/spec/`.**
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
Round 5 added **luma-weighted distance metric + luma-prioritized
median-cut split** (`EncoderOptions::luma_weight`, default `2`) —
two complementary luma-priority levers (Levers F + G) applied at the
codebook-training layer. Each Y-dim squared-error contribution is
multiplied by `luma_weight` in the distance metric (affects
nearest-neighbour selection, Lloyd refinement, LBG split refinement),
and Y-dim extents are multiplied by `luma_weight` in `median_cut`'s
split-dimension selection (biases the initial bisection toward Y-axis
cuts). Under PSNR_Y, packing the codebook tightly in Y is more
valuable than packing it tightly in U/V. PSNR_Y lifts by
**+1.55 dB** on the 64×64 gradient (`encode_rgb24`: 37.85 →
39.39 dB) and **+1.62 dB** combined with the strip-picker
(40.77 → 42.39 dB); the 320×240 gradient via strip-picker lifts by
**+1.01 dB** (40.69 → 41.70 dB). On pure LCG-noise the lever is a
near-wash (within ±0.05 dB) — random content has no luma-vs-chroma
structure to exploit. The headline 64×64 gradient now sits at
**42.39 dB Y at 2554 B** — a +5.49 dB lead over ffmpeg's reference
encoder.
Round 6 (encoder PCL) added **post-classification Lloyd polish** at
the encoder-training layer (`EncoderOptions::pcl_max_iter`, default
`2`) plus a **two-axis RD grid picker** (`encode_rgb24_best_rd_grid`
+ `encode_rgb24_round6` convenience wrapper) — Levers H + I.
Lever H re-trains each *used* codebook slot from only the
actually-selected member vectors after the round-3 Lagrangian RDO
step (the LBG warm-build minimised distortion across all non-skip
vectors, but the RDO routes a fraction of them to V1 — so the LBG
centroids aren't the means of each slot's actual selected member
set; PCL closes that gap without changing slot identity, so
cross-frame persistence and selective-update / chunk-omission wins
on inter strips are unaffected). Lever I sweeps `(strip_count,
rdo_lambda)` from a 3×3 cross-product (`[1, 2, 4]` ×
`[Some(0.0), Some(2.5), opts.rdo_lambda]`) and picks the lowest
Lagrangian-cost result — closes the round-3-picker gap on small
frames where the V4 codebook is effectively saturated and a lower
per-MB lambda harvests +1 dB at modest wire-size cost. Combined
PSNR_Y lift on the 64×64 gradient: **+1.05 dB** (42.39 → 43.44 dB
at 2554 B → 2704 B / +5.9% wire); on the 64×64 single-strip path
PCL alone lifts **+1.64 dB** (36.55 → 38.19 dB at the same wire
size). The headline 64×64 gradient now sits at **43.44 dB Y at
2704 B** — a +6.55 dB lead over ffmpeg's reference encoder.
Round 7 (encoder-PCL upgrade) added a **three-axis RD grid picker
with Y-channel scoring** (`encode_rgb24_best_rd_grid_3axis` +
`encode_rgb24_round7` convenience wrapper) — Levers J + K. Lever J
adds `luma_weight` as a third axis to the round-6 picker (round 6
only varied `(strip_count, rdo_lambda)`, freezing `luma_weight` at
`opts.luma_weight`); different fixtures favour different
`luma_weight` values (64×64 gradient likes `lw=4` / +1.14 dB;
320×240 gradient likes `lw=16` at smaller wire size). Lever K
switches the picker's scoring distortion from RGB SSE per
pixel-channel to **Y-channel SSE per pixel** (BT.601 luma),
aligning the picker's optimisation target with the project's
headline PSNR_Y metric — otherwise Y-improving `luma_weight`
values are actively penalised by RGB-SSE scoring (defeating
Lever J). The round-6 picker (`encode_rgb24_round6`) is unchanged
for callers that care about chroma fidelity. Combined PSNR_Y lift
on the 64×64 gradient: **+1.77 dB** (43.44 → 45.21 dB at 2704 B →
3139 B / +16% wire); on the 320×240 gradient: **+1.63 dB at
smaller wire** (45.25 dB / 14586 B → 46.88 dB / 14364 B). The
headline 64×64 gradient now sits at **45.21 dB Y at 3139 B** — a
+7.77 dB lead over ffmpeg's reference encoder.
Round 8 (per-strip picker) added the **per-strip independent
(lambda, luma_weight) picker** (`encode_rgb24_per_strip_rd` +
`encode_rgb24_round8` convenience wrapper) — Lever L. Round 7's
three-axis grid picker trial-encodes the **whole frame** with a single
`(strip_count, rdo_lambda, luma_weight)` per trial, so the chosen
`(rdo_lambda, luma_weight)` applies to every strip of the frame.
Cinepak's bitstream lets each strip carry its own pair of codebooks
trained independently, so on **split-content** frames where strips
have qualitatively different pixel statistics — a smooth-gradient top
strip favouring high `luma_weight = 8` + V4-saturated `lambda = 0`,
a saturated-chroma stripes bottom strip favouring `luma_weight = 1` +
`lambda = 2.5` — round 7 must compromise on whichever strip is the
"loser". Round 8 plans the strips per `strip_count` candidate then
sweeps `(lambda, luma_weight)` **per strip independently**, picks the
per-strip Y-SSE + λ·R minimiser, and assembles the chosen per-strip
bitstreams. A monotonicity wrapper runs the round-7 picker too and
returns whichever pick has lower cost, so on homogeneous content
round 8 matches round 7 exactly. Headline win on a 256×256
four-strip-split fixture at `q=50`: **-576 B at +0.10 dB PSNR_Y**
(10138 B → 9562 B, 52.91 dB → 53.01 dB) — a wire-size reduction at
iso-cost. On the round-7 64×64 / 320×240 gradient headlines round 8
matches round 7 within ±0.3 dB at smaller or equal wire size
(per-strip greedy converges to the frame-uniform pick on homogeneous
content).
Round 9 (encoder-init upgrade) added **k-means++ initialisation for
cold-start codebook training** (`EncoderOptions::kmeans_pp_init`,
default `true`; `EncoderOptions::kmeans_pp_lloyd_iter`, default `4`)
— Lever M. Rounds 4–8 fed every cold-start (intra frame / first frame
of an inter sequence / first strip of a strip-picker trial encode) into
**median-cut** geometric range-bisection followed by LBG + PCL polish.
Median-cut is greedy on per-cluster widest dimension at split time, so
on long-tailed pixel distributions a pair of dense clusters along the
same dim can end up sharing a centroid while a sparser dim absorbs
more centroids than it needs. Lever M replaces that with the
Arthur–Vassilvitskii 2007 k-means++ seeding rule (SODA 2007) — sample
the first centroid uniformly at random, then each subsequent centroid
with probability proportional to the squared luma-weighted distance
from the nearest already-chosen centroid — followed by up to 4 Lloyd
refinement passes. The k-means++ candidate's total training SSE is
compared against the median-cut candidate's, and the lower-SSE
codebook wins, **guaranteeing the cold-start never regresses training
SSE** vs round 8. The sampling RNG is a deterministic xorshift32
seeded from a content-derived hash, so identical inputs ⇒ identical
bytes (no system entropy consulted). On the 64×64 gradient at q=50:
**45.10 dB Y at 3037 B vs round-8's 44.92 dB at 3019 B (+0.18 dB at
+18 B / +0.6%)** — a picker-cost win (the picker scores Y-SSE + λ·R,
not raw PSNR) by 1.1%. On LCG-noise 64×64 the lever lifts PSNR_Y by
**+0.08 dB at iso-wire** (3322 B). On the 320×240 gradient the picker
chooses a smaller-wire operating point (-150 B at -0.31 dB) because
the k-means++ candidate shifts the cost landscape and the round-7
picker prefers a different `(strip_count, rdo_lambda, luma_weight)`
combination — net PSNR_Y at smaller wire, content-dependent.

Round 93 (decoder Sega Saturn deviant variant) added
**`CinepakDecoder::decode_deviant_frame` + `DeviantConfig`** — closes
the long-standing "Saturn deviant codec-frame slicing is deferred
until a genuine Saturn fixture is acquired" gap from round 2. The
deviant variant is documented in
`docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 125–143
(Saturn `'cvid'`) and line 189 (Lemmings 3DO 6-byte prefix); it
diverges from standard Cinepak in three ways — 2 extra bytes after
the 10-byte frame header (or 6 for Lemmings 3DO), `frame_length`
field 8 bytes short of the real frame body, and codebook chunks may
declare an 0x5FC-byte payload of 255 vectors + 2 trailing pad bytes
instead of the standard 0x600-byte payload of 256 vectors. The
deviant decode is gated behind an explicit
`CinepakDecoder::decode_deviant_frame(bytes, pts,
DeviantConfig::saturn())` entry point so standard AVI / QuickTime
`'cvid'` traffic continues to use the strict `decode_frame` path
byte-for-byte. Also added `FilmDemuxer::variant() -> CinepakVariant`
— a header-driven classifier that picks the right `DeviantConfig`
based on the FILM header version field (`'1.0X'` ASCII ⇒
`DeviantSaturn`, NULL ⇒ `DeviantLemmings3do`, `'sega'` / `'Seg4'` ⇒
`OutOfScope`). 10 new tests cover all three deviations
simultaneously, multi-strip deviant decoding, the
classifier table, and a regression check that standard decode is
unchanged.
Round 96 (encoder bitrate-target rate control) added
**`CinepakEncoder::with_target_bitrate(bits_per_second, fps)`** (and a
direct-byte `with_target_frame_bytes` form) — a stateful per-frame
byte-budget mode that drives the existing three-axis
`(strip_count, rdo_lambda, luma_weight)` RD grid toward a
constant-bitrate per-frame budget (`bits/8/fps`), analogous to the ICER
`with_byte_budget` pattern. The picker commits the highest-quality grid
candidate that fits the budget (lowest BT.601 Y-SSE with bytes ≤ cap)
and, when even the smallest candidate overshoots, emits that floor and
flags the overshoot via `RateStats` (`last_rate_stats()`) — never
erroring on rate. Unlike the round-5/6/7 `TwoPassRateControl`
(throwaway-encoder prefix replay), this rides the encoder's own
carry-over, so inter frames stay budgeted and skip / selectively-update
against the committed previous reconstruction. Rate control is encoder
policy, not a bitstream feature, so the budget-allocation heuristic is
derived from first principles (no external encoder source) and decoded
output stays conformant Cinepak. On a 128×96 gradient (q=85,
unconstrained 6418 B) budgets of 6418 / 4813 / 3209 B land at 95.8 % /
82.1 % / 99.2 % of budget, all under cap. 7 new tests cover budget
adherence across an intra+inter sequence, shrink-vs-unconstrained,
impossible-budget overshoot, size monotonicity, reset/clear semantics,
and fractional/degenerate fps arithmetic.
Round 104 (inter-frame grayscale encode path) added the **stateful
grayscale inter-frame encoder** (`CinepakEncoder::encode_intra_gray8`
+ `encode_inter_gray8`) and the **stateless `encode_gray8_inter`** free
function — the `Gray8` analogs of the `Rgb24` inter-frame pipeline.
Rounds 2..101 grew the colour encoder a full stateful inter pipeline
with rolling V4/V1 codebook persistence, selective-update, and
chunk-omission across frames, while the grayscale path stayed
intra-only (`encode_gray8` + the round-101 picker re-emitted every
codebook from scratch every frame). The per-strip encoder was already
mode-generic — round 104 just threads `PixelMode` through the
`CinepakEncoder` workers and adds the public entry points (with prev-
frame format validation so a grayscale inter call after a colour intra
is rejected with a clear error). Headline on a 32×32 four-quadrant
grayscale fixture (q=50, intra + five identical inter frames):
**88.0 % wire savings** on the inter-frame tail (250 B stateful vs
2090 B stateless full-replace), with the last inter frame entirely
SKIP — same chunk-omission inheritance win the colour path landed in
round 4 now applies to grayscale content. The decoder also picked up a
**chunk-omission mode-hint fallback in `decode_strip_chunks`** so a
fully chunk-omitted grayscale inter frame (no codebook chunks, all SKIP
MBs) stays `Gray8` instead of being misclassified as `Rgb24` — without
the hint the historical `Yuv12` default would have rendered grayscale
content as colour. Standard frames (with codebook chunks pinning the
mode) are unaffected. Target-bitrate mode (round 96) is `Yuv12`-scored
and does not apply to the grayscale path; the grayscale entry points
use the caller's `opts` verbatim.
Round 101 (grayscale RD-grid picker) added
**`encode_gray8_best_rd_grid` + `encode_gray8_round7`** (Lever N) — the
grayscale analog of the 12-bit-YUV frame-level RD-grid picker the colour
path picked up in rounds 47 / 6 / 7. `encode_gray8` only ever emitted a
single frame at the caller's `opts.strip_count` / `opts.rdo_lambda`,
while the colour path could trial-encode several `(strip_count,
rdo_lambda, luma_weight)` operating points and keep the lowest-cost one.
The new picker trial-encodes every `(strip_count, rdo_lambda)`
combination (default `[1, 2, 4]` × `[Some(0.0), Some(2.5),
opts.rdo_lambda]`, deduped — ≤ 9 trials), self-decodes each, and keeps
the bitstream minimising **direct-luma SSE per pixel** plus the
Lagrangian byte cost (`opts.rdo_lambda · R/N`). On grayscale the 8-bit
luminance *is* the Y channel, so no BT.601 weighting is needed; there is
deliberately **no `luma_weight` axis** because for `Gray8` codebook
entries `entry_distance` scales all four Y dims by `luma_weight` — a
uniform positive scale that leaves every nearest-neighbour / clustering
decision invariant. On the 64×64 grayscale gradient at q=50 the picker
lifts PSNR from the fixed default's **45.01 dB to 49.56 dB (+4.55 dB,
+108 B)** by selecting a higher strip count whose per-band V4 codebooks
localise the luminance ramp; on the 320×240 gradient **+5.16 dB** (44.84
→ 50.00 dB, larger wire as the picker chose 4 strips); on LCG-noise
**+0.88 dB**; on a 128×96 gradient the default was already optimal (no
change). 7 new tests cover conformance, the headline gain, no-regression
vs the fixed default across three sizes, noise handling, empty-candidate
rejection, and pure-distortion (`rdo_lambda = None`) ranking.

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

Encode side (rounds 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + r47-encoder-RDO + r4-LBG + r5-luma-weight + r6-encoder-PCL + r7-encoder-PCL + r8-per-strip-picker + r9-kmeans++-init + r104-grayscale-inter-frame):

- `encode_rgb24` / `encode_gray8` — multi-strip intra encoder with
  configurable codebook entry counts (default 64 V4 + 64 V1, matching
  FFmpeg's `-q:v 10` "default quality" point per spec §4 of
  `05-container-carriage.md`).
- `encode_gray8_inter` (round 104) — stateless `Gray8` inter-frame
  encoder, the analog of `encode_rgb24_inter`. Same SKIP-MB / per-pixel
  luma-MSE decision rule as the colour path; always emits full-replace
  codebook chunks. Requires a `Gray8` `prev` reconstruction.
- `CinepakEncoder::encode_intra_gray8` / `encode_inter_gray8`
  (round 104) — the **stateful** grayscale analogs of `encode_intra` /
  `encode_inter`. Carry rolling grayscale codebooks across frames with
  the round-5 cross-frame persistence + round-7 stale-slot reclamation
  machinery (mode-generic at the per-strip layer); emit `0x3100` SKIP /
  selective-update / chunk-omission wire patterns. Validate that the
  prior frame is `Gray8` — `encode_inter_gray8` after a colour intra is
  rejected with a clear error. On a 32×32 four-quadrant static fixture
  at q=50 over 5 inter frames: **88.0 % wire savings** (250 B stateful
  vs 2090 B stateless) with the last inter frame entirely SKIP.
  Target-bitrate mode (round 96) is `Yuv12`-scored and does not apply
  to the grayscale path; the grayscale entry points use the caller's
  `opts` verbatim.
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
- `CinepakEncoder::with_target_bitrate(bits_per_second, fps)` /
  `with_target_frame_bytes(n)` (round 96) — **single-encoder
  bitrate-target rate control** (analogous to the ICER
  `with_byte_budget` pattern). Derives a constant-bitrate per-frame
  budget (`bits/8/fps`; fps clamped ≥ 1.0; fractional rates like
  23.976 supported) and drives the existing three-axis
  `(strip_count, rdo_lambda, luma_weight)` RD grid toward it per
  frame: commits the **highest-quality candidate that fits**
  (lowest BT.601 Y-SSE with bytes ≤ budget), else the smallest
  candidate with the overshoot flagged. Unlike `TwoPassRateControl`
  (which re-encodes the whole prefix via a throwaway encoder per
  frame), this rides the stateful encoder's own carry-over —
  inter frames stay budgeted and skip/selective-update against the
  committed previous reconstruction. Per-frame adherence via
  `last_rate_stats() -> Option<RateStats>` (`target_bytes`,
  `actual_bytes`, `byte_delta`, `within_budget`, `trials`).
  `reset()` preserves the budget; `clear_target_bitrate()` returns
  to quality-controlled mode. Measured on a 128×96 gradient
  (q=85, unconstrained 6418 B): budgets of 6418 / 4813 / 3209 B
  land at 95.8 % / 82.1 % / 99.2 % of budget, all under cap; budgets
  below the grid's ~2833 B smallest-frame floor emit that floor and
  report overshoot. Rate control is encoder policy, not a bitstream
  feature — output stays conformant Cinepak.
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
- `EncoderOptions::luma_weight` (round 5, default `2`) — **luma-
  weighted distance metric + luma-prioritized median-cut split**
  (Levers F + G). Each Y-dim squared-error contribution is multiplied
  by `luma_weight` in the distance metric used for clustering, Lloyd
  refinement, LBG split refinement, and per-MB nearest-neighbour
  selection; the same weight scales Y-dim extents in `median_cut`'s
  split-dimension selection. Under PSNR_Y, packing the codebook
  tightly in Y is more valuable than packing it tightly in U/V.
  Lifts PSNR_Y by +1.55 dB on 64×64 gradient via `encode_rgb24`
  (37.85 → 39.39 dB) and +1.62 dB / +1.01 dB combined with the
  strip-picker on 64×64 / 320×240 gradient (40.77 → 42.39 dB /
  40.69 → 41.70 dB). On pure LCG-noise the lever is a near-wash (no
  luma-vs-chroma structure to exploit). Set `luma_weight = 1` to
  recover the round-4 isotropic behaviour; `0` is a no-op fallback.
- `EncoderOptions::pcl_max_iter` (round 6, default `2`) — **post-
  classification Lloyd polish (PCL)** (Lever H). After the round-3
  Lagrangian RDO step routes each non-skip MB to V4 or V1, each
  *used* codebook slot is re-trained from only the actually-selected
  member vectors (the LBG warm-build minimised distortion across all
  non-skip vectors, but the RDO routes a fraction of them to V1, so
  the LBG centroids aren't the means of each slot's actual selected
  member set). Slot identity is preserved across the polish — unused
  slots stay byte-identical to the LBG output, so cross-frame
  persistence and selective-update / chunk-omission wins on inter
  strips are unaffected. Lifts PSNR_Y by **+1.64 dB** on the 64×64
  single-strip gradient (36.55 → 38.19 dB at the same wire size).
  Set `pcl_max_iter = 0` to disable.
- `encode_rgb24_best_rd_grid` / `encode_rgb24_round6` (round 6) —
  **two-axis RD grid picker** (Lever I). Sweeps every
  `(strip_count, rdo_lambda)` from the user-supplied
  `strip_candidates` × `lambda_candidates` cross-product and returns
  the bitstream minimising the Lagrangian cost
  (`R/N + opts.rdo_lambda · D/N`); `encode_rgb24_round6` is the
  default convenience wrapper with `[1, 2, 4]` × `[Some(0.0),
  Some(2.5), opts.rdo_lambda]`. Closes the round-3-picker gap on
  small frames where the V4 codebook is effectively saturated (e.g.
  64×64 at q=50 with `strip_count=4` exactly fills 64 V4 entries) —
  lowering the per-MB lambda routes more MBs to V4 and harvests the
  residual error at modest wire-size cost. Combined with PCL the
  64×64 gradient lifts to **43.44 dB Y at 2704 B** — a +6.55 dB lead
  over ffmpeg's reference encoder.
- `encode_rgb24_best_rd_grid_3axis` / `encode_rgb24_round7` (round 7) —
  **three-axis RD grid picker with Y-channel scoring** (Levers J + K).
  Sweeps every `(strip_count, rdo_lambda, luma_weight)` from the
  user-supplied `strip_candidates` × `lambda_candidates` ×
  `luma_candidates` cross-product, returning the bitstream that
  minimises **BT.601 Y-channel SSE per pixel** plus the Lagrangian
  byte cost (`D_Y/N + opts.rdo_lambda · R/N`). Lever J adds the third
  axis so the picker can pivot per-content (64×64 gradient likes
  `lw=4`, 320×240 gradient likes `lw=16`); Lever K aligns the picker's
  scoring distortion with the project's headline PSNR_Y metric (the
  round-6 picker's RGB-SSE scoring actively penalises the Y-improving
  `luma_weight` values, defeating Lever J). `encode_rgb24_round7` is
  the default convenience wrapper with `[1, 2, 4]` × `[Some(0.0),
  Some(2.5), opts.rdo_lambda]` × `[opts.luma_weight, 4, 8]` (deduped)
  for ≤ 27 trial encodes per frame. On the 64×64 gradient at q=50:
  **45.21 dB Y at 3139 B** — a +7.77 dB lead over ffmpeg's reference
  encoder, +1.77 dB over `encode_rgb24_round6`. On the 320×240
  gradient: 46.88 dB at 14364 B (+1.63 dB at smaller wire than round
  6's choice). The round-6 picker is unchanged for callers that care
  about chroma fidelity.
- `encode_rgb24_per_strip_rd` / `encode_rgb24_round8` (round 8) —
  **per-strip independent (lambda, luma_weight) picker** (Lever L).
  Round 7's picker trial-encodes the **whole frame** with a single
  `(strip_count, rdo_lambda, luma_weight)` per trial. Round 8 plans
  the strips per `strip_count` candidate, then for each strip
  independently sweeps every `(lambda, luma_weight)` combination and
  picks the one minimising **per-strip Y-SSE + λ·R**, assembling the
  chosen per-strip bitstreams into a multi-strip frame. Cinepak's
  bitstream lets each strip carry its own pair of codebooks trained
  independently (spec §3.4 of `02-codebooks.md`), so per-strip
  `luma_weight` and `rdo_lambda` are first-class — the decoder
  doesn't care which lever values the encoder picked per strip.
  `encode_rgb24_round8` also runs the round-7 picker and returns
  whichever pick has lower Y-SSE + λ·R cost, **guaranteeing round 8
  ≥ round 7** in the picker's scoring metric. Headline on the
  256×256 four-strip-split fixture at q=50: **53.01 dB at 9562 B**
  vs round 7's 52.91 dB at 10138 B (**-576 B / +0.10 dB PSNR_Y**, a
  wire-size reduction at iso-cost). On the round-7 64×64 / 320×240
  gradient headlines round 8 matches round 7 within ±0.3 dB at
  smaller or equal wire size (per-strip greedy converges to the
  frame-uniform pick on homogeneous content). Intra-only; cost
  scales as O(|strips| × |lambdas| × |lumas|) per `strip_count`
  candidate, up to ~65 trial encodes per frame with the default
  3×3×3 grid.
- `EncoderOptions::kmeans_pp_init` (round 9, default `true`) +
  `EncoderOptions::kmeans_pp_lloyd_iter` (default `4`) — **k-means++
  initialisation for cold-start codebook training** (Lever M).
  Reference: Arthur & Vassilvitskii, "k-means++: The Advantages of
  Careful Seeding", SODA 2007 (published academic algorithm; no
  external library source consulted). On every cold-start path (no
  cross-frame seed available — intra frames, first frame of an inter
  sequence, and every strip-picker trial encode), the encoder builds
  both the median-cut codebook **and** a k-means++-seeded codebook
  with up to 4 Lloyd refinement passes, then keeps the one with
  lower training SSE against the strip's vector population —
  **guaranteeing round 9 never regresses training SSE** vs the
  round-8 median-cut cold-start. The k-means++ sampling RNG is a
  deterministic xorshift32 seeded from a content-derived hash so
  identical inputs ⇒ identical bytes (no system entropy consulted).
  Picker-cost lift on 64×64 gradient at q=50: -1.1%
  (45.10 dB Y at 3037 B vs round-8's 44.92 dB at 3019 B — +0.18 dB
  at +18 B). LCG-noise 64×64: +0.08 dB at iso-wire (3322 B). Set
  `kmeans_pp_init = false` to recover the round-8 median-cut-only
  cold-start. Has no effect on the warm-start path (inter strips
  with cross-frame seed); cross-frame slot identity continues to
  rely on `lloyd_max_iter` Lloyd refinement of the prior codebook.
- `encode_gray8_best_rd_grid` / `encode_gray8_round7` (round 101) —
  **grayscale RD-grid frame-level picker** (Lever N), the `Gray8`
  analog of `encode_rgb24_best_rd_grid` / `encode_rgb24_round7`.
  `encode_gray8` always emitted a single frame at the caller's
  `opts.strip_count` / `opts.rdo_lambda`; this picker trial-encodes
  every `(strip_count, rdo_lambda)` from `strip_candidates ×
  lambda_candidates` (default `[1, 2, 4]` × `[Some(0.0), Some(2.5),
  opts.rdo_lambda]`, deduped — ≤ 9 trials), self-decodes each, and
  keeps the bitstream minimising **direct-luma SSE per pixel** plus
  the Lagrangian byte cost (`opts.rdo_lambda · R/N`). No `luma_weight`
  axis: for `Gray8` entries the distance metric scales all four Y dims
  by `luma_weight`, a uniform positive scale that leaves every
  nearest-neighbour / clustering decision invariant — sweeping it would
  only waste trials. On the 64×64 grayscale gradient at q=50: **45.01
  → 49.56 dB (+4.55 dB, +108 B)**; 320×240 gradient +5.16 dB (44.84 →
  50.00 dB, larger wire); LCG-noise +0.88 dB; 128×96 gradient unchanged
  (default already optimal). Intra-only, stateless. `rdo_lambda = None`
  gives pure-distortion (smallest-error) ranking.
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
- `FilmDemuxer::variant() -> CinepakVariant` (round 93) —
  header-driven classifier that picks the right `DeviantConfig`
  based on FILM header version + FDSC FOURCC per
  `Sega_FILM.wiki` lines 200–224. Returns
  `DeviantSaturn` for ASCII `1.0X` versions, `DeviantLemmings3do`
  for NULL versions, `OutOfScope` for `'sega'` / `'Seg4'`
  Cinepak-for-Sega.

Decoder side, deviant Saturn variant (round 93):

- `CinepakDecoder::decode_deviant_frame(bytes, pts,
  DeviantConfig::saturn())` — Sega Saturn / Sega CD `'cvid'`
  deviant decode. Standard `decode_frame` path is unaffected. The
  deviant variant handles all three documented divergences from
  `Sega_FILM.wiki` lines 125–143: 2 extra header bytes (or 6 for
  Lemmings 3DO via `DeviantConfig::lemmings_3do()`), `frame_length`
  short by 8, and codebook chunks may declare a payload size that
  isn't a clean multiple of the entry stride (`floor(len /
  entry_size)` entries are decoded, remainder is skipped).
- `codebook::apply_codebook_chunk_with(kind, payload, cb,
  tolerate_trailing)` — lower-level codebook decode with the
  trailing-pad knob. The strict default (`apply_codebook_chunk`)
  still rejects non-divisible payload sizes; setting
  `tolerate_trailing = true` truncates and discards the remainder.

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
