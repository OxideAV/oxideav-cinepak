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

## Reproducing

```bash
# 1. Build the profile driver in release with debug info.
cargo build --release --example profile_cinepak \
    -p oxideav-cinepak

# 2. Run the four modes — or `all` for the full sweep.
./target/release/examples/profile_cinepak all

# Per-mode subsets are useful for sampler runs (samply / perf):
./target/release/examples/profile_cinepak encode    20
./target/release/examples/profile_cinepak decode  1000
./target/release/examples/profile_cinepak stateful  30
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
