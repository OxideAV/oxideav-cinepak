// Panic-free fuzz target for the Cinepak encoder + encode→decode
// roundtrip.
//
// The decode/parse side already has dedicated targets
// (`decode_frame`, `decode_vector_chunk`, `codebook_chunk_apply`, …);
// this target drives the *encoder* with attacker-shaped pixel buffers,
// dimensions, and option knobs, then feeds every encoder output back
// through `CinepakDecoder` to assert the encoder always produces a
// stream the in-crate decoder accepts without panicking.
//
// Why this matters for the encoder's vector-code dispatch
// (`docs/video/cinepak/spec/03-vectors-and-macroblocks.md` §8): the
// intra path may emit `0x3200` (all-V1) or `0x3000` (mixed), and a
// skip-free inter strip is now routed to `0x3200` / `0x3000` instead
// of `0x3100`. Fuzzing arbitrary content across both pixel modes and
// both intra/inter paths exercises every branch of that dispatch and
// confirms each branch self-decodes.
//
// ## Input layout
//
// Bytes 0..4 are a control header:
//   [0]      mode/path selector (low bits).
//   [1]      quality 0..=255 → folded to 0..=100 for `from_quality`.
//   [2]      mb-columns nibble + mb-rows nibble (1..=16 each ⇒ ≤ 64 px).
//   [3]      strip_count seed (1..=mb_rows after clamp).
// The remaining bytes seed the pixel buffer (tiled if short).
//
// ## Resource caps
//
// Dimensions are capped at 64×64 px (256 macroblocks) so the encoder's
// codebook trainer + RD work stays bounded per input; the raw input is
// capped at 16 KiB. These keep libFuzzer iterations fast and prevent
// the trainer from dominating the fuzzing budget.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_cinepak::{
    encode_gray8, encode_rgb24, encode_rgb24_inter, CinepakDecoder, EncoderOptions,
};

const MAX_INPUT_BYTES: usize = 16 * 1024;
/// Cap on macroblocks-per-axis (× 4 px). 16 ⇒ 64 px ⇒ ≤ 256 MBs.
const MAX_MB_AXIS: u32 = 16;

/// Fill `buf` by tiling `seed` (or zero when `seed` is empty).
fn fill(buf: &mut [u8], seed: &[u8]) {
    if seed.is_empty() {
        return;
    }
    for (i, b) in buf.iter_mut().enumerate() {
        *b = seed[i % seed.len()];
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 || data.len() > MAX_INPUT_BYTES {
        return;
    }

    let sel = data[0];
    let quality = ((data[1] as u32 * 100) / 255) as u8;
    // 4-bit nibbles, each mapped into 1..=MAX_MB_AXIS macroblocks.
    let mb_cols = ((data[2] >> 4) as u32 % MAX_MB_AXIS) + 1;
    let mb_rows = ((data[2] & 0x0f) as u32 % MAX_MB_AXIS) + 1;
    let width = mb_cols * 4;
    let height = mb_rows * 4;
    let strip_seed = (data[3] as u32 % mb_rows) + 1;
    let seed = &data[4..];

    // Two option flavours: the validated single-knob `from_quality`
    // mapping, and a hand-built set that stresses the strip_count /
    // skip_threshold edges. Both must be rejected-or-accepted without
    // panic.
    let opts = if sel & 1 == 0 {
        EncoderOptions::from_quality(quality)
    } else {
        EncoderOptions {
            v4_entries: ((quality as u16) % 256) + 1,
            v1_entries: ((data[1] as u16) % 256) + 1,
            strip_count: strip_seed as u16,
            skip_threshold: (data[1] as f32) / 4.0,
            ..EncoderOptions::default()
        }
    };

    // RGB24 path (3 bytes/pixel).
    if sel & 2 == 0 {
        let mut rgb = vec![0u8; (width * height * 3) as usize];
        fill(&mut rgb, seed);
        if let Ok(bytes) = encode_rgb24(&rgb, width, height, opts) {
            // The encoder's own output must round-trip cleanly.
            let mut dec = CinepakDecoder::new();
            let prev = dec.decode_frame(&bytes, None).expect("encoder output must decode");
            assert_eq!(prev.width, width);
            assert_eq!(prev.height, height);

            // Inter path against the just-decoded previous frame: mutate
            // the pixels so a mix of skip / V1 / V4 / fresh-recode strips
            // is produced, exercising the skip-free inter dispatch.
            let mut rgb2 = rgb.clone();
            for (i, p) in rgb2.iter_mut().enumerate() {
                if i % 3 == 0 {
                    *p = p.wrapping_add((seed.get(i % seed.len().max(1)).copied().unwrap_or(0)) ^ 0x55);
                }
            }
            if let Ok(inter) = encode_rgb24_inter(&rgb2, &prev, width, height, opts) {
                let f = dec.decode_frame(&inter, None).expect("inter output must decode");
                assert_eq!(f.width, width);
                assert_eq!(f.height, height);
            }
        }
    } else {
        // Gray8 path (1 byte/pixel).
        let mut gray = vec![0u8; (width * height) as usize];
        fill(&mut gray, seed);
        if let Ok(bytes) = encode_gray8(&gray, width, height, opts) {
            let mut dec = CinepakDecoder::new();
            let f = dec.decode_frame(&bytes, None).expect("gray encoder output must decode");
            assert_eq!(f.width, width);
            assert_eq!(f.height, height);
        }
    }
});
