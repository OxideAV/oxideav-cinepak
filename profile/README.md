# oxideav-cinepak round-160 profile baseline

This directory captures the profiling-baseline numbers produced by
the `examples/profile_cinepak.rs` driver that round 160 introduces.
The driver is the durable artefact: any future round (or local
A/B run) can reproduce these numbers + capture per-symbol flame-
graphs against it without re-discovering the harness recipe.

## Headline numbers (round 160, Apple M4 Max, release build)

Each scenario is self-contained (deterministic xorshift32-seeded
synthetic input, no external fixtures). The encode / decode /
roundtrip rows use the same stateless `encode_*` / `decode_frame`
entry points the `benches/*.rs` Criterion harnesses target. The
`stateful` row drives the full picker + rolling-codebook + rate-
control machinery over a 5-frame GOP (1 intra + 4 inter), mirroring
the r155 ffmpeg cross-decode fixture pattern.

```
== encode ==
  encode    rgb24/64x64/q50/round7         iters=  40   30.296 ms/iter      0.39 MiB/s (raw)  out=2881B/iter (0.234 of input)
  encode    rgb24/320x240/q50/baseline     iters=  12   24.193 ms/iter      9.08 MiB/s (raw)  out=11458B/iter (0.050 of input)
  encode    rgb24/640x480/q70/baseline     iters=   6  165.493 ms/iter      5.31 MiB/s (raw)  out=35047B/iter (0.038 of input)
  encode    gray8/320x240/q50/baseline     iters=  16   12.959 ms/iter      5.65 MiB/s (raw)  out=6412B/iter (0.083 of input)

== decode ==
  decode    rgb24/64x64/q50/round7         iters=2000    0.003 ms/iter   3544.07 MiB/s (raw)
  decode    rgb24/320x240/q50/baseline     iters= 600    0.059 ms/iter   3744.84 MiB/s (raw)
  decode    rgb24/640x480/q70/baseline     iters= 300    0.198 ms/iter   4435.78 MiB/s (raw)
  decode    gray8/320x240/q50/baseline     iters= 800    0.023 ms/iter   3200.51 MiB/s (raw)

== roundtrip ==
  roundtrip rgb24/64x64/q50/round7         iters=  40   30.493 ms/iter      0.38 MiB/s (raw)
  roundtrip rgb24/320x240/q50/baseline     iters=  12   24.721 ms/iter      8.89 MiB/s (raw)
  roundtrip rgb24/640x480/q70/baseline     iters=   6  170.862 ms/iter      5.14 MiB/s (raw)
  roundtrip gray8/320x240/q50/baseline     iters=  16   13.112 ms/iter      5.59 MiB/s (raw)

== stateful (5-frame GOP at 320x240 q50) ==
  stateful  5-frame-gop                    iters=  12   67.545 ms/seq     16.27 MiB/s (raw)  out=38298B/seq
```

## Reading the numbers

### Decode

- Decode runs at **3.2–4.4 GiB/s of raw output** on the M4 Max — a
  ~3000× realtime ratio at 320×240@30. Per-frame fixed overhead
  (the `CinepakDecoder::new` zero-init + first-strip codebook
  allocation) is ~3 μs on the 64×64 fixture; everything above
  that scales linearly with output pixels. There is no measurable
  inner hot loop that algorithmic work would speed up — further
  decoder gains would require SIMD on the codebook-expansion path,
  not a one-round change.
- The post-r129 hot-path rewrite (decoder rewrite, −17%..−67% per-
  frame time over r126) is what this baseline anchors. Future
  decoder-side optimisation rounds should run this driver first;
  a regression > 10 % on any decode row is the signal to bisect
  before pushing.

### Encode

- Encode is dominated by the **3-axis RD-grid picker** on the
  round-7 row (~27 trial encodes per frame) and by the picker /
  codebook-trainer pair on the baseline rows. The 64×64 round-7
  number is the deliberate worst case: a small fixture amortises
  no per-frame fixed cost, so the picker's 27× multiplier shows
  up unfiltered (~30 ms for a 12 KiB input).
- The 640×480/q70 row is interesting: 6× the area of the 320×240
  row should cost ~6× more, but it costs ~7× more. The extra
  ~17 % is the multi-strip path (3 strips instead of 1) — each
  added strip carries its own codebook training pass, plus the
  strip-count picker adds a candidate.
- Grayscale is the cheapest encode path per pixel (5.65 MiB/s vs
  9.08 MiB/s on the same fixture) because the codebook entries are
  4 B instead of 6 B, but the trade-off is only ~30 % faster, not
  the naive 1.5× from the byte-per-entry ratio. That gap is the
  V1/V4 selection cost — the same number of vector lookups happen
  regardless of entry size.

### Stateful 5-frame GOP

- The real-world story: ~67 ms per 5-frame sequence at 320×240@q50,
  which is ~13.5 ms/frame averaged across intra + 4 inter. The
  intra carries most of the cost (codebook training from scratch);
  each inter is roughly half (cross-frame codebook persistence
  warm-starts the median-cut quantiser, plus SKIP/inter strips
  reuse the prior frame's V1/V4 tables).
- This row is what the r155 ffmpeg cross-decode validation is
  reading: 5 frames in ~67 ms, 38 KiB of CVID output (~7.6 KiB/frame
  average — well-shaped for the 30 fps@~256 kbit/s rate-control
  band our headline targets).

## Round 209 — picker-axis cost sweep + jitter

Round 160's baseline measures the **headline encoder path** on each
scenario (the round-7 3-axis picker on `rgb24/64x64/q50/round7`, the
single-pass `encode_rgb24` everywhere else). Round 209 adds a `picker`
mode that runs **all four public RGB encoder entry points** on the
same fixture set, plus a `samples=N` argument that reports
**median + (max-min)/median jitter** across `N` independent rep
groups so single-sample noise (±30 % on a contended laptop) doesn't
bury real per-pass-difference signal.

### Picker-axis sweep (round 209, Apple M4 Max, release build)

Each row is one fixture × one picker entry point. The `trials≤`
column is the **per-frame trial-count ceiling** the picker can run
internally (round-6 = 9-trial 2-axis `(strip_count, lambda)` grid;
round-7 = 27-trial 3-axis grid with `luma_weight`; round-8 = round-7
grid + per-strip greedy that re-sweeps `lambda × luma_weight` per
strip — ceiling ~81 trials on a 2-strip pick). The dedup logic in
each picker may collapse the grid when seeded `lambda` / `luma_weight`
hit canonical values, so the wall-time numbers are what actually
fired, not the ceiling.

```
rgb24/64x64/q50/round7
  encode_rgb24 (baseline)         iters= 40 trials≤  1    1.379 ms/iter  out= 1684B (0.137 of input)
  encode_rgb24_round6 (2-axis)    iters= 14 trials≤  9   11.026 ms/iter  out= 2845B (0.232 of input)
  encode_rgb24_round7 (3-axis)    iters=  8 trials≤ 27   31.469 ms/iter  out= 2881B (0.234 of input)
  encode_rgb24_round8 (per-strip) iters=  5 trials≤ 81   98.286 ms/iter  out= 2950B (0.240 of input)

rgb24/320x240/q50/baseline
  encode_rgb24 (baseline)         iters= 12 trials≤  1   24.641 ms/iter  out=11458B (0.050 of input)
  encode_rgb24_round6 (2-axis)    iters=  4 trials≤  9  186.606 ms/iter  out=10833B (0.047 of input)
  encode_rgb24_round7 (3-axis)    iters=  3 trials≤ 27  564.703 ms/iter  out=16146B (0.070 of input)
  encode_rgb24_round8 (per-strip) iters=  2 trials≤ 81 1760.400 ms/iter  out=14808B (0.064 of input)

rgb24/640x480/q70/baseline
  encode_rgb24 (baseline)         iters=  6 trials≤  1  169.038 ms/iter  out=  35047B (0.038 of input)
  encode_rgb24_round6 (2-axis)    iters=  2 trials≤  9 1614.384 ms/iter  out=  33547B (0.036 of input)
  encode_rgb24_round7 (3-axis)    iters=  2 trials≤ 27 4934.863 ms/iter  out=  65941B (0.072 of input)
  encode_rgb24_round8 (per-strip) iters=  2 trials≤ 81 15709.616 ms/iter  out=  65941B (0.072 of input)
```

#### Reading the picker rows

- The **baseline → round-6 jump (~8×)** is the
  `(strip_count, lambda)` grid expanding the underlying intra
  encoder call by ~9× — slightly under because the lambda dedup
  trims duplicates when `opts.rdo_lambda` collides with the seed
  set `[0.0, 2.5, opts.rdo_lambda]`. The wire-size delta is
  scenario-dependent (smaller on the 64×64 row where the picker's
  Y-channel scoring picks the higher-quality larger output).
- The **round-6 → round-7 jump (~3×)** is the `luma_weight` axis
  multiplying the round-6 grid by 3× — straightforward arithmetic
  on the grid expansion. Round-7's chosen output is sometimes
  larger than round-6's (notably the 320×240 / q50 row at 16146 B
  vs 10833 B) because round-7's scoring switches from RGB SSE to
  Y-channel SSE per the README narrative; the Y-favouring lever
  picks a higher-bitrate operating point on smooth-content
  fixtures.
- The **round-7 → round-8 jump (~3×)** is the per-strip greedy
  pass running an extra `lambda × luma_weight` sweep per-strip
  per `strip_count` candidate. The chosen output frequently
  matches round-7's exactly on these gradient fixtures (the
  per-strip greedy converges to the frame-uniform pick on
  homogeneous content) — the cost is paid for the option of
  diverging when strip content statistics differ.
- The **640×480 / q70 row** at round-8 takes ~16 s per frame —
  this is the deliberate "highest quality, slowest path" call
  site: the per-strip greedy on a multi-strip large fixture
  multiplies the underlying `encode_intra_frame` cost by ~93×
  the baseline. Callers who want round-8's quality without
  round-8's wall time should use `encode_rgb24_round7` (which is
  in the 22–30× cost band) or pre-compute the per-strip
  `(lambda, luma_weight)` once with round-8 then reuse them via
  the lower-level grid API.

### Decode + encode jitter (round 209, samples=5)

The round-160 single-sample numbers don't tell you whether a single
row's drift is real or noise. Running `samples=5` gives median +
jitter — the spread between min and max as a fraction of the median.

```
encode (median + jitter across 5 rep groups)
  rgb24/64x64/q50/round7      iters= 40 median= 32.126 ms/iter jitter=10.2%    0.36 MiB/s  out= 2881B
  rgb24/320x240/q50/baseline  iters= 12 median= 24.990 ms/iter jitter= 1.1%    8.79 MiB/s  out=11458B
  rgb24/640x480/q70/baseline  iters=  6 median=170.641 ms/iter jitter= 8.8%    5.15 MiB/s  out=35047B
  gray8/320x240/q50/baseline  iters= 16 median= 13.061 ms/iter jitter= 3.8%    5.61 MiB/s  out= 6412B

decode (median + jitter across 5 rep groups)
  rgb24/64x64/q50/round7      iters=2000 median= 0.003 ms/iter jitter= 0.9% 3428.71 MiB/s
  rgb24/320x240/q50/baseline  iters= 600 median= 0.061 ms/iter jitter= 2.8% 3618.10 MiB/s
  rgb24/640x480/q70/baseline  iters= 300 median= 0.202 ms/iter jitter= 2.3% 4340.37 MiB/s
  gray8/320x240/q50/baseline  iters= 800 median= 0.023 ms/iter jitter= 1.5% 3229.71 MiB/s
```

The decode jitter ≤ 3 % everywhere confirms the round-160
single-sample decode numbers were stable — A/B comparisons against
the decode baseline can read 5 %-or-better as real signal. The
encode jitter is bimodal: the 64×64 / round-7 row jitters 10 %
(small fixture, high per-iter variance), while the 320×240
baseline row jitters 1 % (steady-state path with no per-fixture
quirk). A/B comparisons against the encode baseline should use
`samples=5` and read ≥ 10 % deltas as real on the round-7 row, ≥ 3 %
on the rest.

## Round 289 — per-training-phase marginal-cost decomposition

Rounds 160 + 209 measure the encoder at the **whole-call** granularity
(one `encode_rgb24` invocation, or the picker tiers that wrap several
such calls). Neither attributes that cost to the individual
codebook-training phases the encoder layers inside the private
`build_codebooks_and_decisions`: cold median-cut, seed/cold Lloyd
refinement (`lloyd_max_iter`), LBG split-refinement (`lbg_max_passes`),
post-classification Lloyd polish (`pcl_max_iter`), and the k-means++
cold-start (`kmeans_pp_init` + its own Lloyd polish). All five share
the same O(vectors × K) `nearest` inner loop, so before any future
round touches the hot path it needs to know which phase owns it.

The `phases` mode holds the input + codebook size fixed and toggles
each phase ON cumulatively from a training-minimal base (median-cut
only — every optional phase off), reporting the **marginal** ms each
phase adds. It drives only the public stateless `encode_rgb24` entry
point through public `EncoderOptions` fields, so no encoder code is
instrumented and the measured wire output is exactly what those option
vectors already produce (the output bytes shift row-to-row only because
each row is a different — valid — encoder configuration).

### Phase decomposition (round 289, Apple M4 Max, release build)

Each row's `marginal` is its median minus the row above; the row's
output bytes are the wire effect of that phase being on.

```
rgb24/64x64/q50/round7
  median-cut only (base)         0.248 ms/iter  marginal=  +0.248 ms  out= 1699B
  + lloyd_max_iter=2             0.245 ms/iter  marginal=  -0.003 ms  out= 1699B
  + lbg_max_passes=8             0.973 ms/iter  marginal=  +0.728 ms  out= 1681B
  + pcl_max_iter=2               1.078 ms/iter  marginal=  +0.104 ms  out= 1675B
  + kmeans_pp_init (4 lloyd)     1.408 ms/iter  marginal=  +0.330 ms  out= 1684B

rgb24/320x240/q50/baseline
  median-cut only (base)         2.830 ms/iter  marginal=  +2.830 ms  out=11140B
  + lloyd_max_iter=2             2.748 ms/iter  marginal=  -0.082 ms  out=11140B
  + lbg_max_passes=8            18.150 ms/iter  marginal= +15.402 ms  out=10630B
  + pcl_max_iter=2             20.152 ms/iter  marginal=  +2.002 ms  out=11185B
  + kmeans_pp_init (4 lloyd)   24.831 ms/iter  marginal=  +4.679 ms  out=11458B

rgb24/640x480/q70/baseline
  median-cut only (base)        10.137 ms/iter  marginal= +10.137 ms  out=39424B
  + lloyd_max_iter=2            10.226 ms/iter  marginal=  +0.089 ms  out=39424B
  + lbg_max_passes=8           75.085 ms/iter  marginal= +64.859 ms  out=40339B
  + pcl_max_iter=2             82.493 ms/iter  marginal=  +7.408 ms  out=42484B
  + kmeans_pp_init (4 lloyd)  104.298 ms/iter  marginal= +21.805 ms  out=41761B
```

#### Reading the phase rows

- **LBG split-refinement owns the encoder hot path.** On both
  non-trivial fixtures it is by far the largest single marginal:
  +15.4 ms on 320×240 (5.4× the median-cut base) and +64.9 ms on
  640×480 (6.4× the base) — roughly 62 %/62 % of the full
  default-options encode time on those rows. Its wire effect is
  modest (`out` moves <5 %), so a future optimisation round that wants
  encoder throughput should profile `lbg_refine_codebook` first: it
  re-runs a full Lloyd assignment + recentroid pass per split, each
  iterating the O(vectors × K) `nearest` scan, and the
  `lbg_max_passes=8` default pays for 8 such passes per codebook per
  strip. Candidate levers (all to be measured, none landed here):
  capping passes on large strips, early-stopping on SSE-improvement
  plateau, or a partial-reassignment pass that only re-scores vectors
  near the split boundary.
- **Lloyd refinement (`lloyd_max_iter`) is effectively free** here:
  its marginal is within timing noise of zero on every row. On the
  cold-start path it only runs when a seed is present (intra has
  none), so the intra rows are measuring the empty fast-exit — the
  cost it *would* carry shows up folded into the k-means++ row, which
  bundles its own Lloyd polish.
- **Post-classification polish (`pcl_max_iter`) is cheap** (+0.1 to
  +7.4 ms, ≤ 9 % of total) — it re-trains only *used* slots from the
  actually-selected vectors, a much smaller set than the full
  training population, and converges in 1–2 passes.
- **k-means++ cold-start is the second cost centre** (+4.7 to
  +21.8 ms): it computes the D²-sampling distribution (one `nearest`
  scan per already-chosen centroid) plus 4 Lloyd polish passes, and
  always builds the median-cut baseline alongside to guarantee it
  never regresses SSE — so it pays for two cold codebooks where the
  base row paid for one.
- The **64×64 row** compresses all five phases into ~1.4 ms total;
  the small fixture's per-call fixed cost dominates, so the phase
  *ratios* are clearer on the 320×240 / 640×480 rows.

## Reproducing

```bash
# 1. Build the profile driver in release with debug info.
cargo build --release --example profile_cinepak \
    -p oxideav-cinepak

# 2. Run the four round-160 modes — or `all` for the full sweep.
./target/release/examples/profile_cinepak all

# Per-mode subsets are useful for sampler runs (samply / perf):
./target/release/examples/profile_cinepak encode    20
./target/release/examples/profile_cinepak decode  1000
./target/release/examples/profile_cinepak stateful  30

# Round 209 — picker-axis sweep + jitter.
./target/release/examples/profile_cinepak picker
./target/release/examples/profile_cinepak encode 12 samples=5
./target/release/examples/profile_cinepak decode 600 samples=5

# Round 289 — per-training-phase marginal-cost decomposition.
./target/release/examples/profile_cinepak phases
```

### Capturing flamegraphs (samply, no root on macOS)

`samply` is the recommended sampler on macOS — it uses
`task_for_pid` after self-signing, no DTrace / `perf` /
elevated privileges. On Linux substitute `perf record` (root or
`perf_event_paranoid <= 1`) or `samply record` directly.

```bash
cargo install samply
cargo install inferno

# Sample. --unstable-presymbolicate writes a sidecar syms file so
# the JSON profile resolves to source symbols even after the
# binary's debug-info is gone.
samply record --unstable-presymbolicate --save-only \
    -o encode.json.gz \
    -r 1997 \
    -- target/release/examples/profile_cinepak encode 20

# Convert samply's processed-profile JSON to Brendan-Gregg folded
# stacks, then SVG. (The folded-stacks format is the stable
# interchange artefact — drop the JSON afterwards.)
samply export --output encode.folded encode.json.gz
inferno-flamegraph \
    --title "oxideav-cinepak encode (round 160)" \
    --subtitle "samply 1997Hz, 20 iters x 4 scenarios" \
    < encode.folded > encode.svg

# Repeat for decode / roundtrip / stateful.
```

The intermediate `*.json.gz` files are NOT committed — they're a
samply implementation detail. The folded-stack files (`*.folded`)
and SVGs (`*.svg`) are the stable interchange format; future
rounds that capture profiles should commit those alongside this
README baseline.

## Wall

Captured without consulting any external library source. `samply`
is a sampling profiler that only observes the OxideAV binary at
runtime; the captured stacks reference only the project's own
modules + stdlib + macOS runtime (`libsystem_*`, `dyld`). No
third-party Cinepak implementation participated in this baseline.
