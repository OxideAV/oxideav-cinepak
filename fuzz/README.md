# oxideav-cinepak fuzz harnesses

Panic-free decode harnesses for the four user-reachable parse / decode
entry points on the `oxideav-cinepak` public API:

| Target | Surface under test | Why it matters |
|--------|--------------------|----------------|
| `frame_header_parse` | `cinepak::header::FrameHeader::parse` | 10-byte flags + width/height + strip-count header; bounds + alignment checks |
| `strip_header_parse` | `cinepak::header::RawStripHeader::parse` | 12-byte per-strip header that frames the codebook + vector chunks |
| `decode_frame` | `cinepak::CinepakDecoder::decode_frame` | Full intra/inter decode through codebook updates, vector chunks, strip stitching, raster allocation |
| `film_demuxer_parse` | `cinepak::FilmDemuxer::parse` | Sega FILM container parse — `FILM` / `FDSC` / `STAB` boxes + sample-record table |

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
| Max coded pixels (decode_frame) | 256 × 256 | Keeps the per-frame RGB24 raster under ~200 KiB |
| Max raw input size | 64 KiB | libFuzzer default; the cap is enforced again at harness entry as a defence-in-depth |
| Max STAB sample-record count | 4096 | Each record is 16 bytes, so the table itself stays under 64 KiB; without this cap a wire-legal `STAB` size field can claim hundreds of MiB |

If the cap fires the harness early-returns without invoking the
decoder. That's still useful coverage of the parse-only path
(`FrameHeader::parse` / `FilmHeader::parse` validate their own fields
before any allocation).

## Running locally

`cargo-fuzz` needs nightly. The org-level reusable workflow at
`.github/workflows/fuzz.yml` installs it on the CI runner; for local
runs:

```
rustup toolchain install nightly
cargo install cargo-fuzz
cd crates/oxideav-cinepak
cargo +nightly fuzz run frame_header_parse  -- -max_total_time=60
cargo +nightly fuzz run strip_header_parse  -- -max_total_time=60
cargo +nightly fuzz run decode_frame        -- -max_total_time=60
cargo +nightly fuzz run film_demuxer_parse  -- -max_total_time=60
```

Each `cargo fuzz run` exits 0 once the time budget elapses without a
crash; any panic / OOM / timeout creates an entry under
`fuzz/artifacts/<target>/`.

## CI

`.github/workflows/fuzz.yml` is a thin shim around the org-level
`crate-fuzz.yml` reusable workflow. The total daily budget is 1800 s
(30 minutes), split across the four targets (~7.5 min each).
