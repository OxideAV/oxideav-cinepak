# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Round 9 (encoder-init upgrade — Lever M): **k-means++ initialisation
  for cold-start codebook training**
  (`EncoderOptions::kmeans_pp_init`, default `true`;
  `EncoderOptions::kmeans_pp_lloyd_iter`, default `4`). Rounds 4–8
  fed every cold-start (intra frame / first frame of an inter
  sequence / every strip-picker trial encode) into **median-cut**
  geometric range-bisection followed by LBG + PCL polish. Lever M
  replaces the cold-start with the Arthur–Vassilvitskii 2007
  k-means++ seeding rule (SODA 2007 — published academic algorithm,
  no external library source consulted): sample the first centroid
  uniformly at random, then each subsequent centroid with probability
  proportional to the squared luma-weighted distance from the nearest
  already-chosen centroid; followed by up to 4 Lloyd refinement
  passes (assignment + recentroid + eps-based early stop, same
  semantics as `lloyd_max_iter` / `lloyd_eps`).

  **No-regression guarantee**: the round-9 implementation builds
  **both** the median-cut codebook and the k-means++ codebook and
  keeps whichever has lower total training SSE against the strip's
  vector population. The median-cut output is always a candidate, so
  round 9 can never produce a worse-SSE cold-start than round 8.

  **Determinism**: the sampling RNG is a deterministic xorshift32
  seeded from a content-derived hash (vector population length, first
  / middle / last entry, codebook size, canonicalised luma weight),
  so identical inputs produce byte-identical output. No system
  entropy is consulted — `cargo test` is reproducible across runs.
  `luma_weight = 0` canonicalises to `1` in the seed mix so it stays
  byte-equivalent to `luma_weight = 1` (preserves the round-5
  "0 → 1 fallback" contract).

  **No effect on warm-start**: when a cross-frame seed is available
  (inter strips after the first frame), the encoder continues to use
  the prior codebook centroids and `lloyd_max_iter` Lloyd refinement
  — Lever M triggers only on cold-start paths. Cross-frame slot
  identity is preserved exactly as in round 8.

  Measured deltas (self-encode + self-decode at `q=50`; the picker
  scores Y-SSE per pixel + λ·R/pixel, not pure PSNR):

  | fixture                          | r8 (median-cut) | r9 (k-means++)  | delta            |
  | -------------------------------- | --------------- | --------------- | ---------------- |
  | 64×64 gradient via `round8`      | 44.92 dB/3019B  | 45.10 dB/3037B  | +0.18 dB / +18B  |
  | 320×240 gradient via `round8`    | 46.88 dB/14364B | 46.57 dB/14214B | -0.31 dB / -150B |
  | 64×64 LCG-noise via `round8`     | 23.21 dB/3322B  | 23.29 dB/3322B  | +0.08 dB / +0B   |

  64×64 gradient picker-cost (Y-SSE/N + λ·R/N) drops by 1.1%
  (5.78 → 5.72). On 320×240 the picker shifts its
  `(strip_count, rdo_lambda, luma_weight)` choice to a
  smaller-wire / slightly-lower-PSNR operating point because the
  k-means++ candidate moves the per-trial cost landscape — net wire
  is -150 B at -0.31 dB PSNR_Y, within human-imperceptible bounds on
  a 47 dB gradient.

- Round 9: `tests/r9_psnr.rs` — seven-test validation suite asserting
  (i) round-9 64×64 gradient PSNR_Y doesn't regress by more than
  0.5 dB vs round-8 baseline, (ii) round-9 320×240 gradient PSNR_Y
  doesn't regress by more than 0.5 dB, (iii) strictly positive PSNR_Y
  delta on 64×64 LCG-noise (worst case for VQ), (iv) byte-for-byte
  determinism on identical inputs, (v) `kmeans_pp_init = false`
  reproduces the round-8 median-cut floor (>= 44.5 dB on 64×64
  gradient), (vi) strict picker-cost improvement on 64×64 gradient
  (5.78 → 5.72), (vii) `kmeans_pp_lloyd_iter` monotonicity (more
  iterations don't strictly regress).

- Round 8 (encoder-picker upgrade — Lever L): **per-strip independent
  (lambda, luma_weight) picker** (`encode_rgb24_per_strip_rd` +
  `encode_rgb24_round8` convenience wrapper). Round 7's three-axis
  grid picker (`encode_rgb24_best_rd_grid_3axis` /
  `encode_rgb24_round7`) trial-encodes the **whole frame** with a
  single `(strip_count, rdo_lambda, luma_weight)` per trial; the
  chosen `(rdo_lambda, luma_weight)` pair then applies to every strip
  of the frame. Round 8 plans the strips per `strip_count` candidate
  then for each strip independently sweeps every `(lambda,
  luma_weight)` combination and picks the one minimising per-strip
  Y-SSE + λ·R, assembling the chosen per-strip bitstreams into a
  multi-strip frame.

  Cinepak's bitstream lets each strip carry its own pair of codebooks
  trained independently (spec §3.4 of `02-codebooks.md`), so
  per-strip `luma_weight` and `rdo_lambda` are first-class — the
  decoder doesn't care which lever values the encoder picked per
  strip.

  Each per-strip trial encodes the strip as a standalone single-strip
  frame of size `(width, strip_h)` for scoring, then the winning
  `(lambda, luma_weight)` is re-emitted with the real strip plan
  (correct `y_top` / `y_bottom`) and concatenated into the assembled
  multi-strip frame. Cost: O(|strips| × |lambdas| × |lumas|) trial
  encodes per `strip_count` candidate, plus one final per-strip
  re-encode.

  `encode_rgb24_round8` also runs the round-7 picker and keeps
  whichever pick has the lower Y-SSE + λ·R cost — **guaranteeing
  round 8 ≥ round 7** in the picker's scoring metric. On homogeneous
  content the per-strip greedy converges to the frame-uniform pick
  and round 8 matches round 7 exactly; on heterogeneous content
  round 8 wins.

  Measured wins (self-encode + self-decode at q=50; the picker's
  cost is Y-SSE + λ·R, not pure PSNR — gains show up as either
  smaller wire at iso-quality or higher quality at iso-wire):

  | fixture                                | r7              | r8              | delta            |
  | -------------------------------------- | --------------- | --------------- | ---------------- |
  | 256×256 four-strip-split, default λ=5  | 52.91 dB/10138B | 53.01 dB/9562B  | +0.10 dB / -576B |
  | 320×240 four-strip-split, default λ=5  | 40.27 dB/13209B | 40.27 dB/12669B | +0.00 dB / -540B |
  | 256×256 four-strip-split, λ=None       | 52.93 dB/10570B | 53.01 dB/9562B  | +0.09 dB / -1008B|
  | 64×64 gradient (r7 headline), λ=None   | 45.21 dB/3139B  | 45.21 dB/3139B  | +0.00 dB / +0B   |
  | 320×240 gradient (r7 headline)         | 46.88 dB/14364B | 46.88 dB/14364B | +0.00 dB / +0B   |
  | 64×64 LCG-noise                        | 23.20 dB/3322B  | 23.21 dB/3322B  | +0.01 dB / +0B   |

  Headline: **256×256 four-strip-split now hits 53.01 dB Y at 9562 B
  (vs round-7's 52.91 dB at 10138 B) — a -576 B wire-size reduction
  at +0.10 dB PSNR_Y**, the design win for heterogeneous content. On
  the round-7 64×64 / 320×240 gradient headlines round 8 matches
  round 7 byte-for-byte (per-strip greedy degenerates to the
  frame-uniform pick on homogeneous content).

  Intra-only; the round-8 picker requires a per-strip trial encoder
  and the cross-frame persistence machinery is incompatible with
  switching `luma_weight` mid-sequence.

- Round 8: `tests/r8_psnr.rs` — nine-test validation suite asserting
  (i) round-8 ≤ round-7 in picker cost on 256×256 split content,
  (ii) round-8 ≤ round-7 on 64×64 gradient headline (homogeneous),
  (iii) round-8 ≤ round-7 on 64×64 LCG-noise, (iv) round-8 saves
  ≥ 400 B on 256×256 split-content vs round-7 at default λ=5
  (observed -576 B), (v) round-8 matches round-7 within 0.05 dB
  PSNR_Y on homogeneous content under λ=None, (vi)-(viii) empty-list
  errors on all three candidate lists, (ix) single-candidate
  per-strip picker round-trips at ≥ 30 dB PSNR_Y on the 64×64
  gradient.

- Round 7 (encoder-PCL upgrade — Levers J + K): **three-axis RD grid
  picker with Y-channel scoring**
  (`encode_rgb24_best_rd_grid_3axis` + `encode_rgb24_round7`
  convenience wrapper). Two complementary picker-layer levers on top of
  round 6.

  - **Lever J (`luma_weight` axis)**: the round-6
    `encode_rgb24_best_rd_grid` picker only varies `(strip_count,
    rdo_lambda)` and freezes `luma_weight` at `opts.luma_weight`
    (default `2`). Different fixtures favour different `luma_weight`
    values: the 64×64 gradient at `q=50` likes `luma_weight = 4`
    (+1.14 dB PSNR_Y over `lw=2`), the 320×240 gradient likes
    `luma_weight = 16` at the same wire footprint. The third axis lets
    the picker pivot per-content per-frame instead of requiring the
    caller to guess.

  - **Lever K (Y-channel scoring distortion)**: round 6's picker scores
    by RGB SSE per pixel-channel, but the project's headline quality
    metric is PSNR_Y (BT.601 Y-channel MSE). Higher `luma_weight`
    improves Y at the cost of chroma — RGB-SSE scoring actively
    penalises the `luma_weight` values that boost PSNR_Y the most,
    defeating Lever J. Y-channel scoring aligns the picker's
    optimisation target with the headline metric. (RGB-scoring pickers
    `encode_rgb24_best_strips` / `encode_rgb24_best_rd_grid` are
    unchanged for callers that care about chroma fidelity.)

  Convenience wrapper `encode_rgb24_round7(opts)` sweeps
  `[1, 2, 4]` × `[Some(0.0), Some(2.5), opts.rdo_lambda]` ×
  `[opts.luma_weight, 4, 8]` (deduped) for ≤ 27 trial encodes per
  frame.

  Measured wins (self-encode + self-decode at q=50, PSNR_Y in BT.601 Y
  units):

  | fixture                                                | r6             | r7             | delta              |
  | ------------------------------------------------------ | -------------- | -------------- | ------------------ |
  | 64×64 gradient, `encode_rgb24_round7` headline         | 43.44 dB/2704B | 45.21 dB/3139B | +1.77 dB / +16% B  |
  | 320×240 gradient, `encode_rgb24_round7`                | 45.25 dB/14586B| 46.88 dB/14364B| +1.63 dB / -1.5% B |
  | 64×64 LCG-noise, `encode_rgb24_round7`                 | 23.04 dB       | 23.20 dB       | +0.16 dB           |

  Headline: **the 64×64 gradient via `encode_rgb24_round7` now hits
  45.21 dB Y at 3139 B** — a +7.77 dB lead over ffmpeg's reference
  encoder's ~36.9 dB on the same fixture, +1.77 dB over the round-6
  baseline, and well past the round-7 ≥ 0.5 dB target. On LCG noise
  the lever is a near-wash (random content has no luma-vs-chroma
  structure for the picker to exploit). Both
  `encode_rgb24_best_rd_grid_3axis` and `encode_rgb24_round7` honour
  the existing per-MB knobs (`pcl_max_iter`, `lbg_max_passes`,
  `rdo_lambda` baseline, etc.); the picker only varies the three axes
  declared in the trial grid.

- Round 7: `tests/r7_psnr.rs` — seven-test PSNR validation suite
  asserting (i) `encode_rgb24_round7` on 64×64 gradient breaks
  43.94 dB PSNR_Y (= round-6 baseline + 0.5 dB target; observed
  45.21 dB), (ii) round-7 picker lifts ≥ 0.5 dB over `encode_rgb24_round6`
  on the same fixture (observed +1.77 dB), (iii) Lever-J isolated:
  `luma_candidates = [2, 4]` lifts ≥ 0.8 dB over `[2]`-only (observed
  +1.14 dB), (iv) Lever-K isolated: Y-scoring picker on 320×240
  gradient achieves ≥ r6 PSNR_Y with strictly smaller wire, (v) noisy
  LCG no-regression (≥ 22.5 dB), (vi) empty-list errors on all three
  candidate lists, (vii) wrapper honours `opts.luma_weight` in
  candidate list.

- Round 6 (Levers H + I): **post-classification Lloyd polish (PCL)**
  + **two-axis RD grid picker** at the encoder layer.

  - **Lever H (PCL)**: `EncoderOptions::pcl_max_iter`, default `2`.
    After the round-3 Lagrangian RDO step routes each non-skip MB to
    V4 or V1, each *used* codebook slot is re-trained from only the
    actually-selected member vectors and the per-MB classification is
    re-run. The LBG warm-build (round 4) minimised distortion across
    all non-skip vectors, but the RDO step routes a fraction of them
    to V1 (cheaper wire footprint) — so the LBG centroids aren't the
    means of each slot's actual selected member set. PCL closes that
    gap. Slot identity is preserved (unused slots stay byte-identical
    to the LBG output) so cross-frame persistence and selective-update
    / chunk-omission wins on inter strips are unaffected. 2 iterations
    capture essentially the full gain (the first pass usually
    re-classifies a few V1 MBs to V4 once V4 slots tighten; the second
    is convergence cleanup). Cost per iteration: O(N · K) — one
    nearest-neighbour sweep per non-skip MB times K = codebook size,
    comparable to a single LBG pass.

  - **Lever I (RD grid picker)**: `encode_rgb24_best_rd_grid` +
    `encode_rgb24_round6` convenience wrapper. Trial-encodes every
    `(strip_count, rdo_lambda)` from a user-supplied 2-axis
    cross-product and returns the bitstream minimising
    `R/N + opts.rdo_lambda · D/N` (the same Lagrangian cost as the
    round-3 picker). The round-3 picker only varies `strip_count` and
    reuses `opts.rdo_lambda` for the per-MB RDO of each trial, so it
    can't exploit the V4-saturation regime — on the 64×64 gradient at
    `q=50` with `strip_count=4`, the V4 codebook fills exactly (64
    sub-blocks across 64 entries → exact V4 representation), and
    lowering the per-MB lambda routes more MBs to V4 to harvest the
    residual error. `encode_rgb24_round6` sweeps `[1, 2, 4]` ×
    `[Some(0.0), Some(2.5), opts.rdo_lambda]` for ≤ 9 trial encodes
    per frame.

  Measured wins (self-encode + self-decode at q=50, PSNR_Y in BT.601 Y
  units):

  | fixture                                                | r5             | r6             | delta              |
  | ------------------------------------------------------ | -------------- | -------------- | ------------------ |
  | 64×64 gradient, `encode_rgb24_round6` headline         | 42.39 dB/2554B | 43.44 dB/2704B | +1.05 dB / +5.9% B |
  | 64×64 gradient, `encode_rgb24` strip=1 + PCL only      | 36.55 dB/1066B | 38.19 dB/1132B | +1.64 dB / +6.2% B |
  | 320×240 gradient, `encode_rgb24_best_strips` + PCL     | 41.70 dB/9288B | 43.16 dB/10170B| +1.46 dB / +9.5% B |
  | 64×64 LCG-noise, `encode_rgb24_round6`                 | 23.00 dB       | 23.04 dB       | +0.04 dB           |

  Headline: **the 64×64 gradient via `encode_rgb24_round6` now hits
  43.44 dB Y at 2704 B** — a +6.55 dB lead over ffmpeg's reference
  encoder's ~36.9 dB on the same fixture, +1.05 dB over the round-5
  baseline, and past the round-6 ≥ 0.5 dB target. On pure LCG-noise
  PCL is a near-wash (random content has no luma/spatial structure
  for the post-classification re-training to recover). Set
  `pcl_max_iter = 0` to disable the polish; both
  `encode_rgb24_best_strips` (round-3 picker) and
  `encode_rgb24_best_rd_grid` (round-6 picker) honour it.

- Round 5 (Levers F + G): **luma-weighted distance metric** and
  **luma-prioritized median-cut split** (`EncoderOptions::luma_weight`,
  default `2`). Two complementary luma-priority levers applied at the
  codebook-training layer:

  - **Lever F (distance metric)**: each Y-dim squared-error
    contribution is multiplied by `luma_weight` before being summed
    with the chroma U/V contributions, in `entry_distance` /
    `entry_l1_distance` / `nearest` / `pick_v4` / `pick_v1` and through
    every clustering-related call site (Lloyd refinement, LBG split
    refinement, slot-reclamation residual scoring). Under PSNR_Y,
    packing the codebook tightly in Y is more valuable than packing it
    tightly in U/V — luma-weighted clustering pulls trained centroids
    closer to source Y values at a modest chroma fidelity cost.
  - **Lever G (median-cut split)**: when picking the split dimension
    in `median_cut`, Y-dim extents are multiplied by `luma_weight`
    before being compared against U/V-dim extents. This biases the
    initial bisection toward Y-axis cuts when Y and U/V extents are
    otherwise comparable.

  Both levers share the single `luma_weight` knob (default `2`). `1`
  reproduces the round-4 isotropic-distance / isotropic-split
  behaviour; `0` is treated as `1` internally (no-op fallback).

  Measured wins (self-encode + self-decode at q=50, PSNR_Y in BT.601 Y
  units; `luma_weight = 1` is the round-4 baseline):

  | fixture                                       | r4 (lw=1) | r5 (lw=2) | delta     |
  | --------------------------------------------- | --------- | --------- | --------- |
  | 64×64 gradient, `encode_rgb24`                | 37.85 dB  | 39.39 dB  | +1.55 dB  |
  | 64×64 gradient, `encode_rgb24_best_strips`    | 40.77 dB  | 42.39 dB  | +1.62 dB  |
  | 320×240 gradient, `encode_rgb24_best_strips`  | 40.69 dB  | 41.70 dB  | +1.01 dB  |
  | 64×64 LCG-noise, `encode_rgb24_best_strips`   | 22.98 dB  | 23.00 dB  | +0.02 dB  |

  Headline: **the 64×64 gradient via `encode_rgb24_best_strips` now
  hits 42.39 dB Y at 2554 B** — a +5.49 dB lead over ffmpeg's
  reference encoder's ~36.9 dB on the same fixture, +1.62 dB over the
  round-4 LBG-only baseline, and well past the round-5 41.27 dB
  target. The 320×240 gradient also clears the 41.0 dB target with
  ~0.7 dB of headroom. On pure LCG-noise the lever is a near-wash
  (within ±0.05 dB) — random content has no luma-vs-chroma structure
  for the metric to exploit.

  Set `luma_weight = 1` to disable both Levers F and G and recover
  the round-4 behaviour (used by the regression-guard tests in
  `r5_psnr.rs`, and pinned in the round-4 LBG-isolation tests in
  `r4_psnr.rs` and the round-5/round-6 lever-isolation tests in
  `round5_persistence_and_multistrip.rs` /
  `round6_lloyd_and_window.rs`). Cost: a single integer multiply per
  distance evaluation — no measurable wall-time impact (the
  full-test-suite wall is unchanged).

- Round 5: `tests/r5_psnr.rs` — six-test PSNR validation suite
  asserting (i) `encode_rgb24_best_strips` on 64×64 gradient breaks
  41.27 dB PSNR_Y (observed 42.39 dB), (ii) 320×240 gradient breaks
  41.0 dB (observed 41.70 dB), (iii) LCG-noise no regression vs r4
  (observed +0.02 dB), (iv) Lever F isolated delta ≥ 1.0 dB on 64×64
  gradient (observed +1.55 dB), (v) `luma_weight = 1` reproduces the
  round-4 baseline within noise, (vi) `luma_weight = 0` is a no-op
  fallback (byte-identical to `luma_weight = 1`).

- Round 4 (Lever E): **Linde-Buzo-Gray (LBG) split refinement**
  (`EncoderOptions::lbg_max_passes`, default `8`). After the median-cut
  + Lloyd warm-build builds the strip's V4/V1 codebook, the encoder now
  iteratively identifies the highest-distortion populated slot
  ("splitter") and the lowest-population slot ("donor"), replaces the
  donor with a perturbed copy of the splitter centroid (±1 along the
  splitter cluster's widest dimension), and runs one full Lloyd
  reassignment + recentroid pass. The pass terminates when total SSE
  doesn't strictly decrease — typically within 4..=8 passes on smooth
  content. Reference: Linde, Buzo, Gray (1980) "An Algorithm for Vector
  Quantizer Design", IEEE Trans. Communications 28(1) — published
  VQ-design math, no proprietary source consulted.

  Measured wins (self-encode + self-decode at q=50, PSNR_Y in BT.601 Y
  units; `lbg_max_passes = 0` is the round-3 baseline):

  | fixture                                       | r3 (lbg=0) | r4 (lbg=8) | delta     |
  | --------------------------------------------- | ---------- | ---------- | --------- |
  | 64×64 gradient, `encode_rgb24`                | 36.69 dB   | 37.85 dB   | +1.16 dB  |
  | 64×64 gradient, `encode_rgb24_best_strips`    | 40.23 dB   | 40.77 dB   | +0.53 dB  |
  | 320×240 gradient, `encode_rgb24`              | 35.61 dB   | 37.81 dB   | +2.19 dB  |
  | 320×240 gradient, `encode_rgb24_best_strips`  | 38.17 dB   | 40.69 dB   | +2.52 dB  |
  | 64×64 LCG-noise, `encode_rgb24`               | 21.54 dB   | 22.39 dB   | +0.85 dB  |
  | 64×64 LCG-noise, `encode_rgb24_best_strips`   | 22.09 dB   | 22.98 dB   | +0.89 dB  |

  Headline: **the 64×64 gradient via `encode_rgb24_best_strips` now hits
  40.77 dB Y at 2689 B** — a +3.87 dB lead over ffmpeg's reference
  encoder's ~36.9 dB on the same fixture (measured via
  `tests/ffmpeg_avi_roundtrip.rs`), and well past the 38 dB round-4
  target.

  Set `lbg_max_passes = 0` to disable LBG and recover the round-3
  baseline (used by the regression-guard tests in `r4_psnr.rs`).
  Cost per pass: O(N · K) entry distances (N = vectors, K = codebook
  size) — comparable to a single Lloyd iteration; the pass auto-stops
  when no split improves total SSE, so the bound is informational only
  on well-converged seeds.

- Round 4: `tests/r4_psnr.rs` — five-test PSNR validation suite
  asserting (i) `encode_rgb24_best_strips` on 64×64 gradient breaks
  38 dB PSNR_Y (observed 40.77 dB), (ii) 320×240 gradient stays
  ≥ 38 dB (no regression), (iii) noisy LCG fixture lifts by ≥ 0.5 dB
  vs the round-3 `lbg_max_passes = 0` baseline (observed +0.85 dB),
  (iv) gradient LBG-on vs LBG-off delta ≥ 1.0 dB (observed +1.16 dB),
  (v) `lbg_max_passes = 0` reproduces the round-3 baseline within
  noise.

- Round 3 (round-47): **Lagrangian V1/V4 RDO selection**
  (`EncoderOptions::rdo_lambda`, default `Some(5.0)`). The per-MB
  V1-vs-V4 decision now computes pixel-domain Y SSE for both
  candidate reconstructions and applies the Lagrangian `D + lambda · R`
  rule with a 24-bit rate delta favouring V1 (V4: 4 index bytes; V1:
  1 index byte; flag-bit cost identical). The legacy round-2 path
  compared raw codebook-distance sums (apples-to-oranges: V4 sums 4
  sub-block distances, V1 sums 1, so V1 wins by default on any
  non-pathological gradient and the encoder under-utilised V4's
  per-sub-block fidelity). Measured wins (self-encode + self-decode at
  q=50, PSNR_Y in BT.601 Y units):
  - 320×240 horizontal+vertical gradient: legacy 35.17 dB / 6787 B →
    RDO 35.61 dB / 8548 B (**+0.43 dB at +26% wire**).
  - 64×64 gradient (the `ffmpeg_avi_roundtrip.rs` fixture, ffmpeg's
    own encoder produces ~36.9 dB Y): legacy 35.59 dB / 1438 B → RDO
    36.69 dB / 1573 B (**+1.10 dB at +9% wire**, essentially parity
    with ffmpeg's reference encoder quality on this fixture).

  Set `rdo_lambda = None` to recover the round-2 codebook-distance
  comparison; existing tests that isolate other levers
  (`tests/round5_persistence_and_multistrip.rs::cross_frame_persistence_*`,
  `tests/round6_lloyd_and_window.rs::lloyd_max_iter_0_*`) pin this for
  measurement isolation.

- Round 3 (round-47): **per-frame strip-count picker**
  (`encode_rgb24_best_strips(rgb, w, h, opts, &[u16])`). Trial-encodes
  the input at each candidate strip count and returns the bitstream
  with the lowest Lagrangian cost `R + lambda · D`, where `R` is wire
  size in bytes and `D` is self-decode pixel-domain RGB MSE per pixel.
  Lambda is taken from `opts.rdo_lambda` (falls back to `0.0` when
  `None`, i.e. pick the candidate with the lowest MSE regardless of
  byte cost). On the 320×240 gradient fixture at q=50 the picker
  selects 4 strips (`38.17 dB PSNR_Y / 8568 B`) over the
  `EncoderOptions::from_quality(50)` default of 2 strips (35.61 dB /
  8548 B) — **breaks 38 dB PSNR_Y on a single intra frame** with no
  wire-size increase. Cost: N self-decodes per call (N = number of
  candidates), so use sparingly on a per-frame budget. Stateless;
  intra-only (cross-frame state isn't reset between trials).

- Round 3 (round-47) test: `tests/r3_psnr.rs` — five-test PSNR
  validation suite asserting (i) Lever-D RDO lifts PSNR_Y on both
  benchmark fixtures, (ii) Lever-A strip picker breaks 38 dB on the
  320×240 gradient, (iii) `rdo_lambda = None` legacy path remains
  decodable at the round-2 baseline, (iv) empty-candidates list is
  rejected by `encode_rgb24_best_strips`.

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
