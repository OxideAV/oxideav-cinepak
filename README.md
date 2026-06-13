# oxideav-cinepak

Pure-Rust Cinepak (CVID) video decoder for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Rounds 1 + 2 + 3 + 4 + 5 + 6 + 7 + r47-encoder-RDO + r4-LBG + r5-luma-weight + r6-encoder-PCL + r7-encoder-PCL + r8-per-strip-picker + r9-kmeans++-init + r93-deviant-saturn-decoder + r96-bitrate-target-rate-control + r101-grayscale-rd-grid-picker + r104-grayscale-inter-frame + r113-grayscale-rate-control + r121-chroma-CBR-convergence + r143-keyframe-interval + r148-decoder-fuzz + r155-ffmpeg-inter-cross-decode + r160-profile-driver + r187-film-seek-helpers + r192-decode-vector-chunk-fuzz + r196-decode-multi-frame-fuzz + r202-per-parser-fuzz-corpora + r209-profile-picker-sweep + r215-vintage-compat-encoder + r221-film-audio-format-classifier + r228-film-pcm-shaping + r234-film-pcm-fuzz + r240-frame-strips-iter + r243-strip-chunks-iter + r246-v1only-mb-iter + r250-mixed-intra-mb-iter + r253-inter-mb-iter + r256-codebook-entries-iter + r261-stab-samples-iter + r270-stab-header-parser + r289-profile-phase-decomposition — clean-room rebuild from `docs/video/cinepak/spec/`.**
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
mode) are unaffected. Target-bitrate mode (round 96) was `Yuv12`-scored
and originally did not apply to the grayscale path — round 113 closes
that gap (see below).
Round 113 (grayscale rate control) extended the round-96 target-bitrate /
per-frame byte-budget mode to the **stateful grayscale inter path**
(`CinepakEncoder::encode_intra_gray8` / `encode_inter_gray8`). The
round-96 budget worker — which sweeps an RD grid around the caller's
`opts` and commits the highest-quality candidate that fits the per-frame
budget, never erroring on overshoot — was hardcoded to the `Yuv12`
pipeline and BT.601 Y-channel SSE scoring. Round 113 threads `PixelMode`
through it so the grayscale path sweeps a two-axis `(strip_count,
rdo_lambda)` grid (≤ 9 trials; the `luma_weight` axis is a no-op on
`Gray8` entries, dropped per the round-101 rationale) and scores
candidates by direct-luma SSE (the 8-bit luminance *is* the Y channel).
Grayscale inter frames stay budgeted and skip / selectively-update
against the committed previous grayscale reconstruction. The colour
`encode_intra` / `encode_inter` budget path is byte-for-byte unchanged.
7 new tests cover budget adherence across an intra+inter grayscale
sequence, shrink-vs-unconstrained, impossible-budget overshoot, size
monotonicity, the inter budget riding the stateful carry-over, the
≤ 9-trial grayscale grid, and reset/clear semantics.
Round 143 (seek-friendly keyframe interval enforcement) added
**`CinepakEncoder::with_keyframe_interval(n)` + new `encode_frame` /
`encode_frame_gray8` auto-routing entry points** returning the new
`EncodedFrame { bytes, is_keyframe, frame_number_in_gop }` struct. The
encoder dispatches each call to `encode_intra` / `encode_inter`
automatically — frame `0`, `n`, `2n`, … are intra, the rest inter — so
container muxers (AVI / QuickTime / Sega FILM) can mark the per-frame
keyframe flag directly from `is_keyframe` without inspecting the bytes
themselves. Companion helpers: `set_keyframe_interval(n)` /
`clear_keyframe_interval()` / `keyframe_interval()` / `gop_position()`
plus a `force_next_keyframe()` one-shot for scene-cut refresh (request
an extra intra; the GOP counter resets at that frame so subsequent
keyframes are interval-spaced from the forced one). Defensive
auto-intra also fires on pixel-mode switch (`Rgb24` ⇒ `Gray8` or vice
versa), so the inter worker's grayscale-after-colour rejection can't
trip an auto-routed sequence. Composes with all existing rate-control
and persistence levers — the round-96 / round-121 budget worker drives
the RD grid toward the per-frame budget unchanged on both intra and
inter paths, cross-frame codebook persistence (round 5) and slot
reclamation (round 7) ride through inter frames as before, and
`reset()` preserves the configured interval (configuration knob, not
per-sequence state) while zeroing the GOP counter so the next call is
a clean intra. `keyframe_interval = None` (the default) keeps the
legacy manual-routing behaviour exactly: `encode_intra` /
`encode_inter` callers see no change, and `encode_frame` returns a
clear "set the interval first" error. The auto-router's `is_keyframe`
flag agrees with the first strip's `strip_id` on the wire (`0x1000`
intra / `0x1100` inter per spec §2.1 — the conformant intra/inter
dispatch signal). Rate control / GOP scheduling are encoder policy,
not bitstream features (spec `00-scope.md` §"Lossy-codec validation
criterion"); output stays conformant Cinepak. 13 new tests cover error-
without-interval, the period pattern at intervals 1 / 5, zero-interval
clamping, force-keyframe schedule reset + one-shot semantics, reset
preservation, clear-interval round-trip, end-to-end decode of an
interval-3 / 10-frame sequence, the grayscale period pattern, mode-
switch defensive intra, the `gop_position()` accessor, and composition
with `with_target_bitrate` (round 96).
Round 121 (chroma CBR convergence) added a **CBR carry-over accumulator**
to the round-96 / round-113 `encode_budget_frame` worker so a multi-frame
chroma (`encode_intra` / `encode_inter`) sequence's total bytes
**converges** to the bitrate target instead of systematically under-
shooting it. Round 96's per-frame cap kept individual frames ≤ budget but
discarded each frame's leftover; on representative content the picker
selects high-quality candidates well below cap, so a 5-second clip's
total bytes drifted ~20–30 % below `bits_per_second / 8 × duration`. Round
121 tracks `cumulative_target_bytes` and `cumulative_actual_bytes` inside
the encoder; each frame's effective budget becomes `base_budget +
clamp(cumulative_target − cumulative_actual, 0, cap)`, with `cap = 8 ×
base_budget` installed by default when `set_target_bitrate` /
`set_target_frame_bytes` is called (overridable via
`set_carry_over_cap_bytes` / `clear_carry_over_cap_bytes`). Deficits
(post-overshoot) propagate without a cap so overshoots are fully clawed
back. The selection rule is unchanged ("highest-quality candidate that
fits the effective budget"); only the budget the picker sees per frame
changes. Six new fields on `RateStats` (`effective_budget_bytes`,
`effective_byte_delta`, `within_effective_budget`,
`cumulative_target_bytes`, `cumulative_actual_bytes`) report the
post-carry-over view; the round-96 `target_bytes` / `byte_delta` /
`within_budget` fields still report the per-frame **base** budget for
backward compat. The carry-over machinery is shared with the round-113
grayscale path (same worker, same accumulator). Headline measurement on
a synthetic 320×240 moving-colour-gradient at q=50, 15 fps, 5 seconds
(75 frames, 1 intra + 74 inter), target 900 kbps ⇒ target_total
562 500 B: **converged to 562 411 B (rel_err = −0.02 %)** with
`min_psnr_y = 35.79 dB` across the clip. Six new tests cover the
headline convergence, cap-zero positive-surplus suppression, accumulator
correctness, `reset` / `reset_rate_carry_over` / `clear_target_bitrate`
semantics, cap clamp semantics + the default 8× installation, and the
mechanism applying identically to the grayscale path. Rate control is
encoder policy, not a bitstream feature — output stays conformant
Cinepak (`docs/video/cinepak/spec/00-scope.md` §"Lossy-codec validation
criterion" notes encoder-internal rate control is out of spec scope).
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

Round 155 (ffmpeg multi-frame inter cross-decode + frame-header `flags`
fix) closed a long-standing wire-format gap: every ffmpeg-as-decoder
test in the crate ran against **single-frame** AVIs through r154, so the
round-4 stateful selective-update (`0x2100` / `0x2300`) and chunk-
omission wire patterns the `CinepakEncoder` emits on inter frames had
never been validated against ffmpeg as a third-party decoder. Building
that test (`tests/ffmpeg_multi_frame_inter.rs` — a 5-frame 96×64 single-
GOP slow-pan fixture wrapped in a proper multi-entry-`idx1` AVI and
piped through ffmpeg) surfaced a spec-conformance bug in the encoder:
`assemble_frame` was hardcoding `flags = 0x00` in the 10-byte frame
header on every call, including inter frames. Spec
`docs/video/cinepak/spec/01-frame-and-strip.md` §1.1 (lines 88–99) +
`02-codebooks.md` §5.2 require `flags & 0x01` set on an inter frame to
advertise codebook inheritance from the previous strip / previous
frame's last strip; ffmpeg's decoder honours the bit strictly while our
own decoder is permissive and inherits unconditionally (which is why no
in-crate test caught it). The fix threads an explicit `flags: u8` arg
into `assemble_frame` — intra call sites pass `0`, inter call sites pass
`0x01`. With the fix in place the 5-frame inter cross-decode hits a
mean **34.18 dB PSNR** with every per-frame value ≥ 31.7 dB (intra
32.03 / inter 31.73 / 34.75 / 35.70 / 36.67 dB), exactly matching the
self-decode trace.

Round 160 (depth-mode profiling) added a standalone profiling driver
(`examples/profile_cinepak.rs`) plus a captured baseline under
`profile/README.md`. The driver is a flat measure-this-thing harness
designed for `samply` / `cargo flamegraph` capture against the four
cost-axis scenarios the round-126 Criterion benches use
(`rgb24/64x64/q50/round7`, `rgb24/320x240/q50/baseline`,
`rgb24/640x480/q70/baseline`, `gray8/320x240/q50/baseline`), plus a
stateful 5-frame GOP mode that exercises the picker + rolling-codebook
+ rate-control machinery the stateless `encode_*` entry points bypass.
The committed baseline (Apple M4 Max, release) anchors **decode at
3.2–4.4 GiB/s of raw output** (~3000× realtime at 320×240@30, no
measurable inner hot loop for algorithmic gains — further speed-ups
need SIMD), **stateless encode at ~24 ms/iter for 320×240/q50** (~9.1
MiB/s of raw input dominated by the picker + codebook trainer), and
the headline **stateful 5-frame GOP at ~67 ms/seq** (~13.5 ms/frame
mixing intra + 4 inter, ~7.6 KiB CVID per frame averaged). The
artefact's intent: persist a stable visual / numerical baseline that
future encoder-optimisation rounds can A/B-compare their algorithmic
tweaks against, without having to re-discover the capture recipe.

Round 289 (depth-mode profiling) added a `phases` mode to the same
driver that decomposes one stateless intra encode into the marginal
wall-time of each codebook-training phase — cold median-cut, Lloyd
refinement (`lloyd_max_iter`), LBG split-refinement (`lbg_max_passes`),
post-classification Lloyd polish (`pcl_max_iter`), and the k-means++
cold-start (`kmeans_pp_init`). It toggles each phase ON cumulatively
from a median-cut-only base and reports the ms each one adds, driving
only the public `encode_rgb24` entry point through public
`EncoderOptions` fields so no encoder code is instrumented and the wire
output is exactly what those option vectors already produce. The
finding (Apple M4 Max, release, in `profile/README.md`): **LBG
split-refinement owns the encoder hot path** — +15.4 ms on 320×240
(5.4× the median-cut base, ~62 % of full-options encode time) and
+64.9 ms on 640×480 — for a <5 % wire effect, because each of its 8
default passes re-runs a full Lloyd assignment over the O(vectors × K)
`nearest` scan. k-means++ is the second cost centre (+4.7 to
+21.8 ms; it builds two cold codebooks to guarantee no SSE
regression); Lloyd refinement and post-classification polish are
near-free. The artefact tells the next encoder-throughput round
exactly which phase to profile first; no behaviour changed this round.

Round 215 (vintage-decoder-compatible encoder mode) added
**`EncoderOptions::vintage_compat: bool`** (default `false`) — closes
the long-standing "produces conformant streams for vintage Windows /
MacOS Cinepak players" gap. Two `Cinepak.wiki` line 33 structural
constraints — quoted verbatim in `docs/video/cinepak/spec/01-frame-and-strip.md`
§2.3 (strip count ≤ 3) and `02-codebooks.md` §2.2 + §3.4 (both V4 and V1
codebook chunks always present per strip in strict V4-then-V1 order,
even when the codebook is unchanged) — are enforced when set:
`validate_opts` rejects `strip_count > 3` with a clear "vintage_compat
requires strip_count ≤ 3" message (rather than silently clamping the
caller's requested grid), and the rolling-codebook /
selective-update inter path now falls back to a **header-only chunk**
(`chunk_size = 0x0004`, 4 wire bytes) where the modern default
chunk-omission path would have emitted 0 bytes. The intra path is
already conformant — it always emits `0x2000`/`0x2400` then
`0x2200`/`0x2600` full-replace chunks per strip — so the new flag is a
no-op on intra-only sequences (wire-byte-identical to the default). On
the chunk-omission-heavy multi-strip inter sequences where the gain
matters, the wire-size overhead caps at `2 × 4 × strips × inter_frames`
extra bytes (one chunk header per otherwise-omitted chunk, two chunks
per strip), typically < 1 % of the bitstream. Decoder semantics are
identical: header-only and chunk-omitted forms both signal "inherit
previous codebook" per spec §3.4, so a vintage-compat-encoded stream
self-decodes to byte-identical pixels as the same input encoded with
the default path. 9 new tests cover the strip-count-3 ceiling
(accept) + strip-count-4 rejection (with-vintage / without), the
header-only chunk shape and V4-then-V1 ordering on inter strips, the
bounded wire-size overhead, intra-path no-op equivalence, the
multi-strip case where every inter strip carries exactly two
codebook chunks, and the decode-equivalence pixel check. Vintage
support is encoder policy, not a bitstream feature — the wire format
is conformant Cinepak in both modes.

Round 221 (FILM audio-format classifier) extended the Sega FILM
demuxer with a structured audio-stream classifier. The FDSC chunk
already exposed the four raw audio-metadata bytes
(`audio_channels` / `audio_bits` / `audio_compression` /
`audio_sample_rate`) per `Sega_FILM.wiki` lines 74–77, but
consumers wanting to route the interleaved audio payload to a PCM
sink had no first-class API to inspect the encoding rules
documented in wiki lines 147–169 (linear-PCM byte order,
sign convention, ADX ADPCM discriminator, no-audio sentinel).
The new `FilmAudioFormat` enum (`None` /
`LinearPcm { channels, bits_per_sample, sample_rate_hz,
endianness, sign_convention }` / `CriAdxAdpcm { channels,
sample_rate_hz }` / `Unknown { … }`) plus the `PcmEndianness`
(`BigEndian` for 16-bit per wiki line 153, `NotApplicable` for
8-bit) and `PcmSignConvention` (`TwosComplement` for Saturn
ASCII versions per wiki line 151, `SignMagnitude` for Sega CD /
3DO NULL versions per wiki line 162 + 224) discriminators
surface the platform-dependent encoding rules from the wire
metadata alone. Convenience: `Fdsc::has_audio()` /
`Fdsc::audio_format(&film_version)` /
`FilmDemuxer::audio_format()` /
`FilmDemuxer::audio_duration_seconds()` (sum of audio-sample
`sample_length` ÷ linear-PCM byte rate; returns `None` for
non-PCM compression or zero-rate fields) /
`FilmAudioFormat::byte_rate_bps()` (defined only for
`LinearPcm` with `bits_per_sample` a non-zero multiple of 8) /
`FilmAudioFormat::is_linear_pcm()`. Defensive validation:
`SampleRecord::is_well_formed_audio()` enforces wiki line 116's
"audio sample_info_2 is always 1" verbatim, and
`FilmDemuxer::first_malformed_audio_sample()` returns the index
of the first offender (or `None` if all audio rows are
well-formed). 14 new tests cover the no-audio sentinel + any-
field-set detection, Saturn 16-bit stereo big-endian
twos-complement, Saturn 8-bit `NotApplicable`-endianness, Sega CD
NULL-version sign/magnitude inference, the ADX ADPCM branch, the
unknown-compression preservation, byte-rate edge cases
(zero fields, non-multiple-of-8 bits), audio-duration summation
+ no-records / non-PCM fallback to `None`, the `sample_info_2`
validator, the first-malformed pinpoint, the audio classifier's
independence from video FDSC fields, and the abbreviated-FDSC
no-audio rule. Pure additive change to the FILM demuxer — no
encoder / decoder touches, no Cargo.toml changes.

Round 228 (FILM linear-PCM sample-data shaping) closes the natural
follow-up gap noted in r221's
[`FilmAudioFormat::LinearPcm`] docstring ("the consumer is
responsible for re-interleaving before passing to a typical PCM
playback API"). Round 221 surfaced the *metadata* about FILM PCM
payloads — channels, bits-per-sample, sample rate, big-endian byte
order, twos-complement vs sign/magnitude — but a consumer wanting
to route the bytes returned from `FilmDemuxer::audio_samples()` to
a generic PCM sink still had to write three byte transforms by
hand (sign-magnitude → twos-complement, 16-bit BE → host
endianness, half-chunk L/R → interleaved L R L R …). Round 228
adds those transforms as free functions on the `film` module plus
a `FilmAudioFormat::decode_chunk_to_i16(src) -> Option<Vec<i16>>`
one-shot accessor that dispatches across the four documented
combos (mono / stereo × 8-bit / 16-bit). All transforms are
documented in `docs/video/cinepak/reference/wiki/Sega_FILM.wiki`
lines 147–169 (line 151 Saturn = twos-complement, line 153 16-bit
= big-endian, lines 156–160 stereo half-chunk L/R split, lines
163–169 sign-magnitude rule with the verbatim wiki examples
`0x81 ⇒ -1` / `0xFF ⇒ -127`). 30 new tests cover the verbatim wiki
examples, `0x80` "negative zero" collapse to `0i8`, total-function
coverage of every input byte, the big-endian round-trip across the
full `i16` range, the channel re-interleave correctness, and every
length / size precondition (odd source length, non-multiple-of-4
length, dst-size mismatch, source/channel-count mismatch). PCM
playback (rate conversion, channel mixing, gain) and ADX ADPCM
decoding remain out of scope per `00-scope.md`; these helpers only
re-shape the documented FILM wire bytes into the format a generic
PCM sink expects. Pure additive change — no demuxer / decoder /
encoder behaviour change, no Cargo.toml changes.

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
  Round 113 wired these grayscale entry points into the round-96
  target-bitrate / per-frame byte-budget mode: when a budget is
  configured they sweep a two-axis `(strip_count, rdo_lambda)` grid
  (≤ 9 trials — the `luma_weight` axis is a no-op on `Gray8` entries)
  and commit the highest-quality candidate scored by direct-luma SSE
  that fits the budget, with adherence in `last_rate_stats()` and no
  error on overshoot. In quality-controlled mode (no budget) the
  caller's `opts` are still used verbatim.
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
  feature — output stays conformant Cinepak. Round 113 extended this
  budget mode to the grayscale `encode_intra_gray8` /
  `encode_inter_gray8` entry points: the grayscale path sweeps a
  two-axis `(strip_count, rdo_lambda)` grid (the `luma_weight` axis is
  a no-op on `Gray8` entries) and scores candidates by direct-luma SSE.
  Round 121 added a **CBR carry-over accumulator** so a multi-frame
  budget-driven sequence's total bytes **converge** to
  `bits_per_second / 8 × duration_s` (within ±10 % in practice; the
  headline 320×240 / 900 kbps / 5 s chroma fixture converges to
  −0.02 % at min_psnr_y 35.79 dB) rather than systematically
  under-shooting. Each frame's **effective** budget becomes
  `base_budget + clamp(cumulative_target − cumulative_actual, 0, cap)`,
  with `cap = 8 × base_budget` installed by default (overridable via
  `set_carry_over_cap_bytes(n)` / `clear_carry_over_cap_bytes()`).
  Deficits propagate without a cap so overshoots are fully clawed back.
  `RateStats` gained `effective_budget_bytes`, `effective_byte_delta`,
  `within_effective_budget`, `cumulative_target_bytes`,
  `cumulative_actual_bytes`; the original `target_bytes` / `byte_delta`
  / `within_budget` fields still report the per-frame **base** budget.
  Accessors: `CinepakEncoder::cumulative_target_bytes()`,
  `cumulative_actual_bytes()`, `carry_over_cap_bytes()`,
  `reset_rate_carry_over()`. `reset()` zeros the accumulator (per-
  sequence state) but preserves the budget and cap;
  `clear_target_bitrate()` zeros everything.
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
- `CinepakEncoder::with_keyframe_interval(n)` /
  `set_keyframe_interval(n)` / `clear_keyframe_interval()` /
  `keyframe_interval()` / `gop_position()` / `force_next_keyframe()` +
  `CinepakEncoder::encode_frame(rgb, w, h, opts) -> EncodedFrame` and
  `encode_frame_gray8(input, w, h, opts) -> EncodedFrame` (round 143)
  — **seek-friendly keyframe interval enforcement**. Configure an
  interval and the encoder routes each frame to intra / inter
  automatically (`I P P … P I P P …`, intra every `n`-th frame), and
  the returned `EncodedFrame { bytes, is_keyframe,
  frame_number_in_gop }` carries the keyframe-flag metadata container
  muxers need to mark the per-sample keyframe bit (AVI
  `AVIF_KEYFRAME`, QuickTime sync sample, Sega FILM `sample_info_1`
  per `01-frame-and-strip.md` §1.1) without re-inspecting the bytes.
  `force_next_keyframe()` requests a one-shot intra refresh that
  overrides the schedule and resets the GOP counter (useful for
  scene-cut response feeding back from
  `last_rate_stats().byte_delta > 0`). The router also defensively
  re-keyframes on pixel-mode switch (`Rgb24` ⇔ `Gray8`), so the inter
  worker's grayscale-after-colour rejection can't trip an auto-routed
  sequence. Composes with `with_target_bitrate` (round 96 / 121) —
  the budget worker drives the RD grid toward the per-frame budget
  on both the auto-routed intra and inter paths. `reset()` preserves
  the interval (it's a configuration knob, not per-sequence state)
  and zeroes the GOP counter so the next call is a clean intra.
  Default `keyframe_interval = None` preserves the legacy manual-
  routing behaviour exactly: `encode_intra` / `encode_inter` callers
  see no change, and `encode_frame` returns a clear "set the interval
  first" error. Output stays conformant Cinepak — GOP scheduling is
  encoder policy, not a bitstream feature (spec `00-scope.md`
  §"Lossy-codec validation criterion").
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
- `EncoderOptions::vintage_compat` (round 215, default `false`) —
  **vintage Windows / MacOS Cinepak player compatibility mode**.
  Enforces the two `Cinepak.wiki` line 33 structural constraints
  quoted in `docs/video/cinepak/spec/01-frame-and-strip.md` §2.3 +
  `02-codebooks.md` §2.2 / §3.4: (i) `strip_count` is capped at `3`
  (rejected with a clear error at `validate_opts` time rather than
  silently clamped); (ii) inter strips that would otherwise
  chunk-omit (decoder inherits previous codebook, 0 wire bytes per
  spec §3.4) instead emit a **header-only chunk**
  (`chunk_size = 0x0004`, 4 wire bytes) so each strip always carries
  both V4 then V1 codebook chunks in strict V4-then-V1 order — the
  shape vintage MacOS players insist on. The intra path is already
  conformant (always emits `0x2000`/`0x2400` then `0x2200`/`0x2600`
  full-replace per strip), so the flag is a no-op on intra-only
  sequences. Wire-size overhead is bounded at
  `2 × 4 × strips × inter_frames` bytes (one chunk header per
  otherwise-omitted chunk × V4+V1 per strip). Header-only and
  chunk-omitted forms are decoder-equivalent — both signal "inherit
  previous codebook" per spec §3.4 — so a vintage-compat-encoded
  stream self-decodes to byte-identical pixels as the same input
  encoded with the default path. Set `vintage_compat = true` only
  when the produced stream must replay on a vintage player; modern
  decoders (including this crate's [`CinepakDecoder`] and FFmpeg
  7.1.2) accept either form. Vintage support is encoder policy, not
  a bitstream feature — output stays conformant Cinepak in both
  modes.
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
- `Fdsc::has_audio` / `Fdsc::audio_format(version)` /
  `FilmDemuxer::audio_format` / `audio_duration_seconds` /
  `FilmAudioFormat` (`None` / `LinearPcm { … }` / `CriAdxAdpcm { … }`
  / `Unknown { … }`) / `FilmAudioFormat::byte_rate_bps` /
  `FilmAudioFormat::is_linear_pcm` / `PcmEndianness`
  (`BigEndian` per wiki line 153 for 16-bit, `NotApplicable` for
  8-bit) / `PcmSignConvention` (`TwosComplement` for Saturn
  ASCII versions per wiki line 151, `SignMagnitude` for Sega CD /
  3DO NULL versions per wiki line 162 + 224) /
  `SampleRecord::is_well_formed_audio` /
  `FilmDemuxer::first_malformed_audio_sample` (round 221) —
  structured audio-stream classifier per `Sega_FILM.wiki` lines
  147–169 + 116. Audio codec decoding stays out of scope for this
  crate (linear PCM and CRI ADX ADPCM are separate concerns), but
  these helpers give consumers the wire metadata they need to
  route the bytes pulled out via `audio_samples()` to a
  PCM / ADPCM sink with the correct byte order, sign convention,
  and byte rate.
- `FilmDemuxer::audio_samples` / `keyframes` /
  `seek_keyframe_for_tick(target_ticks)` / `duration_ticks` /
  `duration_seconds` + `SampleRecord::next_frame_ticks` (round 187)
  — seek-friendly accessors per `Sega_FILM.wiki` lines 104, 110–116.
  `audio_samples` mirrors `video_samples`. `keyframes` iterates
  only video keyframes (the records playback engines must restart
  decoding from; wiki line 104 explicitly calls out the seek use
  case). `seek_keyframe_for_tick` returns the keyframe whose
  timestamp is the largest value ≤ the requested tick — the
  canonical snap-to-keyframe seek primitive. Tolerates non-sorted
  sample tables (linear O(n) scan over the keyframe subset).
  `duration_ticks` returns `max(ts) + next_frame_ticks(last)` per
  wiki line 116; `duration_seconds` divides by
  `StabHeader::base_frequency` (returns `None` on a degenerate
  zero base). `SampleRecord::next_frame_ticks` surfaces
  `sample_info_2` as the video-only ticks-until-next-frame field
  the wiki documents.

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

## Benchmarks (round 126)

Round 126 wired up three `criterion` benchmark harnesses so future
encoder-optimisation rounds can A/B-test their picker / codebook-
training tweaks. Each harness is self-contained (no committed
fixtures — inputs are synthesised on-the-fly with a deterministic
xorshift32 gradient) and the criterion dependency is dev-only.

- `benches/decode.rs` — decoder hot paths: 320×240 / 64×64 / 640×480
  intra, 320×240 grayscale, and a 320×240 all-SKIP inter case.
- `benches/encode.rs` — encoder picker tiers: stateless `encode_rgb24`
  baseline + `encode_rgb24_round6` (~9 trials) + `encode_rgb24_round7`
  (~27 trials), plus the round-101 grayscale picker.
- `benches/roundtrip.rs` — stateful `CinepakEncoder` + `CinepakDecoder`
  intra+inter sequences (static + slow-drift content; RGB + grayscale).

Run with `cargo bench -p oxideav-cinepak --bench {decode,encode,roundtrip}`
(or `--bench <name> -- --quick` for a fast sanity check). Indicative
release-mode numbers on the development machine (Apple M-class, single
thread; criterion `--quick`):

The decode rows below reflect the **post-round-129 decoder** (the
round-126 figures pre-dated the r129 hot-path rewrite); re-measured in
round 134 with `--warm-up-time 1 --measurement-time 3`.

| Bench                                     | Per-iter time  | Throughput      |
| ----------------------------------------- | -------------- | --------------- |
| `decode rgb24 320×240 q=50`               | ≈  60 µs       | ≈ 3.6 GiB/s     |
| `decode rgb24 64×64 q=50`                 | ≈  3.1 µs      | ≈ 3.7 GiB/s     |
| `decode rgb24 640×480 q=70`               | ≈ 211 µs       | ≈ 4.1 GiB/s     |
| `decode gray8 320×240 q=50`               | ≈  25 µs       | ≈ 2.8 GiB/s     |
| `decode rgb24 320×240 inter-allskip`      | ≈ 112 µs (2-fr)| ≈ 1.9 GiB/s     |
| `encode rgb24 64×64 q=50 baseline`        | ≈ 1.4 ms       | ≈ 8.4 MiB/s     |
| `encode rgb24 64×64 q=50 round6 (~9×)`    | ≈ 11.1 ms      | ≈ 1.0 MiB/s     |
| `encode rgb24 64×64 q=50 round7 (~27×)`   | ≈ 31.5 ms      | ≈ 380 KiB/s     |
| `encode rgb24 320×240 q=50 baseline`      | ≈ 25.0 ms      | ≈ 8.7 MiB/s     |
| `encode gray8 320×240 q=50 baseline`      | ≈ 13.0 ms      | ≈ 5.6 MiB/s     |
| `encode gray8 320×240 q=50 round7 (~9×)`  | ≈ 113.8 ms     | ≈ 650 KiB/s     |
| `roundtrip rgb24 64×64 static (5 frames)` | ≈ 3.7 ms       | ≈ 15.6 MiB/s    |
| `roundtrip rgb24 64×64 drift (5 frames)`  | ≈ 6.1 ms       | ≈ 9.6 MiB/s     |
| `roundtrip rgb24 320×240 (4 frames)`      | ≈ 61.4 ms      | ≈ 14.3 MiB/s    |
| `roundtrip gray8 128×96 static (5 frames)`| ≈ 2.4 ms       | ≈ 24.3 MiB/s    |

The encode-side numbers confirm the round-tier multiplier: `round6 ≈ 8×
baseline`, `round7 ≈ 23× baseline` on the 64×64 fixture — matching the
9 / 27 trial-encode counts within constant factors (some trials hit
cheaper grid points). These tables are descriptive (per-machine
variation is expected); future optimisation rounds should diff the
absolute `criterion` JSON output rather than the README headline.

### Round 129 decoder optimisation deltas (vs the round-126 baseline)

Round 129 used the round-126 bench harness to A/B four decoder
hot-path changes: (i) **per-pixel mode dispatch hoisted out of the inner
loop** — `render_strip` now splits into `render_strip_rgb` /
`render_strip_gray` at the top, so each macroblock writes through a
mode-specialised draw helper instead of running a `PixelMode` match per
pixel (≥ 1 200 redundant matches per 64×64 frame); (ii) **V1 macroblock
collapsed from 16 to 4 `yuv_to_rgb` calls** — V1 entries share one
`(U, V)` across the whole 4×4 so only 4 distinct RGB triples need
converting, with each Yi covering a 2×2 quadrant written via two
12-byte `copy_from_slice` rows; (iii) **direct grayscale buffer
allocation** — when the first strip's chunk pins `Gray8` the output
plane is shrunk to width×height in place, eliminating the post-decode
3-byte → 1-byte repack scan that dominated the grayscale path; and
(iv) **codebook clones eliminated** — per-strip
`self.prev_v4.clone()` (~1.5 KiB per codebook × 2 × strips) replaced
with a `take()` + put-back so the carry-over codebooks move into the
per-frame state without allocating.

Measured deltas against the saved `pre` baseline
(`cargo bench --bench decode -- --baseline pre`):

| Bench                                  | Baseline   | After      | Δ time   | Throughput Δ |
| -------------------------------------- | ---------- | ---------- | -------- | ------------ |
| `decode rgb24 320×240 q=50`            | 78.4 µs    | 62.7 µs    | -20.2 %  | +25.3 %      |
| `decode rgb24 64×64 q=50`              |  4.09 µs   |  3.11 µs   | -23.7 %  | +31.1 %      |
| `decode rgb24 640×480 q=70`            | 256 µs     | 212 µs     | -16.7 %  | +20.0 %      |
| `decode gray8 320×240 q=50`            | 77.3 µs    | 25.7 µs    | -66.8 %  | +201.2 %     |
| `decode rgb24 320×240 inter-allskip`   | 140 µs     | 115 µs     | -18.0 %  | +21.9 %      |

The grayscale path moves from ≈ 947 MiB/s to ≈ 2.79 GiB/s — close to
parity with the RGB path now that no post-decode compaction is needed.
RGB paths gain 17–25 % from the V1 conversion-count collapse and the
mode-dispatch hoist. All 66 + 8 + 3 + … synth-decode / encoder /
roundtrip tests stay bit-exact; the optimisation is purely a hot-path
rewrite, not a behavioural change.

## Fuzz harness (rounds 148 + 175 + 181 + 192 + 196 + 234)

Round 148 added a `cargo-fuzz` harness under `fuzz/` covering the
four user-reachable parse / decode entry points on the public API.
Round 175 extended it to a fifth target so the deviant-frame path
gains its own coverage budget instead of relying on the standard
path's harness to incidentally exercise it. Round 181 added a
sixth target driving the codebook-chunk parser directly, since the
eight-variant `0x2000..0x2700` family had to be reached through a
full frame + strip + chunk header in the integrated path. Round
192 added a seventh target driving the vector-chunk parser
directly for the same reason — the three codes (`0x3200` / `0x3000`
/ `0x3100`) are the other large per-parser surface in the decoder,
and `0x3100`'s bit-grammar straddle path is the bug surface that
motivated the `inter_payload_straddle` regression test and the
round-5 selector-spillover fix. Round 196 added an eighth target
driving the **stateful** decoder over a sequence of frames — the
inter-frame state machine (rolling V1/V4 codebooks +
selective-update inheritance across `decode_frame` calls +
`0x3100` skip-MB copies from the prior frame's reconstructed
raster) was never exercised by the single-frame `decode_frame`
target since it instantiates a fresh `CinepakDecoder` per input.
Round 234 added a ninth target driving the round-228 FILM PCM
shaping helpers (`pcm_decode_8bit`, `pcm_decode_16be_to_i16`,
`pcm_deinterleave_stereo_8bit`, `pcm_deinterleave_stereo_16be`,
`pcm_sign_magnitude_to_i8`) plus the `FilmAudioFormat::decode_chunk_to_i16`
dispatcher — these surfaces consume STAB-indexed audio sample
bytes, a wire surface the `film_demuxer_parse` target exits before
reaching and the `decode_frame` family doesn't touch:

| Target | Surface |
|--------|---------|
| `frame_header_parse` | `header::FrameHeader::parse` — 10-byte frame header |
| `strip_header_parse` | `header::RawStripHeader::parse` — 12-byte strip header |
| `decode_frame` | `CinepakDecoder::decode_frame` — full intra/inter decode pipeline (standard path, single frame per input) |
| `decode_deviant_frame` | `CinepakDecoder::decode_deviant_frame` — Saturn / Sega CD / Lemmings 3DO `'cvid'` deviant paths (round 175) |
| `film_demuxer_parse` | `FilmDemuxer::parse` — Sega FILM container parse |
| `codebook_chunk_apply` | `codebook::apply_codebook_chunk{,_with}` — eight chunk-kind family (V4 vs V1 × full-replace vs selective-update × 12-bit-YUV vs 8-bit-grayscale) parsed directly (round 181) |
| `decode_vector_chunk` | `vector::decode_vector_chunk` — three vector codes (`0x3200` V1-only, `0x3000` mixed intra, `0x3100` inter with skip codes + selector-bit straddle) parsed directly (round 192) |
| `decode_multi_frame` | `CinepakDecoder::decode_frame` driven over a length-prefixed sequence (up to 8 frames per input) on a single decoder instance — rolling V1/V4 codebooks, selective-update inheritance across `decode_frame` calls, `0x3100` skip-MB copy from `prev_frame`, intra-after-inter inheritance wipe, `reset()` (round 196) |
| `film_pcm_decode` | `pcm_decode_8bit` / `pcm_decode_16be_to_i16` / `pcm_deinterleave_stereo_8bit` / `pcm_deinterleave_stereo_16be` / `pcm_sign_magnitude_to_i8` + `FilmAudioFormat::decode_chunk_to_i16` — FILM linear-PCM sample-data shaping across `(8, 1)` / `(8, 2)` / `(16, 1)` / `(16, 2)` LinearPcm cells, both sign conventions, and the `None`-on-unsupported-combo arm (round 234) |

`decode_frame`, `decode_deviant_frame`, and `decode_multi_frame`
all peek at the wire `width`/`height` (bytes 4..8) and short-circuit
inputs above a 256 × 256 coded-pixel budget so the ~12 GiB
worst-case `u16 × u16` RGB24 raster doesn't OOM the runner; the
structural parse harnesses cap raw input at 64 KiB. The
`decode_vector_chunk` target also caps `mb_count` at 4096 since
`decode_vector_chunk` allocates a `Vec<Mb>` of that length before
the per-chunk arithmetic decides whether the payload is well-formed.
`decode_multi_frame` additionally caps the per-input frame count at
8 so the wall-time per fuzz iteration stays comparable to the
single-frame target's. `film_pcm_decode` caps its raw input at the
same 64 KiB budget so the worst-case `Vec<i16>` allocation from a
16-bit decode tops out at ~128 KiB. `.github/workflows/fuzz.yml` is
a thin shim around the org-level reusable fuzz workflow with the
daily budget split evenly across the nine targets.

Round 196 also seeds `corpus/decode_multi_frame/` with two
encoder-round-tripped streams (intra + 1 inter, intra + 2 inters)
at 32 × 32 — these give libFuzzer a structurally-valid starting
point for mutating the inter-frame state machine instead of having
to first synthesise a well-formed Cinepak frame from scratch. A
60-second local run after seeding reached 7.94 M executions
(~130 k execs/s, +1 415 new corpus units) with no crashes.

Round 202 extended the named-seed pattern to the three per-parser
targets that landed in r181 / r192 / r175 without committed
corpora — `codebook_chunk_apply` (18 seeds: every chunk-id leaf in
the 8-code V4/V1 × Full/Selective × YUV/Gray family, both under
`tolerate_trailing = false` and `tolerate_trailing = true` with 2
trailing pad bytes, plus a header-only "inherit previous codebook"
seed and a deliberately-truncated 5-byte payload), `decode_vector_chunk`
(6 seeds: V1-only intra at 4 MBs, mixed V1/V4 intra at 4 MBs and at
33 MBs spanning two flag-word groups, inter all-skip at 32 MBs,
inter with a 34-MB selector-spillover pattern matching the
`inter_payload_straddle` regression test, and an unknown-id 0x0000
negative seed for the dispatcher's reject path), and
`decode_deviant_frame` (3 seeds: Saturn-prefix + Lemmings-3DO-prefix
deviant frames with the spec §3.1 codebook-pad branch live, plus a
standard-control 4×4 frame so libFuzzer can A/B the strict-vs-deviant
branches directly). Generation is reproducible via
`cargo run --example seed_fuzz_corpora --release`; the seeds are
deterministic so re-running the generator produces byte-identical
files. A new `tests/seed_fuzz_corpora.rs` integration test drives
every committed seed through the same public entry points the fuzz
harnesses invoke (`apply_codebook_chunk{,_with}`, `decode_vector_chunk`,
`CinepakDecoder::{decode_deviant_frame,decode_frame}`) and asserts
the expected counts of positive-arm seeds per target, so a future
encoder / parser refactor that silently moves the wire surface the
seeds were drawn from will trip in `cargo test` rather than after
deploying to fuzz CI.

The `decode_deviant_frame` target loops over the three documented
`DeviantConfig` permutations per input — `saturn()`, `lemmings_3do()`,
and the standard-path control via `decode_frame` — so libFuzzer can
shape mutations against the specific deviant-vs-standard branches:
the `extra_header_bytes` prefix (2 vs 6 vs 0), the
`frame_length_short_by` undercount (8 vs 8 vs 0), and the
`tolerate_codebook_pad` chunk-payload tolerance (`true` vs `true`
vs `false`).

Initial 90-second local run of the four targets (`~22M execs / target`)
surfaced two real subtract-with-overflow bugs in the decoder, both now
fixed with regression unit tests in `src/decoder.rs`:

- `rejects_strip_with_x_top_above_x_bottom` — strip with wire
  `x_top > x_bottom` would underflow `sx1 - sx0` at the modulo-4
  check.
- `rejects_strip_with_y_top_above_y_bottom` — strip with resolved
  `actual_y_top > actual_y_bottom` would underflow inside
  `StripHeader::height()` (which also gained a `saturating_sub` belt
  even though the decoder now rejects before reaching it).

After both fixes a follow-up 90-second `decode_frame` run hits ~19.7M
executions with no further crashes.

Round 240 added a **`FrameStrips<'a>` strip-header iterator** at
`header::FrameStrips`, implementing steps 2.1–2.4 of the
`docs/video/cinepak/spec/01-frame-and-strip.md` §3 decoder
algorithm for the spec-standard (non-deviant) frame layout. The
iterator yields a `Result<StripEntry>` per strip, where
`StripEntry` carries the 0-based strip index, the resolved
`StripHeader` (with the spec §2.2 y-coordinate sentinel rule
applied against a private `prev_y_bottom` accumulator), and a
zero-copy `&[u8]` slice covering the strip's chunk-stream
payload (the `strip_size - 12` bytes that follow the 12-byte
strip header). The iterator is read-only: it does not touch
codebooks, vector chunks, or pixels — useful for container code
that wants to enumerate strip-level rectangles + payload
boundaries (e.g. an AVI consumer probing per-strip sizes) and
for validators / fuzz harnesses that need to bound per-strip
mutation independently. Error semantics are one-shot fuse: on
the first malformed strip the iterator yields `Some(Err(_))`
once and then `None` for every subsequent call, so callers do
not need to track an external "is fused" flag. Construction
(`FrameStrips::new(bytes)`) rejects buffers shorter than the
declared `frame_length`; `header()` and `remaining()` /
`size_hint()` accessors round out the typed surface. Coverage:
10 new tests under `header::tests` exercise the spec §2.2
sentinel rule across a synthetic 3-strip frame, the first-strip
literal-coordinate exemption, the `O1` single-strip pattern,
the payload-slice contract (length + content), the spec §3
"sum of payload lengths" invariant (`Σ payload_len ==
frame_length - 10 - 12·strip_count`), strip-header truncation
fuse, and strip-size-overrun fuse. Deviant streams (Sega FILM
Saturn `'cvid'` + Lemmings 3DO 6-byte prefix per
`docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 125–143
and 189) are not in this iterator's scope — those carry an
extra header prefix and/or short `frame_length` that the
standard §1 frame header does not describe; the existing
`CinepakDecoder::decode_deviant_frame` continues to own that
path. Pure additive change — no behaviour change to the
existing decoder, encoder, or FILM demuxer code paths; the
iterator is a new layer over the same `FrameHeader::parse` and
`RawStripHeader::parse` primitives the decoder already calls.

Round 243 added the next layer down — a **`StripChunks<'a>`
chunk-stream iterator** at `codebook::StripChunks` implementing
spec §1 + §2 of `docs/video/cinepak/spec/02-codebooks.md` (common
4-byte chunk header + codebook chunk taxonomy) plus spec §2 of
`docs/video/cinepak/spec/03-vectors-and-macroblocks.md` (vector
chunk taxonomy `0x3000` / `0x3100` / `0x3200`). `StripChunks::new`
takes a strip-payload slice — the `strip_size - 12` bytes that
follow the 12-byte strip header, equivalently the
`StripEntry::payload` slice from r240's `FrameStrips` — and yields
one `Result<StripChunkEntry>` per declared chunk. Each entry
carries the 0-based chunk index, the classified `StripChunkKind`
(codebook vs vector via a new `StripChunkKind::from_id` dispatch),
the raw 16-bit big-endian `chunk_id`, the declared `chunk_size`,
and a zero-copy `&[u8]` slice covering the `chunk_size - 4`
payload bytes. Two new classification types pair this with the
existing `CodebookChunkKind`: `VectorChunkKind` (enumerates
`IntraMixed` / `InterWithSkip` / `IntraV1Only` with bidirectional
`from_id` / `to_id`) and `StripChunkKind` (sum of
`CodebookChunkKind` + `VectorChunkKind`). The iterator is
read-only and content-agnostic — it walks chunk boundaries by
`chunk_size` arithmetic alone, leaving `apply_codebook_chunk` and
the vector chunk decoder out of the call path — so validators,
fuzz harnesses, and wire-format introspection tools can take this
single dependency in place of the full codebook + vector decode
stack. Error semantics are the same one-shot fuse pattern as
r240: a malformed chunk yields `Some(Err(_))` once and then
`None` for every subsequent call, covering truncated header,
`chunk_size < 4`, payload overrun, and unrecognised `chunk_id`
cases. Coverage: 13 new tests under `codebook::tests` exercise
`VectorChunkKind` classification + roundtrip, `StripChunkKind`
dispatch across the codebook / vector grid, the spec §3.4
fixture `T4` natural inter-reuse pattern (two header-only
codebook chunks + inter vector chunk), the spec §3.1 fixture
`T1a` byte-exact V1 entry slice (`48 48 48 48 db 5a`) paired
with a `0x3200` V1-only vector chunk, the spec §1
`Σ declared_size == payload.len()` invariant, all four fuse
paths, and a partial-walk case where two `Ok` chunks land before
the third errors. New crate-root exports: `StripChunks`,
`StripChunkEntry`, `StripChunkKind`, `VectorChunkKind`. Pure
additive change — the existing `decode_strip_chunks` inline
walk inside `decoder.rs` continues to drive the decoder's hot
path; r243 is a layered surface for callers that want to grep
the chunk stream without the codebook + vector decode dependency.

Round 246 added the **next layer below r243** — a typed
**`V1OnlyMacroblocks<'a>` per-macroblock walker** at
`vector::V1OnlyMacroblocks`, implementing spec §3.1 of
`docs/video/cinepak/spec/03-vectors-and-macroblocks.md` (the
`0x3200` V1-only vector chunk: no flag word, one byte per
macroblock, each byte a V1 codebook index in row-major scan
order per spec §1.1). `V1OnlyMacroblocks::new(payload, mb_count)`
checks the spec §3.1 length-equality invariant
(`payload.len() == mb_count`) up-front; the resulting iterator
is then infallible — yields exactly `mb_count`
`V1MacroblockEntry { index, codebook_index }` values and then
`None`. The intended composition is to take a
`StripChunkEntry::payload` slice from r243 whose `kind`
resolves to `VectorChunkKind::IntraV1Only` and feed it
straight into `V1OnlyMacroblocks::new`, so the chunk-stream
layer and the per-macroblock layer share a zero-copy contract.
The walker is read-only and content-agnostic — codebook
expansion (spec §4) and pixel writes stay in
`decoder::decode_strip_chunks`'s hot path. `cursor()`,
`remaining()`, `mb_count()`, and `payload()` accessors round
out the typed surface; `Iterator::size_hint` and
`ExactSizeIterator::len` both report the exact remaining
count. Coverage: 10 new tests under `vector::tests` exercise
scan-order index emission, the spec §3.1 fixture `Y6`
(16-MB strip → 16-byte payload, per-MB-distinct indices), the
spec §3.1 fixture `Y11` worked example (64-MB strip → 64-byte
payload), an empty-strip happy path (`mb_count == 0`),
payload-shorter-than-`mb_count` + payload-longer-than-
`mb_count` rejection at construction, `size_hint` exactness
through full consumption, `ExactSizeIterator::len` honesty,
the `cursor` / `remaining` advance lock-step contract, and a
cross-check that the iterator yields the same V1 codebook
indices as `decode_vector_chunk(0x3200, payload, mb_count)`.
New crate-root exports: `V1MacroblockEntry`, `V1OnlyMacroblocks`.
Pure additive change — `decode_vector_chunk` continues to own
the bulk per-chunk decode for `0x3000` / `0x3100` / `0x3200`;
r246 is a typed layered surface for callers (validators, fuzz
harnesses, wire-format introspection tools) that want to walk
the macroblock-level coverage of the simplest of the three
vector-chunk codes without the codebook + V1-expansion
dependency.

Round 250 added the **spec §3.2 mirror of r246** — a typed
**`MixedIntraMacroblocks<'a>` per-macroblock walker** at
`vector::MixedIntraMacroblocks` for the `0x3000` intra-mixed
vector chunk. Wire-grammar reference: spec §3.2 of
`docs/video/cinepak/spec/03-vectors-and-macroblocks.md` — a
sequence of one-or-more groups, each starting with a 4-byte
big-endian flag word whose 32 bits (scanned MSB-first)
classify each macroblock as V1 (bit clear ⇒ 1 index byte) or
V4 (bit set ⇒ 4 index bytes); a group covers exactly 32
macroblocks unless the strip's macroblock count is exhausted
before 32. `MixedIntraMacroblocks::new(payload, mb_count)`
returns an iterator that yields one
`MixedIntraEntry { index, kind }` per macroblock, where
`kind` is `MixedIntraMb::V1(u8)` or `MixedIntraMb::V4([u8; 4])`
matching the spec §3.2 selector semantics. Intended
composition mirrors r246: a `StripChunkEntry::payload` slice
from r243 whose `kind` resolves to
`VectorChunkKind::IntraMixed` feeds straight into
`MixedIntraMacroblocks::new`, completing the per-MB typed
surface for the two intra vector-chunk codes (`0x3200` /
`0x3000`). Unlike r246's V1-only walker, per-group byte
sizes depend on the in-group V1/V4 mix, so length-consistency
can only be checked during the walk; truncation (mid-flag-word
/ mid-V1-index / mid-V4-index) is reported per-yield as
`Some(Err(_))` and the iterator fuses to `None` afterwards.
Coverage: 12 new tests under `vector::tests` exercise spec
§3.2 fixtures `Y9` (all-V4 16-MB strip, flag word
`0xffff0000`), `Y12` (checkerboard V1/V4, flag word
`0x5a5a0000`), and `Y14` (64-MB strip, two `0xffffffff` flag
words across the group-refill path), the all-V1 case (flag
word `0x00000000`), empty-strip (`mb_count == 0`), the three
truncation-fuse paths, `size_hint` exactness, the
`cursor`/`remaining` per-yield advance contract, a cross-check
against `decode_vector_chunk(0x3000, …)` for `Y12`, and a
group-boundary stress at exactly 32 MBs (33-MB strip forces a
second flag word). New crate-root exports: `MixedIntraEntry`,
`MixedIntraMacroblocks`, `MixedIntraMb`. Pure additive
change.

Round 253 closed the typed per-MB walker surface with the
**spec §3.3 inter sibling** of r246 + r250 — a typed
**`InterMacroblocks<'a>` per-macroblock walker** at
`vector::InterMacroblocks` for the `0x3100` inter-with-skip
vector chunk. Wire-grammar reference: spec §3.3 of
`docs/video/cinepak/spec/03-vectors-and-macroblocks.md`. Each
macroblock is encoded as a 1- or 2-bit variable-length code
packed MSB-first into the flag-word stream: `0` ⇒ SKIP (reuse
the previous frame's reconstructed 4×4 block at the same pixel
position; spec §6), `10` ⇒ V1 (1 index byte follows in the
group's index data), `11` ⇒ V4 (4 index bytes follow). Codes
can straddle a flag-word boundary: when a flag word ends on a
lone `1` (a "set" indicator), the V1 / V4 selector bit is read
from the **next** flag word's MSB and the deferred macroblock's
index bytes belong to that next group's index data block (spec
§3.3 steps 3 + 5, the `pending_set` rule).
`InterMacroblocks::new(payload, mb_count)` returns an iterator
that walks one group at a time — loading the flag word,
classifying up to 33 macroblocks (one resolved-pending entry +
up to 32 fresh codes) into an internal scan-order buffer,
reading the group's index data from the payload, then yielding
one `InterEntry { index, kind }` per macroblock where `kind`
is `InterMb::Skip` / `InterMb::V1(u8)` / `InterMb::V4([u8; 4])`
matching the spec §3.3 grammar. Intended composition mirrors
r246 and r250: a `StripChunkEntry::payload` slice from r243
whose `kind` resolves to `VectorChunkKind::InterWithSkip` feeds
straight into `InterMacroblocks::new`, completing the per-MB
typed surface across all three vector-chunk codes (`0x3200` /
`0x3000` / `0x3100`). Like r250, per-group byte sizes depend
on the in-group SKIP / V1 / V4 mix and the `pending_set` state
so length-consistency can only be checked during the walk;
truncation (mid-flag-word / mid-V1-index / mid-V4-index) is
reported per-yield as `Some(Err(_))` and the iterator fuses to
`None` afterwards. A dangling `pending_set` after the final
macroblock surfaces a per-yield error as well. Coverage: 12
new tests under `vector::tests` exercise spec §3.3 fixture
`Y8` (16-MB strip, 1 V1 update + 15 SKIPs, flag word
`0x80000000`, 5-byte payload), `Y10` (32-MB strip, 2-flag-word
layout exercising the 32-MB group-refill path), an all-SKIP
32-MB strip (single 4-byte flag word, no index data), a single
V4 update + 14 SKIPs (rich V4-vs-SKIP mix), empty-strip
(`mb_count == 0`), the three truncation-fuse paths,
`size_hint` exactness, the `cursor` per-yield advance contract,
a cross-check against `decode_vector_chunk(0x3100, …)` for a
64-MB mixed-mix payload that forces multiple
selector-spillover boundaries, and an explicit `pending_set`
straddle stress (31 SKIPs + V1 + SKIP) that crosses the 32-bit
flag-word boundary inside the V1's `10` code. New crate-root
exports: `InterEntry`, `InterMacroblocks`, `InterMb`. Pure
additive change — the existing `decode_vector_chunk` continues
to own the bulk per-chunk decode, `decode_strip_chunks` continues
to drive the decoder's hot path; r253 is the typed layered
surface for validators / fuzz harnesses / wire-format
introspection tools that want to walk the macroblock-level
coverage of the inter vector-chunk code without the codebook
+ V1/V4-expansion + SKIP-reconstruction dependency.

Round 270 added the **container-side bridge to the r261
`Samples` walker** — `StabHeader::parse_chunk(chunk) ->
(StabHeader, &[u8])` at `src/film.rs` plus the
`STAB_HEADER_SIZE = 16` constant. The r261 `Samples` iterator
walks a STAB *records-only* byte slice, but expected the caller
to compute that slice's offset
(`FILM_HEADER_MIN_SIZE + fdsc_len + 16`) and trim it by hand.
`parse_chunk` takes a STAB chunk starting at its `'STAB'`
signature, parses the fixed 16-byte header (signature + chunk
length + `base_frequency` + `num_entries` per
`docs/video/cinepak/reference/wiki/Sega_FILM.wiki` lines 84-91),
and returns the parsed `StabHeader` paired with the records-only
slice bounded to exactly `num_entries * SAMPLE_RECORD_SIZE`
bytes — i.e. precisely what `Samples::new` wants, so a caller
does `let (hdr, recs) = StabHeader::parse_chunk(chunk)?; let it =
Samples::new(recs)?;` without the `FilmDemuxer::parse` round-trip
or its `Vec<SampleRecord>` allocation. The wire `length` field
(bytes 4-7) is read but **not** used for offset arithmetic —
`Sega_FILM.wiki` line 92 records that some titles omit the first
16 bytes from it — so correctness rests on `num_entries` alone,
and trailing bytes past the declared table are excluded from the
returned slice. Errors on short header, bad signature, or a
`num_entries * 16` table that overruns the chunk (`checked_mul`
/ `checked_add` guard a hostile `num_entries`). 10 new lib tests
(182 → 192). New crate-root export: `STAB_HEADER_SIZE`. Pure
additive change — no decoder / encoder / `FilmDemuxer::parse`
behaviour change.
