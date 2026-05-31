// Panic-free fuzz target for the **stateful** Cinepak decode path: a
// single `CinepakDecoder` instance fed a sequence of frames in
// succession, exercising the inter-frame state machine that no
// existing target covers.
//
// Per `docs/video/cinepak/spec/{01-frame-and-strip,02-codebooks,03-vectors-and-macroblocks}.md`
// the decoder carries three pieces of state across `decode_frame`
// calls:
//
//   - `prev_v4` — the V4 codebook from the most-recently-decoded
//     strip. The next strip's selective-update chunk
//     (`0x21`/`0x23`/`0x25`/`0x27`) patches this codebook in place
//     rather than starting from a freshly-allocated table.
//   - `prev_v1` — same as above for V1.
//   - `prev_frame` — the previous frame's reconstructed pixel
//     buffer, plus dimensions and pixel mode (RGB24 vs Gray8). The
//     inter-frame vector chunk `0x3100` emits SKIP macroblocks that
//     copy 4×4 raster blocks straight out of `prev_frame`; missing
//     or mode-mismatched prev_frame state is supposed to fall back
//     cleanly to a black block, not panic.
//
// The single-frame `decode_frame.rs` target instantiates a fresh
// `CinepakDecoder` per input, so it never exercises:
//
//   - Selective-update arithmetic against a non-empty `prev_v4` /
//     `prev_v1` (the path that patches entry `i` only when the
//     selector bit at position `i` of the bitmap is set, leaving
//     other entries inherited from the prior strip).
//   - Skip-macroblock raster copy from a `prev_frame` whose
//     dimensions / pixel mode were chosen by the fuzzer rather than
//     by the current frame's header.
//   - Mid-stream `reset()`-equivalent transitions where the next
//     frame is an intra (`flags & 0x01 == 0`) and must wipe the
//     selective-update inheritance even though prev_frame state is
//     present.
//
// ## Framing
//
// The fuzz input is parsed as a sequence of length-prefixed frame
// slices: each frame is a big-endian u16 length followed by exactly
// that many payload bytes. We cap the total number of frames at 8
// and the per-frame payload at the same 64 KiB raw-input cap the
// sibling targets enforce — the harness exits early on truncation
// instead of fabricating padding bytes (the goal is to drive the
// state machine across legitimate frame boundaries, not to feed
// the decoder synthesised garbage in the gaps).
//
// ## OOM cap
//
// Each frame slice is peeked for its wire width × height per
// `decode_frame.rs`'s 256 × 256 budget and short-circuited above
// that. We also cap `prev_frame` carry-over indirectly by capping
// the number of frames at 8 — even a worst-case 256 × 256 RGB24
// raster is ~196 KiB, so the steady-state memory the harness can
// pin in `prev_frame` is ~196 KiB regardless of how the fuzzer
// shapes the input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::CinepakDecoder;

/// Per-frame coded-pixel budget; mirrors `decode_frame.rs`. At
/// 256 × 256 the RGB24 raster is 256 * 256 * 3 = 196 608 bytes,
/// well under any reasonable runner memory cap, and bounds the
/// `prev_frame` carry-over state symmetrically.
const MAX_CODED_PIXELS: u32 = 256 * 256;

/// Defence-in-depth raw-input cap; mirrors the sibling targets.
const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Cap on the number of frames driven through the single decoder.
/// Eight frames is enough to exercise:
///
///   - Intra → inter → inter (selective-update inheritance chain).
///   - Intra → intra (codebook wipe + re-train).
///   - Inter with no prior intra (degraded inheritance path —
///     decoder must reject before reading uninitialised state).
///   - Frame-mode mismatch (RGB24 frame following a Gray8 frame
///     in the same decoder).
///
/// The cap also bounds total wall-time per fuzz iteration so the
/// per-target run rate stays comparable to the single-frame
/// target's.
const MAX_FRAMES_PER_INPUT: usize = 8;

/// Per-frame payload cap. The full Cinepak wire format limits a
/// single frame to a 24-bit `frame_length` (~16 MiB), but for fuzz
/// purposes the 64 KiB cap above bounds the per-input budget; this
/// constant just makes the per-frame slice size explicit.
const MAX_FRAME_PAYLOAD: usize = MAX_INPUT_BYTES;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() || data.len() > MAX_INPUT_BYTES {
        return;
    }

    let mut dec = CinepakDecoder::new();
    let mut cursor = 0usize;
    let mut frames_fed = 0usize;

    while cursor + 2 <= data.len() && frames_fed < MAX_FRAMES_PER_INPUT {
        // Length prefix — big-endian u16. The frame payload starts
        // right after.
        let len = u16::from_be_bytes([data[cursor], data[cursor + 1]]) as usize;
        cursor += 2;

        if len == 0 {
            // Zero-length frame slice is a degenerate but interesting
            // input — the frame-header parser must reject it without
            // panicking. Feed an empty slice and continue.
            let _ = dec.decode_frame(&[], None);
            frames_fed += 1;
            continue;
        }

        if len > MAX_FRAME_PAYLOAD {
            return;
        }

        if cursor + len > data.len() {
            // Truncated frame — stop. We deliberately do not feed a
            // padded slice; the harness's purpose is to drive the
            // state machine across well-formed frame boundaries.
            return;
        }

        let frame = &data[cursor..cursor + len];
        cursor += len;

        // Peek width × height before letting the decoder allocate a
        // raster. Per spec §1 width is at bytes 4..6, height at 6..8
        // (big-endian u16). Frames shorter than 8 bytes will be
        // rejected by `FrameHeader::parse` cheaply.
        if frame.len() >= 8 {
            let width = u16::from_be_bytes([frame[4], frame[5]]) as u32;
            let height = u16::from_be_bytes([frame[6], frame[7]]) as u32;
            if width != 0 && height != 0 {
                let coded = width.saturating_mul(height);
                if coded > MAX_CODED_PIXELS {
                    // Skip oversized frames but keep iterating —
                    // an oversized frame in the middle of a stream
                    // is a legitimate fuzzer mutation we still want
                    // to observe the decoder's rejection of without
                    // letting it allocate.
                    frames_fed += 1;
                    continue;
                }
            }
        }

        // Drive the decoder. The result is intentionally discarded:
        // success or failure both leave the decoder's internal
        // state consistent and the next iteration must observe that
        // consistency. The contract under test is panic-free /
        // overflow-free; bit-exactness is the suite's job.
        let _ = dec.decode_frame(frame, None);
        frames_fed += 1;
    }

    // After a multi-frame run, also exercise the explicit reset
    // path — both for coverage of `CinepakDecoder::reset` itself
    // and to confirm that resetting between iterations of the
    // harness leaves no per-input cross-talk visible to libFuzzer.
    dec.reset();
});
