// Panic-free fuzz target for `CinepakDecoder::decode_deviant_frame`.
//
// Cinepak's deviant variants — Sega Saturn / Sega CD `'cvid'` framing
// and the Lemmings 3DO 6-byte prefix — are documented in
// `docs/video/cinepak/spec/01-frame-and-strip.md` §1 and reproduced
// behaviourally in [`DeviantConfig`]. They diverge from the standard
// path in three ways:
//
//   1. The strip prefix is `10 + extra_header_bytes` instead of 10.
//   2. The codec-header `frame_length` field undercounts the real
//      frame size by `frame_length_short_by` bytes (the slice's
//      `bytes.len()` is authoritative).
//   3. Codebook chunks may carry trailing pad bytes; the chunk parser
//      tolerates non-divisible payload sizes (`tolerate_codebook_pad`).
//
// The existing `decode_frame` fuzz target only exercises the strict
// standard path. This target loops over the three documented
// [`DeviantConfig`] permutations (`saturn`, `lemmings_3do`, and the
// standard control under `decode_frame`) so libFuzzer can find inputs
// that misbehave only along the deviant header-prefix / frame-length /
// codebook-pad branches.
//
// ## OOM cap
//
// Same approach as `decode_frame.rs`: peek width / height from the
// standard frame-header offsets 4..8 (BE u16 each) before the decoder
// touches the raster allocator, and bail out if `width * height`
// exceeds the per-frame coded-pixel budget. The deviant header still
// starts with the standard 10-byte frame header — the extra bytes sit
// *between* the frame header and the first strip header — so the
// offsets are identical.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::{CinepakDecoder, DeviantConfig};

/// Per-frame coded-pixel budget. Matches `decode_frame.rs` so the two
/// targets share OOM behaviour and libFuzzer's corpus-shaping learns
/// the same cap.
const MAX_CODED_PIXELS: u32 = 256 * 256;

/// Defence-in-depth raw-input cap; mirrors `decode_frame.rs`.
const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() < 10 || data.len() > MAX_INPUT_BYTES {
        return;
    }

    // Peek the wire width / height before letting the decoder allocate.
    // Per spec §1 these live at byte offsets 4..6 and 6..8 as
    // big-endian u16 values. Identical for standard + deviant — the
    // deviant prefix bytes follow the 10-byte standard header.
    let width = u16::from_be_bytes([data[4], data[5]]) as u32;
    let height = u16::from_be_bytes([data[6], data[7]]) as u32;
    if width == 0 || height == 0 {
        // Let the parser reject it cheaply across all three variants.
        let mut dec = CinepakDecoder::new();
        let _ = dec.decode_deviant_frame(data, None, DeviantConfig::saturn());
        let _ = dec.decode_deviant_frame(data, None, DeviantConfig::lemmings_3do());
        let _ = dec.decode_frame(data, None);
        return;
    }
    let coded = width.saturating_mul(height);
    if coded > MAX_CODED_PIXELS {
        return;
    }

    // Saturn / Sega CD variant: 2 extra header bytes, frame_length
    // short by 8, codebook pad tolerated.
    let mut dec = CinepakDecoder::new();
    let _ = dec.decode_deviant_frame(data, None, DeviantConfig::saturn());

    // Lemmings 3DO variant: 6 extra header bytes, frame_length short
    // by 8, codebook pad tolerated. Fresh decoder so cross-variant
    // carry-over state doesn't mask a bug specific to one branch.
    let mut dec = CinepakDecoder::new();
    let _ = dec.decode_deviant_frame(data, None, DeviantConfig::lemmings_3do());

    // Standard-path control under the same harness shape, so libFuzzer
    // can compare branch coverage and learn inputs that exercise the
    // deviant-vs-standard divergence directly.
    let mut dec = CinepakDecoder::new();
    let _ = dec.decode_frame(data, None);
});
