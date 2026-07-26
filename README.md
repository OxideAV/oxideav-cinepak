# oxideav-cinepak

[![CI](https://github.com/OxideAV/oxideav-cinepak/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-cinepak/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-cinepak.svg)](https://crates.io/crates/oxideav-cinepak) [![docs.rs](https://docs.rs/oxideav-cinepak/badge.svg)](https://docs.rs/oxideav-cinepak) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust Cinepak (CVID) video codec — full decoder plus a
quality- and rate-controlled encoder — for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

Clean-room implementation built from `docs/video/cinepak/spec/` and the
public reverse-engineering references staged under
`docs/video/cinepak/reference/` (multimedia.cx wiki, Tim Ferguson's
`videocodec/cinepak.txt`, US patent 5,467,413). A reference command-line
multimedia tool is used only as an opaque black-box CLI validator
(encode-our-frame → decode-elsewhere PSNR checks); no external decoder
or encoder source is consulted at any phase. The previous on-disk
history is preserved on the `old` branch and is **not** an input for
this implementation.

## Decode

End-to-end, spec-complete:

- 10-byte frame header (`flags`, 24-bit `frame_length`, `width`,
  `height`, `strip_count`) and 12-byte strip header with the
  multi-strip Y-coordinate sentinel rule.
- Full and selective codebook chunks for V4 (12-bit YUV) and V1, plus
  the 8-bit grayscale variants (`0x2000` / `0x2100` / `0x2200` /
  `0x2300` / `0x2400` / `0x2500` / `0x2600` / `0x2700`).
- Vector chunks `0x3200` (V1-only intra), `0x3000` (mixed V1/V4 intra),
  and `0x3100` (inter with skip codes — including selector-bit
  spillover across flag-word boundaries).
- V1 quadrant macroblock expansion and V4 sub-block macroblock
  expansion.
- Codebook inheritance across strips and across frames (a header-only
  chunk means "reuse previous codebook").
- YUV → RGB conversion with truncation-toward-zero on `U / 2` and
  per-channel clamp to `[0, 255]`.
- Skip macroblocks copy 4×4 blocks from the previous frame's
  reconstructed buffer.

Output pixel formats: `Rgb24` (12-bit YUV streams) and `Gray8` (8-bit
luminance-only streams, chunk families `0x24xx` / `0x26xx`).

### Deviant variants

`CinepakDecoder::decode_deviant_frame(bytes, pts, DeviantConfig)`
handles the Sega Saturn / Sega CD / Lemmings 3DO `'cvid'` deviations
documented in `Sega_FILM.wiki`: extra header-prefix bytes (2 for
Saturn, 6 for Lemmings 3DO), a `frame_length` field short by 8 bytes,
and codebook chunks whose payload size isn't a clean multiple of the
entry stride. The standard `decode_frame` path is byte-for-byte
unaffected. `codebook::apply_codebook_chunk_with(..., tolerate_trailing)`
exposes the trailing-pad knob directly.

## Encode

The encoder builds V1 + V4 codebooks per strip with a median-cut
quantiser, optional k-means++ cold-start, Lloyd refinement, Linde–Buzo–
Gray (1980, IEEE TComm 28(1)) split refinement, and post-classification
Lloyd polish; per-macroblock V1-vs-V4 selection is Lagrangian
rate-distortion (`D + λ·R`). All clustering math is published VQ-design
or academic algorithm work — no external encoder source is consulted.

Every strip picks the cheapest legal vector code for its macroblocks.
An intra strip emits the `0x3200` V1-only chunk when every macroblock is
V1-coded (flat / low-detail content), saving the per-group flag word(s)
the `0x3000` mixed form would carry; a strip with any V4 macroblock keeps
`0x3000`. An **inter strip with no skip macroblocks** (a full recode,
e.g. a scene cut) is likewise routed to `0x3200` (all-V1) or `0x3000`
(any V4) rather than `0x3100`: the inter chunk's skip/V1/V4 VLC spends
2 bits per coded macroblock versus 1 bit for `0x3000`, so dropping it
roughly halves the flag-word overhead. `0x3100` is kept only when at
least one macroblock skips (it is the only vector code with a skip
token). Reconstruction is byte-identical across the forms — the choice
is a pure rate win and is reflected in the RD-grid pickers' byte-cost
scoring.

Intra entry points:

- `encode_rgb24` / `encode_gray8` — multi-strip intra encoder.
- `encode_rgb24_best_strips` — per-frame strip-count picker.
- `encode_rgb24_best_rd_grid` / `encode_rgb24_round6` — two-axis
  `(strip_count, rdo_lambda)` RD grid picker.
- `encode_rgb24_best_rd_grid_3axis` / `encode_rgb24_round7` — three-axis
  `(strip_count, rdo_lambda, luma_weight)` picker scoring BT.601 Y-SSE.
- `encode_rgb24_per_strip_rd` / `encode_rgb24_round8` — per-strip
  independent `(lambda, luma_weight)` picker (each strip carries its own
  codebooks per spec §3.4).
- `encode_gray8_best_rd_grid` / `encode_gray8_round7` — grayscale
  frame-level RD-grid picker (no `luma_weight` axis: on `Gray8` it is a
  uniform scale that leaves every clustering decision invariant).

Inter / stateful:

- `encode_rgb24_inter` / `encode_gray8_inter` — stateless inter
  encoders; macroblocks whose per-pixel MSE is below `skip_threshold`
  become `0x3100` SKIP codes.
- `CinepakEncoder` — stateful encoder carrying the rolling V4/V1
  codebook across strips and frames. Emits `0x2100` / `0x2300`
  selective-update chunks (only changed slots) or omits the codebook
  chunk entirely when the previous codebook already serves the strip.
  `encode_intra` / `encode_inter` and the `*_gray8` analogs; cross-frame
  codebook persistence (warm-start training from the prior centroids),
  stale-slot reclamation, and `last_frame_stats()` telemetry.

`EncoderOptions` knobs include `v4_entries` / `v1_entries`,
`strip_count`, `skip_threshold`, `rdo_lambda`, `lbg_max_passes`,
`luma_weight`, `pcl_max_iter`, `lloyd_max_iter` / `lloyd_eps`,
`kmeans_pp_init` / `kmeans_pp_lloyd_iter`, `stale_slot_threshold`, and
`vintage_compat`. `EncoderOptions::from_quality(q)` maps a single
`q ∈ 0..=100` knob onto codebook size, strip count, and skip threshold.

### Rate control and GOP scheduling

Rate control and keyframe scheduling are encoder policy, not bitstream
features — output stays conformant Cinepak in every mode.

- `CinepakEncoder::with_target_bitrate(bits_per_second, fps)` /
  `with_target_frame_bytes(n)` — per-frame byte budget driving the RD
  grid toward the highest-quality candidate that fits; never errors on
  overshoot (the overshoot is flagged via `last_rate_stats()`). A CBR
  carry-over accumulator (`set_carry_over_cap_bytes` /
  `reset_rate_carry_over`) makes a multi-frame sequence's total bytes
  converge to `bits_per_second / 8 × duration`.
- `TwoPassRateControl` — grid-search and windowed-bisection rate
  control (`encode_at_target_window_bytes` /
  `encode_at_target_window_bytes_adaptive`) over a throwaway-encoder
  prefix replay.
- `with_keyframe_interval(n)` + `encode_frame` / `encode_frame_gray8`
  auto-route each frame to intra/inter and return an
  `EncodedFrame { bytes, is_keyframe, frame_number_in_gop }` so muxers
  mark the per-sample keyframe flag without re-inspecting the bytes.
  `force_next_keyframe()` requests a one-shot scene-cut refresh; the
  router also re-keyframes defensively on pixel-mode switch.

### Vintage compatibility

`EncoderOptions::vintage_compat = true` enforces the structural
constraints vintage Windows / classic-MacOS players require (strip
count ≤ 3; both V4 and V1 codebook chunks always present per strip in
strict V4-then-V1 order, emitting a 4-byte header-only chunk where the
modern path would chunk-omit). Header-only and chunk-omitted forms are
decoder-equivalent, so a vintage-compat stream self-decodes to
byte-identical pixels.

## Sega FILM (CPK) container

`FilmDemuxer` walks the FILM + FDSC + STAB chunk layout (standard
32-byte and abbreviated 20-byte FDSC). `probe_film` is a lightweight
`'FILM'` signature check. Accessors cover video/audio sample tables,
keyframe seek (`seek_keyframe_for_tick`), durations, the
`FilmDemuxer::variant()` deviant classifier, and a structured audio
classifier (`FilmAudioFormat` — linear PCM with endianness / sign
convention, CRI ADX ADPCM, or none) with PCM sample-reshaping helpers
(`decode_chunk_to_i16` and the per-transform free functions). Audio
codec decoding itself is out of scope; the helpers only re-shape the
documented FILM wire bytes for a generic PCM sink.

## Typed wire-walking layers

A set of allocation-free, content-agnostic iterators expose the wire
format without the codebook + pixel-decode dependency — useful for
validators, fuzz harnesses, and introspection tools:

- `header::FrameStrips` — per-strip header + payload-slice iterator.
- `codebook::StripChunks` — per-chunk iterator with `StripChunkKind` /
  `CodebookChunkKind` / `VectorChunkKind` classification.
- `vector::V1OnlyMacroblocks` / `MixedIntraMacroblocks` /
  `InterMacroblocks` — per-macroblock walkers for the `0x3200` /
  `0x3000` / `0x3100` vector codes.
- `vector::MixedIntraRgbBlocks` — resolved-RGB capstone over a `0x3000`
  payload: given the strip's V1/V4 codebooks and macroblock-grid width
  it yields each macroblock's fully reconstructed 4×4 RGB block
  (`MixedIntraRgbBlock`) with its `(mb_col, mb_row)` grid position and
  pixel origin — the framebuffer-free counterpart of the decoder's
  intra reconstruction loop (composes the `0x3000` walk with the §§4/5
  expansion and §3 YUV→RGB matrix). Out-of-range indices / truncation
  yield `Some(Err(_))` then fuse.
- `CodebookEntry::luma_subblock` / `expand_v1_luma`, plus
  `expand_v4_luma` / `expand_v4_chroma` — §4/§5 macroblock plane
  geometry without the YUV→RGB matrix.
- `expand_v1_mb_rgb` / `expand_v4_mb_rgb` — the colour-converted 4×4 RGB
  pixel block of one macroblock.
- `film::Samples` / `StabHeader::parse_chunk` — STAB record walking.

## Wire-format conformance lint

Where the decoder answers "can these bytes become pixels?", the
`lint` module answers "do these bytes *conform* to the documented
wire format — and if not, which rule, where, and how badly?".
`lint_frame(bytes)` / `lint_frame_with(bytes, &LintOptions)` walk the
frame → strip → chunk → vector layers read-only and return a
`LintReport` of `LintIssue`s, each carrying the violated `LintRule`,
an `Error` (normative violation) vs `Warning` (SHOULD /
encoder-convention / corpus-observation deviation) severity, the
strip/chunk location, the byte offset, and the grounding spec section
(`LintRule::spec_ref`). The walk is best-effort (reports as many
independent findings as possible) and total (arbitrary bytes never
panic — unparseable structure is itself a finding).

Rule coverage: frame-header field ranges and `frame_length`
accounting; strip-id taxonomy, y-sentinel geometry, vertical tiling,
and strip-size accounting; chunk taxonomy and `Σ chunk_size`
accounting; per-strip-kind chunk restrictions (selective updates and
`0x3100` barred from intra strips); codebook payload arithmetic
(entry alignment, 256-entry ceiling, selective-update group walk);
V4-then-V1 ordering and one-vector-chunk-per-strip placement; mixed
YUV/grayscale flavours in one frame; per-strip macroblock-count byte
balance for all three vector codes; and the intra codebook-occupancy
rule (vector indices referencing entries the strip never defined).

`LintOptions` adds two gated profiles: `vintage` enforces the
vintage-player constraints the encoder's `vintage_compat` targets
(≤ 3 strips, both codebook chunks per strip in strict V4-then-V1
order), and `sequence_start` flags previous-frame/previous-codebook
dependence (skip codes, selective updates) on a stream's first frame
or seek target. `lint_sequence(frames, opts)` maps a decode sequence
with `sequence_start` forced on the first frame.

The encoder is held to the linter:
`tests/lint_encoder_conformance.rs` asserts every public encode entry
point — all intra/gray/inter/stateful/rate-controlled/quality-sweep
paths, plus a `vintage_compat` GOP under the vintage profile — emits
streams with zero errors *and* zero warnings.

`examples/lint_cvid.rs` is the command-line driver: it splits a file
of concatenated raw frames on `frame_length` (spec 01 §1.2) and
prints per-frame findings (`--vintage`, `--mid-stream`; exit 1 on any
error-severity finding).

## Registration

`CVID` is registered (AVI `biCompression`, QuickTime `cvid`, Sega FILM
`cvid`). `probe_cvid` confirms the 10-byte frame header + 12-byte first
strip header, returning `1.0` on a fully-valid structure, `0.0` on any
failed check, and `0.5` without packet bytes.

## Standalone vs registry-integrated

The default `registry` cargo feature wires the crate into
`oxideav-core` (Decoder trait, `register(ctx)` entry point). Disable it
for an `oxideav-core`-free build exposing only the crate-local
`CinepakDecoder`, `CinepakFrame`, `CinepakPixelFormat`, and
`CinepakError` types:

```toml
oxideav-cinepak = { version = "0.0", default-features = false }
```

## Benchmarks

Three `criterion` harnesses synthesise their fixtures on the fly (no
committed bytes; criterion is dev-only):

```sh
cargo bench -p oxideav-cinepak --bench decode
cargo bench -p oxideav-cinepak --bench encode
cargo bench -p oxideav-cinepak --bench roundtrip
```

Indicative release numbers (Apple M-class, single thread): decode runs
at ~3–4 GiB/s of raw output; stateless intra encode is dominated by the
codebook trainer and RD-grid picker. A standalone profiling driver
(`examples/profile_cinepak.rs`, baseline under `profile/README.md`)
supports `samply` / `cargo flamegraph` capture and a per-phase encoder
decomposition; LBG split-refinement is the encoder hot path. Round 430
restructured the training hot paths output-invariantly (byte-identical
wire output, pinned by `tests/golden_wire_hashes.rs`) for a −23%..−31%
whole-call encode-time cut across the fixture set (e.g. 320×240 intra
9.1→12.2 MiB/s, 5-frame stateful GOP 16.6→22.9 MiB/s); see
`profile/README.md` for the per-optimization ledger.

## Fuzzing

A `cargo-fuzz` harness under `fuzz/` covers the public parse/decode
entry points and the FILM PCM-shaping helpers — frame/strip header
parse, full and deviant `decode_frame`, multi-frame stateful decode,
the FILM container parse, the codebook-chunk and vector-chunk parsers
in isolation, the PCM reshaping functions, and the conformance
linter (`lint_frame` across all option profiles, asserting
total-function behaviour and profile monotonicity). An `encode_roundtrip`
target drives the *encoder* with attacker-shaped pixel buffers,
dimensions, and option knobs across both pixel modes and the
intra/inter paths, then feeds every encoder output back through
`CinepakDecoder` to assert the encoder always emits a stream the
decoder accepts (exercising the `0x3000` / `0x3100` / `0x3200`
vector-code dispatch). Each target's contract is that any input yields
a `Result` and never panics, overflows, or OOMs; pixel-budget caps
(decode) and a 64×64 dimension cap (encode) keep worst-case rasters
bounded. Run e.g.:

```sh
cargo +nightly fuzz run decode_frame
cargo +nightly fuzz run encode_roundtrip
```

## License

MIT. Copyright © 2026 Karpelès Lab Inc.
