// Panic-free fuzz target for `cinepak::vector::decode_vector_chunk`.
//
// Per `docs/video/cinepak/spec/03-vectors-and-macroblocks.md` there
// are three vector-chunk codes:
//
//   - `0x3200` V1-only intra — one byte per macroblock, no flag word.
//   - `0x3000` mixed V1/V4 intra — per-32-MB group a big-endian u32
//     flag word selects V1-vs-V4 per macroblock; index bytes follow
//     in scan order (1 B for V1, 4 B for V4).
//   - `0x3100` inter with skip codes — bit grammar `0` = SKIP, `10`
//     = V1, `11` = V4. Codes may straddle flag-word boundaries; when
//     a leading `1` is the last bit of the current flag word the
//     V1/V4 selector lands at the MSB of the next flag word and the
//     index bytes belong to that next group.
//
// The dispatcher (`decode_vector_chunk`) feeds a `chunk_id` +
// payload + `mb_count` triple through one of three sub-decoders.
// All three error paths are reachable from arbitrary attacker input:
//
//   - 0x3200: payload length must equal `mb_count` (returns
//     `CinepakError::Invalid` otherwise).
//   - 0x3000: payload must fit a flag word every 32 MBs plus 1 B
//     per V1 / 4 B per V4 macroblock; truncation mid-flag-word or
//     mid-index returns `Invalid`.
//   - 0x3100: same plus the pending-selector path — a `1` bit at
//     the very end of a flag word defers the V1/V4 selector to the
//     next flag word; a dangling pending selector at end-of-stream
//     returns `Invalid`. The selector-bit / index-byte ordering
//     across straddles is the bug surface that motivated the
//     `inter_payload_straddle` regression test and the round-5
//     selector-spillover fix.
//
// `decode_frame` already exercises this code path inside a full
// strip, but the fuzzer's mutations have to thread through a frame
// header + strip header + chunk header before they reach the
// vector parser, which throttles per-chunk coverage growth. This
// target lets libFuzzer drive the vector parser directly with
// arbitrary `chunk_id` + `mb_count` + payload triples across all
// three chunk codes — completing the per-parser fuzz coverage
// (frame header / strip header / codebook chunks / vector chunks).
//
// ## OOM cap
//
// `decode_vector_chunk` allocates a `Vec<Mb>` of length `mb_count`;
// each `Mb` is 8 bytes (1 enum tag + 4-byte payload + padding). With
// the 16-bit `mb_count` cap from the fuzz input that bounds the
// allocation at ~512 KiB. The 64 KiB raw-input cap matches the
// sibling targets.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::vector::{
    decode_vector_chunk, VECTOR_CHUNK_INTER, VECTOR_CHUNK_INTRA, VECTOR_CHUNK_V1_ONLY,
};

/// Defence-in-depth raw-input cap; mirrors the sibling targets.
const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Cap on `mb_count` so a malicious 65535 doesn't allocate ~512 KiB
/// of `Mb` slots before the parser returns an error. A real strip is
/// at most `(width/4) * (height/4)` macroblocks; the spec caps
/// width/height at u16, so the per-strip cap of 65536 is the same
/// bound, but the fuzzer mostly cares about parser behaviour at
/// small mb_counts where boundary cases live.
const MAX_MB_COUNT: usize = 4096;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 || data.len() > MAX_INPUT_BYTES {
        return;
    }

    // First two bytes pick a wire chunk-id (big-endian per spec §1);
    // bytes 2..4 pick `mb_count` (big-endian, masked into the cap);
    // the rest is payload (the chunk-header bytes are *not* included
    // here — `decode_vector_chunk` consumes payload only).
    let chunk_id = u16::from_be_bytes([data[0], data[1]]);
    let raw_mb_count = u16::from_be_bytes([data[2], data[3]]) as usize;
    let mb_count = raw_mb_count.min(MAX_MB_COUNT);
    let payload = &data[4..];

    // Path A: drive the explicit chunk_id the fuzzer chose. Anything
    // outside the three vector codes maps to the dispatcher's
    // "unknown vector chunk id" error path — part of the panic-free
    // contract is that the entry point must handle the rejection.
    let _ = decode_vector_chunk(chunk_id, payload, mb_count);

    // Paths B/C/D: re-decode the same payload + mb_count under each
    // of the three vector chunk codes explicitly. The dispatcher
    // routes on chunk_id, but the sub-decoders have distinct
    // arithmetic (stride-vs-payload-length for 0x3200, per-32-group
    // flag/index interleave for 0x3000, bit-grammar with straddle
    // for 0x3100) — each needs its own coverage signal so libFuzzer
    // learns the three boundary structures independently.
    let _ = decode_vector_chunk(VECTOR_CHUNK_V1_ONLY, payload, mb_count);
    let _ = decode_vector_chunk(VECTOR_CHUNK_INTRA, payload, mb_count);
    let _ = decode_vector_chunk(VECTOR_CHUNK_INTER, payload, mb_count);
});
