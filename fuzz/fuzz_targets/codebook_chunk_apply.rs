// Panic-free fuzz target for `cinepak::codebook::apply_codebook_chunk`
// (and its `_with` sibling that exposes the Sega Saturn deviant
// `tolerate_trailing` knob).
//
// The codebook chunk parser is the largest single parser surface in
// the Cinepak decoder: per `docs/video/cinepak/spec/02-codebooks.md`
// §2 there are eight chunk-type codes (`0x2000`/`0x2100`/`0x2200`/
// `0x2300`/`0x2400`/`0x2500`/`0x2600`/`0x2700`) that split across a
// 2 × 2 × 2 grid — V4-vs-V1 × full-replacement-vs-selective-update ×
// 12-bit-YUV-vs-8-bit-grayscale. Each variant decodes its payload
// differently:
//
//   - Full-replacement chunks set entries `0..N-1` from a packed run
//     of fixed-stride entries (6 B for YUV, 4 B for grayscale per
//     §3.1).
//   - Selective-update chunks decode in 32-entry groups: each group
//     is prefixed by a big-endian u32 flag word whose set bits select
//     which of the next 32 codebook indices receive a fresh entry
//     from the payload (§3.3).
//
// `decode_frame` already exercises this code path *inside* a full
// strip, but the fuzzer's mutations have to thread through a frame
// header + strip header + chunk header before they reach the
// codebook parser, which throttles per-chunk coverage growth. This
// target lets libFuzzer drive the codebook parser directly with
// arbitrary `chunk_id` + payload pairs across all eight chunk kinds
// and both `tolerate_trailing` settings — the same surface
// `apply_codebook_chunk_with` exposes to `decode_frame` (standard
// path: strict) and `decode_deviant_frame` (Sega Saturn:
// `tolerate_trailing = true`).
//
// Sibling `decode_frame` / `decode_deviant_frame` fuzz targets
// already cover the integrated path; this target completes the
// per-parser fuzz coverage (frame header / strip header / codebook
// chunks parsed directly / vector chunks parsed via decode).
//
// ## OOM cap
//
// `Codebook::default()` allocates 256 × 6 B (≈ 1.5 KiB) per
// instance and the fuzz target builds at most three codebooks per
// input (one per chunk-kind variant exercised), so memory pressure
// is bounded without an explicit raster cap. The defence-in-depth
// 64 KiB input cap matches the sibling targets.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::codebook::{
    apply_codebook_chunk, apply_codebook_chunk_with, Codebook, CodebookChunkKind,
};

/// Defence-in-depth raw-input cap; mirrors the sibling targets.
const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 || data.len() > MAX_INPUT_BYTES {
        return;
    }

    // First two bytes pick a wire chunk-id (big-endian per spec
    // §1); the third's LSB picks the `tolerate_trailing` flag; the
    // rest is payload (the chunk-header bytes are *not* included
    // here — `apply_codebook_chunk` consumes payload only).
    let chunk_id = u16::from_be_bytes([data[0], data[1]]);
    let tolerate_trailing = (data[2] & 1) != 0;
    let payload = &data[3..];

    // Path A: drive the explicit chunk_id the fuzzer chose. If it
    // isn't in the codebook family the parser-id constructor returns
    // None and we exit cheaply — that's part of the panic-free
    // contract: the entry-point must handle the rejection.
    if let Some(kind) = CodebookChunkKind::from_id(chunk_id) {
        let mut cb = Codebook::default();
        let _ = apply_codebook_chunk_with(kind, payload, &mut cb, tolerate_trailing);

        // Path B: re-decode the same payload under the strict
        // `apply_codebook_chunk` entry. The two entry points share
        // backing code, but the public split is what library
        // consumers see, so both surfaces need fuzz coverage.
        let mut cb = Codebook::default();
        let _ = apply_codebook_chunk(kind, payload, &mut cb);
    }

    // Path C: regardless of whether the fuzzer's id was valid,
    // probe the bit-2 grayscale variant of the same selector so
    // libFuzzer learns the mode-switch boundary explicitly. The
    // mode flip changes entry stride (6 B → 4 B), which exercises
    // a different stride-vs-payload-length arithmetic path inside
    // `apply_full` / `apply_selective`.
    let gray_id = chunk_id | 0x0400;
    if let Some(kind) = CodebookChunkKind::from_id(gray_id) {
        let mut cb = Codebook::default();
        let _ = apply_codebook_chunk_with(kind, payload, &mut cb, tolerate_trailing);
    }
});
