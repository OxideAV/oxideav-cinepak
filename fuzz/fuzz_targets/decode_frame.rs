// Panic-free fuzz target for the full Cinepak decode pipeline reached
// through `CinepakDecoder::decode_frame`.
//
// This exercises the chain documented in
// `docs/video/cinepak/spec/{01-frame-and-strip,02-codebooks,03-vectors-and-macroblocks,04-yuv-rgb-matrix}.md`:
// frame-header parse, strip-header parse, codebook chunks
// (`0x20`/`0x22`/`0x24`/`0x26` full + `0x21`/`0x23`/`0x25`/`0x27`
// selective-update), vector chunks (`0x30`/`0x31`/`0x32`),
// macroblock expansion, and RGB24 / Gray8 raster writes.
//
// ## OOM cap
//
// The wire format encodes width and height as `u16`, which means a
// pathological input can request a ~12 GiB RGB24 raster. Peek at
// offsets 4..8 (BE u16 width, BE u16 height — per §1) before invoking
// the decoder and bail out if `width * height` exceeds a small budget.
// libFuzzer will quickly learn the cap and steer mutations under it,
// so this doesn't starve the corpus of useful decode coverage.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::CinepakDecoder;

/// Per-frame coded-pixel budget. At 256 × 256 the RGB24 raster is
/// 256 * 256 * 3 = 196 608 bytes, well under any reasonable runner
/// memory cap.
const MAX_CODED_PIXELS: u32 = 256 * 256;

/// Defence-in-depth raw-input cap; libFuzzer's default is already
/// 4 KiB but stays configurable, so re-enforce in the harness.
const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() < 10 || data.len() > MAX_INPUT_BYTES {
        return;
    }

    // Peek at the wire width / height before letting the decoder
    // allocate. Per spec §1 these live at byte offsets 4..6 and 6..8
    // as big-endian u16 values.
    let width = u16::from_be_bytes([data[4], data[5]]) as u32;
    let height = u16::from_be_bytes([data[6], data[7]]) as u32;
    if width == 0 || height == 0 {
        // The parser will reject it; let it do so cheaply.
        let mut dec = CinepakDecoder::new();
        let _ = dec.decode_frame(data, None);
        return;
    }
    let coded = width.saturating_mul(height);
    if coded > MAX_CODED_PIXELS {
        return;
    }

    let mut dec = CinepakDecoder::new();
    let _ = dec.decode_frame(data, None);
});
