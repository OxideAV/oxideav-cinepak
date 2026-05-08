//! Multi-frame static-fixture wire-size + SKIP-fraction validation
//! (round 4).
//!
//! Drives [`CinepakEncoder`] with five identical RGB frames after one
//! intra keyframe. Asserts:
//!
//! 1. Every frame round-trips through [`CinepakDecoder`] within the
//!    codec's quantisation tolerance.
//! 2. The total wire size of the inter-frame tail is *strictly smaller*
//!    than the equivalent run encoded with the stateless
//!    `encode_rgb24_inter` helper (which always emits full-replace
//!    codebook chunks). Selective-update + chunk-omission wins.
//! 3. The SKIP-MB fraction grows from frame 1 to frame 5 (the first
//!    inter frame may still update many MBs because the encoder's
//!    median-cut codebook differs slightly from the keyframe's; later
//!    frames stabilise as the rolling codebook tracks the input).
//!
//! Wire-format reference: spec §3.4 / §4 / §5 of `02-codebooks.md`
//! describes the chunk-omission / selective-update / full-replace
//! choice; this test exercises the encoder side empirically.
//!
//! The fixture is a 32×32 four-colour-quadrant pattern matching
//! `tests/encoder_roundtrip.rs::rgb24_4_quadrant_roundtrip_32x32`.

#![allow(clippy::needless_range_loop)]

use oxideav_cinepak::{
    encode_rgb24, encode_rgb24_inter, CinepakDecoder, CinepakEncoder, EncoderOptions,
};

const W: usize = 32;
const H: usize = 32;

/// Build the 32×32 four-quadrant RGB fixture.
fn fixture_rgb() -> Vec<u8> {
    let mut rgb = vec![0u8; W * H * 3];
    let colors = [
        (200u8, 30, 30),
        (30, 180, 30),
        (30, 30, 200),
        (180, 180, 60),
    ];
    for r in 0..H {
        for c in 0..W {
            let q = (r / 16) * 2 + (c / 16);
            let off = r * W * 3 + c * 3;
            rgb[off] = colors[q].0;
            rgb[off + 1] = colors[q].1;
            rgb[off + 2] = colors[q].2;
        }
    }
    rgb
}

/// Count SKIP macroblocks in a `0x3100` inter-frame bitstream by
/// re-decoding via [`oxideav_cinepak::vector::decode_vector_chunk`]'s
/// public test helper. We can't access that helper directly from a
/// black-box test, so instead we count the number of `Mb::Skip`-shaped
/// macroblocks indirectly by comparing the decoded pixels to
/// `prev_pixels`: a SKIP'd MB has bit-identical pixels to the prev
/// frame (the decoder copies from the prev reconstruction verbatim).
///
/// This is a behavioural proxy: a non-SKIP MB *might* coincidentally
/// produce the same output bytes (e.g. the codebook entry happens to
/// encode the same colour), but on a coloured 4-quadrant fixture
/// false-positives are negligible.
fn count_skip_mbs(curr_pixels: &[u8], prev_pixels: &[u8], stride: usize) -> usize {
    let mut count = 0;
    let mb_rows = H / 4;
    let mb_cols = W / 4;
    for r in 0..mb_rows {
        for c in 0..mb_cols {
            let mut all_same = true;
            'mb: for dy in 0..4 {
                for dx in 0..4 {
                    let off = (r * 4 + dy) * stride + (c * 4 + dx) * 3;
                    if curr_pixels[off..off + 3] != prev_pixels[off..off + 3] {
                        all_same = false;
                        break 'mb;
                    }
                }
            }
            if all_same {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn static_fixture_5_inter_frames_selective_saves_wire_bytes() {
    let rgb = fixture_rgb();
    // Quality 50 → strip_count=2, codebook size 64. The selective-update
    // win is dramatic on q=50 because two strips × two codebooks = four
    // chunks per frame at full-replace (~4×388 = ~1552 B), whereas the
    // chunk-omission path drops them all on stable frames.
    let opts = EncoderOptions::from_quality(50);

    // --- Stateful encoder (round-4 path) ---
    let mut enc = CinepakEncoder::new();
    let intra = enc.encode_intra(&rgb, W as u32, H as u32, opts).unwrap();
    let mut stateful_inter_total = 0usize;
    let mut stateful_inter_frames: Vec<Vec<u8>> = Vec::new();
    for _ in 0..5 {
        let f = enc.encode_inter(&rgb, W as u32, H as u32, opts).unwrap();
        stateful_inter_total += f.len();
        stateful_inter_frames.push(f);
    }

    // --- Stateless encoder (round-3 path, full-replace every frame) ---
    let mut dec_stateless = CinepakDecoder::new();
    let _ = dec_stateless.decode_frame(&intra, None).unwrap();
    let mut prev = dec_stateless.decode_frame(&intra, None).unwrap();
    let mut stateless_inter_total = 0usize;
    for _ in 0..5 {
        let f = encode_rgb24_inter(&rgb, &prev, W as u32, H as u32, opts).unwrap();
        stateless_inter_total += f.len();
        prev = dec_stateless.decode_frame(&f, None).unwrap();
    }

    // --- Assertion 1: stateful wire size strictly smaller. ---
    assert!(
        stateful_inter_total < stateless_inter_total,
        "selective-update should shrink inter wire size: stateful={stateful_inter_total} vs stateless={stateless_inter_total}"
    );
    let saved = stateless_inter_total - stateful_inter_total;
    let pct = saved as f32 * 100.0 / stateless_inter_total as f32;
    eprintln!(
        "wire-size delta: stateful={stateful_inter_total} stateless={stateless_inter_total} saved={saved} ({pct:.1}%)"
    );

    // The savings should be substantial — at least 25% on this static
    // fixture, since most inter frames omit codebook chunks entirely.
    assert!(pct > 25.0, "expected >25% wire savings, got {pct:.1}%");

    // --- Assertion 2: every frame still round-trips correctly. ---
    let mut dec = CinepakDecoder::new();
    let f0 = dec.decode_frame(&intra, None).unwrap();
    assert_eq!(f0.width, 32);
    assert_eq!(f0.height, 32);
    let mut last_pixels: Vec<u8> = f0.pixels().to_vec();
    let stride = f0.stride();
    let mut skip_counts: Vec<usize> = Vec::new();
    for f in &stateful_inter_frames {
        let img = dec.decode_frame(f, None).unwrap();
        assert_eq!(img.width, 32);
        assert_eq!(img.height, 32);
        let p = img.pixels();
        // Pixel quadrants should still match the source within the
        // codebook quantisation tolerance.
        for q in 0..4 {
            let qr = (q / 2) * 16 + 7;
            let qc = (q % 2) * 16 + 7;
            let off = qr * stride + qc * 3;
            let src_off = qr * W * 3 + qc * 3;
            for ch in 0..3 {
                let actual = p[off + ch] as i32;
                let expected = rgb[src_off + ch] as i32;
                assert!(
                    (actual - expected).abs() <= 12,
                    "ch={ch} got {actual} expected {expected}"
                );
            }
        }
        let n_skip = count_skip_mbs(p, &last_pixels, stride);
        skip_counts.push(n_skip);
        last_pixels.clear();
        last_pixels.extend_from_slice(p);
    }

    // --- Assertion 3: SKIP fraction grows (or saturates). ---
    // Frame 0..4 (5 inter frames). On a perfectly-static fixture the
    // last inter frame should be all-SKIP (8×8 = 64 macroblocks).
    let total_mbs = (W / 4) * (H / 4);
    eprintln!("skip counts per inter frame: {skip_counts:?} (total mbs = {total_mbs})");
    // At least the LAST frame should be all-skip.
    assert_eq!(
        *skip_counts.last().unwrap(),
        total_mbs,
        "last inter frame should be entirely SKIP on a static fixture"
    );
    // Frame 4 (last) should have at least as many SKIPs as frame 0.
    assert!(
        skip_counts[4] >= skip_counts[0],
        "skip fraction should grow or saturate: {skip_counts:?}"
    );
}

/// Encoder reset: after `reset()`, the next call must succeed via
/// `encode_intra`, and `encode_inter` is rejected.
#[test]
fn encoder_reset_invalidates_prev_frame() {
    let rgb = fixture_rgb();
    let mut enc = CinepakEncoder::new();
    let _ = enc
        .encode_intra(&rgb, W as u32, H as u32, EncoderOptions::default())
        .unwrap();
    enc.reset();
    let r = enc.encode_inter(&rgb, W as u32, H as u32, EncoderOptions::default());
    assert!(r.is_err(), "encode_inter after reset must fail");
}

/// Encoder rejects `encode_inter` before any `encode_intra`.
#[test]
fn encode_inter_before_intra_rejected() {
    let rgb = fixture_rgb();
    let mut enc = CinepakEncoder::new();
    let r = enc.encode_inter(&rgb, W as u32, H as u32, EncoderOptions::default());
    assert!(r.is_err());
}

/// `CinepakEncoder` round-trip: an intra+inter pair decodes to images
/// of the right dimensions. Smoke-test with a 16×16 frame using the
/// default options (single strip).
#[test]
fn cinepak_encoder_intra_inter_roundtrip_16x16() {
    let w = 16usize;
    let h = 16usize;
    let rgb = vec![128u8; w * h * 3];
    let mut enc = CinepakEncoder::new();
    let intra_bytes = enc
        .encode_intra(&rgb, w as u32, h as u32, EncoderOptions::default())
        .unwrap();
    let inter_bytes = enc
        .encode_inter(&rgb, w as u32, h as u32, EncoderOptions::default())
        .unwrap();

    let mut dec = CinepakDecoder::new();
    let f1 = dec.decode_frame(&intra_bytes, None).unwrap();
    let f2 = dec.decode_frame(&inter_bytes, None).unwrap();
    assert_eq!(f1.width, w as u32);
    assert_eq!(f2.width, w as u32);
    // On a perfectly-uniform fixture the inter frame should be smaller
    // than the intra frame (mostly SKIP codes).
    assert!(
        inter_bytes.len() < intra_bytes.len(),
        "inter ({}B) should be smaller than intra ({}B) for uniform fixture",
        inter_bytes.len(),
        intra_bytes.len()
    );
    // Sanity: a 64-entry codebook + tiny vector chunks should bottom
    // out at ~600B for intra, ~30B for inter on this fixture.
    assert!(intra_bytes.len() < 1000);
    assert!(inter_bytes.len() < 200);
}

/// Sanity: the stateless `encode_rgb24` path is unchanged by round-4
/// (it never had selective-update support).
#[test]
fn stateless_intra_unchanged() {
    let rgb = fixture_rgb();
    let bytes = encode_rgb24(&rgb, W as u32, H as u32, EncoderOptions::default()).unwrap();
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    assert_eq!(f.width, 32);
}
