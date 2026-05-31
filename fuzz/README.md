# oxideav-cinepak fuzz harnesses

Panic-free decode harnesses for the user-reachable parse / decode
entry points on the `oxideav-cinepak` public API:

| Target | Surface under test | Why it matters |
|--------|--------------------|----------------|
| `frame_header_parse` | `cinepak::header::FrameHeader::parse` | 10-byte flags + width/height + strip-count header; bounds + alignment checks |
| `strip_header_parse` | `cinepak::header::RawStripHeader::parse` | 12-byte per-strip header that frames the codebook + vector chunks |
| `decode_frame` | `cinepak::CinepakDecoder::decode_frame` | Full intra/inter decode through codebook updates, vector chunks, strip stitching, raster allocation; single frame per input |
| `decode_deviant_frame` | `cinepak::CinepakDecoder::decode_deviant_frame` | Saturn / Sega CD / Lemmings 3DO `'cvid'` deviation paths — `extra_header_bytes` prefix, `frame_length_short_by` undercount, `tolerate_codebook_pad` chunk-payload tolerance |
| `film_demuxer_parse` | `cinepak::FilmDemuxer::parse` | Sega FILM container parse — `FILM` / `FDSC` / `STAB` boxes + sample-record table |
| `codebook_chunk_apply` | `cinepak::codebook::apply_codebook_chunk{,_with}` | Eight-variant `0x2000..0x2700` codebook-chunk family (V4 vs V1 × full-replace vs selective-update × 12-bit-YUV vs 8-bit-grayscale) driven directly without a wrapping frame / strip / chunk header |
| `decode_vector_chunk` | `cinepak::vector::decode_vector_chunk` | Three vector-chunk codes (`0x3200` V1-only intra, `0x3000` mixed V1/V4 intra, `0x3100` inter with skip codes + selector-bit straddle) driven directly |
| `decode_multi_frame` | `cinepak::CinepakDecoder::decode_frame` over a length-prefixed sequence on a single decoder instance | Inter-frame state machine — rolling V1/V4 codebooks across `decode_frame` calls, selective-update inheritance from the prior strip's codebook, `0x3100` SKIP-MB copy from the previous frame's reconstructed raster, intra-after-inter inheritance wipe, `reset()` |

The harnesses are **decode-only**: they assert that the public API
never panics, never overflows arithmetic in debug, and never grows the
output raster past the bounded caps the harness imposes (see
"OOM caps" below). They do not compare against any reference codec.

## OOM caps

The wire-format width / height fields are `u16` (so up to 65535 each).
A worst-case RGB24 raster therefore costs ~12 GiB, which would OOM the
fuzz runner instantly and yield no useful coverage. Each harness that
allocates a raster (or a sample-record table) gates input dimensions
to a small budget before calling the decoder:

| Cap | Value | Source |
|-----|-------|--------|
| Max coded pixels (`decode_frame`, `decode_deviant_frame`, `decode_multi_frame` per frame) | 256 × 256 | Keeps the per-frame RGB24 raster under ~200 KiB |
| Max raw input size | 64 KiB | libFuzzer default; the cap is enforced again at harness entry as a defence-in-depth |
| Max STAB sample-record count (`film_demuxer_parse`) | 4096 | Each record is 16 bytes, so the table itself stays under 64 KiB; without this cap a wire-legal `STAB` size field can claim hundreds of MiB |
| Max `mb_count` (`decode_vector_chunk`) | 4096 | `decode_vector_chunk` allocates a `Vec<Mb>` (8 B/slot) of that length before the per-chunk arithmetic decides whether the payload is well-formed |
| Max frames per input (`decode_multi_frame`) | 8 | Bounds the per-iteration wall time so the run rate stays comparable to the single-frame target's; the steady-state `prev_frame` carry-over still fits in ~196 KiB per the coded-pixel cap |

If the cap fires the harness early-returns without invoking the
decoder. That's still useful coverage of the parse-only path
(`FrameHeader::parse` / `FilmHeader::parse` validate their own fields
before any allocation).

## `decode_multi_frame` framing

The fuzz input is parsed as a sequence of length-prefixed frame
slices: each frame is a big-endian u16 length followed by exactly
that many payload bytes. The harness exits early on truncation
rather than fabricating padding bytes — the goal is to drive the
state machine across legitimate frame boundaries, not to feed the
decoder synthesised garbage in the gaps.

`corpus/decode_multi_frame/` is seeded with two encoder-round-tripped
streams (intra + 1 inter, intra + 2 inters) at 32 × 32 so libFuzzer
has a structurally-valid starting point for mutating the
inter-frame state machine instead of having to first synthesise a
well-formed Cinepak frame from scratch.

## Running locally

`cargo-fuzz` needs nightly. The org-level reusable workflow at
`.github/workflows/fuzz.yml` installs it on the CI runner; for local
runs:

```
rustup toolchain install nightly
cargo install cargo-fuzz
cd crates/oxideav-cinepak
cargo +nightly fuzz run frame_header_parse   -- -max_total_time=60
cargo +nightly fuzz run strip_header_parse   -- -max_total_time=60
cargo +nightly fuzz run decode_frame         -- -max_total_time=60
cargo +nightly fuzz run decode_deviant_frame -- -max_total_time=60
cargo +nightly fuzz run film_demuxer_parse   -- -max_total_time=60
cargo +nightly fuzz run codebook_chunk_apply -- -max_total_time=60
cargo +nightly fuzz run decode_vector_chunk  -- -max_total_time=60
cargo +nightly fuzz run decode_multi_frame   -- -max_total_time=60
```

Each `cargo fuzz run` exits 0 once the time budget elapses without a
crash; any panic / OOM / timeout creates an entry under
`fuzz/artifacts/<target>/`.

## CI

`.github/workflows/fuzz.yml` is a thin shim around the org-level
`crate-fuzz.yml` reusable workflow. The total daily budget is 1800 s
(30 minutes), split across the eight targets (~3.75 min each).
