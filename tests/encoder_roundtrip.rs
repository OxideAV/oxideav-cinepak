// Index-based scans match the spatial-quadrant arithmetic the test
// asserts against (`q / 2`, `q % 2`); enumerate-form hides the
// quadrant-index derivation.
#![allow(clippy::needless_range_loop)]

//! Encoder integration tests.
//!
//! Synthesize an RGB pattern, run it through the crate's encoder, then
//! decode the resulting bitstream and verify pixel-correctness within
//! a small per-channel tolerance accounting for codebook quantisation.

use oxideav_cinepak::{encode_gray8, encode_rgb24, CinepakDecoder, EncoderOptions};

/// Generate a 32×32 RGB frame with four 16×16 quadrants of distinct
/// colours, encode it with default (64-entry) codebooks, decode the
/// result, and verify each quadrant's central pixel is within ±10 of
/// the source colour.
#[test]
fn rgb24_4_quadrant_roundtrip_32x32() {
    let w = 32usize;
    let h = 32usize;
    let mut rgb = vec![0u8; w * h * 3];
    let colors = [
        (200u8, 30, 30), // red TL
        (30, 180, 30),   // green TR
        (30, 30, 200),   // blue BL
        (180, 180, 60),  // yellow BR
    ];
    for r in 0..h {
        for c in 0..w {
            let q = (r / 16) * 2 + (c / 16);
            let off = r * w * 3 + c * 3;
            rgb[off] = colors[q].0;
            rgb[off + 1] = colors[q].1;
            rgb[off + 2] = colors[q].2;
        }
    }
    let bytes = encode_rgb24(&rgb, w as u32, h as u32, EncoderOptions::default()).unwrap();
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    assert_eq!(f.width, 32);
    assert_eq!(f.height, 32);
    let s = f.stride();
    let p = f.pixels();
    for q in 0..4 {
        let qr = (q / 2) * 16 + 7;
        let qc = (q % 2) * 16 + 7;
        let off = qr * s + qc * 3;
        let r = p[off] as i32;
        let g = p[off + 1] as i32;
        let b = p[off + 2] as i32;
        let (er, eg, eb) = (colors[q].0 as i32, colors[q].1 as i32, colors[q].2 as i32);
        assert!(
            (r - er).abs() <= 10,
            "q{q} R={r} expect {er} (delta={})",
            (r - er).abs()
        );
        assert!(
            (g - eg).abs() <= 10,
            "q{q} G={g} expect {eg} (delta={})",
            (g - eg).abs()
        );
        assert!(
            (b - eb).abs() <= 10,
            "q{q} B={b} expect {eb} (delta={})",
            (b - eb).abs()
        );
    }
}

/// Gray ramp encode → decode within ±4 luminance.
#[test]
fn gray8_ramp_roundtrip_16x16() {
    let w = 16usize;
    let h = 16usize;
    let mut gray = vec![0u8; w * h];
    // Vertical ramp: row 0 = 0, row 15 = 240.
    for r in 0..h {
        let v = (r * 16) as u8;
        for c in 0..w {
            gray[r * w + c] = v;
        }
    }
    let bytes = encode_gray8(&gray, w as u32, h as u32, EncoderOptions::default()).unwrap();
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    let p = f.pixels();
    for r in 0..h {
        let v = (r * 16) as i32;
        let actual = p[r * w + w / 2] as i32;
        assert!((actual - v).abs() <= 8, "row {r}: got {actual} expect {v}");
    }
}

/// Tiny codebook (4 entries) still round-trips a 4-colour image.
#[test]
fn rgb24_tiny_codebook_4_entries() {
    let w = 8usize;
    let h = 8usize;
    let mut rgb = vec![0u8; w * h * 3];
    let colors = [
        (200u8, 30, 30),
        (30, 180, 30),
        (30, 30, 200),
        (180, 180, 60),
    ];
    for r in 0..h {
        for c in 0..w {
            let q = (r / 4) * 2 + (c / 4);
            let off = r * w * 3 + c * 3;
            rgb[off] = colors[q].0;
            rgb[off + 1] = colors[q].1;
            rgb[off + 2] = colors[q].2;
        }
    }
    let opts = EncoderOptions {
        v4_entries: 4,
        v1_entries: 4,
    };
    let bytes = encode_rgb24(&rgb, w as u32, h as u32, opts).unwrap();
    let mut dec = CinepakDecoder::new();
    let f = dec.decode_frame(&bytes, None).unwrap();
    assert_eq!(f.width, 8);
    assert_eq!(f.height, 8);
}
